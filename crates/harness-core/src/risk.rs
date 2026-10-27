//! Deterministic, zero-token risk classifier.
//!
//! Each file gets a [`RiskLevel`] from path globs plus extra signals (CI files,
//! dependency manifests, size, binary files, secret-like added lines). The overall
//! risk is the maximum over files and PR-wide signals. Every level comes with reasons.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{RiskConfig, RiskLevel};
use crate::diff::{Diff, FileDiff, LineKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    PathRule,
    UnmatchedPath,
    LargeFile,
    CiWorkflow,
    DependencyManifest,
    SecretLike,
    BinaryFile,
    HugeChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub signal: Signal,
    pub level: RiskLevel,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRisk {
    pub path: String,
    pub level: RiskLevel,
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskReport {
    pub overall: RiskLevel,
    pub total_changed_lines: usize,
    /// PR-wide reasons (e.g. a huge change), separate from per-file reasons.
    pub reasons: Vec<Reason>,
    pub files: Vec<FileRisk>,
}

impl RiskReport {
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "### Risk: **{}** ({} changed lines, {} files)\n\n",
            self.overall.to_string().to_uppercase(),
            self.total_changed_lines,
            self.files.len()
        );
        for r in &self.reasons {
            out.push_str(&format!("- PR-wide ({}): {}\n", r.level, r.detail));
        }
        for f in &self.files {
            out.push_str(&format!("- `{}`: **{}**\n", f.path, f.level));
            for r in &f.reasons {
                out.push_str(&format!("  - {} ({})\n", r.detail, r.level));
            }
        }
        out
    }
}

struct SecretPattern {
    name: &'static str,
    re: Regex,
}

pub struct Classifier {
    high: GlobSet,
    medium: GlobSet,
    low: GlobSet,
    large_file_lines: usize,
    huge_change_lines: usize,
    secrets: Vec<SecretPattern>,
}

fn build(globs: &[String]) -> Result<GlobSet, globset::Error> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g)?);
    }
    b.build()
}

fn secret_patterns() -> Vec<SecretPattern> {
    let defs: [(&str, &str); 7] = [
        ("AWS access key id", r"\bAKIA[0-9A-Z]{16}\b"),
        ("private key block", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        (
            "GitHub token",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,})\b",
        ),
        ("Slack token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}"),
        ("Google API key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
        ("provider secret key (sk-)", r"\bsk-[A-Za-z0-9_-]{20,}"),
        (
            "hard-coded credential assignment",
            r#"(?i)\b(?:api[_-]?key|secret|passw(?:or)?d|token)\b["']?\s*[:=]\s*["'][^"'\s]{12,}["']"#,
        ),
    ];
    defs.into_iter()
        .map(|(name, re)| SecretPattern {
            name,
            re: Regex::new(re).expect("static secret regex is valid"),
        })
        .collect()
}

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "pyproject.toml",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "go.mod",
    "go.sum",
    "Gemfile",
    "Gemfile.lock",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

fn is_manifest(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    MANIFESTS.contains(&name) || (name.starts_with("requirements") && name.ends_with(".txt"))
}

fn is_ci(path: &str) -> bool {
    path.starts_with(".github/workflows/")
        || path.starts_with(".circleci/")
        || path == ".gitlab-ci.yml"
        || path == "Jenkinsfile"
        || path == "azure-pipelines.yml"
}

impl Classifier {
    pub fn new(cfg: &RiskConfig) -> Result<Self, globset::Error> {
        Ok(Self {
            high: build(&cfg.high)?,
            medium: build(&cfg.medium)?,
            low: build(&cfg.low)?,
            large_file_lines: cfg.large_file_lines as usize,
            huge_change_lines: cfg.huge_change_lines as usize,
            secrets: secret_patterns(),
        })
    }

    pub fn classify(&self, diff: &Diff) -> RiskReport {
        let files: Vec<FileRisk> = diff.files.iter().map(|f| self.classify_file(f)).collect();
        let total = diff.total_changed_lines();
        let mut reasons = Vec::new();
        if total >= self.huge_change_lines {
            reasons.push(Reason {
                signal: Signal::HugeChange,
                level: RiskLevel::High,
                detail: format!(
                    "{total} changed lines (threshold {})",
                    self.huge_change_lines
                ),
            });
        }
        let overall = files
            .iter()
            .map(|f| f.level)
            .chain(reasons.iter().map(|r| r.level))
            .max()
            .unwrap_or(RiskLevel::Low);
        RiskReport {
            overall,
            total_changed_lines: total,
            reasons,
            files,
        }
    }

