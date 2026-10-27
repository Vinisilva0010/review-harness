# review-harness

Automatic review for pull requests. It checks every PR before it can be merged and gives one of three results:

| Result | Meaning | What you do |
|---|---|---|
| **Pass** | Tests passed, no security problems, only low-risk files changed | Merge it |
| **Needs human** | Everything passed, but the PR touches sensitive files | Read it carefully before merging |
| **Fail** | Tests failed or a security problem was found | Fix it. Merging stays blocked |

## Why

AI coding agents write more code than a person can read line by line. Reading everything does not scale. Reading nothing is unsafe.

This tool checks every PR automatically, so your attention goes only to the PRs that need it.

It is based on the process described in "How the Claude Code team does code review" (Anthropic, October 2026), adapted for a small budget: free checks run first, and AI is used only where the risk justifies the cost.

## What it checks, in order

1. **Tests.** Runs the test commands you configure. Without passing tests, a PR never gets "Pass".
2. **Risk.** Looks at which files changed. You list the sensitive paths of your project (login, payments, CI config, secrets). A PR that touches them gets "Needs human".
3. **Security.** Runs gitleaks (finds leaked keys and passwords) and cargo-audit (finds dependencies with known vulnerabilities).
4. **AI review (optional, off by default).** Reads only the changed lines of medium- and high-risk PRs, with a token limit and at most 3 rounds per PR. If the AI points to a line that is not in the PR, the finding is discarded. The AI can report problems. It can never approve a PR.
5. **Decision.** Posts one comment on the PR with the result of each step.

## Use it in your repository

Three steps.

### 1. Add the workflow

Create `.github/workflows/review-harness.yml`. This file is the same in every project:

```yaml
name: review-harness

on:
  pull_request:
    types: [opened, synchronize, reopened]

permissions:
  contents: read
  pull-requests: write

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6.1.0
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: Vinisilva0010/review-harness@main
        with:
          base-sha: ${{ github.event.pull_request.base.sha }}
          head-sha: ${{ github.event.pull_request.head.sha }}
          pr-number: ${{ github.event.pull_request.number }}
```

The tool can only read the code and comment on the PR. It cannot change anything in your repository.

### 2. Add the config

Create `.review-harness.toml` in the root of your project. This file changes per project:

```toml
[verify]
checks = [
  { name = "test", command = "cargo test --all" },
]

[risk]
high = [".github/workflows/**", "**/*auth*/**", "**/.env*"]

[review]
provider = "none"
```

- `[verify]`: the command that runs your tests (`cargo test`, `npm test`, `pytest`...).
- `[risk]`: the sensitive files and folders of your project.
- `[review]`: the AI review. `"none"` means off.

The config is always read from the main branch, not from the PR. A PR cannot change its own rules.

### 3. Protect the main branch

In GitHub: **Settings → Rules → Rulesets → New branch ruleset**. Target the default branch and turn on:

- **Require a pull request before merging**
- **Require status checks to pass**, adding the `review` check and your test checks
- **Block force pushes**
- **Restrict deletions**

Without this step, anyone, including an AI agent, could push straight to main and skip the review.

## Turn on the AI review (optional)

1. Add your API key as a repository secret: **Settings → Secrets and variables → Actions → New repository secret**, named `GEMINI_API_KEY`.
2. Pass it to the review step in the workflow:

```yaml
      - uses: Vinisilva0010/review-harness@main
        env:
          GEMINI_API_KEY: ${{ secrets.GEMINI_API_KEY }}
```

3. In `.review-harness.toml`:

```toml
[review]
provider = "gemini"
model = "<model id>"
api_key_env = "GEMINI_API_KEY"
```

Anthropic and OpenAI-compatible APIs (such as Groq) are also supported. The key is read only from an environment variable and is never written to logs. PRs opened from forks never receive the key.

## Run it on your machine (optional)

```bash
cargo install --git https://github.com/Vinisilva0010/review-harness harness-cli --locked
review-harness init     # writes a starter .review-harness.toml
review-harness --help   # lists all commands
```

Requires Rust 1.98.1 or newer. Not needed for the GitHub setup above.

## More

- [ARCHITECTURE.md](ARCHITECTURE.md): how it is built and why
- [SECURITY.md](SECURITY.md): threat model
- [docs/GUIA-PT.md](docs/GUIA-PT.md): guide in Portuguese

License: MIT
