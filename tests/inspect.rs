use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_inhibitor-lab");

struct Fixture {
    root: PathBuf,
    log: PathBuf,
    events: Vec<Value>,
}

impl Fixture {
    fn run(scenario: &str, action: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "inhibitor-inspect-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let log = root.join("events.jsonl");
        let config = root.join("rules.json");
        fs::write(&config, include_str!("../examples/pi-rules.json")).unwrap();
        let result = Command::new(BIN)
            .args(["run", "--format", "pi-rpc", "--rules"])
            .arg(config)
            .arg("--out")
            .arg(&log)
            .args([
                "--prompt",
                "fixture",
                "--on-trigger",
                action,
                "--timeout-ms",
                "3000",
                "--control-timeout-ms",
                "500",
            ])
            .args(if scenario.starts_with("gate-") {
                &["--tool-gate"][..]
            } else {
                &[][..]
            })
            .arg("--")
            .arg(env!("CARGO_BIN_EXE_mock-agent"))
            .arg(scenario)
            .output()
            .unwrap();
        assert!(
            result.status.code().is_some(),
            "fixture terminated without exit code"
        );
        let events = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        Self { root, log, events }
    }

    fn block() -> Self {
        Self::run("gate-block", "block")
    }

    fn save(&self) {
        let mut bytes = Vec::new();
        for event in &self.events {
            serde_json::to_writer(&mut bytes, event).unwrap();
            bytes.push(b'\n');
        }
        fs::write(&self.log, bytes).unwrap();
    }

    fn report(&self) -> (i32, Value) {
        let before = fs::read(&self.log).unwrap();
        let files = fs::read_dir(&self.root).unwrap().count();
        let output = Command::new(BIN)
            .args(["inspect", "--log"])
            .arg(&self.log)
            .args(["--format", "json"])
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert_eq!(
            fs::read(&self.log).unwrap(),
            before,
            "inspect changed the journal"
        );
        assert_eq!(
            fs::read_dir(&self.root).unwrap().count(),
            files,
            "inspect created files"
        );
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.code().unwrap(), report)
    }

    fn reject(&self) {
        let (exit, report) = self.report();
        assert_eq!(exit, 2, "{report}");
        assert_eq!(report["eligible_for_manual_restart"], false, "{report}");
        assert!(!report["reasons"].as_array().unwrap().is_empty());
    }

    fn data(&mut self, kind: &str) -> &mut Value {
        &mut self.events.iter_mut().find(|v| v["kind"] == kind).unwrap()["data"]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent().unwrap(), std::env::temp_dir());
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("inhibitor-inspect-")
        );
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn complete_block_and_closure_without_denials_are_distinguished() {
    for (scenario, count, outcome) in [
        ("gate-block", 1, "blocked"),
        ("gate-no-next", 0, "gate_closed"),
    ] {
        let fixture = Fixture::run(scenario, "block");
        let (code, report) = fixture.report();
        assert_eq!(code, 0, "{report}");
        assert_eq!(report["eligible_for_manual_restart"], true, "{report}");
        assert_eq!(report["confirmed_blocked_tools"], count);
        assert_eq!(report["finish"]["control_outcome"], outcome);
        assert_eq!(report["triggers"][0]["threshold"], 2);
        assert!(
            report["triggers"][0]["raw_event_sequence"]
                .as_u64()
                .is_some()
        );
    }
}

#[test]
fn record_abort_and_failed_runs_are_diagnosed_without_restart_eligibility() {
    for (scenario, action) in [
        ("gate-record", "record"),
        ("rpc-abort", "abort"),
        ("rpc-bad-exit", "abort"),
    ] {
        let fixture = Fixture::run(scenario, action);
        let (code, report) = fixture.report();
        assert_eq!(code, 0, "{scenario}: {report}");
        assert_eq!(report["eligible_for_manual_restart"], false);
        assert!(!report["reasons"].as_array().unwrap().is_empty());
    }
}

#[test]
fn incomplete_missing_and_malformed_tail_never_qualify() {
    let mut fixture = Fixture::block();
    fixture.events.pop();
    fixture.save();
    let (code, report) = fixture.report();
    assert_eq!(code, 2);
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["eligible_for_manual_restart"], false);
    for tail in [b"{\"kind\":".as_slice(), b"{\"kind\":\n", b"\xff\n", b"\n"] {
        fixture.save();
        let mut bytes = fs::read(&fixture.log).unwrap();
        bytes.extend_from_slice(tail);
        fs::write(&fixture.log, bytes).unwrap();
        fixture.reject();
    }
    fs::write(&fixture.log, b"").unwrap();
    fixture.reject();
}

#[test]
fn version_run_identity_sequence_and_unknown_events_are_validated() {
    let mut fixture = Fixture::block();
    let original = fixture.events.clone();
    for (key, value) in [
        ("schema_version", json!(2)),
        ("run_id", json!("another-run")),
        ("sequence", json!(99)),
        ("kind", json!("unknown_future_event")),
        ("at_unix_ms", json!(-1)),
    ] {
        fixture.events = original.clone();
        fixture.events[1][key] = value;
        fixture.save();
        fixture.reject();
    }
    fixture.events = original;
    fixture.events.push(fixture.events.last().unwrap().clone());
    let last = fixture.events.len() - 1;
    fixture.events[last]["sequence"] = json!(last + 1);
    fixture.save();
    fixture.reject();
}

