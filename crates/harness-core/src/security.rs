//! Deterministic security scanners: `cargo audit` and `gitleaks`.
//!
//! A scanner that is not installed (or cannot run) is reported as
//! [`ScanStatus::Skipped`] / [`ScanStatus::Error`]; it is never reported as a pass.
//! Matched secrets are never captured: gitleaks runs with `--redact` and only the
//! rule, file and line are kept.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::config::SecurityConfig;

/// Cap on scanner stdout/report bytes.
pub const MAX_SCANNER_OUTPUT: usize = 8 * 1024 * 1024;
/// Cap on findings kept per scanner.
pub const MAX_FINDINGS_PER_SCANNER: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    /// Ran to completion and found nothing.
    Passed,
    /// Ran to completion and reported findings.
    Findings,
    /// Not run (not installed, disabled, or not applicable). Proves nothing.
    Skipped,
    /// Started but failed or timed out. Proves nothing.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityFinding {
    pub scanner: String,
    pub id: String,
    pub title: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub scanner: String,
    pub status: ScanStatus,
    pub detail: String,
    pub findings: Vec<SecurityFinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityVerdict {
    /// Every scanner ran and found nothing.
    Clean,
    Findings,
    /// No findings, but at least one scanner did not complete (or none ran).
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SecurityReport {
    pub results: Vec<ScanResult>,
}

impl SecurityReport {
    pub fn verdict(&self) -> SecurityVerdict {
        if self
            .results
            .iter()
            .any(|r| r.status == ScanStatus::Findings)
        {
            SecurityVerdict::Findings
        } else if !self.results.is_empty()
            && self.results.iter().all(|r| r.status == ScanStatus::Passed)
        {
            SecurityVerdict::Clean
        } else {
            SecurityVerdict::Incomplete
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut out =
            String::from("### Security scanners\n\n| Scanner | Status | Detail |\n|---|---|---|\n");
        for r in &self.results {
            out.push_str(&format!(
                "| {} | {:?} | {} |\n",
                r.scanner,
                r.status,
                r.detail.replace('|', "\\|").replace('\n', " ")
            ));
        }
        for f in self.results.iter().flat_map(|r| r.findings.iter()) {
            out.push_str(&format!(
                "\n- **{}** `{}`: {}{}",
                f.scanner,
                f.id,
                f.title,
                f.location
                    .as_ref()
                    .map_or(String::new(), |l| format!(" (`{l}`)"))
            ));
        }
        out.push('\n');
        out
    }
}

/// Program names, overridable so tests can substitute stubs.
#[derive(Debug, Clone)]
pub struct Tools {
    pub cargo: String,
    pub gitleaks: String,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            cargo: "cargo".into(),
            gitleaks: "gitleaks".into(),
        }
    }
}

#[derive(Debug)]
enum RunError {
    NotFound,
    Timeout,
    TooLarge,
    Other(String),
}

struct Output {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

fn run_capped(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<Output, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                RunError::NotFound
            } else {
                RunError::Other(e.to_string())
            }
        })?;
    let out = child.stdout.take().expect("piped");
    let err = child.stderr.take().expect("piped");
    let out_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.take(MAX_SCANNER_OUTPUT as u64 + 1).read_to_end(&mut b);
        b
    });
    let err_t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.take(4096).read_to_end(&mut b);
        b
    });
    let status = match child.wait_timeout(timeout) {
        Ok(Some(s)) => Some(s),
        _ => {
            crate::verify::kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    let Some(status) = status else {
        return Err(RunError::Timeout);
    };
    if stdout.len() > MAX_SCANNER_OUTPUT {
        return Err(RunError::TooLarge);
    }
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
    })
}

fn result(scanner: &str, status: ScanStatus, detail: impl Into<String>) -> ScanResult {
    ScanResult {
        scanner: scanner.into(),
        status,
        detail: detail.into(),
        findings: Vec::new(),
    }
}

fn run_error(scanner: &str, e: RunError, timeout: Duration) -> ScanResult {
    match e {
        RunError::NotFound => result(scanner, ScanStatus::Skipped, "not installed"),
        RunError::Timeout => result(
            scanner,
            ScanStatus::Error,
            format!("timed out after {}s", timeout.as_secs()),
        ),
        RunError::TooLarge => result(scanner, ScanStatus::Error, "output exceeded size cap"),
        RunError::Other(m) => result(scanner, ScanStatus::Error, format!("failed to run: {m}")),
    }
}

