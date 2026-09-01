//! Parses the companion server's SSE wire format: `data: {json}\n\n` frames,
//! with `:hb\n\n` heartbeat comments every `SSE_HEARTBEAT` (`server.rs`).
//!
//! Pure over any `Read` -- no socket here, so it is tested exhaustively
//! against byte slices in this module's own tests. [`stream`](super::stream)
//! is what wraps a live `TcpStream` around this and adds the liveness
//! bounds; this module only knows how to find frame boundaries in bytes.

use std::io::Read;

/// Cap on a single buffered line before a `\n` is found. Matches
/// [`MAX_FRAME`] in magnitude: a real frame is one `data:` line, so the two
/// caps coincide for the traffic this client actually sees; kept as two
/// named constants because they guard two different things (an
/// unterminated line vs. an accumulated event) and a future multi-line
/// event should not have to reconsider both at once.
pub const MAX_LINE: usize = 1024 * 1024;

/// Cap on one event's accumulated `data:` payload (joined across however
/// many `data:` lines make up the event, per the SSE spec). Sized to match
/// `companion::hub::MAX_SERIALIZED` -- the server itself never emits a
/// snapshot JSON body past that cap, swapping in a small `"grid too large"`
/// error payload instead, so a real peer never trips this; a peer that does
/// is refused rather than buffered forever.
pub const MAX_FRAME: usize = 1024 * 1024;

const READ_CHUNK: usize = 4096;

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum FrameError {
    /// A single line, or one event's accumulated `data:` payload, exceeded
    /// its cap. The stream must be treated as ended, not resumed: a
    /// truncated frame would parse as malformed JSON anyway, so silently
    /// truncating would just trade a loud, honest error for a confusing
    /// one further down the line.
    TooLarge,
    /// The underlying reader failed for a reason other than the caps
    /// above. For a live socket this is where a caller distinguishes a
    /// read timeout (see `is_timeout` in `peer_client`) from a genuine
    /// I/O failure.
    Io(std::io::Error),
}

