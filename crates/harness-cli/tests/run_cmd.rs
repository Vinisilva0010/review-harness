use std::os::unix::fs::PermissionsExt;
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

fn repo(files: &[(&str, &str)], config: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("README.md"), "x\n").unwrap();
    std::fs::write(d.path().join("Cargo.lock"), "").unwrap();
    std::fs::write(d.path().join(".review-harness.toml"), config).unwrap();
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

/// A PATH where `cargo` and `gitleaks` are clean stubs (no network, no real scanners).
fn stub_path() -> (tempfile::TempDir, String) {
    let d = tempfile::tempdir().unwrap();
    for (name, body) in [
        (
            "cargo",
            r#"echo '{"vulnerabilities":{"found":false,"count":0,"list":[]}}'"#,
        ),
        ("gitleaks", "exit 0"),
    ] {
        let p = d.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:/usr/bin:/bin", d.path().display());
    (d, path)
}

fn run(repo: &Path, path: &str) -> Command {
    let mut c = Command::cargo_bin("review-harness").unwrap();
    c.env("PATH", path)
        .env_remove("GEMINI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .args(["run", "--base", "main", "--dir"])
        .arg(repo);
    c
}

const OK_CFG: &str = "[verify]\nchecks = [{ name = \"ok\", command = \"true\" }]\n";

#[test]
fn all_green_low_risk_passes_and_writes_artifacts() {
    let r = repo(&[("docs/a.md", "hi\n")], OK_CFG);
    let (_s, path) = stub_path();
    let rec = r.path().join("records");
    let comment = r.path().join("comment.md");
    run(r.path(), &path)
        .arg("--record-dir")
        .arg(&rec)
        .arg("--comment-file")
        .arg(&comment)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("review-harness: PASS"))
        .stdout(predicate::str::contains("skipped: no provider configured"));
    let files: Vec<_> = std::fs::read_dir(&rec).unwrap().collect();
    assert_eq!(files.len(), 1);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap())
            .unwrap();
    assert_eq!(v["gate"]["decision"], "pass");
    assert!(std::fs::read_to_string(comment)
        .unwrap()
        .contains("review-harness:report"));
}

#[test]
fn failing_check_exits_one() {
    let r = repo(
        &[("docs/a.md", "hi\n")],
        "[verify]\nchecks = [{ name = \"bad\", command = \"exit 1\" }]\n",
    );
    let (_s, path) = stub_path();
    run(r.path(), &path)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("FAIL"));
}

#[test]
fn high_risk_change_needs_human_even_when_green() {
    let r = repo(&[(".github/workflows/x.yml", "on: push\n")], OK_CFG);
    let (_s, path) = stub_path();
    run(r.path(), &path)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("NEEDS HUMAN"))
        .stdout(predicate::str::contains("high-risk"));
}

#[test]
fn disabled_scanners_cannot_pass() {
    let cfg = format!("{OK_CFG}[security]\ncargo_audit = false\ngitleaks = false\n");
    let r = repo(&[("docs/a.md", "hi\n")], &cfg);
    let (_s, path) = stub_path();
    run(r.path(), &path)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("security scan incomplete"));
}

#[test]
fn no_checks_configured_cannot_pass() {
    let r = repo(&[("docs/a.md", "hi\n")], "");
    let (_s, path) = stub_path();
    run(r.path(), &path)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("no verification evidence"));
}

#[test]
fn json_output_is_a_run_record() {
    let r = repo(&[("docs/a.md", "hi\n")], OK_CFG);
    let (_s, path) = stub_path();
    let out = run(r.path(), &path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["review"]["status"]["kind"], "skipped");
}