/// Parse `cargo audit --json` output.
pub fn parse_cargo_audit(json: &str) -> Result<Vec<SecurityFinding>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = v
        .pointer("/vulnerabilities/list")
        .and_then(|l| l.as_array())
        .ok_or("missing vulnerabilities.list")?;
    let s =
        |v: &serde_json::Value, p: &str| v.pointer(p).and_then(|x| x.as_str()).map(str::to_string);
    Ok(list
        .iter()
        .take(MAX_FINDINGS_PER_SCANNER)
        .map(|item| {
            let pkg = s(item, "/package/name").unwrap_or_else(|| "?".into());
            let ver = s(item, "/package/version").unwrap_or_else(|| "?".into());
            SecurityFinding {
                scanner: "cargo-audit".into(),
                id: s(item, "/advisory/id").unwrap_or_else(|| "UNKNOWN".into()),
                title: s(item, "/advisory/title").unwrap_or_default(),
                location: Some(format!("{pkg} {ver}")),
            }
        })
        .collect())
}

/// Parse a gitleaks JSON report (an array of leaks). Secret values are ignored.
pub fn parse_gitleaks(json: &str) -> Result<Vec<SecurityFinding>, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| e.to_string())?;
    let arr = match v {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Array(a) => a,
        _ => return Err("expected a JSON array".into()),
    };
    Ok(arr
        .iter()
        .take(MAX_FINDINGS_PER_SCANNER)
        .map(|item| {
            let get = |k: &str| {
                item.get(k)
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let line = item.get("StartLine").and_then(|x| x.as_u64()).unwrap_or(0);
            SecurityFinding {
                scanner: "gitleaks".into(),
                id: get("RuleID"),
                title: get("Description"),
                location: Some(format!("{}:{line}", get("File"))),
            }
        })
        .collect())
}

pub fn run_cargo_audit(tools: &Tools, dir: &Path, timeout: Duration) -> ScanResult {
    const NAME: &str = "cargo-audit";
    if !dir.join("Cargo.lock").is_file() {
        return result(NAME, ScanStatus::Skipped, "no Cargo.lock in repository");
    }
    let out = match run_capped(&tools.cargo, &["audit", "--json"], dir, timeout) {
        Ok(o) => o,
        Err(e) => return run_error(NAME, e, timeout),
    };
    if out.stderr.contains("no such command") {
        return result(NAME, ScanStatus::Skipped, "cargo-audit not installed");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match parse_cargo_audit(&text) {
        Ok(f) if f.is_empty() => result(NAME, ScanStatus::Passed, "no known vulnerabilities"),
        Ok(f) => ScanResult {
            scanner: NAME.into(),
            status: ScanStatus::Findings,
            detail: format!("{} vulnerable dependencies", f.len()),
            findings: f,
        },
        Err(e) => result(
            NAME,
            ScanStatus::Error,
            format!("unparseable output (exit {:?}): {e}", out.code),
        ),
    }
}

/// Scan with gitleaks: the commit range if given (`base..head`), else the working tree.
pub fn run_gitleaks(
    tools: &Tools,
    dir: &Path,
    range: Option<&str>,
    timeout: Duration,
) -> ScanResult {
    const NAME: &str = "gitleaks";
    let report = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => return result(NAME, ScanStatus::Error, format!("tempfile: {e}")),
    };
    let report_path = report.path().to_string_lossy().into_owned();
    let dir_s = dir.to_string_lossy().into_owned();
    let log_opts = range.map(|r| format!("--log-opts={r}"));
    let mut args: Vec<&str> = match &log_opts {
        Some(l) => vec!["git", l.as_str()],
        None => vec!["dir"],
    };
    args.extend([
        "--redact",
        "--no-banner",
        "--exit-code",
        "0",
        "--report-format",
        "json",
        "--report-path",
        &report_path,
        &dir_s,
    ]);
    let out = match run_capped(&tools.gitleaks, &args, dir, timeout) {
        Ok(o) => o,
        Err(e) => return run_error(NAME, e, timeout),
    };
    if out.code != Some(0) {
        return result(
            NAME,
            ScanStatus::Error,
            format!("exit {:?}: {}", out.code, out.stderr.trim()),
        );
    }
    let mut text = String::new();
    let read = std::fs::File::open(report.path())
        .and_then(|f| f.take(MAX_SCANNER_OUTPUT as u64).read_to_string(&mut text));
    if let Err(e) = read {
        return result(NAME, ScanStatus::Error, format!("cannot read report: {e}"));
    }
    match parse_gitleaks(&text) {
        Ok(f) if f.is_empty() => result(NAME, ScanStatus::Passed, "no leaks detected"),
        Ok(f) => ScanResult {
            scanner: NAME.into(),
            status: ScanStatus::Findings,
            detail: format!("{} potential secrets", f.len()),
            findings: f,
        },
        Err(e) => result(NAME, ScanStatus::Error, format!("unparseable report: {e}")),
    }
}

