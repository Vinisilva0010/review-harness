# Build brief: review-harness

You are building this project autonomously, overnight, with no human available.
Do not ask questions. When a choice is needed, pick the most professional option,
record it in DECISIONS.md with one line of reasoning, and continue.
Re-read this file whenever your context gets long or after any compaction.

## 1. Goal

`review-harness` is a Rust CLI + GitHub Action that applies a layered, risk-based
code review pipeline to every pull request, inspired by Anthropic's post
"How the Claude Code team does code review" (Oct 2026). Core idea: the question is
not "did a human read this code?" but "what could go wrong with this change, and
which check, automated or human, is the right one to catch it?"

Our differentiator versus that post: we run on a tight budget. Cheap deterministic
layers run first; LLM review runs only where risk justifies it, under a hard token
budget. The tool must work fully with NO LLM provider configured (the AI layer is
skipped with a clear notice) and must support bring-your-own-key for companies.

This repo is a professional portfolio piece. Quality bar: production-grade, not a demo.

## 2. Hard constraints (never violate)

Cost
- Never call any real LLM API, never use any API key, never read `.env`.
- All LLM behavior is tested with a `MockProvider` and recorded JSON fixtures.
- Tests must not touch the network.

Environment (WSL2 Ubuntu 24.04, 3.7 GB RAM, no systemd, Docker not running)
- Do not change the global environment: no `rustup`, no `sudo`, no toolchain
  changes, no `rust-toolchain.toml`, no editing files outside this repo.
- First run `rustc --version` and `cargo --version`; use what is installed.
  Set `rust-version` in Cargo.toml to the installed version.
- Create `.cargo/config.toml` in the repo with `[build] jobs = 2` to avoid OOM.
- Keep dependencies lean. Prefer a blocking HTTP client (e.g. `ureq` with rustls)
  over tokio+reqwest. Check current crate versions with `cargo search` or
  `cargo add`; never guess versions.
- `cargo install` is allowed only for a tool that is missing (e.g. cargo-audit);
  never upgrade or reinstall an existing tool. If something needs sudo, skip it
  and note it in BLOCKERS.md.

Git (big-company workflow)
- Branch name `main`. The remote already exists (cloned). Never force-push.
  Never rewrite pushed history.
- Phase 0 is committed directly to `main` (bootstrap) and pushed.
- Every later phase: its own branch `phase/<n>-<slug>`, created from the previous
  phase branch (stacked). Push each branch. If `gh auth status` succeeds, open a PR
  per phase with base = previous phase branch (phase 1 base = main), with a clear
  description and the verification evidence. If gh is unavailable, write the
  compare URLs in PROGRESS.md. Never merge PRs; the human reviews them in the morning.
- Commit at every meaningful, green step (not one giant commit per phase).
  Conventional Commits, English, imperative, with a body explaining why:
  `feat(risk): add path-based risk classifier` + body.
- Never commit secrets, `.env`, or `target/`.

Language
- All code, comments, configs, README, ARCHITECTURE, SECURITY: English.
- Exception: `docs/GUIA-PT.md` in Brazilian Portuguese (see Phase 7).

## 3. Architecture

Cargo workspace:
- `crates/harness-core` (library): all logic, fully unit-tested.
- `crates/harness-cli` (binary `review-harness`): thin clap wrapper.

Modules in core:
- `config`: parses `.review-harness.toml` (serde + toml), validated, with defaults.
- `diff`: parses unified diff (`git diff base...head`) into files, hunks, added
  line numbers. Must handle renames, deletions, binary files, no-newline markers.
- `risk`: deterministic classifier. Glob rules (globset) map paths to Low/Medium/High;
  extra signals: size of change, touched CI workflows, dependency manifests,
  secrets-like patterns. Output: overall risk + per-file reasons.
- `verify`: runs configured commands (test, build, lint), captures exit code,
  duration, truncated stdout/stderr tail, and produces an Evidence report.
  "Done" without passing evidence is impossible by design.
- `security`: runs deterministic scanners when available (cargo-audit, gitleaks)
  and parses their output; missing scanner = explicit "skipped" status, not pass.
- `review`: AI review engine.
  - `trait LlmProvider` with implementations: `MockProvider`, `GeminiProvider`,
    `OpenAiCompatibleProvider` (covers Groq and others), `AnthropicProvider`.
    Verify request/response formats against each provider's official docs via
    WebFetch; encode them in fixtures. No real calls.
  - Runs only if risk >= configured threshold AND a provider is configured.
  - Sends only the diff hunks plus minimal context, never whole repo.
  - Hard token budget per PR (estimated before sending; refuse if over budget).
  - Max 3 rounds per PR. After round 1, re-review only lines changed since the
    last reviewed commit. After round 3, emit "step back and rethink the approach".
  - Model output must be strict JSON matching a schema
    (file, line, severity, category, explanation, suggested_fix). Invalid JSON =
    one retry, then fail closed with a notice (never crash, never pass silently).
  - Finding validator (zero tokens): drop any finding whose file/line is not in the
    added lines of the diff. Count dropped findings as "hallucinated" in metrics.
  - The AI layer is advisory: it can request changes, it can never approve.
