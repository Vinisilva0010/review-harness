# Branch protection: exact steps

These settings turn the review pipeline into an enforced policy. They cannot be committed
to the repository; a repository admin has to apply them once in the GitHub UI.

Note on CODEOWNERS: GitHub does not let you approve your own pull request. With a single
maintainer, either add a second account as a collaborator, or keep "Require approvals" at 1
and use "Allow specified actors to bypass" for yourself only in emergencies.

## 1. Create a ruleset for `main`

1. Open the repository on GitHub, then **Settings** -> **Rules** -> **Rulesets**.
2. Click **New ruleset** -> **New branch ruleset**.
3. **Ruleset name**: `protect-main`. **Enforcement status**: `Active`.
4. **Target branches** -> **Add target** -> **Include by pattern** -> `main`.
5. Enable these rules:
   - **Restrict deletions**
   - **Block force pushes**
   - **Require a pull request before merging**
     - **Required approvals**: `1`
     - **Dismiss stale pull request approvals when new commits are pushed**
     - **Require review from Code Owners** (this is what makes the CODEOWNERS file bind,
       and it is how "high risk always needs a human" is enforced)
     - **Require approval of the most recent reviewable push**
     - **Require conversation resolution before merging**
   - **Require status checks to pass**
     - **Require branches to be up to date before merging**
     - Add these checks (they appear after the workflows have run once):
       `check`, `audit` (from the CI workflow) and `review` (from the review-harness workflow)
   - **Require linear history** (optional, recommended)
6. Click **Create**.

## 2. Restrict who can change the policy files

CODEOWNERS already covers `.github/`, `action.yml`, `.review-harness.toml` and `SECURITY.md`.
With "Require review from Code Owners" on, no change to them can merge without the owner.

## 3. Workflow permissions

**Settings** -> **Actions** -> **General**:
- **Workflow permissions**: `Read repository contents and packages permissions`
  (the workflows request `pull-requests: write` explicitly where needed).
- **Fork pull request workflows from outside collaborators**: `Require approval for all
  outside collaborators`.
- Leave "Send secrets to workflows from fork pull requests" **off**.

## 4. Optional: enable the AI layer

1. **Settings** -> **Secrets and variables** -> **Actions** -> **New repository secret**,
   name `GEMINI_API_KEY` (or the variable your provider uses).
2. In `.review-harness.toml` set `provider`, `model` and `api_key_env` (this file is read from
   the base branch, so it takes effect after merge).

## 5. Verify the policy

Open a throwaway PR that edits `.github/workflows/ci.yml`. Expected: the review-harness comment
says NEEDS HUMAN REVIEW (high risk), and merging is blocked until the code owner approves.
