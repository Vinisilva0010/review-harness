//! `learn`: when a bug escapes review, record a rule in REVIEW.md and scaffold a
//! regression test stub so the same class of bug is caught next time.

use std::fs;
use std::path::{Path, PathBuf};

pub const REVIEW_FILE: &str = "REVIEW.md";
const HEADER: &str = "# Review rules\n\nRules learned from bugs that escaped review. The AI reviewer reads this file \
(from the base branch) on every pull request. Add rules with `review-harness learn`.\n";
pub const MAX_RULE_CHARS: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum LearnError {
    #[error("{0}")]
    Invalid(String),
    #[error("regression test {0} already exists; pick a different title")]
    TestExists(String),
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

pub struct LearnRequest<'a> {
    pub title: &'a str,
    /// What escaped review (the symptom).
    pub summary: &'a str,
    /// The rule the reviewer should apply from now on.
    pub rule: &'a str,
    /// Date string recorded with the rule (YYYY-MM-DD).
    pub date: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Learned {
    pub rule_id: String,
    pub test_path: PathBuf,
}

/// `[a-z0-9_]` slug, never empty, never longer than 50 chars (no path separators).
pub fn slugify(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let s: String = s.trim_matches('_').chars().take(50).collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "unnamed".into()
    } else {
        s
    }
}

