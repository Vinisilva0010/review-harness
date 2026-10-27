//! Aggregates run records into the metrics Anthropic's post tracks: review rounds per
//! PR, findings by severity, the hallucination rate, skipped layers, tokens spent and
//! reverts detected from git history.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::gate::Decision;
use crate::report::RunRecord;
use crate::review::{ReviewStatus, SkipReason};
use crate::security::ScanStatus;
use crate::verify::Verdict;

pub const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_RECORDS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub runs: usize,
    pub pull_requests: usize,
    pub decisions: BTreeMap<String, usize>,
    pub avg_rounds_per_pr: f64,
    pub max_rounds_per_pr: u8,
    pub findings_by_severity: BTreeMap<String, usize>,
    pub findings_raw: usize,
    pub findings_kept: usize,
    pub hallucinated: usize,
    /// hallucinated / raw findings (0 when the model returned nothing).
    pub hallucination_rate: f64,
    pub layers_skipped: BTreeMap<String, usize>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub llm_calls: u64,
}

fn key(r: &RunRecord) -> String {
    match (r.pr, &r.head) {
        (Some(pr), _) => format!("pr-{pr}"),
        (None, Some(h)) => format!("head-{h}"),
        (None, None) => "unknown".into(),
    }
}

pub fn aggregate(records: &[RunRecord]) -> Metrics {
    let mut m = Metrics {
        runs: records.len(),
        ..Metrics::default()
    };
    let mut rounds: BTreeMap<String, u8> = BTreeMap::new();
    for r in records {
        let e = rounds.entry(key(r)).or_default();
        *e = (*e).max(r.review.round);
        let d = match r.gate.decision {
            Decision::Pass => "pass",
            Decision::NeedsHuman => "needs_human",
            Decision::Fail => "fail",
        };
        *m.decisions.entry(d.into()).or_default() += 1;

        let rm = &r.review.metrics;
        m.findings_raw += rm.findings_raw;
        m.findings_kept += rm.findings_kept;
        m.hallucinated += rm.hallucinated;
        m.input_tokens += u64::from(rm.input_tokens);
        m.output_tokens += u64::from(rm.output_tokens);
        m.llm_calls += u64::from(rm.llm_calls);
        for (sev, n) in &rm.by_severity {
            *m.findings_by_severity.entry(sev.clone()).or_default() += n;
        }

        let mut skip = |layer: &str| *m.layers_skipped.entry(layer.into()).or_default() += 1;
        if r.verify.verdict() == Verdict::NoEvidence {
            skip("verify");
        }
        for s in r
            .security
            .results
            .iter()
            .filter(|s| s.status == ScanStatus::Skipped)
        {
            skip(&format!("security:{}", s.scanner));
        }
        if let ReviewStatus::Skipped { reason } = &r.review.status {
            skip(match reason {
                SkipReason::NoProviderConfigured => "ai:no_provider",
                SkipReason::ApiKeyMissing { .. } => "ai:key_missing",
                SkipReason::BelowRiskThreshold { .. } => "ai:below_risk_threshold",
                SkipReason::NoReviewableChanges => "ai:nothing_to_review",
                SkipReason::EarlierLayerFailed => "ai:earlier_layer_failed",
            });
        }
    }
    m.pull_requests = rounds.len();
    m.max_rounds_per_pr = rounds.values().copied().max().unwrap_or(0);
    if !rounds.is_empty() {
        m.avg_rounds_per_pr =
            rounds.values().map(|r| f64::from(*r)).sum::<f64>() / rounds.len() as f64;
    }
    if m.findings_raw > 0 {
        m.hallucination_rate = m.hallucinated as f64 / m.findings_raw as f64;
    }
    m
}

/// Load `*.json` run records from `dir`. Unreadable, oversized or invalid files are
/// reported as warnings and skipped; they never abort aggregation.
pub fn load_records(dir: &Path) -> std::io::Result<(Vec<RunRecord>, Vec<String>)> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    if paths.len() > MAX_RECORDS {
        warnings.push(format!(
            "only the first {MAX_RECORDS} of {} files were read",
            paths.len()
        ));
        paths.truncate(MAX_RECORDS);
    }
    for p in paths {
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut text = String::new();
        let read = std::fs::File::open(&p)
            .and_then(|f| f.take(MAX_RECORD_BYTES + 1).read_to_string(&mut text));
        if let Err(e) = read {
            warnings.push(format!("{name}: unreadable ({e})"));
        } else if text.len() as u64 > MAX_RECORD_BYTES {
            warnings.push(format!(
                "{name}: larger than {MAX_RECORD_BYTES} bytes, skipped"
            ));
        } else {
            match serde_json::from_str::<RunRecord>(&text) {
                Ok(r) => records.push(r),
                Err(e) => warnings.push(format!("{name}: not a run record ({e})")),
            }
        }
    }
    Ok((records, warnings))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RevertStats {
    pub commits_scanned: usize,
    pub reverts: usize,
    pub revert_rate: f64,
    /// Short hashes of the revert commits (capped).
    pub revert_commits: Vec<String>,
}

