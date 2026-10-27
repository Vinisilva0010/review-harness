# Decisions

- Toolchain: rustc/cargo 1.98.1 are installed; `rust-version = "1.98.1"` per brief.
- Workspace-level `unsafe_code = "forbid"` lint plus `#![forbid(unsafe_code)]` in each crate.
- `actions/checkout` pinned to v6.1.0 commit SHA resolved via `git ls-remote`.
- `.claude/` is git-ignored: it holds local agent settings, not project source.
