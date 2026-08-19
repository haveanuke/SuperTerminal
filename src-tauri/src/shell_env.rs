use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

/// Run `$SHELL -ilc <arg>` capturing stdout, with a hard timeout.
/// Mirrors the Electron main process's login-shell tricks: GUI apps on macOS
/// don't inherit the user's shell PATH/env.
fn run_login_shell(arg: &str, timeout: Duration) -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(&shell)
        .args(["-ilc", arg])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(timeout) {
        Ok(out) => {
            let _ = child.wait();
            Some(out)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

pub fn parse_env_output(out: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for line in out.lines() {
        if let Some(idx) = line.find('=') {
            if idx > 0 {
                env.insert(line[..idx].to_string(), line[idx + 1..].to_string());
            }
        }
    }
    env
}

pub fn fallback_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("PATH") {
        if !p.is_empty() {
            parts.push(p);
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        parts.push(dir.to_string());
    }
    if !home.is_empty() {
        parts.push(format!("{home}/.local/bin"));
    }
    parts.join(":")
}

/// Full login-shell environment, captured once. Falls back to the process env.
pub fn shell_env() -> &'static HashMap<String, String> {
    static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();
    ENV.get_or_init(|| match run_login_shell("env", Duration::from_secs(5)) {
        Some(out) if !out.trim().is_empty() => parse_env_output(&out),
        _ => std::env::vars().collect(),
    })
}

/// Login-shell PATH, captured once, with a static fallback chain.
pub fn shell_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        match run_login_shell("echo -n \"$PATH\"", Duration::from_secs(3)) {
            Some(out) if !out.trim().is_empty() => out.trim().to_string(),
            _ => fallback_path(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_env_lines() {
        let out = "PATH=/usr/bin:/bin\nHOME=/Users/x\nEMPTY=\n";
        let env = parse_env_output(out);
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin:/bin");
        assert_eq!(env.get("HOME").unwrap(), "/Users/x");
        assert_eq!(env.get("EMPTY").unwrap(), "");
    }

    #[test]
    fn skips_lines_without_equals_and_keeps_values_with_equals() {
        let out = "garbage\nA=b=c\n";
        let env = parse_env_output(out);
        assert!(!env.contains_key("garbage"));
        assert_eq!(env.get("A").unwrap(), "b=c");
    }

    #[test]
    fn fallback_path_contains_standard_dirs() {
        let p = fallback_path();
        assert!(p.contains("/usr/bin"));
        assert!(p.contains("/opt/homebrew/bin"));
    }
}