/// Parse `git log --format=%H%x1f%s%x1f%b%x1e` output.
pub fn parse_reverts(log: &str) -> RevertStats {
    let mut stats = RevertStats::default();
    let mut seen = BTreeSet::new();
    for entry in log.split('\u{1e}').map(str::trim).filter(|e| !e.is_empty()) {
        let mut parts = entry.splitn(3, '\u{1f}');
        let (Some(hash), Some(subject), body) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        stats.commits_scanned += 1;
        let is_revert = subject.starts_with("Revert \"")
            || subject.to_lowercase().starts_with("revert:")
            || subject.to_lowercase().starts_with("revert(")
            || body.is_some_and(|b| b.contains("This reverts commit "));
        if is_revert && seen.insert(hash.to_string()) {
            stats.reverts += 1;
            if stats.revert_commits.len() < 50 {
                stats.revert_commits.push(hash.chars().take(12).collect());
            }
        }
    }
    if stats.commits_scanned > 0 {
        stats.revert_rate = stats.reverts as f64 / stats.commits_scanned as f64;
    }
    stats
}

pub fn detect_reverts(repo: &Path, max_commits: usize) -> Result<RevertStats, String> {
    let mut child = Command::new("git")
        .args([
            "log",
            &format!("--max-count={max_commits}"),
            "--format=%H%x1f%s%x1f%b%x1e",
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let t = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = stdout.take(16 * 1024 * 1024).read_to_end(&mut b);
        b
    });
    let status = child
        .wait_timeout(Duration::from_secs(60))
        .map_err(|e| e.to_string())?;
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("git log timed out".into());
    };
    let out = t.join().unwrap_or_default();
    if !status.success() {
        return Err("git log failed (not a repository, or no commits yet)".into());
    }
    Ok(parse_reverts(&String::from_utf8_lossy(&out)))
}

impl Metrics {
    pub fn to_markdown(&self, reverts: Option<&RevertStats>) -> String {
        let pct = |x: f64| format!("{:.1}%", x * 100.0);
        let mut out = String::from("## review-harness metrics\n\n");
        out.push_str(&format!(
            "- Runs: {} across {} pull request(s)\n- Rounds per PR: avg {:.2}, max {}\n",
            self.runs, self.pull_requests, self.avg_rounds_per_pr, self.max_rounds_per_pr
        ));
        out.push_str(&format!("- Decisions: {}\n", fmt_map(&self.decisions)));
        out.push_str(&format!(
            "- Findings: {} raw, {} kept; by severity: {}\n",
            self.findings_raw,
            self.findings_kept,
            fmt_map(&self.findings_by_severity)
        ));
        out.push_str(&format!(
            "- Hallucination rate (findings outside the diff): {} ({}/{})\n",
            pct(self.hallucination_rate),
            self.hallucinated,
            self.findings_raw
        ));
        out.push_str(&format!(
            "- Layers skipped: {}\n",
            fmt_map(&self.layers_skipped)
        ));
        out.push_str(&format!(
            "- Tokens: {} in / {} out over {} call(s)\n",
            self.input_tokens, self.output_tokens, self.llm_calls
        ));
        match reverts {
            Some(r) => out.push_str(&format!(
                "- Reverts in git history: {} of {} commits ({})\n",
                r.reverts,
                r.commits_scanned,
                pct(r.revert_rate)
            )),
            None => out.push_str("- Reverts in git history: not computed\n"),
        }
        out
    }
}

