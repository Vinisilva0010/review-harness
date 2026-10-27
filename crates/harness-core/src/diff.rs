//! Unified diff parser (`git diff` output) and a capped `git diff` runner.
//!
//! The parser is lenient: malformed input never panics, it just yields fewer
//! files/hunks. Hunk bodies are consumed using the line counts in the `@@` header,
//! so content lines that look like headers (`--- x`, `+++ y`) are handled correctly.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

/// Upper bound on diff bytes accepted from `git diff`.
pub const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    /// Followed by a `\ No newline at end of file` marker.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub kind: ChangeKind,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The path the file has after the change (or before it, for deletions).
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    /// Line numbers (new side) of every added line.
    pub fn added_lines(&self) -> BTreeSet<u32> {
        self.hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == LineKind::Added)
            .filter_map(|l| l.new_line)
            .collect()
    }

    pub fn additions(&self) -> usize {
        self.count(LineKind::Added)
    }

    pub fn deletions(&self) -> usize {
        self.count(LineKind::Removed)
    }

    pub fn changed_lines(&self) -> usize {
        self.additions() + self.deletions()
    }

    fn count(&self, kind: LineKind) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .filter(|l| l.kind == kind)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Diff {
    pub files: Vec<FileDiff>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn total_changed_lines(&self) -> usize {
        self.files.iter().map(FileDiff::changed_lines).sum()
    }

    /// Added line numbers of the file at `path`, if the diff touches it.
    pub fn added_lines_of(&self, path: &str) -> Option<BTreeSet<u32>> {
        self.files
            .iter()
            .find(|f| f.path() == path)
            .map(FileDiff::added_lines)
    }

    /// Parse unified diff text.
    pub fn parse(input: &str) -> Diff {
        let lines: Vec<&str> = input.split('\n').collect();
        let mut files: Vec<FileDiff> = Vec::new();
        let mut cur: Option<FileDiff> = None;
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if let Some(rest) = line.strip_prefix("diff --git ") {
                files.extend(cur.take());
                let (old, new) = split_git_header(rest);
                cur = Some(new_file(old, new));
            } else if let Some(rest) = line.strip_prefix("--- ") {
                // A plain unified diff (no `diff --git`) starts a file at `---`.
                let starts_new = match &cur {
                    None => true,
                    Some(f) => !f.hunks.is_empty() && f.old_path.is_some(),
                };
                if starts_new && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ ")) {
                    files.extend(cur.take());
                    cur = Some(new_file(None, None));
                }
                if let Some(f) = cur.as_mut() {
                    f.old_path = header_path(rest);
                }
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                if let Some(f) = cur.as_mut() {
                    f.new_path = header_path(rest);
                }
            } else if line.starts_with("@@ ") {
                if let (Some(f), Some((os, ol, ns, nl))) = (cur.as_mut(), parse_hunk_header(line)) {
                    let (hunk, used) = read_hunk_body(&lines[i + 1..], os, ol, ns, nl);
                    f.hunks.push(hunk);
                    i += used;
                }
            } else if let Some(f) = cur.as_mut() {
                apply_extended_header(f, line);
            }
            i += 1;
        }
        files.extend(cur);
        Diff { files }
    }
}

fn new_file(old: Option<String>, new: Option<String>) -> FileDiff {
    FileDiff {
        old_path: old,
        new_path: new,
        kind: ChangeKind::Modified,
        binary: false,
        hunks: Vec::new(),
    }
}

