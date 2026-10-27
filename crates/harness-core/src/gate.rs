//! Gate: combines every layer into pass / needs-human / fail.
//!
//! Invariants (enforced by exhaustive tests):
//! * `Pass` needs passing evidence, a clean security scan, and risk below `High`.
//! * The AI layer can only make the result stricter, never more lenient.
//! * Skipped or failed layers never count as passed.

use serde::{Deserialize, Serialize};

use crate::config::RiskLevel;
use crate::review::{ReviewReport, ReviewStatus};
use crate::security::SecurityVerdict;
use crate::verify::Verdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pass,
    NeedsHuman,
    Fail,
}

impl Decision {
    /// Process exit code: 0 pass, 3 needs-human, 1 fail (2 is reserved for errors).
    pub fn exit_code(self) -> u8 {
        match self {
            Decision::Pass => 0,
            Decision::Fail => 1,
            Decision::NeedsHuman => 3,
        }
    }
}

/// Outcome of the (advisory) AI layer, as the gate sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOutcome {
    /// Did not run (no provider, below threshold, nothing to review...). Not a pass signal.
    Skipped,
    /// Ran and reported nothing that requests changes.
    Clean,
    RequestsChanges,
    /// Round limit reached.
    StepBack,
    OverBudget,
    /// Failed closed.
    Failed,
}

