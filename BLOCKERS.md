# Blockers



- gitleaks is not installed in the build environment (and `cargo install` cannot provide it). The gitleaks integration is tested against stub executables and a recorded report format only; a real run needs a one-time manual check by the human (`review-harness security` with gitleaks on PATH).
