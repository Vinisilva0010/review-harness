//! `review-harness` binary: a thin wrapper over `harness-core`.
#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use harness_core::config::{Config, RiskLevel, CONFIG_FILE, DEFAULT_CONFIG_TOML};
use harness_core::diff::{self, Diff, MAX_DIFF_BYTES};
use harness_core::pipeline::{self, PipelineInput};
use harness_core::review::{self, ReviewEngine, ReviewInput, ReviewState, ReviewStatus};
use harness_core::risk::Classifier;
use harness_core::security::{self, SecurityVerdict, Tools};
use harness_core::verify::{self, Check, Limits, Verdict};

#[derive(Parser)]
#[command(
    name = "review-harness",
    version,
    about = "Layered, risk-based code review"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run verification commands and print an evidence report.
    Verify {
        /// Check as `name=command`; repeatable. Overrides `[verify].checks` from the config.
        #[arg(long = "check", value_name = "NAME=COMMAND")]
        checks: Vec<String>,
        #[command(flatten)]
        common: Common,
        /// Per-check timeout in seconds (default: from config).
        #[arg(long)]
        timeout_secs: Option<u64>,
        /// Emit the JSON evidence record instead of Markdown.
        #[arg(long)]
        json: bool,
    },
    /// Classify the risk of a change (deterministic, zero tokens).
    Classify {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        source: DiffSource,
        /// Exit with code 1 if overall risk is at least this level.
        #[arg(long, value_enum)]
        fail_on: Option<Level>,
        #[arg(long)]
        json: bool,
    },
    /// Run deterministic security scanners (cargo-audit, gitleaks).
    ///
    /// Exit codes: 0 clean, 1 findings, 3 incomplete (a scanner was skipped or failed).
    Security {
        #[command(flatten)]
        common: Common,
        /// Base ref; gitleaks scans `base..head` instead of the working tree.
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long)]
        json: bool,
    },
    /// Run every layer (classify, verify, security, AI review) and apply the gate.
    ///
    /// Exit codes: 0 pass, 1 fail, 3 needs human review, 2 error.
    Run {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        source: DiffSource,
        /// JSON file holding AI review rounds/tokens; read and rewritten.
        #[arg(long)]
        state_file: Option<PathBuf>,
        /// Directory to write the JSON run record into.
        #[arg(long)]
        record_dir: Option<PathBuf>,
        /// Also write the Markdown PR comment to this file.
        #[arg(long)]
        comment_file: Option<PathBuf>,
        /// Print the JSON run record instead of Markdown.
        #[arg(long)]
        json: bool,
    },
    /// Run the advisory AI review (skipped without a configured provider).
    ///
    /// Exit codes: 0 completed/skipped, 1 changes requested, 4 failed or over budget.
    Review {
        #[command(flatten)]
        common: Common,
        #[command(flatten)]
        source: DiffSource,
        /// JSON file holding rounds/tokens/last reviewed commit; read and rewritten.
        #[arg(long)]
        state_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Write a commented default `.review-harness.toml`.
    Init {
        #[command(flatten)]
        common: Common,
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct Common {
    /// Repository directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    /// Config file (default: `<dir>/.review-harness.toml`).
    #[arg(long)]
    config: Option<PathBuf>,
}

impl Common {
    fn load_config(&self) -> Result<Config> {
        let path = self
            .config
            .clone()
            .unwrap_or_else(|| self.dir.join(CONFIG_FILE));
        Config::load_or_default(&path).with_context(|| format!("loading {}", path.display()))
    }
}

#[derive(Args)]
struct DiffSource {
    /// Base ref; the diff is `base...head`.
    #[arg(long, conflicts_with = "diff_file")]
    base: Option<String>,
    /// Head ref.
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Read a unified diff from this file instead (`-` for stdin).
    #[arg(long)]
    diff_file: Option<PathBuf>,
}

impl DiffSource {
    fn load(&self, dir: &Path) -> Result<Diff> {
        let text = if let Some(f) = &self.diff_file {
            let mut buf = Vec::new();
            if f.as_os_str() == "-" {
                std::io::stdin()
                    .take(MAX_DIFF_BYTES as u64 + 1)
                    .read_to_end(&mut buf)?;
            } else {
                std::fs::File::open(f)
                    .with_context(|| format!("opening {}", f.display()))?
                    .take(MAX_DIFF_BYTES as u64 + 1)
                    .read_to_end(&mut buf)?;
            }
            if buf.len() > MAX_DIFF_BYTES {
                bail!("diff exceeds the {MAX_DIFF_BYTES} byte limit");
            }
            String::from_utf8_lossy(&buf).into_owned()
        } else if let Some(base) = &self.base {
            diff::git_diff(
                dir,
                base,
                &self.head,
                MAX_DIFF_BYTES,
                Duration::from_secs(60),
            )?
        } else {
            bail!("provide --base <ref> or --diff-file <path|->");
        };
        Ok(Diff::parse(&text))
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Level {
    Low,
    Medium,
    High,
}

impl From<Level> for RiskLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::Low => RiskLevel::Low,
            Level::Medium => RiskLevel::Medium,
            Level::High => RiskLevel::High,
        }
    }
}

struct ReviewContext {
    diff: Diff,
    state: ReviewState,
    head_commit: Option<String>,
    incremental: Option<Diff>,
}

fn review_context(
    common: &Common,
    source: &DiffSource,
    state_file: Option<&Path>,
) -> Result<ReviewContext> {
    let diff = source.load(&common.dir)?;
    let state: ReviewState = match state_file {
        Some(p) if p.exists() => serde_json::from_str(&std::fs::read_to_string(p)?)
            .with_context(|| format!("parsing state file {}", p.display()))?,
        _ => ReviewState::default(),
    };
    let head_commit = source
        .base
        .as_ref()
        .map(|_| git_rev_parse(&common.dir, &source.head))
        .transpose()?;
    // Rounds after the first only look at what changed since the last review.
    let incremental = match (&state.last_reviewed_commit, &head_commit) {
        (Some(last), Some(head)) if state.rounds_done > 0 => {
            // After a force-push the old commit may be unreachable: review everything.
            diff::git_diff(
                &common.dir,
                last,
                head,
                MAX_DIFF_BYTES,
                Duration::from_secs(60),
            )
            .ok()
            .map(|t| Diff::parse(&t))
        }
        _ => None,
    };
    Ok(ReviewContext {
        diff,
        state,
        head_commit,
        incremental,
    })
}

fn git_rev_parse(dir: &Path, rev: &str) -> Result<String> {
    if rev.starts_with('-') {
        bail!("invalid ref {rev:?}");
    }
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options", rev])
        .current_dir(dir)
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        bail!("cannot resolve ref {rev:?}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn parse_check(raw: &str) -> Result<Check> {
    match raw.split_once('=') {
        Some((name, command)) if !name.trim().is_empty() && !command.trim().is_empty() => {
            Ok(Check {
                name: name.trim().to_string(),
                command: command.trim().to_string(),
            })
        }
        _ => bail!("invalid --check {raw:?}: expected NAME=COMMAND"),
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify {
            checks,
            common,
            timeout_secs,
            json,
        } => {
            let config = common.load_config()?;
            let checks = if checks.is_empty() {
                config.verify.checks.clone()
            } else {
                checks
                    .iter()
                    .map(|c| parse_check(c))
                    .collect::<Result<Vec<_>>>()?
            };
            let limits = Limits {
                timeout: Duration::from_secs(timeout_secs.unwrap_or(config.verify.timeout_secs)),
                ..Limits::default()
            };
            let evidence = verify::run_checks(&checks, &common.dir, limits);
            if json {
                println!("{}", serde_json::to_string_pretty(&evidence)?);
            } else {
                println!("{}", evidence.to_markdown());
            }
            Ok(match evidence.verdict() {
                Verdict::Verified => ExitCode::SUCCESS,
                Verdict::Failed | Verdict::NoEvidence => ExitCode::from(1),
            })
        }
        Command::Classify {
            common,
            source,
            fail_on,
            json,
        } => {
            let config = common.load_config()?;
            let diff = source.load(&common.dir)?;
            let report = Classifier::new(&config.risk)?.classify(&diff);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.to_markdown());
            }
            Ok(match fail_on {
                Some(l) if report.overall >= l.into() => ExitCode::from(1),
                _ => ExitCode::SUCCESS,
            })
        }
        Command::Security {
            common,
            base,
            head,
            json,
        } => {
            let config = common.load_config()?;
            let range = base.map(|b| format!("{b}..{head}"));
            let report = security::run_all(
                &config.security,
                &Tools::default(),
                &common.dir,
                range.as_deref(),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.to_markdown());
            }
            Ok(match report.verdict() {
                SecurityVerdict::Clean => ExitCode::SUCCESS,
                SecurityVerdict::Findings => ExitCode::from(1),
                SecurityVerdict::Incomplete => ExitCode::from(3),
            })
        }
        Command::Review {
            common,
            source,
            state_file,
            json,
        } => {
            let config = common.load_config()?;
            let ctx = review_context(&common, &source, state_file.as_deref())?;
            let risk = Classifier::new(&config.risk)?.classify(&ctx.diff).overall;
            let slot = review::build_default_provider(&config.review, &|k| std::env::var(k).ok())
                .map_err(|e| anyhow::anyhow!("provider setup: {e}"))?;
            let report = ReviewEngine::new(config.review.clone(), slot).review(ReviewInput {
                diff: &ctx.diff,
                incremental: ctx.incremental.as_ref(),
                risk,
                head_commit: ctx.head_commit,
                state: ctx.state,
            });
            if let Some(p) = &state_file {
                std::fs::write(p, serde_json::to_string_pretty(&report.state)?)
                    .with_context(|| format!("writing state file {}", p.display()))?;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.to_markdown());
            }
            Ok(match report.status {
                ReviewStatus::Failed { .. } | ReviewStatus::OverBudget { .. } => ExitCode::from(4),
                _ if report.requests_changes => ExitCode::from(1),
                _ => ExitCode::SUCCESS,
            })
        }
        Command::Run {
            common,
            source,
            state_file,
            record_dir,
            comment_file,
            json,
        } => {
            let config = common.load_config()?;
            let ctx = review_context(&common, &source, state_file.as_deref())?;
            let provider =
                review::build_default_provider(&config.review, &|k| std::env::var(k).ok())
                    .map_err(|e| anyhow::anyhow!("provider setup: {e}"))?;
            let record = pipeline::run(PipelineInput {
                config: &config,
                dir: &common.dir,
                diff: &ctx.diff,
                incremental: ctx.incremental.as_ref(),
                base: source.base.clone(),
                head: ctx.head_commit.clone(),
                state: ctx.state,
                provider,
                tools: Tools::default(),
            })
            .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(p) = &state_file {
                std::fs::write(p, serde_json::to_string_pretty(&record.review.state)?)
                    .with_context(|| format!("writing state file {}", p.display()))?;
            }
            if let Some(dir) = &record_dir {
                std::fs::create_dir_all(dir)?;
                let tag = record.head.as_deref().unwrap_or("nohead");
                let name = format!(
                    "{}-{}.json",
                    record.generated_at_unix,
                    &tag[..tag.len().min(12)]
                );
                std::fs::write(dir.join(name), record.to_json()?)?;
            }
            if let Some(p) = &comment_file {
                std::fs::write(p, record.to_markdown())
                    .with_context(|| format!("writing {}", p.display()))?;
            }
            if json {
                println!("{}", record.to_json()?);
            } else {
                println!("{}", record.to_markdown());
            }
            Ok(ExitCode::from(record.gate.decision.exit_code()))
        }
        Command::Init { common, force } => {
            let path = common
                .config
                .clone()
                .unwrap_or_else(|| common.dir.join(CONFIG_FILE));
            if path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
            }
            std::fs::write(&path, DEFAULT_CONFIG_TOML)
                .with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
