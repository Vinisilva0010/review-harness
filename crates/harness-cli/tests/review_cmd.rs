use std::path::Path;
use std::process::Command as Std;

use assert_cmd::Command;
use predicates::prelude::*;

fn git(dir: &Path, args: &[&str]) {
    let st = Std::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success());
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("README.md"), "x\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    git(d.path(), &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "change"]);
    d
}

fn bin() -> Command {
    let mut c = Command::cargo_bin("review-harness").unwrap();
    // Make sure no ambient key can ever reach a provider in tests.
    c.env_remove("GEMINI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GROQ_API_KEY");
    c
}

#[test]
fn review_without_provider_is_skipped_with_notice_and_exit_zero() {
    let r = repo();
    bin()
        .args(["review", "--base", "main", "--dir"])
        .arg(r.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped: no provider configured"));
}

#[test]
fn review_with_unset_key_is_skipped_naming_the_variable() {
    let r = repo();
    std::fs::write(
        r.path().join(".review-harness.toml"),
        "[review]\nprovider = \"gemini\"\nmodel = \"m\"\napi_key_env = \"GEMINI_API_KEY\"\n",
    )
    .unwrap();
    bin()
        .args(["review", "--base", "main", "--dir"])
        .arg(r.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("GEMINI_API_KEY"));
}

#[test]
fn review_json_and_state_file_are_written() {
    let r = repo();
    let state = r.path().join("state.json");
    let out = bin()
        .args(["review", "--json", "--base", "main", "--state-file"])
        .arg(&state)
        .arg("--dir")
        .arg(r.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["status"]["kind"], "skipped");
    assert!(state.exists());
}
