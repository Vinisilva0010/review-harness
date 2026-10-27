//! AI review engine: advisory, budgeted, schema-validated.
//!
//! The model can only return findings. There is no way for its output to approve a
//! change or alter the gate: the schema has no verdict field and unknown fields are
//! rejected.

pub mod adapters;
pub mod finding;
pub mod prompt;
pub mod provider;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{ProviderKind, ReviewConfig, RiskLevel};
use crate::diff::Diff;

use adapters::{AnthropicProvider, GeminiProvider, OpenAiCompatibleProvider};
use finding::{parse_findings, validate, AllowedLines, Category, Finding, Severity};
use provider::{HttpTransport, LlmProvider, LlmRequest, RetryPolicy, Secret, UreqTransport};

/// Output budget requested from the model per call.
pub const MAX_OUTPUT_TOKENS: u32 = 2048;

/// A provider that is ready, or the reason there is none.
pub enum ProviderSlot {
    Ready(Box<dyn LlmProvider>),
    NotConfigured,
    KeyMissing { env: String },
}

/// Build the provider from config; keys come from `env` (the process environment).
pub fn build_provider(
    cfg: &ReviewConfig,
    env: &dyn Fn(&str) -> Option<String>,
    transport: Arc<dyn HttpTransport>,
    retry: RetryPolicy,
) -> Result<ProviderSlot, String> {
    if cfg.provider == ProviderKind::None {
        return Ok(ProviderSlot::NotConfigured);
    }
    let env_name = cfg
        .api_key_env
        .clone()
        .ok_or("review.api_key_env missing")?;
    let Some(key) = env(&env_name).filter(|k| !k.trim().is_empty()) else {
        return Ok(ProviderSlot::KeyMissing { env: env_name });
    };
    let key = Secret::new(key.trim());
    let model = cfg.model.clone().ok_or("review.model missing")?;
    let base = cfg.base_url.as_deref();
    let p: Box<dyn LlmProvider> = match cfg.provider {
        ProviderKind::Gemini => Box::new(GeminiProvider::new(&model, key, base, transport, retry)?),
        ProviderKind::Anthropic => {
            Box::new(AnthropicProvider::new(&model, key, base, transport, retry)?)
        }
        ProviderKind::OpenaiCompatible => Box::new(OpenAiCompatibleProvider::new(
            &model,
            key,
            base.ok_or("review.base_url missing")?,
            transport,
            retry,
        )?),
        ProviderKind::None => unreachable!("handled above"),
    };
    Ok(ProviderSlot::Ready(p))
}

