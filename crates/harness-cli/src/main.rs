//! `review-harness` binary: a thin wrapper over `harness-core`.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
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
        /// Check as `name=command`; repeatable.
        #[arg(long = "check", value_name = "NAME=COMMAND")]
        checks: Vec<String>,
        /// Directory to run the commands in.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Per-check timeout in seconds.
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,
        /// Emit the JSON evidence record instead of Markdown.
        #[arg(long)]
        json: bool,
    },
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
            dir,
            timeout_secs,
            json,
        } => {
            let checks = checks
                .iter()
                .map(|c| parse_check(c))
                .collect::<Result<Vec<_>>>()?;
            let limits = Limits {
                timeout: Duration::from_secs(timeout_secs),
                ..Limits::default()
            };
            let evidence = verify::run_checks(&checks, &dir, limits);
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
