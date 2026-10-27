//! Findings: strict schema parsing and the zero-token validator.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const MAX_FINDINGS: usize = 50;
pub const MAX_TEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Bug,
    Security,
    Performance,
    Maintainability,
    Tests,
    Style,
    PromptInjection,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub category: Category,
    pub explanation: String,
    #[serde(default)]
    pub suggested_fix: Option<String>,
}

/// The only shape a model may answer with. It has no verdict field by design.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    findings: Vec<Finding>,
}

/// JSON schema text embedded in the prompt.
pub const OUTPUT_SCHEMA: &str = r#"{"findings":[{"file":"<path exactly as shown>","line":<integer, a line marked + in the diff>,"severity":"info|low|medium|high","category":"bug|security|performance|maintainability|tests|style|prompt_injection|other","explanation":"<why this is a problem>","suggested_fix":"<optional concrete fix or null>"}]}"#;

/// Parse and validate the model's reply. Accepts surrounding whitespace and a
/// single Markdown code fence; anything else outside the schema is an error.
pub fn parse_findings(text: &str) -> Result<Vec<Finding>, String> {
    let body = strip_fence(text.trim());
    let env: Envelope = serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if env.findings.len() > MAX_FINDINGS {
        return Err(format!("too many findings ({})", env.findings.len()));
    }
    for f in &env.findings {
        if f.file.trim().is_empty() || f.line == 0 {
            return Err("finding needs a file and a line >= 1".into());
        }
        if f.explanation.trim().is_empty() {
            return Err("finding needs an explanation".into());
        }
        let too_long = f.explanation.chars().count() > MAX_TEXT_CHARS
            || f.suggested_fix
                .as_ref()
                .is_some_and(|s| s.chars().count() > MAX_TEXT_CHARS);
        if too_long {
            return Err("finding text exceeds length limit".into());
        }
    }
    Ok(env.findings)
}

fn strip_fence(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim().strip_suffix("```").map_or(s, str::trim)
}

/// Lines each file may be commented on.
pub type AllowedLines = BTreeMap<String, BTreeSet<u32>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    pub kept: Vec<Finding>,
    /// Findings pointing outside the allowed (added) lines: counted as hallucinated.
    pub hallucinated: usize,
    /// Exact repeats of an already kept finding.
    pub duplicates: usize,
}

