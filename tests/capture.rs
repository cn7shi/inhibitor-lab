use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);

fn sandbox() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "inhibitor-lab-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT_CASE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn execute(scenario: &str, rules: &str) -> (std::process::Output, Vec<Value>) {
    let root = sandbox();
    let config = root.join("rules.json");
    let journal = root.join("events.jsonl");
    fs::write(&config, rules).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_inhibitor-lab"))
        .args(["run", "--rules"])
        .arg(&config)
        .arg("--out")
        .arg(&journal)
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_mock-agent"))
        .arg(scenario)
        .output()
        .unwrap();
    let events = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // Each case owns a fresh, uniquely named directory under the OS temp directory.
    assert!(root.starts_with(std::env::temp_dir()));
    fs::remove_dir_all(root).unwrap();
    (output, events)
}

const RULES: &str =
    r#"{"rules":[{"id":"repeat","source":"stderr","contains":"ERROR","threshold":2}]}"#;

#[test]
fn repeated_output_is_captured_and_trigger_references_its_evidence() {
    let (output, events) = execute("repeat", RULES);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("finished"));
    let triggers: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "rule_triggered")
        .collect();
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0]["data"]["trigger"]["matched_lines"], 2);
    let evidence = &events[triggers[0]["data"]["matched_event_sequence"]
        .as_u64()
        .unwrap() as usize
        - 1];
    assert_eq!(evidence["kind"], "output");
    assert_eq!(evidence["data"]["source"], "stderr");
    assert!(evidence["data"]["text"].as_str().unwrap().contains("ERROR"));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "output")
            .count(),
        5
    );
    assert_eq!(events.last().unwrap()["kind"], "run_finished");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["sequence"], index + 1);
        assert_eq!(event["run_id"], events[0]["run_id"]);
    }
}

#[test]
fn ordinary_stderr_does_not_trigger_and_child_exit_code_is_preserved() {
    let (normal, events) = execute("normal", RULES);
    assert!(normal.status.success());
    assert!(!events.iter().any(|event| event["kind"] == "rule_triggered"));
    let (failed, events) = execute("fail", RULES);
    assert_eq!(failed.status.code(), Some(7));
    assert_eq!(events.last().unwrap()["data"]["exit_code"], 7);
}

#[test]
fn long_lines_are_marked_and_not_matched_and_final_partial_line_is_captured() {
    let (output, events) = execute(
        "long-line",
        r#"{"rules":[{"id":"long","contains":"x","threshold":1}]}"#,
    );
    assert!(output.status.success());
    assert!(!events.iter().any(|event| event["kind"] == "rule_triggered"));
    assert!(
        events
            .iter()
            .any(|event| event["data"]["truncated"] == true)
    );
    assert!(
        events
            .iter()
            .any(|event| event["data"]["text"] == "final line without newline")
    );
}

#[test]
fn existing_output_file_is_preserved() {
    let root = sandbox();
    let config = root.join("rules.json");
    let journal = root.join("events.jsonl");
    fs::write(&config, RULES).unwrap();
    fs::write(&journal, "existing evidence").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_inhibitor-lab"))
        .args(["run", "--rules"])
        .arg(config)
        .arg("--out")
        .arg(&journal)
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_mock-agent"))
        .arg("repeat")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read_to_string(journal).unwrap(), "existing evidence");
    assert!(root.starts_with(std::env::temp_dir()));
    fs::remove_dir_all(root).unwrap();
}