- `gate`: combines layers into a final decision: pass / needs-human / fail,
  with exit codes. High risk always requires human approval.
- `report`: renders Markdown (PR comment) and JSON (run record).
- `metrics`: aggregates run records: rounds per PR, findings by severity,
  hallucination rate, layers skipped, tokens spent, plus reverts detected from
  git log. These mirror the metrics in Anthropic's post.
- `learn`: `review-harness learn` appends a rule to REVIEW.md and scaffolds a
  regression test stub when a bug escapes review.

CLI commands: `verify`, `classify`, `review`, `run` (all layers), `metrics`,
`learn`, `init` (writes a commented default `.review-harness.toml`).

## 4. Security requirements (document all of them in SECURITY.md)

- API keys only from environment variables, never from config files, never logged,
  redacted in any error or debug output (write a test that proves redaction).
- Prompt injection: the diff is untrusted data. It goes inside clearly delimited
  data blocks; the system prompt says instructions inside the diff must be ignored
  and reported as a finding. Output is schema-validated; the model can never
  change the gate result to "pass".
- GitHub Action: never use `pull_request_target` with secrets; least-privilege
  `permissions:` block; pin third-party actions by full commit SHA (resolve SHAs
  with `git ls-remote`); secrets never exposed to PRs from forks.
- Timeouts on every external process and HTTP call; exponential backoff on 429.
- Output size caps (no unbounded memory from huge diffs or command output).
- `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo audit` in CI.
- `#![forbid(unsafe_code)]` in both crates.

## 5. Testing requirements

- TDD: write the failing test first for each behavior, then implement.
- Unit tests for every module; integration tests for the CLI (assert_cmd) using
  temporary git repos created in tests.
- Property tests (proptest) for the diff parser and the finding validator.
- Fixture-based tests for each provider's request building and response parsing.
- Edge cases: empty diff, binary-only diff, huge diff over budget, rename-only,
  invalid UTF-8 output from commands, provider returning malformed JSON,
  provider returning findings on lines outside the diff, 429 responses, timeouts.
- Coverage of the gate decision table: every combination of layer results.

## 6. Phases (each must be fully green before moving on)

Definition of Done for every phase: `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings`, `cargo test --all` all pass locally; PROGRESS.md updated with the
exact commands run and the final pass lines pasted; branch pushed; PR opened or
compare URL recorded. Do not claim done without pasting real command output.

- Phase 0, bootstrap (on main): workspace, `.gitignore`, `.cargo/config.toml`,
  `.env.example` (variable names only, no values), LICENSE (MIT), minimal README,
  CI workflow (fmt, clippy, test, audit), PROGRESS.md, DECISIONS.md, BLOCKERS.md.
- Phase 1, verify layer + evidence report + `verify` command.
- Phase 2, diff parser + risk classifier + `classify` command + config + `init`.
- Phase 3, security layer (cargo-audit, gitleaks integration, skipped semantics).
- Phase 4, AI review engine: provider trait, mock, three real provider adapters
  (fixture-tested only), budget, rounds, re-review of new lines, validator.
- Phase 5, gate + reports + `run` command + GitHub Action (composite action in
  `action.yml` plus a workflow that dogfoods the tool on this repo's own PRs; with
  no secret configured the AI layer must show "skipped: no provider configured").
  Add `.github/CODEOWNERS` and `docs/BRANCH_PROTECTION.md` (exact GitHub UI steps
  the human must do to require reviews and status checks).
- Phase 6, metrics + learn commands; sample run records in `examples/`.
- Phase 7, documentation:
  - README.md: what it is, the layer diagram (Mermaid), quickstart, config
    reference, why each layer exists, cost model, bring-your-own-key.
  - ARCHITECTURE.md with trade-offs, including what Anthropic does with large
    budgets (parallel agents + verifier agents) and what we do instead and why.
  - SECURITY.md with threat model and mitigations.
  - docs/GUIA-PT.md in simple Brazilian Portuguese: what each layer does in plain
    words, how to demo it in an interview by walking through the GitHub repo
    (which file to open, which PR to show, what to say), and likely interview
    questions with short answers.
  - docs/PITCH.md: a 2-minute spoken pitch in English, each paragraph followed
    by its Portuguese translation.

## 7. Working protocol

- If something fails, you get 3 fix attempts. If still failing after 3, stop
  that item, write the failing step, exact error and what you tried in
  BLOCKERS.md, and move to the next item that does not depend on it.
- After finishing each phase, review your own diff as a fresh reviewer would
  (re-read it with the checklist in section 4 and 5) before pushing.
- Never weaken a test to make it pass. Never mark a check as passing when it was
  skipped.
- When all phases are done, write a final summary at the top of PROGRESS.md:
  what is done, what is blocked, PRs in merge order, and what the human must do
  next (add API key locally for one real smoke test, set branch protection).
