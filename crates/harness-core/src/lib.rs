//! Core library for review-harness.
#![forbid(unsafe_code)]

pub mod config;
pub mod diff;
pub mod gate;
pub mod learn;
pub mod metrics;
pub mod pipeline;
pub mod report;
pub mod review;
pub mod risk;
pub mod security;
pub mod verify;

/// Crate version, exposed for the CLI `--version` output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!VERSION.is_empty());
    }
}