fn apply_extended_header(f: &mut FileDiff, line: &str) {
    if line.starts_with("new file mode") {
        f.kind = ChangeKind::Added;
        f.old_path = None;
    } else if line.starts_with("deleted file mode") {
        f.kind = ChangeKind::Deleted;
        f.new_path = None;
    } else if let Some(p) = line.strip_prefix("rename from ") {
        f.kind = ChangeKind::Renamed;
        f.old_path = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("rename to ") {
        f.kind = ChangeKind::Renamed;
        f.new_path = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("copy from ") {
        f.kind = ChangeKind::Copied;
        f.old_path = Some(unquote(p));
    } else if let Some(p) = line.strip_prefix("copy to ") {
        f.kind = ChangeKind::Copied;
        f.new_path = Some(unquote(p));
    } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
        f.binary = true;
    }
}

/// Decode a possibly C-quoted path (as emitted by git for unusual file names).
fn unquote(s: &str) -> String {
    let s = s.trim_end_matches('\r');
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            d @ b'0'..=b'3' => {
                let mut v = u32::from(d - b'0');
                let mut n = 1;
                while n < 3 && i + 1 < bytes.len() && (b'0'..=b'7').contains(&bytes[i + 1]) {
                    v = v * 8 + u32::from(bytes[i + 1] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(u8::try_from(v).unwrap_or(b'?'));
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Path from a `---`/`+++` header; `None` for `/dev/null`.
fn header_path(rest: &str) -> Option<String> {
    // Git may append a tab (and timestamp, in non-git diffs) after the path.
    let rest = rest.split('\t').next().unwrap_or(rest);
    let p = unquote(rest);
    if p == "/dev/null" {
        return None;
    }
    Some(
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(&p)
            .to_string(),
    )
}

/// Best-effort split of `a/<old> b/<new>`. Exact when both sides are equal;
/// otherwise later `---`/`+++`/`rename` lines supply the real paths.
fn split_git_header(rest: &str) -> (Option<String>, Option<String>) {
    let rest = rest.trim_end_matches('\r');
    if let Some(stripped) = rest.strip_prefix('"') {
        // Quoted first path: find the closing quote.
        if let Some(end) = stripped.find("\" ") {
            let first = unquote(&rest[..end + 2]);
            let second = unquote(&rest[end + 3..]);
            return (strip_ab(&first), strip_ab(&second));
        }
        return (None, None);
    }
    if let Some(after_a) = rest.strip_prefix("a/") {
        let total = after_a.len();
        // "a/P b/P": after_a = "P b/P" so P has (total - 3) / 2 bytes.
        if total >= 3 && (total - 3) % 2 == 0 {
            let n = (total - 3) / 2;
            if after_a.is_char_boundary(n)
                && after_a[n..].starts_with(" b/")
                && after_a[..n] == after_a[n + 3..]
            {
                let p = after_a[..n].to_string();
                return (Some(p.clone()), Some(p));
            }
        }
    }
    (None, None)
}

fn strip_ab(p: &str) -> Option<String> {
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .map(str::to_string)
}

fn parse_range(s: &str, prefix: char) -> Option<(u32, u32)> {
    let s = s.strip_prefix(prefix)?;
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = line.strip_prefix("@@ ")?.split(' ');
    let (os, ol) = parse_range(parts.next()?, '-')?;
    let (ns, nl) = parse_range(parts.next()?, '+')?;
    if parts.next()? != "@@" {
        return None;
    }
    Some((os, ol, ns, nl))
}

/// Returns the hunk and the number of body lines consumed.
fn read_hunk_body(rest: &[&str], os: u32, ol: u32, ns: u32, nl: u32) -> (Hunk, usize) {
    let mut hunk = Hunk {
        old_start: os,
        old_len: ol,
        new_start: ns,
        new_len: nl,
        lines: Vec::new(),
    };
    let (mut old_rem, mut new_rem) = (ol, nl);
    let (mut old_no, mut new_no) = (os, ns);
    let mut used = 0;
    for raw in rest {
        if raw.starts_with('\\') {
            if let Some(last) = hunk.lines.last_mut() {
                last.no_newline = true;
            }
            used += 1;
            continue;
        }
        if old_rem == 0 && new_rem == 0 {
            break;
        }
        let (kind, content) = match raw.chars().next() {
            Some('+') if new_rem > 0 => (LineKind::Added, &raw[1..]),
            Some('-') if old_rem > 0 => (LineKind::Removed, &raw[1..]),
            Some(' ') if old_rem > 0 && new_rem > 0 => (LineKind::Context, &raw[1..]),
            // Some tools strip the single space of an empty context line.
            None if old_rem > 0 && new_rem > 0 => (LineKind::Context, ""),
            _ => break,
        };
        let mut line = DiffLine {
            kind,
            content: content.to_string(),
            old_line: None,
            new_line: None,
            no_newline: false,
        };
        if kind != LineKind::Added {
            line.old_line = Some(old_no);
            old_no = old_no.saturating_add(1);
            old_rem -= 1;
        }
        if kind != LineKind::Removed {
            line.new_line = Some(new_no);
            new_no = new_no.saturating_add(1);
            new_rem -= 1;
        }
        hunk.lines.push(line);
        used += 1;
    }
    (hunk, used)
}

#[derive(Debug, thiserror::Error)]
pub enum GitDiffError {
    #[error("failed to run git: {0}")]
    Spawn(std::io::Error),
    #[error("git diff failed: {0}")]
    Failed(String),
    #[error("diff exceeds the {0} byte limit")]
    TooLarge(usize),
    #[error("git diff timed out")]
    Timeout,
}

/// Run `git diff base...head` in `repo`, capping output at `max_bytes`.
pub fn git_diff(
    repo: &Path,
    base: &str,
    head: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<String, GitDiffError> {
    if base.starts_with('-') || head.starts_with('-') {
        return Err(GitDiffError::Failed("refs must not start with '-'".into()));
    }
    let mut child = Command::new("git")
        .args([
            "-c",
            "core.quotepath=off",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-M",
            &format!("{base}...{head}"),
            "--",
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(GitDiffError::Spawn)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.take(max_bytes as u64 + 1).read_to_end(&mut buf);
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.take(4096).read_to_end(&mut buf);
        buf
    });
    let status = match child.wait_timeout(timeout) {
        Ok(Some(s)) => Some(s),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    if out.len() > max_bytes {
        let _ = child.kill();
        return Err(GitDiffError::TooLarge(max_bytes));
    }
    match status {
        None => Err(GitDiffError::Timeout),
        Some(s) if s.success() => Ok(String::from_utf8_lossy(&out).into_owned()),
        Some(_) => Err(GitDiffError::Failed(
            String::from_utf8_lossy(&err).trim().to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MODIFY: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 111..222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ fn ctx
 keep
-old
+new
+extra
 tail
 end
";

    #[test]
    fn parses_modification_with_line_numbers() {
        let d = Diff::parse(MODIFY);
        assert_eq!(d.files.len(), 1);
        let f = &d.files[0];
        assert_eq!(f.path(), "src/lib.rs");
        assert_eq!(f.kind, ChangeKind::Modified);
        assert_eq!(f.added_lines().into_iter().collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!((f.additions(), f.deletions()), (2, 1));
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_len, h.new_start, h.new_len),
            (1, 4, 1, 5)
        );
    }

    #[test]
    fn empty_diff() {
        assert!(Diff::parse("").is_empty());
        assert!(Diff::parse("\n\n").is_empty());
    }

    #[test]
    fn new_file_and_deleted_file() {
        let d = Diff::parse(
            "diff --git a/n.txt b/n.txt
new file mode 100644
index 0000000..111
--- /dev/null
+++ b/n.txt
@@ -0,0 +1,2 @@
+a
+b
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 111..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
",
        );
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.files[0].kind, ChangeKind::Added);
        assert_eq!(d.files[0].old_path, None);
        assert_eq!(d.files[0].added_lines().len(), 2);
        assert_eq!(d.files[1].kind, ChangeKind::Deleted);
        assert_eq!(d.files[1].path(), "gone.txt");
        assert_eq!(d.files[1].deletions(), 2);
        assert!(d.files[1].added_lines().is_empty());
    }

    #[test]
    fn binary_only_diff() {
        let d = Diff::parse(
            "diff --git a/img.png b/img.png
index 111..222 100644
Binary files a/img.png and b/img.png differ
",
        );
        assert_eq!(d.files.len(), 1);
        assert!(d.files[0].binary);
        assert_eq!(d.files[0].path(), "img.png");
        assert!(d.files[0].hunks.is_empty());
        assert_eq!(d.total_changed_lines(), 0);
    }

    #[test]
    fn rename_only() {
        let d = Diff::parse(
            "diff --git a/old name.rs b/new name.rs
similarity index 100%
rename from old name.rs
rename to new name.rs
",
        );
        let f = &d.files[0];
        assert_eq!(f.kind, ChangeKind::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("old name.rs"));
        assert_eq!(f.path(), "new name.rs");
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn rename_with_edit() {
        let d = Diff::parse(
            "diff --git a/a.rs b/b.rs
similarity index 80%
rename from a.rs
rename to b.rs
--- a/a.rs
+++ b/b.rs
@@ -1 +1 @@
-x
+y
",
        );
        assert_eq!(d.files[0].kind, ChangeKind::Renamed);
        assert_eq!(d.files[0].path(), "b.rs");
        assert_eq!(d.files[0].additions(), 1);
    }

    #[test]
    fn no_newline_marker_is_attached() {
        let d = Diff::parse(
            "diff --git a/f b/f
--- a/f
+++ b/f
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
",
        );
        let lines = &d.files[0].hunks[0].lines;
        assert_eq!(lines.len(), 2);
        assert!(lines[0].no_newline && lines[1].no_newline);
    }

    #[test]
    fn content_that_looks_like_headers_stays_content() {
        // Removing a line "-- x" and adding "++ y" yields "--- x" / "+++ y" body lines.
        let d = Diff::parse(
            "diff --git a/f b/f
--- a/f
+++ b/f
@@ -1 +1 @@
--- x
+++ y
",
        );
        assert_eq!(d.files.len(), 1);
        let f = &d.files[0];
        assert_eq!(f.path(), "f");
        assert_eq!(f.hunks[0].lines[0].content, "-- x");
        assert_eq!(f.hunks[0].lines[1].content, "++ y");
    }

    #[test]
    fn multiple_hunks_track_new_line_numbers() {
        let d = Diff::parse(
            "diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,2 +1,2 @@
 a
-b
+B
@@ -10,2 +10,3 @@
 j
+k
 l
",
        );
        assert_eq!(
            d.files[0].added_lines().into_iter().collect::<Vec<_>>(),
            vec![2, 11]
        );
    }

    #[test]
    fn quoted_paths_are_decoded() {
        let d = Diff::parse(
            "diff --git \"a/sp\\303\\251cial \\\"q\\\".txt\" \"b/sp\\303\\251cial \\\"q\\\".txt\"
--- \"a/sp\\303\\251cial \\\"q\\\".txt\"
+++ \"b/sp\\303\\251cial \\\"q\\\".txt\"
@@ -1 +1 @@
-a
+b
",
        );
        assert_eq!(d.files[0].path(), "spécial \"q\".txt");
    }

    #[test]
    fn mode_only_change_keeps_path() {
        let d = Diff::parse(
            "diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
",
        );
        assert_eq!(d.files[0].path(), "run.sh");
        assert!(d.files[0].hunks.is_empty());
    }

    #[test]
    fn plain_unified_diff_without_git_header() {
        let d = Diff::parse("--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].path(), "x.txt");
    }

    #[test]
    fn crlf_content_is_preserved() {
        let d = Diff::parse("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\r\n+b\r\n");
        assert_eq!(d.files[0].hunks[0].lines[1].content, "b\r");
    }

    #[test]
    fn truncated_hunk_does_not_panic() {
        let d = Diff::parse("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,50 +1,50 @@\n a\n+b");
        assert_eq!(d.files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn added_lines_of_lookup() {
        let d = Diff::parse(MODIFY);
        assert!(d.added_lines_of("src/lib.rs").unwrap().contains(&2));
        assert!(d.added_lines_of("nope").is_none());
    }

    proptest! {
        #[test]
        fn parser_never_panics_on_arbitrary_text(s in "\\PC*") {
            let _ = Diff::parse(&s);
        }

        #[test]
        fn parser_never_panics_on_diff_like_text(
            lines in proptest::collection::vec(
                prop_oneof![
                    Just("diff --git a/x b/x".to_string()),
                    Just("--- a/x".to_string()),
                    Just("+++ b/x".to_string()),
                    Just("@@ -1,3 +1,3 @@".to_string()),
                    Just("@@ -0,0 +4294967295,4294967295 @@".to_string()),
                    "[ +\\\\-].{0,20}",
                    ".{0,30}",
                ],
                0..40,
            )
        ) {
            let d = Diff::parse(&lines.join("\n"));
            for f in &d.files {
                for h in &f.hunks {
                    prop_assert!(h.lines.len() <= (h.old_len as usize).saturating_add(h.new_len as usize));
                }
            }
        }

        #[test]
        fn added_lines_match_generated_hunk(
            ctx_before in 0u32..5, adds in 1u32..8, start in 1u32..1000
        ) {
            let mut text = String::from("diff --git a/f b/f\n--- a/f\n+++ b/f\n");
            let old_len = ctx_before;
            let new_len = ctx_before + adds;
            text.push_str(&format!("@@ -{start},{old_len} +{start},{new_len} @@\n"));
            for _ in 0..ctx_before { text.push_str(" c\n"); }
            for _ in 0..adds { text.push_str("+a\n"); }
            let d = Diff::parse(&text);
            let got: Vec<u32> = d.files[0].added_lines().into_iter().collect();
            let want: Vec<u32> = (start + ctx_before..start + ctx_before + adds).collect();
            prop_assert_eq!(got, want);
        }
    }

    #[test]
    fn git_diff_rejects_option_like_refs() {
        let r = git_diff(
            Path::new("."),
            "--output=x",
            "HEAD",
            1000,
            Duration::from_secs(5),
        );
        assert!(matches!(r, Err(GitDiffError::Failed(_))));
    }
}
