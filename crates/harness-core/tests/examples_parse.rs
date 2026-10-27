use std::path::Path;

use harness_core::metrics::{aggregate, load_records};

#[test]
fn committed_example_records_match_the_current_schema() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/run-records");
    let (records, warnings) = load_records(&dir).unwrap();
    assert!(warnings.is_empty(), "schema drift: {warnings:?}");
    assert_eq!(records.len(), 5);
    let m = aggregate(&records);
    assert_eq!(m.pull_requests, 4);
    assert_eq!(m.hallucinated, 2);
}
