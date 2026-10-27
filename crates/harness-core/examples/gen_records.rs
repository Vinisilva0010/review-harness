//! Regenerates `examples/run-records/*.json` from scripted scenarios (MockProvider,
//! no network, no real LLM): `cargo run -p harness-core --example gen_records`.

use std::path::Path;

use harness_core::config::{Config, ProviderKind};
use harness_core::diff::Diff;
use harness_core::pipeline::{self, PipelineInput};
use harness_core::review::provider::{LlmError, LlmResponse, MockProvider};
use harness_core::review::{ProviderSlot, ReviewState};
use harness_core::security::Tools;
use harness_core::verify::Check;

fn diff(path: &str, added: &[&str]) -> Diff {
    let mut t = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
        added.len()
    );
    for a in added {
        t.push_str(&format!("+{a}\n"));
    }
    Diff::parse(&t)
}

fn config(check_cmd: &str, ai: bool) -> Config {
    let mut c = Config::default();
    c.verify.checks = vec![Check {
        name: "test".into(),
        command: check_cmd.into(),
    }];
    c.security.cargo_audit = false;
    c.security.gitleaks = false;
    if ai {
        c.review.provider = ProviderKind::Gemini;
        c.review.model = Some("example-model".into());
        c.review.api_key_env = Some("GEMINI_API_KEY".into());
    }
    c
}

fn reply(findings: &[(&str, u32, &str, &str)]) -> String {
    let items: Vec<String> = findings
        .iter()
        .map(|(f, l, s, c)| {
            format!(r#"{{"file":"{f}","line":{l},"severity":"{s}","category":"{c}","explanation":"Scripted example finding on line {l}.","suggested_fix":null}}"#)
        })
        .collect();
    format!(r#"{{"findings":[{}]}}"#, items.join(","))
}

struct Scenario<'a> {
    name: &'a str,
    pr: u64,
    unix: u64,
    diff: &'a Diff,
    config: &'a Config,
    state: ReviewState,
}

fn write(dir: &Path, sc: Scenario<'_>, slot: ProviderSlot) {
    let tmp = tempfile::tempdir().unwrap();
    let mut rec = pipeline::run(PipelineInput {
        config: sc.config,
        dir: tmp.path(),
        diff: sc.diff,
        incremental: None,
        base: Some("0000000000000000000000000000000000000001".into()),
        head: Some(format!("{:040x}", sc.pr)),
        pr: Some(sc.pr),
        project_rules: None,
        state: sc.state,
        provider: slot,
        tools: Tools::default(),
    })
    .unwrap();
    rec.generated_at_unix = sc.unix;
    std::fs::write(dir.join(sc.name), rec.to_json().unwrap() + "\n").unwrap();
}

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/run-records");
    std::fs::create_dir_all(&out).unwrap();
    let src = diff(
        "src/lib.rs",
        &["pub fn add(a: u32, b: u32) -> u32 {", "    a + b", "}"],
    );
    let docs = diff("docs/guide.md", &["Hello"]);
    let wf = diff(".github/workflows/ci.yml", &["on: push"]);
    let (ok_ai, ok_plain, bad_ai) = (
        config("true", true),
        config("true", false),
        config("exit 1", true),
    );
    let fresh = ReviewState::default;
    let mock = |texts: Vec<Result<LlmResponse, LlmError>>| {
        ProviderSlot::Ready(Box::new(MockProvider::new(texts)))
    };
    let resp = |t: String, i, o| {
        Ok(LlmResponse {
            text: t,
            input_tokens: Some(i),
            output_tokens: Some(o),
        })
    };

    // PR 101: docs only, no provider configured -> AI skipped.
    let sc = Scenario {
        name: "pr-101-round-1.json",
        pr: 101,
        unix: 1_790_000_000,
        diff: &docs,
        config: &ok_plain,
        state: fresh(),
    };
    write(&out, sc, ProviderSlot::NotConfigured);

    // PR 102: failing test -> fail, AI skipped to save tokens.
    let sc = Scenario {
        name: "pr-102-round-1.json",
        pr: 102,
        unix: 1_790_001_000,
        diff: &src,
        config: &bad_ai,
        state: fresh(),
    };
    write(&out, sc, mock(vec![]));

    // PR 103: AI finds one real issue and hallucinates two lines.
    let r1 = reply(&[
        ("src/lib.rs", 2, "high", "bug"),
        ("src/lib.rs", 90, "medium", "bug"),
        ("src/other.rs", 1, "low", "style"),
    ]);
    let sc = Scenario {
        name: "pr-103-round-1.json",
        pr: 103,
        unix: 1_790_002_000,
        diff: &src,
        config: &ok_ai,
        state: fresh(),
    };
    write(&out, sc, mock(vec![resp(r1, 1800, 240)]));

    // PR 103 round 2: clean after the fix.
    let state = ReviewState {
        rounds_done: 1,
        last_reviewed_commit: None,
        tokens_spent: 2040,
    };
    let sc = Scenario {
        name: "pr-103-round-2.json",
        pr: 103,
        unix: 1_790_003_000,
        diff: &src,
        config: &ok_ai,
        state,
    };
    write(&out, sc, mock(vec![resp(reply(&[]), 900, 10)]));

    // PR 104: workflow change (high risk) with a provider that times out: fail closed.
    let sc = Scenario {
        name: "pr-104-round-1.json",
        pr: 104,
        unix: 1_790_004_000,
        diff: &wf,
        config: &ok_ai,
        state: fresh(),
    };
    write(&out, sc, mock(vec![Err(LlmError::Timeout)]));
}
