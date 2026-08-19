use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

const STDOUT_CAP: usize = 1024 * 1024;
const STDERR_CAP: usize = 64 * 1024;
const LOCAL_TIMEOUT: Duration = Duration::from_secs(10);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}
const SIGKILL: i32 = 9;

fn kill_group(pid: u32) {
    unsafe {
        let _ = kill(-(pid as i32), SIGKILL);
    }
}

fn timeout_msg(t: Duration) -> String {
    if t < Duration::from_secs(1) {
        format!("timeout after {}ms", t.as_millis())
    } else {
        format!("timeout after {}s", t.as_secs())
    }
}

/// Run `git` against an optional repo with the full hygiene contract:
/// literal pathspecs, no prompts, no optional locks, C locale, null stdin,
/// output caps, and a hard timeout enforced by process-group SIGKILL.
pub fn run_git(repo: Option<&Path>, args: &[&str], network: bool) -> Result<GitOutput, String> {
    let repo_str;
    let mut full: Vec<&str> = Vec::new();
    if let Some(r) = repo {
        repo_str = r.to_string_lossy().into_owned();
        full.push("-C");
        full.push(&repo_str);
    }
    full.extend_from_slice(args);
    let timeout = if network { NETWORK_TIMEOUT } else { LOCAL_TIMEOUT };
    run_command_with_timeout_and_cap("git", &full, timeout, STDOUT_CAP)
}

/// Internal engine (public in-crate so unit tests can drive it with arbitrary
/// binaries, timeouts, and caps).
pub(crate) fn run_command_with_timeout_and_cap(
    cmd: &str,
    args: &[&str],
    timeout: Duration,
    cap: usize,
) -> Result<GitOutput, String> {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C");
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            // Own process group so the whole tree can be SIGKILLed; failure is
            // non-fatal (child.kill() still covers the direct child).
            setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let tx_err = tx.clone();
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let so = std::thread::spawn(move || {
        let mut buf = [0u8; 32 * 1024];
        while let Ok(n) = stdout_pipe.read(&mut buf) {
            if n == 0 || tx.send((true, buf[..n].to_vec())).is_err() {
                break;
            }
        }
    });
    let se = std::thread::spawn(move || {
        let mut buf = [0u8; 8 * 1024];
        while let Ok(n) = stderr_pipe.read(&mut buf) {
            if n == 0 || tx_err.send((false, buf[..n].to_vec())).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr_bytes: Vec<u8> = Vec::new();
    let mut failure: Option<String> = None;

    // Receive until both readers finish (Disconnected), the cap trips, or the
    // deadline passes. Cap is checked BEFORE appending so allocation is bounded.
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok((is_out, chunk)) => {
                let (buf, cap_n) = if is_out {
                    (&mut stdout, cap)
                } else {
                    (&mut stderr_bytes, STDERR_CAP)
                };
                if buf.len() + chunk.len() > cap_n {
                    failure = Some("output too large".to_string());
                    break;
                }
                buf.extend_from_slice(&chunk);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    failure = Some(timeout_msg(timeout));
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if failure.is_some() {
        kill_group(pid);
        let _ = child.kill();
    }

    // Bounded reap: a process can close its pipes and keep running — never
    // block on wait() without a deadline, on ANY path.
    let reap_deadline = if failure.is_some() {
        Instant::now() + Duration::from_secs(5)
    } else {
        deadline
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if Instant::now() >= reap_deadline {
                    if failure.is_none() {
                        failure = Some(timeout_msg(timeout));
                    }
                    kill_group(pid);
                    let _ = child.kill();
                    // Post-SIGKILL bounded poll — still no unbounded wait().
                    let grace = Instant::now() + Duration::from_secs(2);
                    while child.try_wait().map(|s| s.is_none()).unwrap_or(false)
                        && Instant::now() < grace
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                if failure.is_none() {
                    failure = Some(e.to_string());
                }
                kill_group(pid);
                let _ = child.kill();
                break None;
            }
        }
    };

    if let Some(f) = failure {
        // Do NOT join the reader threads on failure paths: if the child is
        // unkillable (D-state) its pipes stay open and a join would hang.
        // The detached readers exit on their own once the pipes close.
        return Err(f);
    }

    let _ = so.join();
    let _ = se.join();
    let status = status.expect("status present when no failure");
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    if status.success() {
        Ok(GitOutput { stdout, stderr })
    } else {
        // Full (already 64KB-capped) stderr: the renderer caps its toast at
        // 200 chars but console-logs the whole thing for diagnostics.
        let code = status.code().unwrap_or(-1);
        Err(format!("exit {code}: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_and_reports_exit_errors() {
        let out = run_git(None, &["--version"], false).unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
        let err = run_git(None, &["definitely-not-a-command"], false).unwrap_err();
        assert!(err.starts_with("exit "), "{err}");
    }

    #[test]
    fn kills_on_timeout() {
        let start = Instant::now();
        let err = run_command_with_timeout_and_cap(
            "/bin/sleep",
            &["30"],
            Duration::from_millis(300),
            STDOUT_CAP,
        )
        .unwrap_err();
        assert_eq!(err, "timeout after 300ms");
        assert!(start.elapsed() < Duration::from_secs(10), "must not wait for sleep");
    }

    #[test]
    fn caps_output_and_kills_fast_producer() {
        let start = Instant::now();
        let err = run_command_with_timeout_and_cap(
            "/usr/bin/yes",
            &[],
            Duration::from_secs(10),
            64 * 1024,
        )
        .unwrap_err();
        assert_eq!(err, "output too large");
        assert!(start.elapsed() < Duration::from_secs(8), "cap must trip quickly");
    }
}