fn fmt_map(m: &BTreeMap<String, usize>) -> String {
    if m.is_empty() {
        return "none".into();
    }
    m.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskLevel;
    use crate::gate::GateResult;
    use crate::report::SCHEMA_VERSION;
    use crate::review::{ReviewReport, ReviewState};
    use crate::risk::RiskReport;
    use crate::security::{ScanResult, SecurityReport};
    use crate::verify::Evidence;

    fn rec(pr: Option<u64>, round: u8, decision: Decision) -> RunRecord {
        let mut review =
            ReviewReport::skipped(SkipReason::NoProviderConfigured, ReviewState::default());
        review.round = round;
        RunRecord {
            schema_version: SCHEMA_VERSION,
            generated_at_unix: 0,
            pr,
            base: None,
            head: Some("h".into()),
            risk: RiskReport {
                overall: RiskLevel::Low,
                total_changed_lines: 0,
                reasons: vec![],
                files: vec![],
            },
            verify: Evidence::default(),
            security: SecurityReport {
                results: vec![ScanResult {
                    scanner: "gitleaks".into(),
                    status: ScanStatus::Skipped,
                    detail: String::new(),
                    findings: vec![],
                }],
            },
            review,
            gate: GateResult {
                decision,
                reasons: vec![],
            },
        }
    }

    #[test]
    fn empty_input_is_all_zero_without_nan() {
        let m = aggregate(&[]);
        assert_eq!(m.runs, 0);
        assert_eq!(m.avg_rounds_per_pr, 0.0);
        assert_eq!(m.hallucination_rate, 0.0);
    }

    #[test]
    fn rounds_are_grouped_per_pr_using_the_max() {
        let m = aggregate(&[
            rec(Some(1), 1, Decision::Fail),
            rec(Some(1), 3, Decision::Pass),
            rec(Some(2), 1, Decision::Pass),
        ]);
        assert_eq!(m.pull_requests, 2);
        assert_eq!(m.max_rounds_per_pr, 3);
        assert!((m.avg_rounds_per_pr - 2.0).abs() < 1e-9);
        assert_eq!(m.decisions["pass"], 2);
        assert_eq!(m.decisions["fail"], 1);
    }

    #[test]
    fn counts_findings_hallucinations_tokens_and_skips() {
        let mut a = rec(Some(1), 1, Decision::NeedsHuman);
        a.review.metrics.findings_raw = 4;
        a.review.metrics.findings_kept = 3;
        a.review.metrics.hallucinated = 1;
        a.review.metrics.input_tokens = 500;
        a.review.metrics.output_tokens = 50;
        a.review.metrics.llm_calls = 1;
        a.review.metrics.by_severity.insert("high".into(), 2);
        a.review.metrics.by_severity.insert("low".into(), 1);
        let m = aggregate(&[a, rec(Some(2), 0, Decision::Pass)]);
        assert_eq!(m.findings_by_severity["high"], 2);
        assert!((m.hallucination_rate - 0.25).abs() < 1e-9);
        assert_eq!((m.input_tokens, m.output_tokens, m.llm_calls), (500, 50, 1));
        assert_eq!(m.layers_skipped["verify"], 2);
        assert_eq!(m.layers_skipped["security:gitleaks"], 2);
        assert_eq!(m.layers_skipped["ai:no_provider"], 2);
    }

    #[test]
    fn parses_reverts_from_git_log_format() {
        let log = "aaa111\u{1f}Revert \"feat: x\"\u{1f}This reverts commit bbb.\u{1e}\n\
                   ccc222\u{1f}feat: y\u{1f}body\u{1e}\n\
                   ddd333\u{1f}fix: z\u{1f}This reverts commit eee because it broke prod\u{1e}\n\
                   fff444\u{1f}revert: q\u{1f}\u{1e}\n";
        let s = parse_reverts(log);
        assert_eq!(s.commits_scanned, 4);
        assert_eq!(s.reverts, 3);
        assert!((s.revert_rate - 0.75).abs() < 1e-9);
        assert_eq!(s.revert_commits[0], "aaa111");
        assert_eq!(parse_reverts("").reverts, 0);
    }

    #[test]
    fn load_records_skips_bad_files_with_warnings() {
        let d = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&rec(Some(1), 1, Decision::Pass)).unwrap();
        std::fs::write(d.path().join("a.json"), good).unwrap();
        std::fs::write(d.path().join("b.json"), "{not json").unwrap();
        std::fs::write(d.path().join("c.txt"), "ignored").unwrap();
        let (records, warnings) = load_records(d.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("b.json"));
    }

    #[test]
    fn detect_reverts_on_real_repo() {
        let d = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@e.com"])
                .args(args)
                .current_dir(d.path())
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(d.path().join("f"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "feat: add f"]);
        std::fs::write(d.path().join("f"), "2").unwrap();
        git(&[
            "commit",
            "-q",
            "-am",
            "Revert \"feat: add f\"",
            "-m",
            "This reverts commit abc.",
        ]);
        let s = detect_reverts(d.path(), 100).unwrap();
        assert_eq!((s.commits_scanned, s.reverts), (2, 1));
    }

    #[test]
    fn detect_reverts_outside_repo_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect_reverts(d.path(), 10).is_err());
    }

    #[test]
    fn markdown_mentions_every_metric() {
        let md = aggregate(&[rec(Some(1), 2, Decision::Pass)])
            .to_markdown(Some(&RevertStats::default()));
        for needle in [
            "Rounds per PR",
            "Hallucination rate",
            "Layers skipped",
            "Tokens",
            "Reverts",
        ] {
            assert!(md.contains(needle), "{needle}");
        }
    }
}
