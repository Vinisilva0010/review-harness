use std::path::Path;
use std::process::Command as Std;

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("review-harness").unwrap()
}

#[test]
fn learn_writes_rule_and_stub_then_refuses_duplicates() {
    let d = tempfile::tempdir().unwrap();
    bin()
        .args([
            "learn",
            "--title",
            "Null deref in parser",
            "--summary",
            "crash on empty input",
            "--rule",
            "Check empty input before indexing",
            "--dir",
        ])
        .arg(d.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("R-001"));
    assert!(d
        .path()
        .join("tests/regression/null_deref_in_parser.rs")
        .exists());
    let md = std::fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
    assert!(md.contains("Check empty input before indexing"));
    bin()
        .args([
            "learn",
            "--title",
            "Null deref in parser",
            "--summary",
            "s",
            "--rule",
            "r",
            "--dir",
        ])
        .arg(d.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn learn_rejects_path_traversal() {
    let d = tempfile::tempdir().unwrap();
    bin()
        .args([
            "learn",
            "--title",
            "t",
            "--summary",
            "s",
            "--rule",
            "r",
            "--test-dir",
            "../x",
            "--dir",
        ])
        .arg(d.path())
        .assert()
        .code(2);
}

#[test]
fn metrics_over_example_records() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/run-records");
    bin()
        .args(["metrics", "--records"])
        .arg(&examples)
        .assert()
        .success()
        .stdout(predicate::str::contains("Hallucination rate"))
        .stdout(predicate::str::contains("Rounds per PR"));
}

#[test]
fn metrics_json_with_git_reverts() {
    let d = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        assert!(Std::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@e.com"])
            .args(args)
            .current_dir(d.path())
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("f"), "1").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "Revert \"x\""]);
    let rec = tempfile::tempdir().unwrap();
    let out = bin()
        .args(["metrics", "--json", "--git-limit", "50", "--records"])
        .arg(rec.path())
        .arg("--dir")
        .arg(d.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["reverts"]["reverts"], 1);
    assert_eq!(v["metrics"]["runs"], 0);
}

#[test]
fn metrics_missing_dir_is_an_error() {
    bin()
        .args(["metrics", "--records", "/nonexistent-dir-xyz"])
        .assert()
        .code(2);
}
