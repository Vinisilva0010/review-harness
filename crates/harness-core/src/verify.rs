//! Verification layer: runs configured commands and produces an [`Evidence`] report.
//!
//! "Done" is only expressible through [`Evidence::verdict`]: with no checks configured
//! the verdict is [`Verdict::NoEvidence`], never a pass.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

/// Default cap, in bytes, for the stdout/stderr tail kept per check.
pub const DEFAULT_TAIL_BYTES: usize = 4096;
/// Default per-check timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// A named command to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub command: String,
}

/// Limits applied to every check.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub tail_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            tail_bytes: DEFAULT_TAIL_BYTES,
        }
    }
}

/// How a single check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    TimedOut,
    /// The command could not be started at all.
    SpawnError,
}

/// Evidence for one executed check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub command: String,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    /// True when output was longer than the tail cap and the head was dropped.
    pub output_truncated: bool,
}

/// Overall verification verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Verified,
    Failed,
    /// No checks ran, so nothing was proven.
    NoEvidence,
}

/// The evidence report for a verification run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Evidence {
    pub results: Vec<CheckResult>,
}

impl Evidence {
    /// `Verified` only if at least one check ran and every check passed.
    pub fn verdict(&self) -> Verdict {
        if self.results.is_empty() {
            Verdict::NoEvidence
        } else if self.results.iter().all(|r| r.outcome == Outcome::Passed) {
            Verdict::Verified
        } else {
            Verdict::Failed
        }
    }

    /// Render a Markdown summary suitable for a PR comment.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("### Verification evidence\n\n");
        if self.results.is_empty() {
            out.push_str("No checks configured: **no evidence**, nothing is proven.\n");
            return out;
        }
        out.push_str("| Check | Outcome | Exit | Duration |\n|---|---|---|---|\n");
        for r in &self.results {
            let exit = r.exit_code.map_or("-".to_string(), |c| c.to_string());
            out.push_str(&format!(
                "| `{}` | {:?} | {} | {} ms |\n",
                r.name, r.outcome, exit, r.duration_ms
            ));
        }
        for r in self.results.iter().filter(|r| r.outcome != Outcome::Passed) {
            out.push_str(&format!(
                "\n<details><summary>{} output</summary>\n\n",
                r.name
            ));
            out.push_str(&format!(
                "```\n{}\n{}\n```\n</details>\n",
                r.stdout_tail.replace("```", "'''"),
                r.stderr_tail.replace("```", "'''")
            ));
        }
        out
    }
}

/// Keeps only the last `cap` bytes read from `reader`, draining it completely.
fn read_tail<R: Read>(mut reader: R, cap: usize) -> (Vec<u8>, bool) {
    let mut tail: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > cap {
                    truncated = true;
                    let drop = tail.len() - cap;
                    tail.drain(..drop);
                }
            }
        }
    }
    (tail, truncated)
}

pub(crate) fn kill_group(pid: u32) {
    // The child leads its own process group, so this also reaps grandchildren.
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Run a single check in `cwd` under `limits`.
pub fn run_check(check: &Check, cwd: &std::path::Path, limits: Limits) -> CheckResult {
    let start = Instant::now();
    let mut result = CheckResult {
        name: check.name.clone(),
        command: check.command.clone(),
        outcome: Outcome::SpawnError,
        exit_code: None,
        duration_ms: 0,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        output_truncated: false,
    };

    let spawned = Command::new("sh")
        .arg("-c")
        .arg(&check.command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            result.stderr_tail = format!("failed to spawn: {e}");
            return result;
        }
    };

    let cap = limits.tail_bytes;
    let out = child
        .stdout
        .take()
        .map(|s| thread::spawn(move || read_tail(s, cap)));
    let err = child
        .stderr
        .take()
        .map(|s| thread::spawn(move || read_tail(s, cap)));

    match child.wait_timeout(limits.timeout) {
        Ok(Some(status)) => {
            result.exit_code = status.code();
            result.outcome = if status.success() {
                Outcome::Passed
            } else {
                Outcome::Failed
            };
        }
        Ok(None) => {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            result.outcome = Outcome::TimedOut;
        }
        Err(e) => {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            result.stderr_tail = format!("failed to wait: {e}");
        }
    }

    for (handle, is_out) in [(out, true), (err, false)] {
        if let Some(h) = handle {
            if let Ok((bytes, trunc)) = h.join() {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                result.output_truncated |= trunc;
                if is_out {
                    result.stdout_tail = text;
                } else if result.stderr_tail.is_empty() {
                    result.stderr_tail = text;
                } else {
                    result.stderr_tail.push_str(&text);
                }
            }
        }
    }
    result.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    result
}

