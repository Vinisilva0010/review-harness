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
use harness_core::risk::Classifier;
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
