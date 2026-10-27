# review-harness

A layered, risk-based code review pipeline for pull requests, as a Rust CLI and GitHub Action.

Cheap deterministic layers run first (verification, risk classification, security scanners);
LLM review runs only where risk justifies it, under a hard token budget. Works fully with no
LLM provider configured.

> Status: under construction. See [PROGRESS.md](PROGRESS.md).

## License

MIT
