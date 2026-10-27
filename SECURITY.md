# Security

## Reporting

Please open a private security advisory on the GitHub repository (Security tab) rather than a public
issue.

## Threat model

review-harness reads **untrusted input** (a pull request's diff, file names, commit text, and the model's
reply) and runs with **valuable capabilities** (an LLM API key, a repository token, the ability to
execute configured commands). The design goal is that an attacker who controls a PR cannot (a) exfiltrate
the key, (b) make the tool approve their change, (c) execute code with more privilege than normal CI, or
(d) exhaust memory, time or money.

| Threat | Mitigation |
|---|---|
| **API key leakage** (logs, errors, reports, config) | Keys are read only from environment variables whose *names* appear in config; a config value that does not look like a variable name is rejected without being echoed. `Secret` has a redacted `Debug` and no `Display`; `HttpRequest`'s `Debug` prints header names only; every provider error body is passed through `Secret::redact`. Tests prove the key never appears in errors, Markdown, JSON or `Debug` output. `.env` is never read. Gemini's key goes in the `x-goog-api-key` header, never in the URL. |
| **Prompt injection** via the diff | The diff is wrapped in `<<<DATA-nonce BEGIN/END>>>` markers; the nonce is derived from the content and regenerated if the diff happens to contain it, so it cannot forge the closing marker. The system prompt states that the content is data, instructions inside it must be ignored and reported as a `prompt_injection` finding, and the model cannot approve anything. Lines are truncated at 400 characters. |
| **Model output manipulates the gate** | Output must match `{"findings": [...]}` exactly (`deny_unknown_fields`); there is no verdict field. Invalid output is retried once, then the layer fails closed. Findings can only add scrutiny (needs-human). The validator drops findings that do not point at added lines. |
| **Hostile model/PR text in PR comments** (HTML, @mentions) | Text from findings, paths and notices is HTML-escaped and @mentions are defanged before it is rendered. Comments are capped below GitHub's limit. |
| **PR rewrites its own rules** | In the Action, `.review-harness.toml` and `REVIEW.md` are read from the **base** commit. Config is strict and validated. |
| **Secrets exposed to fork PRs** | The workflow uses `pull_request`, never `pull_request_target`. Fork PRs get a read-only token and an empty LLM key (the expression is gated on the head repo being this repo). No secret is ever written to disk or echoed. |
| **Over-privileged workflow token** | Top-level `permissions: contents: read`, plus `pull-requests: write` only where the comment is posted. `persist-credentials: false` on checkout. |
| **Supply chain in CI** | Third-party actions (`actions/checkout`, `actions/upload-artifact`) are pinned by full commit SHA resolved with `git ls-remote`. The only downloaded binary (gitleaks) is pinned by version and SHA-256. `cargo audit` runs in CI and the Action. The comment is posted with `gh api`, avoiding a third-party comment action. Dependencies are kept small (no async runtime). |
| **Denial of service / resource exhaustion** | Every external process has a timeout and is killed as a process group; every HTTP call has a 60 s timeout; 429/503 are retried with exponential backoff (honouring `Retry-After`, capped). Caps: diff 8 MiB, command output tails 4 KiB, scanner output 8 MiB, provider response 2 MiB, run records 4 MiB each, findings 50 per reply, 2000 characters per finding text. The token budget bounds LLM spend per PR. |
| **Insecure transport / redirects** | The HTTP transport refuses non-HTTPS URLs (except localhost for tests) and does not follow redirects. Config rejects `http://` base URLs except localhost. Model ids are validated before being placed in a URL path. |
| **Path traversal / injection in `learn`** | Titles become `[a-z0-9_]` slugs; absolute and `..` test directories are rejected; existing tests are never overwritten; newlines are collapsed so a rule cannot forge another heading. |
| **Option injection into git** | Refs starting with `-` are rejected; `--end-of-options` is used with `rev-parse`. |
| **Scanner skipped silently** | A missing, disabled, failing or unparseable scanner is `skipped`/`error` and the verdict is *incomplete*, which the gate maps to needs-human, never pass. gitleaks always runs with `--redact` and only rule, file and line are kept. |
| **Memory-unsafe bugs** | `#![forbid(unsafe_code)]` in both crates, plus a workspace `unsafe_code = "forbid"` lint. |

## Known limitations

* Configured `verify` checks run the pull request's code (as any CI does). The safeguards are that the
  workflow runs without secrets for forks, the check commands come from the base commit, and the token is
  least-privilege. Do not put deployment credentials in the job environment.
* A same-repository collaborator can edit the workflow in their own PR and the run uses that edited
  version (standard for `pull_request`). Protect `.github/` with CODEOWNERS and required reviews
  ([docs/BRANCH_PROTECTION.md](docs/BRANCH_PROTECTION.md)).
* The AI layer can be wrong or be manipulated into false positives. It is advisory by construction and
  cannot approve; high-risk changes always need a human.
* Secret-like detection in the risk classifier is a heuristic. gitleaks is the authoritative scanner.
* The prompt-injection defences reduce, but cannot eliminate, the chance that a model follows hostile
  text. The design assumes this and limits what a compromised model reply can do.

## CI hygiene

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` and `cargo audit`
run in CI on every push and pull request.
