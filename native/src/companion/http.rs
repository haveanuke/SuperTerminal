//! Minimal HTTP/1.1 request parser with hard limits. Tailnet peers are
//! authenticated devices but still untrusted input: everything is capped,
//! ambiguity (duplicate lengths, transfer encodings, absolute-form targets)
//! is rejected rather than interpreted, and one ordinary request is served
//! per connection.

use std::io::BufRead;

pub const MAX_REQUEST_LINE: usize = 2048;
pub const MAX_HEADER_BYTES: usize = 8192;
pub const MAX_HEADER_COUNT: usize = 64;
pub const MAX_BODY: usize = 4096;
pub const MAX_SEGMENT: usize = 64;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, PartialEq)]
pub struct Request {
    pub method: Method,
    /// Percent-decoded path, query stripped.
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Names lowercased; values trimmed.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    BadRequest(&'static str),
    TooLarge,
    /// Method we do not serve (includes OPTIONS — rejected by design).
    Unsupported,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one CRLF-terminated line with a byte cap (cap excludes the CRLF).
fn read_line_capped(reader: &mut impl BufRead, cap: usize) -> Result<String, ParseError> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match std::io::Read::read_exact(reader, &mut byte) {
            Ok(()) => {}
            Err(_) => return Err(ParseError::BadRequest("truncated")),
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return String::from_utf8(line).map_err(|_| ParseError::BadRequest("not utf-8"));
        }
        if line.len() >= cap {
            return Err(ParseError::TooLarge);
        }
        line.push(byte[0]);
    }
}

fn percent_decode(raw: &str) -> Result<String, ParseError> {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16));
                let lo = bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => out.push((hi * 16 + lo) as u8),
                    _ => return Err(ParseError::BadRequest("bad percent-encoding")),
                }
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    if out.iter().any(|b| *b < 0x20 || *b == 0x7f) {
        return Err(ParseError::BadRequest("control bytes in target"));
    }
    String::from_utf8(out).map_err(|_| ParseError::BadRequest("target not utf-8"))
}

