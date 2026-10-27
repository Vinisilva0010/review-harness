//! `.review-harness.toml` parsing and validation.
//!
//! API keys are never part of the config: providers name an environment variable
//! (`api_key_env`) and the key is read from the process environment at call time.

use std::path::Path;

use globset::Glob;
use serde::{Deserialize, Serialize};

use crate::verify::Check;

/// Config file name looked up in the repository root.
pub const CONFIG_FILE: &str = ".review-harness.toml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid config syntax: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Risk levels, ordered `Low < Medium < High`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    #[default]
    Low,
    Medium,
    High,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RiskConfig {
    /// Globs whose files are always high risk.
    pub high: Vec<String>,
    /// Globs whose files are at least medium risk.
    pub medium: Vec<String>,
    /// Globs whose files are low risk (unless another signal raises them).
    pub low: Vec<String>,
    /// Lines changed in one file at or above which the file is at least medium.
    pub large_file_lines: u32,
    /// Total lines changed in the PR at or above which the PR is high risk.
    pub huge_change_lines: u32,
}

impl Default for RiskConfig {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect();
        Self {
            high: s(&[
                ".github/workflows/**",
                "**/action.yml",
                "**/Dockerfile*",
                "**/migrations/**",
                "**/*auth*/**",
                "**/*crypto*/**",
                "**/*secret*",
                "**/.env*",
            ]),
            medium: s(&[
                "**/*.rs",
                "**/*.py",
                "**/*.ts",
                "**/*.js",
                "**/*.go",
                "**/*.java",
                "**/*.sh",
            ]),
            low: s(&["**/*.md", "docs/**", "**/*.txt", "LICENSE"]),
            large_file_lines: 300,
            huge_change_lines: 1500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VerifyConfig {
    pub timeout_secs: u64,
    pub checks: Vec<Check>,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 600,
            checks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
    pub cargo_audit: bool,
    pub gitleaks: bool,
    pub timeout_secs: u64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cargo_audit: true,
            gitleaks: true,
            timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    None,
    Gemini,
    OpenaiCompatible,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReviewConfig {
    pub provider: ProviderKind,
    pub model: Option<String>,
    /// Base URL override (required for `openai_compatible`).
    pub base_url: Option<String>,
    /// NAME of the environment variable holding the API key. Never the key itself.
    pub api_key_env: Option<String>,
    /// AI review runs only when overall risk is at least this level.
    pub min_risk: RiskLevel,
    /// Hard cap on estimated input tokens per PR.
    pub token_budget: u32,
    /// Maximum review rounds per PR (1..=3).
    pub max_rounds: u8,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::None,
            model: None,
            base_url: None,
            api_key_env: None,
            min_risk: RiskLevel::Medium,
            token_budget: 30_000,
            max_rounds: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub risk: RiskConfig,
    pub verify: VerifyConfig,
    pub security: SecurityConfig,
    pub review: ReviewConfig,
}

impl Config {
    /// Parse and validate a config from TOML text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load `path`; a missing file yields the defaults.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        for (section, globs) in [
            ("risk.high", &self.risk.high),
            ("risk.medium", &self.risk.medium),
            ("risk.low", &self.risk.low),
        ] {
            for g in globs {
                if let Err(e) = Glob::new(g) {
                    return bad(format!("{section}: bad glob {g:?}: {e}"));
                }
            }
        }
        if self.risk.large_file_lines == 0 || self.risk.huge_change_lines == 0 {
            return bad("risk thresholds must be greater than zero".into());
        }
        if self.verify.timeout_secs == 0 {
            return bad("verify.timeout_secs must be greater than zero".into());
        }
        for c in &self.verify.checks {
            if c.name.trim().is_empty() || c.command.trim().is_empty() {
                return bad("verify.checks entries need a non-empty name and command".into());
            }
        }
        if self.security.timeout_secs == 0 {
            return bad("security.timeout_secs must be greater than zero".into());
        }
        let r = &self.review;
        if r.token_budget == 0 {
            return bad("review.token_budget must be greater than zero".into());
        }
        if !(1..=3).contains(&r.max_rounds) {
            return bad("review.max_rounds must be between 1 and 3".into());
        }
        if r.provider != ProviderKind::None {
            if r.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
                return bad("review.model is required when a provider is set".into());
            }
            if r.api_key_env.as_deref().is_none_or(|m| m.trim().is_empty()) {
                return bad("review.api_key_env (an env var NAME) is required".into());
            }
        }
        if r.provider == ProviderKind::OpenaiCompatible && r.base_url.is_none() {
            return bad("review.base_url is required for openai_compatible".into());
        }
        if let Some(name) = &r.api_key_env {
            let looks_like_name = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if !looks_like_name {
                return bad(
                    "review.api_key_env must be an environment variable NAME (e.g. GEMINI_API_KEY), not a key".into(),
                );
            }
        }
        if let Some(url) = &r.base_url {
            if !url.starts_with("https://") && !url.starts_with("http://localhost") {
                return bad("review.base_url must use https (http only for localhost)".into());
            }
        }
        Ok(())
    }
}

