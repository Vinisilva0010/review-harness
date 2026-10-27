# Progress

## Final summary

**Status: all 8 phases (0 to 7) are done and green locally. Nothing is blocked; two items could not be
verified in this environment (see below).**

What exists: a Rust workspace (`harness-core` library, `review-harness` CLI) with the verify, risk, diff,
security, AI-review, gate, report, pipeline, metrics and learn modules; a composite GitHub Action and a
dogfood workflow; CODEOWNERS and branch-protection instructions; README, ARCHITECTURE, SECURITY, the
Portuguese guide and the pitch. About 200 tests (unit, property, fixture, CLI integration with temporary
git repos). No real LLM call and no API key was ever used.

### Pull requests, in merge order (stacked; each targets the previous branch)

Phase 0 was committed directly to `main` as the brief requires.

1. https://github.com/Vinisilva0010/review-harness/pull/1  phase 1, verify layer (base: main)
2. https://github.com/Vinisilva0010/review-harness/pull/2  phase 2, diff, risk, config, classify/init
3. https://github.com/Vinisilva0010/review-harness/pull/3  phase 3, security layer
4. https://github.com/Vinisilva0010/review-harness/pull/4  phase 4, AI review engine
5. https://github.com/Vinisilva0010/review-harness/pull/5  phase 5, gate, run, GitHub Action
6. https://github.com/Vinisilva0010/review-harness/pull/6  phase 6, metrics, learn, examples
7. https://github.com/Vinisilva0010/review-harness/pull/7  phase 7, documentation

Merge them in order (1 first). After merging #1, GitHub retargets #2 to `main`, and so on. Review each as
a diff against its base branch.

### What is blocked or unverified (details in BLOCKERS.md)

* gitleaks is not installed here: its integration is tested with stub executables and the documented
  report format only.
* The GitHub Action and workflows could not run locally. They were YAML-parsed, shell-checked with
  `bash -n`, and the same binary was run end-to-end on this repo. The first PR run on GitHub is the real
  test (check that the runner's Rust is at least 1.98.1).

### What you need to do next

1. Review and merge PRs #1 to #7 in order.
2. Apply branch protection from `docs/BRANCH_PROTECTION.md` (ruleset on `main`, required checks
   `check`, `audit`, `review`, code-owner review). You cannot approve your own PR, so add a second account
   or plan a bypass for yourself.
3. One real smoke test with your own key, locally: `export GEMINI_API_KEY=...` (or another provider),
   set `provider`, `model` and `api_key_env` in `.review-harness.toml`, then
   `review-harness review --base origin/main --json`. Verify the findings look sane and check the token
   counts. This is the only step that spends money, and the request formats were checked against official
   docs but never exercised live. (OpenAI's own docs returned 403 to the fetcher, so the OpenAI-compatible
   format follows Groq's documentation.)
4. Install gitleaks locally and run `review-harness security` once to confirm the real report parses.
5. Optionally add the `GEMINI_API_KEY` repository secret to enable the AI layer in CI.

## Phase 0: bootstrap (main) - done
Workspace, CI, docs scaffolding pushed to `main`.

## Phase 1: verify layer - done (branch `phase/1-verify`)
Commands run and final output:

```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 2: diff parser, risk classifier, config, classify/init - done (branch `phase/2-diff-risk`)
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 57 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.33s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 3: security layer - done (branch `phase/3-security`)
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 72 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 4: AI review engine - done (branch `phase/4-ai-review`)
No real LLM call was made; all behaviour is tested with MockProvider, FakeTransport and JSON fixtures.
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.65s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 112 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.33s
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 5: gate, reports, run command, GitHub Action - done (branch `phase/5-gate-action`)
Local end-to-end dogfood: `review-harness run --base phase/4-ai-review --head HEAD` against this repo
ran fmt/clippy/test as evidence (all Passed), cargo-audit Passed, gitleaks Skipped (not installed locally),
so the honest decision was NEEDS HUMAN REVIEW (exit 3). The workflow itself can only be exercised on GitHub (see BLOCKERS.md).
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.78s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 129 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 6: metrics, learn, examples - done (branch `phase/6-metrics-learn`)
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.66s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.02s
test result: ok. 146 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.36s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Phase 7: documentation - done (branch `phase/7-docs`)
README (Mermaid diagram, quickstart, config reference, cost model, BYOK), ARCHITECTURE (trade-offs, large-budget
comparison), SECURITY (threat model), docs/GUIA-PT.md, docs/PITCH.md. Code is unchanged in this phase; the
checks below confirm nothing regressed.
```
$ cargo fmt --all --check
(ok)
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.38s
$ cargo test --all
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
test result: ok. 146 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