pub fn parse_request(reader: &mut impl BufRead) -> Result<Request, ParseError> {
    let request_line = read_line_capped(reader, MAX_REQUEST_LINE)?;
    let mut parts = request_line.split(' ');
    let method = match parts.next() {
        Some("GET") => Method::Get,
        Some("POST") => Method::Post,
        _ => return Err(ParseError::Unsupported),
    };
    let target = parts.next().ok_or(ParseError::BadRequest("no target"))?;
    if !target.starts_with('/') {
        return Err(ParseError::BadRequest("absolute-form target"));
    }
    let (raw_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let path = percent_decode(raw_path)?;
    if path.split('/').any(|segment| segment.len() > MAX_SEGMENT) {
        return Err(ParseError::BadRequest("path segment too long"));
    }
    let mut query = Vec::new();
    if let Some(raw_query) = raw_query {
        for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            query.push((percent_decode(name)?, percent_decode(value)?));
        }
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut header_bytes = 0usize;
    loop {
        let line = read_line_capped(reader, MAX_HEADER_BYTES)?;
        if line.is_empty() {
            break;
        }
        header_bytes += line.len();
        if header_bytes > MAX_HEADER_BYTES || headers.len() >= MAX_HEADER_COUNT {
            return Err(ParseError::TooLarge);
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(ParseError::BadRequest("header without colon"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    if headers.iter().any(|(n, _)| n == "transfer-encoding") {
        return Err(ParseError::BadRequest("transfer-encoding not served"));
    }
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(n, _)| n == "content-length")
        .map(|(_, v)| v.as_str())
        .collect();
    let body = match method {
        Method::Get => {
            if !lengths.is_empty() {
                return Err(ParseError::BadRequest("body on GET"));
            }
            Vec::new()
        }
        Method::Post => {
            if lengths.len() != 1 {
                return Err(ParseError::BadRequest(
                    "exactly one content-length required",
                ));
            }
            let len: usize = lengths[0]
                .parse()
                .map_err(|_| ParseError::BadRequest("bad content-length"))?;
            if len > MAX_BODY {
                return Err(ParseError::TooLarge);
            }
            let mut body = vec![0u8; len];
            std::io::Read::read_exact(reader, &mut body)
                .map_err(|_| ParseError::BadRequest("short body"))?;
            body
        }
    };

    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(raw: &str) -> Result<Request, ParseError> {
        parse_request(&mut Cursor::new(raw.as_bytes().to_vec()))
    }

    #[test]
    fn simple_get_parses_path_and_query() {
        let req = parse("GET /stream/term-1?t=abc&x=1%202 HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.path, "/stream/term-1");
        assert_eq!(req.query_param("t"), Some("abc"));
        assert_eq!(req.query_param("x"), Some("1 2"));
        assert!(req.body.is_empty());
    }

    #[test]
    fn header_names_lowercase_values_trimmed() {
        let req =
            parse("GET / HTTP/1.1\r\nHoSt:  my-host:1\r\nX-Companion-Token: tok\r\n\r\n").unwrap();
        assert_eq!(req.header("host"), Some("my-host:1"));
        assert_eq!(req.header("x-companion-token"), Some("tok"));
    }

    #[test]
    fn post_with_exact_content_length_reads_body() {
        let req =
            parse("POST /input/x HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\n\r\nBODY").unwrap();
        assert_eq!(req.body, b"BODY");
    }

    #[test]
    fn post_without_content_length_is_rejected() {
        assert!(matches!(
            parse("POST /input/x HTTP/1.1\r\nHost: h\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
    }

    #[test]
    fn duplicate_or_conflicting_content_length_rejected() {
        let raw = "POST / HTTP/1.1\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\nBODY";
        assert!(matches!(parse(raw), Err(ParseError::BadRequest(_))));
        let raw2 = "POST / HTTP/1.1\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\nBODY";
        assert!(matches!(parse(raw2), Err(ParseError::BadRequest(_))));
    }

    #[test]
    fn any_transfer_encoding_rejected() {
        let raw = "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(matches!(parse(raw), Err(ParseError::BadRequest(_))));
    }

    #[test]
    fn oversized_request_line_rejected() {
        let raw = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_REQUEST_LINE));
        assert_eq!(parse(&raw), Err(ParseError::TooLarge));
    }

    #[test]
    fn oversized_headers_rejected() {
        let raw = format!(
            "GET / HTTP/1.1\r\nX-Big: {}\r\n\r\n",
            "v".repeat(MAX_HEADER_BYTES)
        );
        assert_eq!(parse(&raw), Err(ParseError::TooLarge));
    }

    #[test]
    fn too_many_headers_rejected() {
        let mut raw = String::from("GET / HTTP/1.1\r\n");
        for i in 0..(MAX_HEADER_COUNT + 1) {
            raw.push_str(&format!("X-{i}: v\r\n"));
        }
        raw.push_str("\r\n");
        assert_eq!(parse(&raw), Err(ParseError::TooLarge));
    }

    #[test]
    fn oversized_body_rejected() {
        let raw = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert_eq!(parse(&raw), Err(ParseError::TooLarge));
    }

    #[test]
    fn oversized_path_segment_rejected() {
        let raw = format!(
            "GET /input/{} HTTP/1.1\r\n\r\n",
            "s".repeat(MAX_SEGMENT + 1)
        );
        assert!(matches!(parse(&raw), Err(ParseError::BadRequest(_))));
    }

    #[test]
    fn absolute_form_target_rejected() {
        assert!(matches!(
            parse("GET http://evil/ HTTP/1.1\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
    }

    #[test]
    fn malformed_percent_encoding_rejected() {
        assert!(matches!(
            parse("GET /a%zz HTTP/1.1\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
    }

    #[test]
    fn control_bytes_in_target_rejected() {
        assert!(matches!(
            parse("GET /a%00b HTTP/1.1\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
        assert!(matches!(
            parse("GET /a%0db HTTP/1.1\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
    }

    #[test]
    fn unsupported_methods_rejected() {
        for m in ["OPTIONS", "PUT", "DELETE", "HEAD", "PATCH", "TRACE"] {
            assert_eq!(
                parse(&format!("{m} / HTTP/1.1\r\n\r\n")),
                Err(ParseError::Unsupported),
                "{m} must be Unsupported"
            );
        }
    }

    #[test]
    fn header_without_colon_rejected() {
        assert!(matches!(
            parse("GET / HTTP/1.1\r\nBadHeader\r\n\r\n"),
            Err(ParseError::BadRequest(_))
        ));
    }
}