/// Drop findings whose file/line is not an allowed added line; remove exact duplicates.
pub fn validate(findings: Vec<Finding>, allowed: &AllowedLines) -> Validated {
    let mut out = Validated {
        kept: Vec::new(),
        hallucinated: 0,
        duplicates: 0,
    };
    let mut seen = BTreeSet::new();
    for f in findings {
        if !allowed.get(&f.file).is_some_and(|l| l.contains(&f.line)) {
            out.hallucinated += 1;
        } else if seen.insert((f.file.clone(), f.line, f.category, f.explanation.clone())) {
            out.kept.push(f);
        } else {
            out.duplicates += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn finding(file: &str, line: u32) -> Finding {
        Finding {
            file: file.into(),
            line,
            severity: Severity::Medium,
            category: Category::Bug,
            explanation: format!("problem at {line}"),
            suggested_fix: None,
        }
    }

    fn allowed(file: &str, lines: &[u32]) -> AllowedLines {
        AllowedLines::from([(file.to_string(), lines.iter().copied().collect())])
    }

    const OK: &str = r#"{"findings":[{"file":"a.rs","line":3,"severity":"high","category":"security","explanation":"x","suggested_fix":"y"}]}"#;

    #[test]
    fn parses_valid_reply() {
        let f = parse_findings(OK).unwrap();
        assert_eq!(f[0].severity, Severity::High);
        assert_eq!(f[0].suggested_fix.as_deref(), Some("y"));
        assert!(parse_findings(r#"{"findings":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn suggested_fix_is_optional_or_null() {
        let t = r#"{"findings":[{"file":"a","line":1,"severity":"low","category":"style","explanation":"e","suggested_fix":null}]}"#;
        assert!(parse_findings(t).unwrap()[0].suggested_fix.is_none());
        let t = r#"{"findings":[{"file":"a","line":1,"severity":"low","category":"style","explanation":"e"}]}"#;
        assert!(parse_findings(t).is_ok());
    }

    #[test]
    fn accepts_a_single_code_fence() {
        assert!(parse_findings(&format!("```json\n{OK}\n```")).is_ok());
        assert!(parse_findings(&format!("```\n{OK}\n```")).is_ok());
    }

    #[test]
    fn rejects_non_schema_output() {
        for bad in [
            "",
            "I found no issues.",
            "[]",
            r#"{"findings":"none"}"#,
            r#"{"findings":[],"verdict":"approve"}"#,
            r#"{"findings":[{"file":"a","line":1,"severity":"urgent","category":"bug","explanation":"e"}]}"#,
            r#"{"findings":[{"file":"a","line":0,"severity":"low","category":"bug","explanation":"e"}]}"#,
            r#"{"findings":[{"file":"","line":1,"severity":"low","category":"bug","explanation":"e"}]}"#,
            r#"{"findings":[{"file":"a","line":1,"severity":"low","category":"bug","explanation":" "}]}"#,
            r#"{"findings":[{"file":"a","line":1,"severity":"low","category":"bug","explanation":"e","extra":1}]}"#,
            r#"{"findings":[{"file":"a","line":-1,"severity":"low","category":"bug","explanation":"e"}]}"#,
        ] {
            assert!(parse_findings(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_oversized_text_and_too_many_findings() {
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let t = format!(
            r#"{{"findings":[{{"file":"a","line":1,"severity":"low","category":"bug","explanation":"{long}"}}]}}"#
        );
        assert!(parse_findings(&t).is_err());
        let one = r#"{"file":"a","line":1,"severity":"low","category":"bug","explanation":"e"}"#;
        let many = vec![one; MAX_FINDINGS + 1].join(",");
        assert!(parse_findings(&format!(r#"{{"findings":[{many}]}}"#)).is_err());
    }

    #[test]
    fn validator_drops_out_of_diff_findings_and_counts_them() {
        let v = validate(
            vec![
                finding("a.rs", 2),
                finding("a.rs", 99),
                finding("other.rs", 2),
            ],
            &allowed("a.rs", &[1, 2, 3]),
        );
        assert_eq!(v.kept.len(), 1);
        assert_eq!(v.hallucinated, 2);
    }

    #[test]
    fn validator_dedupes_exact_repeats_without_calling_them_hallucinations() {
        let v = validate(
            vec![finding("a.rs", 2), finding("a.rs", 2)],
            &allowed("a.rs", &[2]),
        );
        assert_eq!((v.kept.len(), v.hallucinated, v.duplicates), (1, 0, 1));
    }

    #[test]
    fn empty_allowed_set_drops_everything() {
        let v = validate(vec![finding("a.rs", 1)], &AllowedLines::new());
        assert_eq!((v.kept.len(), v.hallucinated), (0, 1));
    }

    proptest! {
        #[test]
        fn validator_invariants(
            lines in proptest::collection::vec((0u8..3, 1u32..30), 0..40),
            allowed_lines in proptest::collection::btree_set(1u32..30, 0..15),
        ) {
            let files = ["a.rs", "b.rs", "c.rs"];
            let input: Vec<Finding> = lines
                .iter()
                .map(|(f, l)| finding(files[*f as usize], *l))
                .collect();
            let n = input.len();
            let allowed: AllowedLines = AllowedLines::from([("a.rs".to_string(), allowed_lines.clone())]);
            let v = validate(input, &allowed);
            prop_assert_eq!(v.kept.len() + v.hallucinated + v.duplicates, n);
            for k in &v.kept {
                prop_assert_eq!(k.file.as_str(), "a.rs");
                prop_assert!(allowed_lines.contains(&k.line));
            }
        }

        #[test]
        fn parser_never_panics(s in "\\PC*") {
            let _ = parse_findings(&s);
        }
    }
}