/// Run all checks sequentially and collect the evidence.
pub fn run_checks(checks: &[Check], cwd: &std::path::Path, limits: Limits) -> Evidence {
    Evidence {
        results: checks.iter().map(|c| run_check(c, cwd, limits)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cmd: &str) -> Check {
        Check {
            name: "t".into(),
            command: cmd.into(),
        }
    }

    fn run(cmd: &str, limits: Limits) -> CheckResult {
        run_check(&check(cmd), std::path::Path::new("."), limits)
    }

    #[test]
    fn passing_command_is_passed() {
        let r = run("echo hi", Limits::default());
        assert_eq!(r.outcome, Outcome::Passed);
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.stdout_tail.trim(), "hi");
    }

    #[test]
    fn failing_command_records_exit_code_and_stderr() {
        let r = run("echo boom >&2; exit 3", Limits::default());
        assert_eq!(r.outcome, Outcome::Failed);
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.stderr_tail.trim(), "boom");
    }

    #[test]
    fn missing_command_fails() {
        let r = run("definitely-not-a-command-xyz", Limits::default());
        assert_eq!(r.outcome, Outcome::Failed);
        assert_eq!(r.exit_code, Some(127));
    }

    #[test]
    fn bad_cwd_is_spawn_error() {
        let r = run_check(
            &check("true"),
            std::path::Path::new("/nonexistent-dir-xyz"),
            Limits::default(),
        );
        assert_eq!(r.outcome, Outcome::SpawnError);
        assert!(r.stderr_tail.contains("failed to spawn"));
    }

    #[test]
    fn timeout_kills_command_including_grandchildren() {
        let limits = Limits {
            timeout: Duration::from_millis(200),
            ..Limits::default()
        };
        let start = Instant::now();
        let r = run("sleep 30 & sleep 30", limits);
        assert_eq!(r.outcome, Outcome::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_is_capped_to_tail() {
        let limits = Limits {
            tail_bytes: 16,
            ..Limits::default()
        };
        let r = run("seq 1 100000", limits);
        assert_eq!(r.outcome, Outcome::Passed);
        assert!(r.stdout_tail.len() <= 16);
        assert!(r.output_truncated);
        assert!(r.stdout_tail.trim_end().ends_with("100000"));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let r = run("printf '\\377\\376ok'", Limits::default());
        assert_eq!(r.outcome, Outcome::Passed);
        assert!(r.stdout_tail.contains('\u{fffd}'));
        assert!(r.stdout_tail.ends_with("ok"));
    }

    #[test]
    fn empty_evidence_is_no_evidence() {
        assert_eq!(Evidence::default().verdict(), Verdict::NoEvidence);
        assert!(Evidence::default().to_markdown().contains("no evidence"));
    }

    #[test]
    fn verdict_requires_all_passed() {
        let l = Limits::default();
        let ok = run_checks(
            &[check("true"), check("true")],
            std::path::Path::new("."),
            l,
        );
        assert_eq!(ok.verdict(), Verdict::Verified);
        let bad = run_checks(
            &[check("true"), check("false")],
            std::path::Path::new("."),
            l,
        );
        assert_eq!(bad.verdict(), Verdict::Failed);
    }

    #[test]
    fn timed_out_check_fails_verdict() {
        let l = Limits {
            timeout: Duration::from_millis(100),
            ..Limits::default()
        };
        let e = run_checks(&[check("sleep 5")], std::path::Path::new("."), l);
        assert_eq!(e.verdict(), Verdict::Failed);
    }

    #[test]
    fn markdown_lists_failures_with_output() {
        let e = run_checks(
            &[check("echo oops; exit 1")],
            std::path::Path::new("."),
            Limits::default(),
        );
        let md = e.to_markdown();
        assert!(md.contains("Failed"));
        assert!(md.contains("oops"));
    }

    #[test]
    fn evidence_json_roundtrip() {
        let e = run_checks(
            &[check("true")],
            std::path::Path::new("."),
            Limits::default(),
        );
        let json = serde_json::to_string(&e).unwrap();
        let back: Evidence = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
