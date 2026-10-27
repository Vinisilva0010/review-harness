use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("review-harness").unwrap()
}

#[test]
fn passing_check_exits_zero() {
    bin()
        .args(["verify", "--check", "ok=true"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Passed"));
}

#[test]
fn failing_check_exits_one_and_shows_output() {
    bin()
        .args(["verify", "--check", "bad=echo nope; exit 2"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("nope"));
}

#[test]
fn no_checks_is_not_a_pass() {
    bin()
        .arg("verify")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("no evidence"));
}

#[test]
fn malformed_check_is_usage_error() {
    bin()
        .args(["verify", "--check", "nonsense"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("NAME=COMMAND"));
}

#[test]
fn json_output_is_parseable() {
    let out = bin()
        .args(["verify", "--json", "--check", "ok=true"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["results"][0]["outcome"], "passed");
}

#[test]
fn timeout_is_reported() {
    bin()
        .args(["verify", "--timeout-secs", "1", "--check", "slow=sleep 20"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("TimedOut"));
}
