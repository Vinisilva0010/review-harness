# Progress

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
