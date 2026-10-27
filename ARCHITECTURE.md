# Architecture

## Shape

A Cargo workspace with a thin binary over a fully unit-tested library:

* `crates/harness-core`: all logic, `#![forbid(unsafe_code)]`.
* `crates/harness-cli`: the `review-harness` binary, a clap wrapper with no business logic beyond
  argument handling and process exit codes.

| Module | Responsibility |
|---|---|
| `config` | Parse and validate `.review-harness.toml` (strict, defaults, no secrets) |
| `diff` | Unified diff parser; capped `git diff` runner |
| `risk` | Deterministic classifier: globs plus signals, with reasons |
| `verify` | Run commands with timeouts; `Evidence` where zero checks means no evidence |
| `security` | cargo-audit and gitleaks wrappers with skipped/error semantics |
| `review` | Provider trait, adapters, prompt, strict schema, validator, budget, rounds |
| `gate` | The decision table |
| `pipeline` | Orchestration, cheapest layer first |
| `report` | Markdown PR comment and JSON run record |
| `metrics` | Aggregate run records, detect reverts |
| `learn` | Escaped-bug feedback loop into `REVIEW.md` and a test stub |

## Data flow

1. The CLI loads config (from the base commit in CI) and obtains a diff (`git diff base...head`, capped
   at 8 MiB, or a file/stdin).
2. `risk` classifies. `verify` runs the configured checks. `security` runs the scanners.
3. If a deterministic layer already failed, the AI layer is skipped (no tokens). Otherwise `review`
   applies its own gates (provider, threshold, scope, budget) and may call the model.
4. `gate` folds the four results into pass / needs-human / fail. `report` renders the comment and run
   record. `metrics` reads run records later.

## Key design decisions and trade-offs

**Deterministic first, model last.** A regex over added lines for a leaked AWS key costs nothing and is
exactly reproducible; a model is neither. We use the model for what scanners cannot do (logic errors)
and only after the cheap evidence exists. Trade-off: the AI layer never sees a PR that already fails
checks, so a contributor may learn about model-found issues one round later. That is the price of the
budget.

**The gate is a pure function.** `gate::decide` takes four small enums and returns a decision. The whole
input space (162 combinations) is tested against an independently written oracle, plus invariants: high
risk never passes, the AI can only tighten the result, skipped layers never pass.

**The model cannot influence the gate beyond asking for a human.** The output schema is
`{"findings": [...]}` with unknown fields rejected. There is no verdict field to forge. Findings can at
most turn a pass into needs-human. AI-requested changes map to *needs-human* rather than *fail* because
AI findings are advisory and sometimes wrong: a human adjudicates.

**Fail closed, never crash, never pass silently.** Invalid JSON gets one retry with the validation error
appended; then the layer reports `failed` and the gate asks a human. Provider errors, timeouts and rate
limits behave the same way.

**A zero-token validator.** Any finding whose file/line is not an *added* line of the reviewed diff is
dropped and counted as hallucinated. That makes the hallucination rate a measurable metric rather than a
vibe, and stops comments on code the author did not touch.

**Budget before call.** Cost is bounded by arithmetic, not hope: estimate, compare with the cumulative
per-PR budget, refuse if over. Estimates are conservative (3 bytes per token); provider-reported usage
replaces them afterwards.

**Rounds and incremental review.** State (`rounds_done`, `last_reviewed_commit`, `tokens_spent`) is a
small JSON file the caller persists. Round 2 and 3 review only lines changed since the last reviewed
commit and still present in the PR. After round 3 the tool says to step back and rethink the approach
instead of patching forever. If the last reviewed commit is unreachable (force-push) it reviews
everything again.

**Blocking I/O, tiny dependency tree.** One short-lived process per PR does not need an async runtime.
`ureq` (rustls) plus `wait-timeout` give timeouts and size caps with far less code, compile time and
memory than tokio plus reqwest, which matters on a 3.7 GB build machine. Process timeouts kill the whole
process group, because otherwise a grandchild holding a pipe open defeats the timeout.

**Trusted inputs come from the base commit.** `.review-harness.toml` (which defines executed commands and
risk rules) and `REVIEW.md` are read from the base ref in CI, so a pull request cannot grant itself an
easier review. Note the inherent limit: configured checks still execute the PR's code, as any CI does.

## What Anthropic does with large budgets, and what we do instead

As described in the post that inspired this project, a team with a large token budget can run **several
review agents in parallel** (each looking for a different class of problem) and then **verifier
agents** that re-check each reported finding, discarding the ones that do not hold up. That buys
recall (more angles) and precision (a second opinion on every claim) by spending many model calls per PR.

We run on a small budget, so we substitute cheaper mechanisms for each of those two ideas:

| Large-budget technique | Our substitute | Trade-off |
|---|---|---|
| Parallel specialised reviewers for recall | Deterministic scanners and a risk classifier cover the classes machines are good at (secrets, known CVEs, CI tampering, size), and one focused model call covers the rest, scoped to risky changes only | Lower recall on subtle logic bugs in low-risk code, which we deliberately do not spend tokens on |
| Verifier agents for precision | A zero-token validator (finding must sit on an added line, schema must match, exact duplicates removed) plus mandatory human sign-off for high risk | Cannot detect a plausible but wrong finding that points at a real added line; the human and the measured hallucination rate are the backstop |
| Many rounds of agentic exploration of the repository | Diff hunks plus a maintainers' `REVIEW.md`, max 3 rounds, incremental re-review | Less context: the model may miss a bug that depends on code outside the diff |
| Learning from escapes | `learn` turns each escaped bug into a rule fed to the reviewer and a regression test | Manual trigger; it only helps after a bug has already escaped once |

If budget grows, the natural next steps are exactly the missing two: fan out specialised prompts in
parallel (the `LlmProvider` trait and budget accounting already support multiple calls) and add a second
model call that must confirm each finding before it is kept.

## Testing strategy

TDD per behaviour. Unit tests in every module; CLI integration tests with `assert_cmd` against temporary
git repositories and stub scanner executables; property tests (proptest) for the diff parser and the
finding validator; JSON fixtures for each provider's request and response; an exhaustive gate table; a
test that the committed example run records still parse. Nothing touches the network.