    fn classify_file(&self, f: &FileDiff) -> FileRisk {
        let path = f.path().to_string();
        let mut reasons = Vec::new();
        let mut push = |signal, level, detail: String| {
            reasons.push(Reason {
                signal,
                level,
                detail,
            });
        };

        if self.high.is_match(&path) {
            push(
                Signal::PathRule,
                RiskLevel::High,
                "matches a high-risk path rule".into(),
            );
        } else if self.medium.is_match(&path) {
            push(
                Signal::PathRule,
                RiskLevel::Medium,
                "matches a medium-risk path rule".into(),
            );
        } else if self.low.is_match(&path) {
            push(
                Signal::PathRule,
                RiskLevel::Low,
                "matches a low-risk path rule".into(),
            );
        } else {
            push(
                Signal::UnmatchedPath,
                RiskLevel::Medium,
                "no path rule matches; unknown file types default to medium".into(),
            );
        }
        if is_ci(&path) {
            push(
                Signal::CiWorkflow,
                RiskLevel::High,
                "CI/CD configuration changed".into(),
            );
        }
        if is_manifest(&path) {
            push(
                Signal::DependencyManifest,
                RiskLevel::Medium,
                "dependency manifest or lockfile changed".into(),
            );
        }
        if f.binary {
            push(
                Signal::BinaryFile,
                RiskLevel::Medium,
                "binary file cannot be reviewed as text".into(),
            );
        }
        if f.changed_lines() >= self.large_file_lines {
            push(
                Signal::LargeFile,
                RiskLevel::Medium,
                format!(
                    "{} changed lines (threshold {})",
                    f.changed_lines(),
                    self.large_file_lines
                ),
            );
        }
        // Secret-like content: report pattern and line only, never the matched text.
        let mut seen: Vec<(&'static str, u32)> = Vec::new();
        for line in f.hunks.iter().flat_map(|h| h.lines.iter()) {
            if line.kind != LineKind::Added {
                continue;
            }
            for p in &self.secrets {
                if p.re.is_match(&line.content) {
                    seen.push((p.name, line.new_line.unwrap_or(0)));
                }
            }
        }
        for (name, line_no) in seen.into_iter().take(10) {
            push(
                Signal::SecretLike,
                RiskLevel::High,
                format!("possible {name} on added line {line_no}"),
            );
        }

        let level = reasons
            .iter()
            .map(|r| r.level)
            .max()
            .unwrap_or(RiskLevel::Low);
        FileRisk {
            path,
            level,
            reasons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> Classifier {
        Classifier::new(&RiskConfig::default()).unwrap()
    }

    fn file_diff(path: &str, added: &[&str]) -> String {
        let mut s = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
            added.len()
        );
        for a in added {
            s.push_str(&format!("+{a}\n"));
        }
        s
    }

    fn level_of(path: &str, added: &[&str]) -> RiskLevel {
        classifier()
            .classify(&Diff::parse(&file_diff(path, added)))
            .overall
    }

    #[test]
    fn empty_diff_is_low() {
        let r = classifier().classify(&Diff::default());
        assert_eq!(r.overall, RiskLevel::Low);
        assert!(r.files.is_empty());
    }

    #[test]
    fn docs_are_low_code_is_medium_workflows_are_high() {
        assert_eq!(level_of("README.md", &["hi"]), RiskLevel::Low);
        assert_eq!(level_of("docs/a/b.md", &["hi"]), RiskLevel::Low);
        assert_eq!(
            level_of("src/main.rs", &["fn main() {}"]),
            RiskLevel::Medium
        );
        assert_eq!(
            level_of(".github/workflows/ci.yml", &["on: push"]),
            RiskLevel::High
        );
        assert_eq!(level_of("src/auth/login.rs", &["x"]), RiskLevel::High);
        assert_eq!(level_of("db/migrations/001.sql", &["x"]), RiskLevel::High);
    }

    #[test]
    fn unknown_file_types_default_to_medium() {
        assert_eq!(level_of("data/blob.xyz", &["x"]), RiskLevel::Medium);
    }

    #[test]
    fn ci_signal_is_high_even_if_low_rules_would_match() {
        let cfg = RiskConfig {
            high: vec![],
            low: vec!["**".into()],
            ..RiskConfig::default()
        };
        let c = Classifier::new(&cfg).unwrap();
        let r = c.classify(&Diff::parse(&file_diff(".github/workflows/x.yml", &["a"])));
        assert_eq!(r.overall, RiskLevel::High);
        assert!(r.files[0]
            .reasons
            .iter()
            .any(|r| r.signal == Signal::CiWorkflow));
    }

    #[test]
    fn dependency_manifests_are_at_least_medium() {
        assert_eq!(level_of("Cargo.lock", &["x"]), RiskLevel::Medium);
        assert_eq!(level_of("web/package.json", &["{}"]), RiskLevel::Medium);
        assert_eq!(level_of("requirements-dev.txt", &["x"]), RiskLevel::Medium);
    }

    #[test]
    fn secret_like_lines_raise_to_high_without_leaking_value() {
        let key = format!("AKIA{}", "A".repeat(16));
        let line = format!("let k = \"{key}\";");
        let r = classifier().classify(&Diff::parse(&file_diff("notes.md", &[&line])));
        assert_eq!(r.overall, RiskLevel::High);
        let md = r.to_markdown();
        assert!(md.contains("AWS access key id"));
        assert!(md.contains("added line 1"));
        assert!(!md.contains(&key));
        assert!(!serde_json::to_string(&r).unwrap().contains(&key));
    }

    #[test]
    fn credential_assignment_and_private_key_detected() {
        let pw = format!("password = \"{}\"", "x".repeat(14));
        assert_eq!(level_of("a.md", &[&pw]), RiskLevel::High);
        let pk = format!("-----BEGIN {} PRIVATE KEY-----", "RSA");
        assert_eq!(level_of("a.md", &[&pk]), RiskLevel::High);
        let gh = format!("ghp_{}", "a".repeat(36));
        assert_eq!(level_of("a.md", &[&gh]), RiskLevel::High);
    }

    #[test]
    fn removed_secrets_and_short_values_do_not_trigger() {
        let d = "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1 +0,0 @@\n-password = \"hunter2hunter2hunter2\"\n";
        assert_eq!(
            classifier().classify(&Diff::parse(d)).overall,
            RiskLevel::Low
        );
        assert_eq!(level_of("a.md", &["password = \"short\""]), RiskLevel::Low);
    }

    #[test]
    fn large_file_raises_to_medium() {
        let many: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(level_of("notes.md", &refs), RiskLevel::Medium);
    }

    #[test]
    fn huge_change_raises_pr_to_high() {
        let cfg = RiskConfig {
            huge_change_lines: 10,
            ..RiskConfig::default()
        };
        let c = Classifier::new(&cfg).unwrap();
        let many: Vec<String> = (0..12).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let r = c.classify(&Diff::parse(&file_diff("a.md", &refs)));
        assert_eq!(r.overall, RiskLevel::High);
        assert_eq!(r.files[0].level, RiskLevel::Low);
        assert_eq!(r.reasons[0].signal, Signal::HugeChange);
    }

    #[test]
    fn binary_files_are_medium() {
        let d = "diff --git a/logo.md b/logo.md\nBinary files a/logo.md and b/logo.md differ\n";
        let r = classifier().classify(&Diff::parse(d));
        assert_eq!(r.overall, RiskLevel::Medium);
    }

    #[test]
    fn deleted_and_renamed_use_their_path() {
        let d = "diff --git a/src/a.rs b/docs/a.md\nsimilarity index 100%\nrename from src/a.rs\nrename to docs/a.md\n";
        let r = classifier().classify(&Diff::parse(d));
        assert_eq!(r.files[0].path, "docs/a.md");
        assert_eq!(r.overall, RiskLevel::Low);
    }

    #[test]
    fn overall_is_max_of_files() {
        let d = format!(
            "{}{}",
            file_diff("README.md", &["a"]),
            file_diff(".github/workflows/x.yml", &["b"])
        );
        let r = classifier().classify(&Diff::parse(&d));
        assert_eq!(r.overall, RiskLevel::High);
        assert_eq!(r.files.len(), 2);
    }
}
