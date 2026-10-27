//! Run record (JSON) and PR comment (Markdown) rendering.

use serde::{Deserialize, Serialize};

use crate::gate::{Decision, GateResult};
use crate::review::ReviewReport;
use crate::risk::RiskReport;
use crate::security::SecurityReport;
use crate::verify::Evidence;

/// Hidden marker so the Action can find and update its own comment.
pub const COMMENT_MARKER: &str = "<!-- review-harness:report -->";
/// GitHub rejects comments over 65,536 characters.
pub const MAX_COMMENT_CHARS: usize = 60_000;
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    pub generated_at_unix: u64,
    /// Pull request number, when known (groups rounds per PR for metrics).
    #[serde(default)]
    pub pr: Option<u64>,
    pub base: Option<String>,
    pub head: Option<String>,
    pub risk: RiskReport,
    pub verify: Evidence,
    pub security: SecurityReport,
    pub review: ReviewReport,
    pub gate: GateResult,
}

/// Neutralise text that came from a model, a diff or a command before it is put in
/// a PR comment: no raw HTML, no @mentions, no auto-linked issue refs spam.
pub fn sanitize_md(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('@', "@\u{200b}")
}

impl RunRecord {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn to_markdown(&self) -> String {
        let headline = match self.gate.decision {
            Decision::Pass => "PASS",
            Decision::NeedsHuman => "NEEDS HUMAN REVIEW",
            Decision::Fail => "FAIL",
        };
        let mut out = format!("{COMMENT_MARKER}\n## review-harness: {headline}\n\n");
        for r in &self.gate.reasons {
            out.push_str(&format!("- {}\n", sanitize_md(r)));
        }
        out.push('\n');
        out.push_str(&sanitize_risk(&self.risk));
        out.push('\n');
        out.push_str(&self.verify.to_markdown());
        out.push('\n');
        out.push_str(&self.security.to_markdown());
        out.push('\n');
        out.push_str(&sanitize_review(&self.review));
        out.push_str(
            "\n---\nThe AI layer is advisory: it can request changes but can never approve.\n",
        );
        if out.chars().count() > MAX_COMMENT_CHARS {
            out = out.chars().take(MAX_COMMENT_CHARS).collect();
            out.push_str("\n\n_(report truncated; see the run record artifact)_\n");
        }
        out
    }
}

fn sanitize_risk(r: &RiskReport) -> String {
    // Paths come from the diff, so they are untrusted too.
    let mut clean = r.clone();
    for f in &mut clean.files {
        f.path = sanitize_md(&f.path);
    }
    clean.to_markdown()
}

fn sanitize_review(r: &ReviewReport) -> String {
    let mut clean = r.clone();
    for f in &mut clean.findings {
        f.file = sanitize_md(&f.file);
        f.explanation = sanitize_md(&f.explanation);
        f.suggested_fix = f.suggested_fix.as_deref().map(sanitize_md);
    }
    for n in &mut clean.notices {
        *n = sanitize_md(n);
    }
    clean.to_markdown()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskLevel;
    use crate::review::finding::{Category, Finding, Severity};
    use crate::review::{ReviewState, ReviewStatus, SkipReason};

    fn record(decision: Decision) -> RunRecord {
        RunRecord {
            schema_version: SCHEMA_VERSION,
            generated_at_unix: 1,
            pr: None,
            base: Some("main".into()),
            head: Some("abc".into()),
            risk: RiskReport {
                overall: RiskLevel::Low,
                total_changed_lines: 0,
                reasons: vec![],
                files: vec![],
            },
            verify: Evidence::default(),
            security: SecurityReport::default(),
            review: ReviewReport::skipped(SkipReason::NoProviderConfigured, ReviewState::default()),
            gate: GateResult {
                decision,
                reasons: vec!["because".into()],
            },
        }
    }

    #[test]
    fn markdown_has_marker_headline_and_all_layers() {
        let md = record(Decision::NeedsHuman).to_markdown();
        assert!(md.starts_with(COMMENT_MARKER));
        assert!(md.contains("NEEDS HUMAN REVIEW"));
        assert!(md.contains("Risk:") && md.contains("Verification evidence"));
        assert!(md.contains("Security scanners") && md.contains("AI review (advisory)"));
        assert!(md.contains("skipped: no provider configured"));
    }

    #[test]
    fn each_decision_has_its_headline() {
        assert!(record(Decision::Pass).to_markdown().contains(": PASS"));
        assert!(record(Decision::Fail).to_markdown().contains(": FAIL"));
    }

    #[test]
    fn model_text_is_sanitised_in_markdown() {
        let mut r = record(Decision::Fail);
        r.review.status = ReviewStatus::Completed;
        r.review.findings.push(Finding {
            file: "a.rs".into(),
            line: 1,
            severity: Severity::High,
            category: Category::Bug,
            explanation: "<img src=x onerror=alert(1)> ping @octocat".into(),
            suggested_fix: Some("<script>x</script>".into()),
        });
        let md = r.to_markdown();
        assert!(!md.contains("<img") && !md.contains("<script"));
        assert!(!md.contains("@octocat"));
    }

    #[test]
    fn oversized_report_is_truncated_under_github_limit() {
        let mut r = record(Decision::Fail);
        r.gate.reasons = (0..20_000).map(|i| format!("reason number {i}")).collect();
        let md = r.to_markdown();
        assert!(md.chars().count() < 65_536);
        assert!(md.contains("truncated"));
    }

    #[test]
    fn json_round_trips() {
        let r = record(Decision::Pass);
        let back: RunRecord = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        assert_eq!(r, back);
        assert!(r.to_json().unwrap().contains("\"schema_version\": 1"));
    }
}
