# Decisions

- Toolchain: rustc/cargo 1.98.1 are installed; `rust-version = "1.98.1"` per brief.
- Workspace-level `unsafe_code = "forbid"` lint plus `#![forbid(unsafe_code)]` in each crate.
- `actions/checkout` pinned to v6.1.0 commit SHA resolved via `git ls-remote`.
- `.claude/` is git-ignored: it holds local agent settings, not project source.
- `verify` exit codes: 0 verified, 1 failed or no evidence, 2 usage/internal error. No evidence is never a pass.
- Commands run via `sh -c` in their own process group so a timeout kills grandchildren; output is tail-capped (4 KiB) while draining pipes to avoid blocking or unbounded memory.
- Blocking-only stack (no tokio): `wait-timeout` for process timeouts, `ureq` later for HTTP.
- Risk: unmatched file types default to Medium (unknown is not safe); precedence high > medium > low; CI files, manifests, binaries, large files and secret-like added lines are extra signals that can only raise risk.
- Secret-like detection reports pattern name and line number only, never the matched text.
- Config is untrusted when it comes from a PR head (it can define commands and lower risk). The GitHub Action (phase 5) must read config from the base ref, not the PR branch.
- `classify --fail-on <level>` exits 1 at or above the level; default exit is 0 since classification is informational.
