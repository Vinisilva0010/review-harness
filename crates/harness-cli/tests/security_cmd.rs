use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn stub(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn cmd(path_dir: &Path, repo: &Path) -> Command {
    let mut c = Command::cargo_bin("review-harness").unwrap();
    c.env("PATH", path_dir)
        .args(["security", "--dir"])
        .arg(repo);
    c
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.lock"), "").unwrap();
    d
}

#[test]
fn no_scanners_installed_is_incomplete_not_pass() {
    let empty = tempfile::tempdir().unwrap();
    cmd(empty.path(), repo().path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("Skipped"));
}

#[test]
fn clean_scanners_exit_zero() {
    let bin = tempfile::tempdir().unwrap();
    stub(
        bin.path(),
        "cargo",
        r#"echo '{"vulnerabilities":{"found":false,"count":0,"list":[]}}'"#,
    );
    stub(bin.path(), "gitleaks", "exit 0");
    cmd(bin.path(), repo().path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Passed"));
}

#[test]
fn vulnerability_exits_one() {
    let bin = tempfile::tempdir().unwrap();
    stub(
        bin.path(),
        "cargo",
        r#"echo '{"vulnerabilities":{"found":true,"count":1,"list":[{"advisory":{"id":"RUSTSEC-1","title":"t"},"package":{"name":"p","version":"1"}}]}}'; exit 1"#,
    );
    stub(bin.path(), "gitleaks", "exit 0");
    cmd(bin.path(), repo().path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("RUSTSEC-1"));
}