/// Run all enabled scanners; disabled ones are reported as skipped.
pub fn run_all(
    cfg: &SecurityConfig,
    tools: &Tools,
    dir: &Path,
    range: Option<&str>,
) -> SecurityReport {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let mut results = Vec::new();
    results.push(if cfg.cargo_audit {
        run_cargo_audit(tools, dir, timeout)
    } else {
        result("cargo-audit", ScanStatus::Skipped, "disabled in config")
    });
    results.push(if cfg.gitleaks {
        run_gitleaks(tools, dir, range, timeout)
    } else {
        result("gitleaks", ScanStatus::Skipped, "disabled in config")
    });
    SecurityReport { results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const AUDIT_VULN: &str = r#"{"database":{},"lockfile":{"dependency-count":3},
      "vulnerabilities":{"found":true,"count":1,"list":[
        {"advisory":{"id":"RUSTSEC-2099-0001","title":"Bad thing","package":"foo"},
         "package":{"name":"foo","version":"1.2.3"}}]},"warnings":{}}"#;
    const AUDIT_CLEAN: &str =
        r#"{"vulnerabilities":{"found":false,"count":0,"list":[]},"warnings":{}}"#;
    const LEAKS: &str = r#"[{"RuleID":"aws-access-token","Description":"AWS Access Key","File":"src/a.rs","StartLine":7,"Secret":"REDACTED","Match":"REDACTED"}]"#;

    fn stub(dir: &Path, name: &str, body: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn repo_with_lock() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.lock"), "").unwrap();
        d
    }

    #[test]
    fn parses_cargo_audit_vulnerabilities() {
        let f = parse_cargo_audit(AUDIT_VULN).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "RUSTSEC-2099-0001");
        assert_eq!(f[0].location.as_deref(), Some("foo 1.2.3"));
        assert!(parse_cargo_audit(AUDIT_CLEAN).unwrap().is_empty());
        assert!(parse_cargo_audit("not json").is_err());
        assert!(parse_cargo_audit("{}").is_err());
    }

    #[test]
    fn parses_gitleaks_report_without_secret_values() {
        let f = parse_gitleaks(LEAKS).unwrap();
        assert_eq!(f[0].id, "aws-access-token");
        assert_eq!(f[0].location.as_deref(), Some("src/a.rs:7"));
        assert!(parse_gitleaks("").unwrap().is_empty());
        assert!(parse_gitleaks("[]").unwrap().is_empty());
        assert!(parse_gitleaks("null").unwrap().is_empty());
        assert!(parse_gitleaks("{}").is_err());
    }

    #[test]
    fn missing_scanners_are_skipped_not_passed() {
        let d = repo_with_lock();
        let tools = Tools {
            cargo: "definitely-missing-cargo".into(),
            gitleaks: "definitely-missing-gitleaks".into(),
        };
        let r = run_all(&SecurityConfig::default(), &tools, d.path(), None);
        assert!(r.results.iter().all(|x| x.status == ScanStatus::Skipped));
        assert_eq!(r.verdict(), SecurityVerdict::Incomplete);
    }

    #[test]
    fn cargo_audit_subcommand_missing_is_skipped() {
        let d = repo_with_lock();
        let bin = tempfile::tempdir().unwrap();
        let cargo = stub(
            bin.path(),
            "cargo",
            "echo 'error: no such command: `audit`' >&2; exit 101",
        );
        let r = run_cargo_audit(
            &Tools {
                cargo,
                ..Tools::default()
            },
            d.path(),
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Skipped);
    }

    #[test]
    fn no_cargo_lock_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let r = run_cargo_audit(&Tools::default(), d.path(), Duration::from_secs(5));
        assert_eq!(r.status, ScanStatus::Skipped);
    }

    #[test]
    fn cargo_audit_findings_even_with_nonzero_exit() {
        let d = repo_with_lock();
        let bin = tempfile::tempdir().unwrap();
        let body = format!("cat <<'EOF'\n{AUDIT_VULN}\nEOF\nexit 1");
        let cargo = stub(bin.path(), "cargo", &body);
        let r = run_cargo_audit(
            &Tools {
                cargo,
                ..Tools::default()
            },
            d.path(),
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Findings);
        assert_eq!(r.findings.len(), 1);
    }

    #[test]
    fn cargo_audit_clean_is_passed() {
        let d = repo_with_lock();
        let bin = tempfile::tempdir().unwrap();
        let cargo = stub(bin.path(), "cargo", &format!("echo '{AUDIT_CLEAN}'"));
        let r = run_cargo_audit(
            &Tools {
                cargo,
                ..Tools::default()
            },
            d.path(),
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Passed);
    }

    #[test]
    fn garbage_output_is_error_not_pass() {
        let d = repo_with_lock();
        let bin = tempfile::tempdir().unwrap();
        let cargo = stub(bin.path(), "cargo", "echo 'oops, offline'; exit 1");
        let r = run_cargo_audit(
            &Tools {
                cargo,
                ..Tools::default()
            },
            d.path(),
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Error);
    }

    #[test]
    fn scanner_timeout_is_error() {
        let d = repo_with_lock();
        let bin = tempfile::tempdir().unwrap();
        let cargo = stub(bin.path(), "cargo", "sleep 20");
        let r = run_cargo_audit(
            &Tools {
                cargo,
                ..Tools::default()
            },
            d.path(),
            Duration::from_millis(200),
        );
        assert_eq!(r.status, ScanStatus::Error);
        assert!(r.detail.contains("timed out"));
    }

    #[test]
    fn gitleaks_findings_from_report_file_and_redact_flag_used() {
        let d = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        // Stub: record args, write the report to the path after --report-path.
        let args_file = bin.path().join("args.txt");
        let body = format!(
            "echo \"$@\" > '{}'\nwhile [ $# -gt 0 ]; do if [ \"$1\" = --report-path ]; then echo '{LEAKS}' > \"$2\"; fi; shift; done",
            args_file.display()
        );
        let gl = stub(bin.path(), "gitleaks", &body);
        let r = run_gitleaks(
            &Tools {
                gitleaks: gl,
                ..Tools::default()
            },
            d.path(),
            Some("main..HEAD"),
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Findings);
        let args = std::fs::read_to_string(args_file).unwrap();
        assert!(args.contains("--redact"));
        assert!(args.contains("git --log-opts=main..HEAD"));
    }

    #[test]
    fn gitleaks_empty_report_is_passed_and_failure_is_error() {
        let d = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let ok = stub(bin.path(), "gl-ok", "exit 0");
        let r = run_gitleaks(
            &Tools {
                gitleaks: ok,
                ..Tools::default()
            },
            d.path(),
            None,
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Passed);
        let bad = stub(bin.path(), "gl-bad", "echo boom >&2; exit 2");
        let r = run_gitleaks(
            &Tools {
                gitleaks: bad,
                ..Tools::default()
            },
            d.path(),
            None,
            Duration::from_secs(5),
        );
        assert_eq!(r.status, ScanStatus::Error);
        assert!(r.detail.contains("boom"));
    }

    #[test]
    fn disabled_scanners_are_skipped() {
        let cfg = SecurityConfig {
            cargo_audit: false,
            gitleaks: false,
            timeout_secs: 5,
        };
        let r = run_all(&cfg, &Tools::default(), Path::new("."), None);
        assert!(r.results.iter().all(|x| x.status == ScanStatus::Skipped));
        assert_eq!(r.verdict(), SecurityVerdict::Incomplete);
    }

    #[test]
    fn verdict_table() {
        let mk = |s: &[ScanStatus]| SecurityReport {
            results: s.iter().map(|st| result("x", *st, "")).collect(),
        };
        use ScanStatus::*;
        assert_eq!(mk(&[]).verdict(), SecurityVerdict::Incomplete);
        assert_eq!(mk(&[Passed, Passed]).verdict(), SecurityVerdict::Clean);
        assert_eq!(
            mk(&[Passed, Skipped]).verdict(),
            SecurityVerdict::Incomplete
        );
        assert_eq!(mk(&[Passed, Error]).verdict(), SecurityVerdict::Incomplete);
        assert_eq!(
            mk(&[Findings, Skipped]).verdict(),
            SecurityVerdict::Findings
        );
        assert_eq!(mk(&[Findings, Passed]).verdict(), SecurityVerdict::Findings);
    }

    #[test]
    fn markdown_lists_findings() {
        let r = SecurityReport {
            results: vec![ScanResult {
                scanner: "gitleaks".into(),
                status: ScanStatus::Findings,
                detail: "1 potential secrets".into(),
                findings: parse_gitleaks(LEAKS).unwrap(),
            }],
        };
        let md = r.to_markdown();
        assert!(md.contains("aws-access-token") && md.contains("src/a.rs:7"));
    }
}
