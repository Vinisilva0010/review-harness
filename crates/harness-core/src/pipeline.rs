//! Orchestrates every layer, cheapest first, and produces a [`RunRecord`].

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::diff::Diff;
use crate::gate::{decide, AiOutcome, GateInput};
use crate::report::{RunRecord, SCHEMA_VERSION};
use crate::review::{
    ProviderSlot, ReviewEngine, ReviewInput, ReviewReport, ReviewState, SkipReason,
};
use crate::risk::Classifier;
use crate::security::{self, SecurityVerdict, Tools};
use crate::verify::{self, Limits, Verdict};
use std::time::Duration;

pub struct PipelineInput<'a> {
    pub config: &'a Config,
    pub dir: &'a Path,
    pub diff: &'a Diff,
    pub incremental: Option<&'a Diff>,
    pub base: Option<String>,
    pub head: Option<String>,
    pub pr: Option<u64>,
    pub project_rules: Option<&'a str>,
    pub state: ReviewState,
    pub provider: ProviderSlot,
    pub tools: Tools,
}

/// Run classify -> verify -> security -> (AI review only if worthwhile) -> gate.
pub fn run(input: PipelineInput<'_>) -> Result<RunRecord, String> {
    let classifier = Classifier::new(&input.config.risk).map_err(|e| e.to_string())?;
    let risk = classifier.classify(input.diff);

    let limits = Limits {
        timeout: Duration::from_secs(input.config.verify.timeout_secs),
        ..Limits::default()
    };
    let evidence = verify::run_checks(&input.config.verify.checks, input.dir, limits);

    let range = input
        .base
        .as_ref()
        .zip(input.head.as_ref())
        .map(|(b, h)| format!("{b}..{h}"));
    let security = security::run_all(
        &input.config.security,
        &input.tools,
        input.dir,
        range.as_deref(),
    );

    // Do not spend tokens when a deterministic layer already decided the outcome.
    let review: ReviewReport = if evidence.verdict() == Verdict::Failed
        || security.verdict() == SecurityVerdict::Findings
    {
        ReviewReport::skipped(SkipReason::EarlierLayerFailed, input.state)
    } else {
        ReviewEngine::new(input.config.review.clone(), input.provider).review(ReviewInput {
            diff: input.diff,
            incremental: input.incremental,
            risk: risk.overall,
            head_commit: input.head.clone(),
            project_rules: input.project_rules,
            state: input.state,
        })
    };

    let gate = decide(GateInput {
        risk: risk.overall,
        verify: evidence.verdict(),
        security: security.verdict(),
        ai: AiOutcome::from(&review),
    });
    Ok(RunRecord {
        schema_version: SCHEMA_VERSION,
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        pr: input.pr,
        base: input.base,
        head: input.head,
        risk,
        verify: evidence,
        security,
        review,
        gate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Decision;
    use crate::review::provider::MockProvider;
    use crate::verify::Check;

    const DIFF: &str = "diff --git a/docs/a.md b/docs/a.md\n--- a/docs/a.md\n+++ b/docs/a.md\n@@ -0,0 +1 @@\n+hello\n";

    fn no_tools() -> Tools {
        Tools {
            cargo: "missing-cargo-xyz".into(),
            gitleaks: "missing-gitleaks-xyz".into(),
        }
    }

    fn cfg(checks: &[(&str, &str)]) -> Config {
        let mut c = Config::default();
        c.verify.checks = checks
            .iter()
            .map(|(n, cmd)| Check {
                name: (*n).into(),
                command: (*cmd).into(),
            })
            .collect();
        c
    }

    fn run_with(config: &Config, provider: ProviderSlot, diff: &Diff) -> RunRecord {
        let dir = tempfile::tempdir().unwrap();
        run(PipelineInput {
            config,
            dir: dir.path(),
            diff,
            incremental: None,
            base: None,
            head: None,
            pr: None,
            project_rules: None,
            state: ReviewState::default(),
            provider,
            tools: no_tools(),
        })
        .unwrap()
    }

    #[test]
    fn scanners_missing_means_needs_human_even_when_everything_else_passes() {
        let d = Diff::parse(DIFF);
        let r = run_with(&cfg(&[("ok", "true")]), ProviderSlot::NotConfigured, &d);
        assert_eq!(r.gate.decision, Decision::NeedsHuman);
        assert!(r
            .gate
            .reasons
            .iter()
            .any(|x| x.contains("security scan incomplete")));
    }

    #[test]
    fn failing_check_fails_and_skips_ai_without_spending_tokens() {
        let d = Diff::parse(DIFF.replace("docs/a.md", "src/a.rs").as_str());
        let mock = MockProvider::with_texts(&[]);
        let r = run_with(
            &cfg(&[("bad", "exit 1")]),
            ProviderSlot::Ready(Box::new(mock.clone())),
            &d,
        );
        assert_eq!(r.gate.decision, Decision::Fail);
        assert!(mock.requests().is_empty());
        assert!(r.review.notices[0].contains("deterministic layer already failed"));
    }

    #[test]
    fn ai_requests_changes_blocks_pass_and_reports_findings() {
        let d = Diff::parse(DIFF.replace("docs/a.md", "src/a.rs").as_str());
        let reply = r#"{"findings":[{"file":"src/a.rs","line":1,"severity":"high","category":"bug","explanation":"bad","suggested_fix":null}]}"#;
        let mock = MockProvider::with_texts(&[reply]);
        let r = run_with(
            &cfg(&[("ok", "true")]),
            ProviderSlot::Ready(Box::new(mock)),
            &d,
        );
        assert!(r.review.requests_changes);
        assert_eq!(r.gate.decision, Decision::NeedsHuman);
        assert!(r.to_markdown().contains("src/a.rs:1"));
    }

    #[test]
    fn no_checks_configured_is_never_a_pass() {
        let d = Diff::parse(DIFF);
        let r = run_with(&cfg(&[]), ProviderSlot::NotConfigured, &d);
        assert_ne!(r.gate.decision, Decision::Pass);
    }

    #[test]
    fn invalid_risk_glob_is_an_error_not_a_panic() {
        let mut c = cfg(&[]);
        c.risk.high = vec!["[".into()];
        let d = Diff::parse(DIFF);
        let dir = tempfile::tempdir().unwrap();
        let e = run(PipelineInput {
            config: &c,
            dir: dir.path(),
            diff: &d,
            incremental: None,
            base: None,
            head: None,
            pr: None,
            project_rules: None,
            state: ReviewState::default(),
            provider: ProviderSlot::NotConfigured,
            tools: no_tools(),
        });
        assert!(e.is_err());
    }
}