/// Wraps a reader and yields one complete `data:` payload per
/// [`next_frame`](Self::next_frame) call, silently skipping `:`-prefixed
/// heartbeat/comment lines and any other line that is not a `data:` field
/// (unknown SSE fields are ignored, matching the spec).
///
/// Carries a byte buffer across calls so a frame split across multiple
/// underlying `read()`s -- the normal case for a live socket -- is
/// reassembled correctly regardless of where the split lands.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct FrameReader<R> {
    reader: R,
    buf: Vec<u8>,
    data_accum: Vec<u8>,
    has_data: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<R: Read> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            data_accum: Vec::new(),
            has_data: false,
        }
    }

    /// Seeds the internal buffer with bytes the caller already read from
    /// `reader` before handing it to this reader -- e.g. body bytes that
    /// arrived in the same `read()` as the tail of an HTTP header block.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Mutable access to the wrapped reader, so a caller holding a live
    /// socket can retune its read timeout (e.g. for a rolling idle gap)
    /// between calls without this type needing to know anything about
    /// timeouts itself.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Reads and parses until one complete `data:` event is assembled,
    /// skipping any number of heartbeats/comments/unknown fields along the
    /// way. Returns `Ok(None)` if the underlying reader hit a clean EOF
    /// before completing an event -- including with a partial event
    /// already buffered, which is discarded rather than returned, per this
    /// module's doc comment.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            while let Some(line) = self.take_line() {
                if let Some(rest) = line.strip_prefix(b"data:") {
                    let val = match rest.first() {
                        Some(b' ') => &rest[1..],
                        _ => rest,
                    };
                    if self.has_data {
                        self.data_accum.push(b'\n');
                    }
                    self.data_accum.extend_from_slice(val);
                    self.has_data = true;
                    if self.data_accum.len() > MAX_FRAME {
                        return Err(FrameError::TooLarge);
                    }
                } else if line.is_empty() {
                    if self.has_data {
                        let payload = std::mem::take(&mut self.data_accum);
                        self.has_data = false;
                        return Ok(Some(payload));
                    }
                    // A blank line with no pending data: either a bare
                    // heartbeat's dispatch or a stray blank. Nothing to
                    // yield; keep looking.
                }
                // Any other line (a `:` comment, or an unrecognized SSE
                // field) is ignored outright, per the SSE spec.
            }

            if self.buf.len() > MAX_LINE {
                return Err(FrameError::TooLarge);
            }

            let mut chunk = [0u8; READ_CHUNK];
            match self.reader.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(FrameError::Io(e)),
            }
        }
    }

    /// Pulls one `\n`-terminated line out of the front of `buf`, stripping
    /// a trailing `\r` so CRLF and bare-LF sources both work. `None` when
    /// no complete line is buffered yet.
    fn take_line(&mut self) -> Option<Vec<u8>> {
        let pos = self.buf.iter().position(|&b| b == b'\n')?;
        let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
        line.pop(); // trailing \n
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;

    /// A reader that hands back exactly one pre-chopped chunk per `read()`
    /// call, so a test can force a frame to split across read boundaries
    /// at an exact, chosen byte -- something a single in-memory `Cursor`
    /// can never exercise, since it always returns everything asked for in
    /// one call.
    struct Dribble<'a> {
        chunks: VecDeque<&'a [u8]>,
    }

    impl<'a> Dribble<'a> {
        fn new(chunks: Vec<&'a [u8]>) -> Self {
            Self {
                chunks: chunks.into(),
            }
        }
    }

    impl<'a> Read for Dribble<'a> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    buf[..chunk.len()].copy_from_slice(chunk);
                    Ok(chunk.len())
                }
                None => Ok(0),
            }
        }
    }

    #[test]
    fn a_well_formed_frame_yields_its_payload() {
        let mut r = FrameReader::new(Cursor::new(b"data: {\"a\":1}\n\n".as_slice()));
        assert_eq!(r.next_frame().unwrap(), Some(b"{\"a\":1}".to_vec()));
    }

    #[test]
    fn a_heartbeat_comment_yields_nothing_and_does_not_end_the_stream() {
        // Standalone heartbeat, then EOF: a clean, non-error "nothing to
        // report" -- not a crash, not a fabricated payload.
        let mut alone = FrameReader::new(Cursor::new(b":hb\n\n".as_slice()));
        assert_eq!(alone.next_frame().unwrap(), None);

        // A heartbeat followed by a real frame: the heartbeat must not
        // stop the reader from reaching the frame that comes after it.
        let mut then_frame =
            FrameReader::new(Cursor::new(b":hb\n\ndata: {\"a\":1}\n\n".as_slice()));
        assert_eq!(
            then_frame.next_frame().unwrap(),
            Some(b"{\"a\":1}".to_vec())
        );
    }

    #[test]
    fn two_frames_back_to_back_yield_both_in_order() {
        let mut r = FrameReader::new(Cursor::new(
            b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\n".as_slice(),
        ));
        assert_eq!(r.next_frame().unwrap(), Some(b"{\"a\":1}".to_vec()));
        assert_eq!(r.next_frame().unwrap(), Some(b"{\"a\":2}".to_vec()));
    }

    #[test]
    fn a_frame_split_across_read_boundaries_is_reassembled() {
        // Three separate read() calls: mid-value, mid-line, and the final
        // blank line, none of which land on a convenient boundary.
        let mut r = FrameReader::new(Dribble::new(vec![b"data: {\"a\":", b"1}\n", b"\n"]));
        assert_eq!(r.next_frame().unwrap(), Some(b"{\"a\":1}".to_vec()));
    }

    #[test]
    fn a_frame_larger_than_the_cap_is_refused_rather_than_buffered_forever() {
        let huge = vec![b'x'; MAX_FRAME + 1];
        let mut input = Vec::new();
        input.extend_from_slice(b"data: ");
        input.extend_from_slice(&huge);
        input.extend_from_slice(b"\n\n");
        let mut r = FrameReader::new(Cursor::new(input.as_slice()));
        assert!(matches!(r.next_frame(), Err(FrameError::TooLarge)));
    }

    #[test]
    fn a_stream_that_ends_mid_frame_yields_nothing() {
        let mut r = FrameReader::new(Cursor::new(b"data: {\"a\":1".as_slice()));
        assert_eq!(r.next_frame().unwrap(), None);
    }

    #[test]
    fn crlf_and_lf_line_endings_both_work() {
        let mut crlf = FrameReader::new(Cursor::new(b"data: {\"a\":1}\r\n\r\n".as_slice()));
        assert_eq!(crlf.next_frame().unwrap(), Some(b"{\"a\":1}".to_vec()));

        let mut lf = FrameReader::new(Cursor::new(b"data: {\"a\":1}\n\n".as_slice()));
        assert_eq!(lf.next_frame().unwrap(), Some(b"{\"a\":1}".to_vec()));
    }
}
