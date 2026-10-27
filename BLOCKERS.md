# Blockers



- gitleaks is not installed in the build environment (and `cargo install` cannot provide it). The gitleaks integration is tested against stub executables and a recorded report format only; a real run needs a one-time manual check by the human (`review-harness security` with gitleaks on PATH).

- The GitHub Actions workflows/composite action could not be executed locally (no runner, `actionlint` unavailable). They were YAML-parsed, the shell steps were syntax-checked with `bash -n`, and the same binary was run end-to-end locally against this repo (`review-harness run`). The first PR run on GitHub is the real test; if `ubuntu-latest`'s Rust is older than 1.98.1, add a toolchain install step.