/// Commented default config written by `review-harness init`.
pub const DEFAULT_CONFIG_TOML: &str = r#"# review-harness configuration.
# Every section is optional; the values below are the defaults unless noted.

[risk]
# Files matching `high` always require human approval.
high = [".github/workflows/**", "**/action.yml", "**/Dockerfile*", "**/migrations/**", "**/*auth*/**", "**/*crypto*/**", "**/*secret*", "**/.env*"]
medium = ["**/*.rs", "**/*.py", "**/*.ts", "**/*.js", "**/*.go", "**/*.java", "**/*.sh"]
low = ["**/*.md", "docs/**", "**/*.txt", "LICENSE"]
# A single file changing this many lines is at least medium risk.
large_file_lines = 300
# A PR changing this many lines in total is high risk.
huge_change_lines = 1500

[verify]
timeout_secs = 600
# Example: checks = [{ name = "test", command = "cargo test --all" }]
checks = []

[security]
# Scanners that are not installed are reported as "skipped", never as passed.
cargo_audit = true
gitleaks = true
timeout_secs = 300

[review]
# One of: "none" (default, AI layer skipped), "gemini", "openai_compatible", "anthropic".
provider = "none"
# model = "your-model-id"
# base_url = "https://api.example.com/v1"   # required for openai_compatible
# The NAME of the environment variable holding your key. Never put the key here.
# api_key_env = "GEMINI_API_KEY"
# AI review only runs when overall risk is at least this level.
min_risk = "medium"
# Hard cap on estimated input tokens per PR; larger reviews are refused, not truncated.
token_budget = 30000
# Maximum review rounds per PR (1..=3).
max_rounds = 3
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_yields_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn default_template_parses_to_defaults() {
        assert_eq!(
            Config::parse(DEFAULT_CONFIG_TOML).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        assert!(Config::parse("[risk]\nhigh_typo = []").is_err());
        assert!(Config::parse("[nope]\n").is_err());
    }

    #[test]
    fn bad_glob_is_rejected() {
        let e = Config::parse("[risk]\nhigh = [\"[unclosed\"]").unwrap_err();
        assert!(e.to_string().contains("bad glob"));
    }

    #[test]
    fn security_section_parses() {
        let c = Config::parse("[security]\ngitleaks = false\n").unwrap();
        assert!(!c.security.gitleaks && c.security.cargo_audit);
        assert!(Config::parse("[security]\ntimeout_secs = 0").is_err());
    }

    #[test]
    fn rounds_out_of_range_rejected() {
        assert!(Config::parse("[review]\nmax_rounds = 4").is_err());
        assert!(Config::parse("[review]\nmax_rounds = 0").is_err());
        assert!(Config::parse("[review]\nmax_rounds = 3").is_ok());
    }

    #[test]
    fn provider_requires_model_and_key_env() {
        assert!(Config::parse("[review]\nprovider = \"gemini\"").is_err());
        let ok = "[review]\nprovider = \"gemini\"\nmodel = \"m\"\napi_key_env = \"GEMINI_API_KEY\"";
        assert!(Config::parse(ok).is_ok());
    }

    #[test]
    fn api_key_env_must_be_a_name_not_a_key() {
        let t = "[review]\nprovider = \"gemini\"\nmodel = \"m\"\napi_key_env = \"AIzaSyD-abc123\"";
        let e = Config::parse(t).unwrap_err();
        assert!(e.to_string().contains("NAME"));
        // The rejected value must not be echoed back into the error.
        assert!(!e.to_string().contains("AIzaSyD"));
    }

    #[test]
    fn openai_compatible_requires_base_url_and_https() {
        let base = "[review]\nprovider = \"openai_compatible\"\nmodel = \"m\"\napi_key_env = \"GROQ_API_KEY\"\n";
        assert!(Config::parse(base).is_err());
        assert!(Config::parse(&format!("{base}base_url = \"http://evil.example\"")).is_err());
        assert!(Config::parse(&format!(
            "{base}base_url = \"https://api.groq.com/openai/v1\""
        ))
        .is_ok());
        assert!(Config::parse(&format!("{base}base_url = \"http://localhost:8080/v1\"")).is_ok());
    }

    #[test]
    fn zero_values_rejected() {
        assert!(Config::parse("[review]\ntoken_budget = 0").is_err());
        assert!(Config::parse("[verify]\ntimeout_secs = 0").is_err());
        assert!(Config::parse("[risk]\nlarge_file_lines = 0").is_err());
    }

    #[test]
    fn checks_parse_and_validate() {
        let t = "[verify]\nchecks = [{ name = \"t\", command = \"cargo test\" }]";
        assert_eq!(Config::parse(t).unwrap().verify.checks.len(), 1);
        assert!(Config::parse("[verify]\nchecks = [{ name = \"\", command = \"x\" }]").is_err());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let c = Config::load_or_default(Path::new("/nonexistent/.review-harness.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn risk_levels_are_ordered() {
        assert!(RiskLevel::Low < RiskLevel::Medium && RiskLevel::Medium < RiskLevel::High);
    }
}
