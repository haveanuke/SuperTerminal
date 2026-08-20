use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::shell_env;

// Sanity bounds against malformed IPC payloads — not a security boundary.
// The renderer already has arbitrary local exec via pty_write, so locking down
// the binary here would be theater. Args are spawned without a shell so
// metacharacters in args can't escape.
const MAX_ARGS: usize = 16;
const MAX_ARG_LEN: usize = 4096;
const MAX_TEXT_LEN: usize = 280;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuddyRequest {
    pub command: String,
    pub args: Vec<String>,
    pub prompt: String,
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct BuddyResult {
    pub ok: bool,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BuddyResult {
    fn fail(error: &str) -> Self {
        Self {
            ok: false,
            text: String::new(),
            error: Some(error.to_string()),
        }
    }
}

/// Strip CSI escape sequences: ESC '[' [0-9;?]* final-letter.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            while let Some(&p) = chars.peek() {
                if p.is_ascii_digit() || p == ';' || p == '?' {
                    chars.next();
                } else {
                    break;
                }
            }
            if chars.peek().is_some_and(|p| p.is_ascii_alphabetic()) {
                chars.next(); // consume final byte
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// First meaningful block of stdout: ANSI-stripped, first paragraph,
/// whitespace collapsed, capped at 280 chars.
pub fn clean_output(stdout: &str) -> String {
    let cleaned = strip_ansi(stdout);
    let trimmed = cleaned.trim();
    // First block = up to the first blank (whitespace-only) line.
    let mut first_block_lines: Vec<&str> = Vec::new();
    for line in trimmed.lines() {
        if line.trim().is_empty() {
            break;
        }
        first_block_lines.push(line);
    }
    let joined = first_block_lines.join(" ");
    let collapsed: String = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_TEXT_LEN {
        let truncated: String = collapsed.chars().take(MAX_TEXT_LEN - 3).collect();
        format!("{truncated}...")
    } else {
        collapsed
    }
}

pub fn run(req: BuddyRequest) -> BuddyResult {
    if req.command.trim().is_empty() {
        return BuddyResult::fail("empty command");
    }
    if req.args.len() > MAX_ARGS || req.args.iter().any(|a| a.len() > MAX_ARG_LEN) {
        return BuddyResult::fail("invalid args");
    }
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(25_000));
    let args: Vec<String> = req
        .args
        .iter()
        .map(|a| a.replace("{prompt}", &req.prompt))
        .collect();

    let mut child = match Command::new(&req.command)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", shell_env::shell_path())
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return BuddyResult::fail(&e.to_string()),
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(ref mut p) = stdout_pipe {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(ref mut p) = stderr_pipe {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });

    // Poll try_wait with a deadline: std has no owned-elsewhere kill, and this
    // keeps `child` in one place so timeout can kill it immediately.
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let Some(status) = status else {
        // Readers EOF once the child is dead; join to avoid leaking threads.
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
        return BuddyResult::fail("timeout");
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    let text = clean_output(&stdout);
    if code == 0 && !text.is_empty() {
        BuddyResult {
            ok: true,
            text,
            error: None,
        }
    } else {
        let stderr_head: String = strip_ansi(&stderr).chars().take(200).collect();
        BuddyResult::fail(&format!("exit {code}: {stderr_head}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(strip_ansi("\x1b[?25lhide\x1b[?25h"), "hide");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }

    #[test]
    fn clean_output_takes_first_block_collapses_ws_and_caps() {
        assert_eq!(clean_output("hello  world\n\nsecond block"), "hello world");
        let long = "x".repeat(400);
        let cleaned = clean_output(&long);
        assert_eq!(cleaned.chars().count(), 280);
        assert!(cleaned.ends_with("..."));
    }

    #[test]
    fn rejects_empty_command_and_bad_args() {
        let r = run(BuddyRequest {
            command: "  ".into(),
            args: vec![],
            prompt: "p".into(),
            timeout_ms: None,
        });
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("empty command"));

        let too_many = BuddyRequest {
            command: "echo".into(),
            args: (0..17).map(|i| i.to_string()).collect(),
            prompt: "p".into(),
            timeout_ms: None,
        };
        let r = run(too_many);
        assert_eq!(r.error.as_deref(), Some("invalid args"));

        let too_long = BuddyRequest {
            command: "echo".into(),
            args: vec!["y".repeat(4097)],
            prompt: "p".into(),
            timeout_ms: None,
        };
        assert_eq!(run(too_long).error.as_deref(), Some("invalid args"));
    }

    #[test]
    fn substitutes_prompt_and_returns_stdout() {
        let r = run(BuddyRequest {
            command: "/bin/echo".into(),
            args: vec!["reply: {prompt}".into()],
            prompt: "hi there".into(),
            timeout_ms: Some(10_000),
        });
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.text, "reply: hi there");
    }

    #[test]
    fn nonzero_exit_reports_error_with_stderr() {
        let r = run(BuddyRequest {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "echo boom >&2; exit 7".into()],
            prompt: "".into(),
            timeout_ms: Some(10_000),
        });
        assert!(!r.ok);
        let e = r.error.unwrap();
        assert!(e.contains("exit 7") && e.contains("boom"), "{e}");
    }

    #[test]
    fn times_out_and_kills() {
        let r = run(BuddyRequest {
            command: "/bin/sleep".into(),
            args: vec!["30".into()],
            prompt: "".into(),
            timeout_ms: Some(300),
        });
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("timeout"));
    }
}