#[test]
fn missing_forward_and_wrong_kind_references_are_rejected() {
    let mut fixture = Fixture::block();
    let original = fixture.events.clone();
    for (kind, key, value) in [
        ("rule_triggered", "matched_event_sequence", 9999),
        ("gate_closed", "trigger_sequence", 1),
        ("gate_applied", "decision_sequence", 1),
        ("agent_event", "raw_event_sequence", 0),
    ] {
        fixture.events = original.clone();
        fixture.data(kind)[key] = json!(value);
        fixture.save();
        fixture.reject();
    }
}

#[test]
fn fabricated_finish_cannot_substitute_for_control_and_shutdown_evidence() {
    let mut fixture = Fixture::block();
    let original = fixture.events.clone();
    for field in [
        "blocked_tool_count",
        "trigger_count",
        "success",
        "capture_complete",
        "adapter_complete",
        "forced_cleanup",
        "exit_code",
    ] {
        fixture.events = original.clone();
        let value = match field {
            "success" | "capture_complete" | "adapter_complete" => json!(false),
            "forced_cleanup" => json!(true),
            _ => json!(9),
        };
        fixture.data("run_finished")[field] = value;
        fixture.save();
        fixture.reject();
    }
    for kind in [
        "gate_ready",
        "gate_closed",
        "gate_applied",
        "rpc_shutdown_requested",
        "rpc_input_closed",
    ] {
        fixture.events = original.clone();
        let event = fixture
            .events
            .iter_mut()
            .find(|e| e["kind"] == kind)
            .unwrap();
        // Preserve envelope/sequence so only the missing semantic evidence causes rejection.
        event["kind"] = json!("output");
        event["data"] = json!({"source":"stderr","text":"removed evidence","truncated":false,"invalid_utf8":false});
        fixture.save();
        fixture.reject();
    }
}

#[test]
fn changed_tool_results_decisions_and_confirmations_cannot_claim_blocking() {
    let mut fixture = Fixture::block();
    let original = fixture.events.clone();
    for kind in [
        "agent_event",
        "gate_decision",
        "gate_applied",
        "tool_blocked_result",
    ] {
        fixture.events = original.clone();
        match kind {
            "agent_event" => fixture.data(kind)["event"]["failed"] = json!(false),
            "tool_blocked_result" => fixture.data(kind)["counted_as_tool_failure"] = json!(true),
            _ => fixture.data(kind)["allowed"] = json!(false),
        }
        fixture.save();
        fixture.reject();
    }
}

#[test]
fn impossible_threshold_cannot_be_backed_by_a_single_result_reference() {
    let mut fixture = Fixture::block();
    let trigger = &mut fixture.data("rule_triggered")["trigger"];
    trigger["matched_events"] = json!(999);
    trigger["threshold"] = json!(999);
    fixture.save();
    fixture.reject();
}

#[test]
fn log_program_and_terminal_controls_remain_data() {
    let mut fixture = Fixture::block();
    fixture.events[0]["data"]["program"] =
        json!("powershell -Command Set-Content inspect-executed.txt bad");
    fixture.data("rule_triggered")["trigger"]["rule_id"] = json!("\u{1b}[2Jtest\nspoof");
    fixture.save();
    assert_eq!(fixture.report().1["eligible_for_manual_restart"], true);
    let output = Command::new(BIN)
        .args(["inspect", "--log"])
        .arg(&fixture.log)
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(!fixture.root.join("inspect-executed.txt").exists());
}

#[test]
fn premature_input_close_cannot_be_used_as_clean_shutdown_evidence() {
    let mut fixture = Fixture::block();
    let closed = fixture
        .events
        .iter()
        .find(|e| e["kind"] == "rpc_input_closed")
        .unwrap()["data"]
        .clone();
    let original = fixture
        .events
        .iter_mut()
        .find(|e| e["kind"] == "rpc_input_closed")
        .unwrap();
    original["kind"] = json!("output");
    original["data"] =
        json!({"source":"stderr","text":"removed close","truncated":false,"invalid_utf8":false});
    let early = fixture
        .events
        .iter_mut()
        .find(|e| e["kind"] == "rpc_command_written")
        .unwrap();
    early["kind"] = json!("rpc_input_closed");
    early["data"] = closed;
    fixture.save();
    fixture.reject();
}

#[test]
fn large_lines_files_and_missing_files_fail_without_execution() {
    let fixture = Fixture::block();
    fs::write(
        &fixture.log,
        [vec![b' '; 1024 * 1024], vec![b'\n']].concat(),
    )
    .unwrap();
    fixture.reject();
    fs::File::create(&fixture.log)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    fixture.reject();
    let output = Command::new(BIN)
        .args(["inspect", "--log"])
        .arg(fixture.root.join("missing.jsonl"))
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["status"],
        "invalid"
    );
}

#[test]
fn inspect_cli_rejects_run_options_duplicates_and_missing_arguments() {
    for args in [
        vec!["inspect"],
        vec!["inspect", "--log"],
        vec!["inspect", "--log", "none", "--log", "none"],
        vec!["inspect", "--log", "none", "--format", "pi-rpc"],
        vec!["inspect", "--log", "none", "--", "program"],
        vec!["inspect", "--log", "none", "--out", "new.jsonl"],
    ] {
        let output = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