impl From<&ReviewReport> for AiOutcome {
    fn from(r: &ReviewReport) -> Self {
        match r.status {
            ReviewStatus::Skipped { .. } => AiOutcome::Skipped,
            ReviewStatus::StepBack => AiOutcome::StepBack,
            ReviewStatus::OverBudget { .. } => AiOutcome::OverBudget,
            ReviewStatus::Failed { .. } => AiOutcome::Failed,
            ReviewStatus::Completed if r.requests_changes => AiOutcome::RequestsChanges,
            ReviewStatus::Completed => AiOutcome::Clean,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GateInput {
    pub risk: RiskLevel,
    pub verify: Verdict,
    pub security: SecurityVerdict,
    pub ai: AiOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub decision: Decision,
    pub reasons: Vec<String>,
}

pub fn decide(i: GateInput) -> GateResult {
    let mut fail = Vec::new();
    let mut human = Vec::new();

    match i.verify {
        Verdict::Verified => {}
        Verdict::Failed => fail.push("verification checks failed".to_string()),
        Verdict::NoEvidence => {
            human.push("no verification evidence: nothing was proven".to_string())
        }
    }
    match i.security {
        SecurityVerdict::Clean => {}
        SecurityVerdict::Findings => fail.push("security scanners reported findings".to_string()),
        SecurityVerdict::Incomplete => {
            human.push("security scan incomplete (scanner skipped or failed)".to_string());
        }
    }
    match i.ai {
        AiOutcome::Skipped | AiOutcome::Clean => {}
        AiOutcome::RequestsChanges => human.push("AI review requests changes".to_string()),
        AiOutcome::StepBack => {
            human.push("review rounds exhausted: step back and rethink the approach".to_string());
        }
        AiOutcome::OverBudget => human.push("change too large for the AI token budget".to_string()),
        AiOutcome::Failed => human.push("AI review failed closed".to_string()),
    }
    if i.risk == RiskLevel::High {
        human.push("high-risk change always requires human approval".to_string());
    }

    if !fail.is_empty() {
        fail.extend(human);
        GateResult {
            decision: Decision::Fail,
            reasons: fail,
        }
    } else if !human.is_empty() {
        GateResult {
            decision: Decision::NeedsHuman,
            reasons: human,
        }
    } else {
        GateResult {
            decision: Decision::Pass,
            reasons: vec!["all layers passed and risk is below high".to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RISKS: [RiskLevel; 3] = [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High];
    const VERIFY: [Verdict; 3] = [Verdict::Verified, Verdict::Failed, Verdict::NoEvidence];
    const SEC: [SecurityVerdict; 3] = [
        SecurityVerdict::Clean,
        SecurityVerdict::Findings,
        SecurityVerdict::Incomplete,
    ];
    const AI: [AiOutcome; 6] = [
        AiOutcome::Skipped,
        AiOutcome::Clean,
        AiOutcome::RequestsChanges,
        AiOutcome::StepBack,
        AiOutcome::OverBudget,
        AiOutcome::Failed,
    ];

    fn all() -> impl Iterator<Item = GateInput> {
        RISKS.into_iter().flat_map(|risk| {
            VERIFY.into_iter().flat_map(move |verify| {
                SEC.into_iter().flat_map(move |security| {
                    AI.into_iter().map(move |ai| GateInput {
                        risk,
                        verify,
                        security,
                        ai,
                    })
                })
            })
        })
    }

    #[test]
    fn full_decision_table_matches_oracle() {
        let mut n = 0;
        for i in all() {
            n += 1;
            let expected = if i.verify == Verdict::Failed || i.security == SecurityVerdict::Findings
            {
                Decision::Fail
            } else if i.risk == RiskLevel::High
                || i.verify != Verdict::Verified
                || i.security != SecurityVerdict::Clean
                || !matches!(i.ai, AiOutcome::Skipped | AiOutcome::Clean)
            {
                Decision::NeedsHuman
            } else {
                Decision::Pass
            };
            let got = decide(i);
            assert_eq!(got.decision, expected, "{i:?}");
            assert!(!got.reasons.is_empty(), "{i:?}");
        }
        assert_eq!(n, 3 * 3 * 3 * 6);
    }

    #[test]
    fn high_risk_never_passes() {
        for i in all().filter(|i| i.risk == RiskLevel::High) {
            assert_ne!(decide(i).decision, Decision::Pass, "{i:?}");
        }
    }

    #[test]
    fn ai_can_only_tighten_never_loosen() {
        let rank = |d: Decision| match d {
            Decision::Pass => 0,
            Decision::NeedsHuman => 1,
            Decision::Fail => 2,
        };
        for i in all() {
            let baseline = decide(GateInput {
                ai: AiOutcome::Skipped,
                ..i
            });
            assert!(rank(decide(i).decision) >= rank(baseline.decision), "{i:?}");
        }
    }

    #[test]
    fn skipped_layers_never_produce_pass() {
        for i in all() {
            if i.verify == Verdict::NoEvidence || i.security == SecurityVerdict::Incomplete {
                assert_ne!(decide(i).decision, Decision::Pass, "{i:?}");
            }
        }
    }

    #[test]
    fn exit_codes_are_distinct() {
        assert_eq!(Decision::Pass.exit_code(), 0);
        assert_eq!(Decision::Fail.exit_code(), 1);
        assert_eq!(Decision::NeedsHuman.exit_code(), 3);
    }

    #[test]
    fn failure_reasons_include_human_reasons_too() {
        let r = decide(GateInput {
            risk: RiskLevel::High,
            verify: Verdict::Failed,
            security: SecurityVerdict::Clean,
            ai: AiOutcome::Skipped,
        });
        assert_eq!(r.decision, Decision::Fail);
        assert!(r.reasons.len() >= 2);
    }

    #[test]
    fn ai_outcome_mapping() {
        use crate::review::{ReviewState, SkipReason};
        let skipped =
            ReviewReport::skipped(SkipReason::NoProviderConfigured, ReviewState::default());
        assert_eq!(AiOutcome::from(&skipped), AiOutcome::Skipped);
        let mut r = skipped.clone();
        r.status = ReviewStatus::Completed;
        assert_eq!(AiOutcome::from(&r), AiOutcome::Clean);
        r.requests_changes = true;
        assert_eq!(AiOutcome::from(&r), AiOutcome::RequestsChanges);
        r.status = ReviewStatus::Failed { reason: "x".into() };
        assert_eq!(AiOutcome::from(&r), AiOutcome::Failed);
    }
}
