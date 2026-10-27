use std::path::Path;
use std::process::Command as Std;

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("review-harness").unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let st = Std::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn repo_with_branch(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("README.md"), "hello\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    git(d.path(), &["checkout", "-q", "-b", "feature"]);
    for (name, body) in files {
        let p = d.path().join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "change"]);
    d
}

#[test]
fn classify_docs_only_change_is_low() {
    let r = repo_with_branch(&[("docs/a.md", "x\n")]);
    bin()
        .args(["classify", "--base", "main", "--dir"])
        .arg(r.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Risk: **LOW**"));
}

#[test]
fn classify_workflow_change_is_high_and_fail_on_trips() {
    let r = repo_with_branch(&[(".github/workflows/x.yml", "on: push\n")]);
    bin()
        .args(["classify", "--base", "main", "--fail-on", "high", "--dir"])
        .arg(r.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("HIGH"));
}

#[test]
fn classify_json_from_stdin() {
    let diff =
        "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let out = bin()
        .args(["classify", "--diff-file", "-", "--json"])
        .write_stdin(diff)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["overall"], "medium");
    assert_eq!(v["files"][0]["path"], "src/a.rs");
}

#[test]
fn classify_empty_diff_is_low() {
    bin()
        .args(["classify", "--diff-file", "-"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(predicate::str::contains("LOW"));
}

#[test]
fn classify_requires_a_source() {
    bin()
        .arg("classify")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--base"));
}

#[test]
fn classify_bad_ref_reports_git_error() {
    let r = repo_with_branch(&[("docs/a.md", "x\n")]);
    bin()
        .args(["classify", "--base", "no-such-ref", "--dir"])
        .arg(r.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("git diff failed"));
}

#[test]
fn classify_uses_repo_config() {
    let r = repo_with_branch(&[("docs/a.md", "x\n")]);
    std::fs::write(
        r.path().join(".review-harness.toml"),
        "[risk]\nhigh = [\"docs/**\"]\n",
    )
    .unwrap();
    bin()
        .args(["classify", "--base", "main", "--dir"])
        .arg(r.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("HIGH"));
}

#[test]
fn invalid_config_is_a_clear_error() {
    let r = repo_with_branch(&[("docs/a.md", "x\n")]);
    std::fs::write(
        r.path().join(".review-harness.toml"),
        "[review]\nmax_rounds = 9\n",
    )
    .unwrap();
    bin()
        .args(["classify", "--base", "main", "--dir"])
        .arg(r.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("max_rounds"));
}

#[test]
fn init_writes_config_and_refuses_overwrite() {
    let d = tempfile::tempdir().unwrap();
    bin()
        .args(["init", "--dir"])
        .arg(d.path())
        .assert()
        .success();
    let text = std::fs::read_to_string(d.path().join(".review-harness.toml")).unwrap();
    assert!(text.contains("[risk]") && text.contains("Never put the key here"));
    bin()
        .args(["init", "--dir"])
        .arg(d.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
    bin()
        .args(["init", "--force", "--dir"])
        .arg(d.path())
        .assert()
        .success();
}

#[test]
fn verify_uses_checks_from_config() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".review-harness.toml"),
        "[verify]\nchecks = [{ name = \"ok\", command = \"true\" }]\n",
    )
    .unwrap();
    bin()
        .args(["verify", "--dir"])
        .arg(d.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("`ok`"));
}