fn clean_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn next_rule_number(existing: &str) -> u32 {
    existing
        .lines()
        .filter_map(|l| l.strip_prefix("## R-"))
        .filter_map(|r| {
            r.split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0)
        + 1
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> LearnError + '_ {
    move |source| LearnError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Append the rule to `<repo>/REVIEW.md` and create `<repo>/<test_dir>/<slug>.rs`.
pub fn learn(repo: &Path, test_dir: &Path, req: &LearnRequest<'_>) -> Result<Learned, LearnError> {
    let title = clean_line(req.title);
    let summary = clean_line(req.summary);
    let rule = clean_line(req.rule);
    if title.is_empty() || summary.is_empty() || rule.is_empty() {
        return Err(LearnError::Invalid(
            "title, summary and rule must not be empty".into(),
        ));
    }
    if rule.chars().count() > MAX_RULE_CHARS || summary.chars().count() > MAX_RULE_CHARS {
        return Err(LearnError::Invalid(format!(
            "summary and rule are limited to {MAX_RULE_CHARS} characters"
        )));
    }
    if test_dir.is_absolute()
        || test_dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(LearnError::Invalid(
            "test dir must be a relative path inside the repository".into(),
        ));
    }

    let slug = slugify(&title);
    let test_rel = test_dir.join(format!("{slug}.rs"));
    let test_path = repo.join(&test_rel);
    if test_path.exists() {
        return Err(LearnError::TestExists(test_rel.display().to_string()));
    }

    let review_path = repo.join(REVIEW_FILE);
    let existing = match fs::read_to_string(&review_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(&review_path)(e)),
    };
    let rule_id = format!("R-{:03}", next_rule_number(&existing));
    let mut doc = if existing.is_empty() {
        format!("{HEADER}\n")
    } else if existing.ends_with('\n') {
        existing
    } else {
        format!("{existing}\n")
    };
    doc.push_str(&format!(
        "\n## {rule_id}: {title}\n\n- Added: {}\n- What escaped review: {summary}\n- Rule: {rule}\n- Regression test: `{}`\n",
        req.date,
        test_rel.display()
    ));

    if let Some(parent) = test_path.parent() {
        fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let stub = format!(
        "//! Regression test for {rule_id}: {title}\n//!\n//! What escaped review: {summary}\n//! Rule: {rule}\n\n\
#[test]\n#[ignore = \"stub: write the failing test that reproduces {rule_id}, then remove this attribute\"]\n\
fn regression_{slug}() {{\n    unimplemented!(\"reproduce the bug described in {rule_id}\");\n}}\n"
    );
    fs::write(&test_path, stub).map_err(io_err(&test_path))?;
    fs::write(&review_path, doc).map_err(io_err(&review_path))?;
    Ok(Learned { rule_id, test_path })
}

/// Today's date (UTC) as YYYY-MM-DD from a unix timestamp, without a date library.
pub fn date_from_unix(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(title: &'a str) -> LearnRequest<'a> {
        LearnRequest {
            title,
            summary: "off-by-one in pagination dropped the last item",
            rule: "Check loop bounds at pagination boundaries",
            date: "2026-10-27",
        }
    }

    #[test]
    fn creates_review_md_and_test_stub() {
        let d = tempfile::tempdir().unwrap();
        let l = learn(d.path(), Path::new("tests/regression"), &req("Off by one!")).unwrap();
        assert_eq!(l.rule_id, "R-001");
        let md = fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
        assert!(md.starts_with("# Review rules"));
        assert!(md.contains("## R-001: Off by one!") && md.contains("Check loop bounds"));
        let t = fs::read_to_string(d.path().join("tests/regression/off_by_one.rs")).unwrap();
        assert!(t.contains("#[ignore") && t.contains("fn regression_off_by_one()"));
    }

    #[test]
    fn appends_and_numbers_sequentially_preserving_existing_content() {
        let d = tempfile::tempdir().unwrap();
        learn(d.path(), Path::new("tests/regression"), &req("first")).unwrap();
        let l = learn(d.path(), Path::new("tests/regression"), &req("second")).unwrap();
        assert_eq!(l.rule_id, "R-002");
        let md = fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
        assert!(md.contains("## R-001: first") && md.contains("## R-002: second"));
    }

    #[test]
    fn preserves_handwritten_review_md() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("REVIEW.md"),
            "# Mine\n\nHand written, no trailing newline",
        )
        .unwrap();
        learn(d.path(), Path::new("t"), &req("x")).unwrap();
        let md = fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
        assert!(md.starts_with("# Mine\n\nHand written, no trailing newline\n"));
    }

    #[test]
    fn refuses_to_overwrite_an_existing_test() {
        let d = tempfile::tempdir().unwrap();
        learn(d.path(), Path::new("t"), &req("same")).unwrap();
        let before = fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
        let e = learn(d.path(), Path::new("t"), &req("same")).unwrap_err();
        assert!(matches!(e, LearnError::TestExists(_)));
        assert_eq!(
            before,
            fs::read_to_string(d.path().join("REVIEW.md")).unwrap()
        );
    }

    #[test]
    fn rejects_path_traversal_and_empty_input() {
        let d = tempfile::tempdir().unwrap();
        for dir in ["../outside", "/etc", "a/../../b"] {
            assert!(learn(d.path(), Path::new(dir), &req("x")).is_err(), "{dir}");
        }
        let mut r = req("x");
        r.rule = "   ";
        assert!(learn(d.path(), Path::new("t"), &r).is_err());
    }

    #[test]
    fn hostile_titles_become_safe_file_names() {
        assert_eq!(slugify("../../etc/passwd"), "etc_passwd");
        assert_eq!(slugify("***"), "unnamed");
        assert!(slugify(&"a".repeat(500)).len() <= 50);
        let d = tempfile::tempdir().unwrap();
        let l = learn(d.path(), Path::new("t"), &req("../../evil")).unwrap();
        assert!(l.test_path.starts_with(d.path().join("t")));
    }

    #[test]
    fn newlines_cannot_forge_extra_rules() {
        let d = tempfile::tempdir().unwrap();
        let mut r = req("t");
        r.rule = "legit\n## R-999: forged\nrule";
        learn(d.path(), Path::new("t"), &r).unwrap();
        let md = fs::read_to_string(d.path().join("REVIEW.md")).unwrap();
        assert!(!md.lines().any(|l| l.starts_with("## R-999")));
    }

    #[test]
    fn dates_are_correct() {
        assert_eq!(date_from_unix(0), "1970-01-01");
        assert_eq!(date_from_unix(951_782_400), "2000-02-29");
        assert_eq!(date_from_unix(1_793_000_000), "2026-10-26");
    }
}