/// Convenience: real transport and default retry policy.
pub fn build_default_provider(
    cfg: &ReviewConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ProviderSlot, String> {
    build_provider(
        cfg,
        env,
        Arc::new(UreqTransport::default()),
        RetryPolicy::default(),
    )
}

/// Persisted between runs of the same PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReviewState {
    pub rounds_done: u8,
    pub last_reviewed_commit: Option<String>,
    pub tokens_spent: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkipReason {
    NoProviderConfigured,
    ApiKeyMissing {
        env: String,
    },
    BelowRiskThreshold {
        risk: RiskLevel,
        min_risk: RiskLevel,
    },
    NoReviewableChanges,
    /// A deterministic layer already failed; spending tokens would not change the outcome.
    EarlierLayerFailed,
}

impl SkipReason {
    pub fn describe(&self) -> String {
        match self {
            Self::NoProviderConfigured => "skipped: no provider configured".into(),
            Self::ApiKeyMissing { env } => {
                format!("skipped: provider configured but environment variable {env} is not set")
            }
            Self::BelowRiskThreshold { risk, min_risk } => {
                format!("skipped: risk {risk} is below the review threshold {min_risk}")
            }
            Self::NoReviewableChanges => "skipped: no added lines to review".into(),
            Self::EarlierLayerFailed => {
                "skipped: a deterministic layer already failed (no tokens spent)".into()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewStatus {
    Skipped {
        reason: SkipReason,
    },
    Completed,
    /// Max rounds already used: stop iterating and rethink the approach.
    StepBack,
    /// Estimated cost exceeds the token budget; nothing was sent.
    OverBudget {
        estimated: u32,
        budget: u32,
    },
    /// Fail closed: the layer did not produce a trustworthy result.
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReviewMetrics {
    pub findings_raw: usize,
    pub findings_kept: usize,
    /// Findings dropped by the validator for pointing outside the diff.
    pub hallucinated: usize,
    pub duplicates: usize,
    pub by_severity: BTreeMap<String, usize>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub llm_calls: u32,
    pub invalid_json_retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub status: ReviewStatus,
    pub round: u8,
    pub findings: Vec<Finding>,
    /// The AI layer is advisory: it can request changes, never approve.
    pub requests_changes: bool,
    pub notices: Vec<String>,
    pub metrics: ReviewMetrics,
    pub state: ReviewState,
}

impl ReviewReport {
    /// A report for a layer that did not run.
    pub fn skipped(reason: SkipReason, state: ReviewState) -> Self {
        Self {
            status: ReviewStatus::Skipped {
                reason: reason.clone(),
            },
            round: state.rounds_done,
            findings: Vec::new(),
            requests_changes: false,
            notices: vec![format!("AI review {}", reason.describe())],
            metrics: ReviewMetrics::default(),
            state,
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::from("### AI review (advisory)\n\n");
        match &self.status {
            ReviewStatus::Skipped { reason } => out.push_str(&format!("{}\n", reason.describe())),
            ReviewStatus::Completed => out.push_str(&format!(
                "Round {}: {} finding(s), {} dropped as outside the diff.\n",
                self.round, self.metrics.findings_kept, self.metrics.hallucinated
            )),
            ReviewStatus::StepBack => out.push_str("Round limit reached.\n"),
            ReviewStatus::OverBudget { estimated, budget } => out.push_str(&format!(
                "refused: estimated {estimated} tokens exceeds the budget of {budget}; nothing was sent.\n"
            )),
            ReviewStatus::Failed { reason } => {
                out.push_str(&format!("**failed (not a pass)**: {reason}\n"))
            }
        }
        for n in &self.notices {
            out.push_str(&format!("\n> {n}\n"));
        }
        for f in &self.findings {
            out.push_str(&format!(
                "\n- **{:?}** `{}:{}` ({:?}): {}",
                f.severity, f.file, f.line, f.category, f.explanation
            ));
            if let Some(fix) = &f.suggested_fix {
                out.push_str(&format!("\n  - suggested fix: {fix}"));
            }
        }
        out.push('\n');
        out
    }
}

pub struct ReviewInput<'a> {
    /// The full PR diff (used to validate that findings point at PR lines).
    pub diff: &'a Diff,
    /// Diff of changes since the last reviewed commit, for rounds after the first.
    pub incremental: Option<&'a Diff>,
    pub risk: RiskLevel,
    pub head_commit: Option<String>,
    pub state: ReviewState,
}

pub struct ReviewEngine {
    cfg: ReviewConfig,
    provider: ProviderSlot,
}

impl ReviewEngine {
    pub fn new(cfg: ReviewConfig, provider: ProviderSlot) -> Self {
        Self { cfg, provider }
    }

    pub fn review(&self, input: ReviewInput<'_>) -> ReviewReport {
        let mut report = ReviewReport {
            status: ReviewStatus::Completed,
            round: input.state.rounds_done,
            findings: Vec::new(),
            requests_changes: false,
            notices: Vec::new(),
            metrics: ReviewMetrics::default(),
            state: input.state.clone(),
        };
        let skip = |mut r: ReviewReport, reason: SkipReason| {
            r.notices.push(format!("AI review {}", reason.describe()));
            r.status = ReviewStatus::Skipped { reason };
            r
        };

        let provider = match &self.provider {
            ProviderSlot::Ready(p) => p.as_ref(),
            ProviderSlot::NotConfigured => return skip(report, SkipReason::NoProviderConfigured),
            ProviderSlot::KeyMissing { env } => {
                return skip(report, SkipReason::ApiKeyMissing { env: env.clone() })
            }
        };
        if input.risk < self.cfg.min_risk {
            return skip(
                report,
                SkipReason::BelowRiskThreshold {
                    risk: input.risk,
                    min_risk: self.cfg.min_risk,
                },
            );
        }
        if input.state.rounds_done >= self.cfg.max_rounds {
            report.status = ReviewStatus::StepBack;
            report.notices.push(step_back_notice(self.cfg.max_rounds));
            return report;
        }

        // Scope: later rounds only look at lines changed since the last reviewed commit.
        let full: AllowedLines = file_lines(input.diff);
        let (scope_diff, allowed) = match (input.state.rounds_done > 0, input.incremental) {
            (true, Some(inc)) => {
                let inc_lines = file_lines(inc);
                let allowed: AllowedLines = inc_lines
                    .into_iter()
                    .filter_map(|(path, lines)| {
                        let both: std::collections::BTreeSet<u32> = full
                            .get(&path)
                            .map(|f| lines.intersection(f).copied().collect())
                            .unwrap_or_default();
                        (!both.is_empty()).then_some((path, both))
                    })
                    .collect();
                (inc, allowed)
            }
            _ => (input.diff, full),
        };
        let allowed: AllowedLines = allowed.into_iter().filter(|(_, l)| !l.is_empty()).collect();
        if allowed.is_empty() {
            return skip(report, SkipReason::NoReviewableChanges);
        }

        let system = prompt::system_prompt();
        let user = prompt::user_prompt(&prompt::render_diff(scope_diff, &allowed));
        let budget = self.cfg.token_budget;
        let estimate = prompt::estimate_tokens(&system) + prompt::estimate_tokens(&user);
        if input.state.tokens_spent.saturating_add(estimate) > budget {
            report.status = ReviewStatus::OverBudget {
                estimated: input.state.tokens_spent.saturating_add(estimate),
                budget,
            };
            report
                .notices
                .push("AI review refused: over the token budget (nothing was sent)".into());
            return report;
        }

        let mut req = LlmRequest {
            system,
            user,
            max_output_tokens: MAX_OUTPUT_TOKENS,
        };
        let mut parsed: Option<Vec<Finding>> = None;
        let mut last_err = String::new();
        for attempt in 0..2 {
            if attempt == 1 {
                req.user.push_str(&prompt::retry_addendum(&last_err));
                let est = prompt::estimate_tokens(&req.system) + prompt::estimate_tokens(&req.user);
                if report.state.tokens_spent.saturating_add(est) > budget {
                    return fail(
                        report,
                        "invalid model output and no budget left to retry".into(),
                    );
                }
                report.metrics.invalid_json_retries += 1;
            }
            report.metrics.llm_calls += 1;
            let resp = match provider.complete(&req) {
                Ok(r) => r,
                Err(e) => return fail(report, format!("provider error: {e}")),
            };
            let est = prompt::estimate_tokens(&req.system) + prompt::estimate_tokens(&req.user);
            let used_in = resp.input_tokens.unwrap_or(est);
            let used_out = resp.output_tokens.unwrap_or(0);
            report.metrics.input_tokens = report.metrics.input_tokens.saturating_add(used_in);
            report.metrics.output_tokens = report.metrics.output_tokens.saturating_add(used_out);
            report.state.tokens_spent = report
                .state
                .tokens_spent
                .saturating_add(used_in)
                .saturating_add(used_out);
            match parse_findings(&resp.text) {
                Ok(f) => {
                    parsed = Some(f);
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        let Some(raw) = parsed else {
            return fail(
                report,
                format!("model output was not valid after one retry ({last_err})"),
            );
        };

        let raw_count = raw.len();
        let v = validate(raw, &allowed);
        report.metrics.findings_raw = raw_count;
        report.metrics.findings_kept = v.kept.len();
        report.metrics.hallucinated = v.hallucinated;
        report.metrics.duplicates = v.duplicates;
        for f in &v.kept {
            *report
                .metrics
                .by_severity
                .entry(format!("{:?}", f.severity).to_lowercase())
                .or_default() += 1;
        }
        report.requests_changes = v
            .kept
            .iter()
            .any(|f| f.severity >= Severity::Medium || f.category == Category::PromptInjection);
        report.findings = v.kept;
        report.round = input.state.rounds_done + 1;
        report.state.rounds_done = report.round;
        report.state.last_reviewed_commit = input.head_commit;
        if report.round >= self.cfg.max_rounds && !report.findings.is_empty() {
            report.notices.push(step_back_notice(self.cfg.max_rounds));
        }
        report
    }
}

fn file_lines(d: &Diff) -> AllowedLines {
    d.files
        .iter()
        .map(|f| (f.path().to_string(), f.added_lines()))
        .collect()
}

fn step_back_notice(max_rounds: u8) -> String {
    format!(
        "{max_rounds} review rounds used: step back and rethink the approach instead of patching further."
    )
}

fn fail(mut r: ReviewReport, reason: String) -> ReviewReport {
    r.notices.push(format!("AI review failed closed: {reason}"));
    r.status = ReviewStatus::Failed { reason };
    r
}
