use serde_json::{Value, json};
use std::fs;
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn execute(scenario: &str, action: &str, rules: &str, prompt: &str) -> (Output, Vec<Value>) {
    let root = std::env::temp_dir().join(format!(
        "inhibitor-control-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let config = root.join("rules.json");
    let journal = root.join("events.jsonl");
    fs::write(&config, rules).unwrap();
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_inhibitor-lab"))
        .args(["run", "--format", "pi-rpc", "--rules"])
        .arg(config)
        .arg("--out")
        .arg(&journal)
        .args([
            "--on-trigger",
            action,
            "--prompt",
            prompt,
            "--timeout-ms",
            "3000",
            "--control-timeout-ms",
            "1000",
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
        started.elapsed() < Duration::from_secs(10),
        "RPC deadline failed"
    );
    let events = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(root.parent().unwrap(), std::env::temp_dir());
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("inhibitor-control-")
    );
    fs::remove_dir_all(root).unwrap();
    (output, events)
}

const RULES: &str = include_str!("../examples/pi-rules.json");

#[test]
fn protocol_confirmation_does_not_hide_a_failed_child_shutdown() {
    let (output, events) = execute("rpc-bad-exit", "abort", RULES, "test");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "control_confirmed")
    );
    assert!(events.iter().any(|event| event["kind"] == "control_failed"));
    assert_eq!(events.last().unwrap()["data"]["exit_code"], 7);
    assert_eq!(events.last().unwrap()["data"]["control_outcome"], "failed");
}

#[test]
fn abort_requires_explicit_rpc_mode_before_any_program_is_started() {
    let output = Command::new(env!("CARGO_BIN_EXE_inhibitor-lab"))
        .args([
            "run",
            "--rules",
            "unused.json",
            "--format",
            "pi-json",
            "--on-trigger",
            "abort",
            "--",
        ])
        .arg(env!("CARGO_BIN_EXE_mock-agent"))
        .arg("repeat")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("require --format pi-rpc"));
    assert!(output.stdout.is_empty());
}

fn commands(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter(|event| event["kind"] == "rpc_command_requested")
        .map(|event| event["data"]["command"].as_str().unwrap())
        .collect()
}

#[test]
fn abort_requires_correlated_responses_idle_state_and_process_exit() {
    let (output, events) = execute("rpc-abort", "abort", RULES, "test");
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        commands(&events),
        ["prompt", "clear_queue", "abort", "get_state"]
    );
    let trigger = events
        .iter()
        .find(|event| event["kind"] == "rule_triggered")
        .unwrap();
    let confirmation = events
        .iter()
        .find(|event| event["kind"] == "control_confirmed")
        .unwrap();
    assert_eq!(
        confirmation["data"]["trigger_sequence"],
        trigger["sequence"]
    );
    assert_eq!(trigger["data"]["trigger"]["matched_events"], 2);
    assert_eq!(events.last().unwrap()["data"]["control_outcome"], "aborted");
    assert_eq!(events.last().unwrap()["data"]["exit_code"], 0);
    assert_eq!(events.last().unwrap()["data"]["forced_cleanup"], false);
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "rpc_input_closed")
    );
}

#[test]
fn natural_completion_and_record_only_mode_do_not_abort() {
    for (scenario, action) in [("rpc-natural", "abort"), ("rpc-record", "record")] {
        let (output, events) = execute(scenario, action, RULES, "test");
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(commands(&events), ["prompt"]);
        assert_eq!(
            events.last().unwrap()["data"]["control_outcome"],
            "completed"
        );
    }
}

#[test]
fn simultaneous_rules_start_only_one_abort_sequence() {
    let mut config: Value = serde_json::from_str(RULES).unwrap();
    let mut second = config["rules"][0].clone();
    second["id"] = json!("second-rule");
    config["rules"].as_array_mut().unwrap().push(second);
    let (output, events) = execute("rpc-abort", "abort", &config.to_string(), "test");
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "rule_triggered")
            .count(),
        2
    );
    assert_eq!(
        commands(&events),
        ["prompt", "clear_queue", "abort", "get_state"]
    );
}

#[test]
fn rejection_wrong_response_and_nonidle_state_cannot_confirm_abort() {
    for scenario in [
        "rpc-reject",
        "rpc-wrong-id",
        "rpc-wrong-command",
        "rpc-active",
        "rpc-premature",
    ] {
        let (output, events) = execute(scenario, "abort", RULES, "test");
        assert_eq!(output.status.code(), Some(2), "{scenario}");
        assert!(
            !events
                .iter()
                .any(|event| event["kind"] == "control_confirmed"),
            "{scenario}"
        );
        assert_eq!(events.last().unwrap()["data"]["control_outcome"], "failed");
    }
}

#[test]
fn missing_response_and_agent_end_alone_time_out_instead_of_reporting_success() {
    for (scenario, action) in [("rpc-ignore", "abort"), ("rpc-ended-only", "record")] {
        let (output, events) = execute(scenario, action, RULES, "test");
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(events.last().unwrap()["data"]["forced_cleanup"], true);
        assert_eq!(events.last().unwrap()["data"]["capture_complete"], false);
        assert!(events.iter().any(|event| {
            event["kind"] == "control_failed"
                && event["data"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("deadline")
        }));
    }
}

#[test]
fn child_not_reading_stdin_cannot_block_the_supervisor_deadline() {
    let (output, events) = execute("rpc-no-stdin", "abort", RULES, &"x".repeat(20000));
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(events.last().unwrap()["data"]["forced_cleanup"], true);
}

#[test]
fn tools_started_during_abort_are_recorded_as_a_control_race() {
    let (output, events) = execute("rpc-race", "abort", RULES, "test");
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "tool_started_after_trigger")
            .count(),
        1
    );
}

#[test]
fn gate_blocks_new_admission_with_correlated_application_evidence() {
    let (output, events) = execute("gate-block", "block", RULES, "test");
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(commands(&events), ["get_commands", "prompt"]);
    let decisions: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "gate_decision")
        .collect();
    let applied: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "gate_applied")
        .collect();
    assert_eq!(
        decisions
            .iter()
            .map(|e| e["data"]["allowed"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [true, true, false]
    );
    assert_eq!(applied.len(), decisions.len());
    for (decision, application) in decisions.iter().zip(applied) {
        assert_eq!(
            application["data"]["decision_sequence"],
            decision["sequence"]
        );
        assert_eq!(application["data"]["allowed"], decision["data"]["allowed"]);
    }
    assert_eq!(
        events.iter().filter(|e| e["kind"] == "agent_event").count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "tool_blocked_result")
            .count(),
        1
    );
    let closed = events.iter().find(|e| e["kind"] == "gate_closed").unwrap();
    let trigger = events
        .iter()
        .find(|e| e["kind"] == "rule_triggered")
        .unwrap();
    assert_eq!(closed["data"]["trigger_sequence"], trigger["sequence"]);
    assert_eq!(
        decisions[2]["data"]["trigger_sequence"],
        trigger["sequence"]
    );
    let ready = events
        .iter()
        .position(|e| e["kind"] == "gate_ready")
        .unwrap();
    let prompt = events
        .iter()
        .position(|e| e["kind"] == "rpc_command_requested" && e["data"]["command"] == "prompt")
        .unwrap();
    assert!(ready < prompt);
    let finished = &events.last().unwrap()["data"];
    assert_eq!(finished["control_outcome"], "blocked");
    assert_eq!(finished["blocked_tool_count"], 1);
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["forced_cleanup"], false);
}

#[test]
fn gate_record_allows_tools_and_closure_alone_is_not_reported_as_a_block() {
    for (scenario, action, exit, outcome, results) in [
        ("gate-record", "record", 0, "completed", 3),
        ("gate-no-next", "block", 3, "gate_closed", 2),
    ] {
        let (output, events) = execute(scenario, action, RULES, "test");
        assert_eq!(output.status.code(), Some(exit), "{scenario}");
        assert_eq!(events.last().unwrap()["data"]["control_outcome"], outcome);
        assert_eq!(events.last().unwrap()["data"]["blocked_tool_count"], 0);
        assert_eq!(
            events.iter().filter(|e| e["kind"] == "agent_event").count(),
            results
        );
        assert!(
            events
                .iter()
                .filter(|e| e["kind"] == "gate_decision")
                .all(|e| e["data"]["allowed"] == true)
        );
    }
}

#[test]
fn missing_gate_extension_prevents_the_business_prompt() {
    let (output, events) = execute("gate-missing", "block", RULES, "test");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(commands(&events), ["get_commands"]);
    assert!(
        !events
            .iter()
            .any(|e| e["kind"] == "gate_ready" || e["kind"] == "gate_decision")
    );
    assert_eq!(events.last().unwrap()["data"]["control_outcome"], "failed");
}

#[test]
fn invalid_gate_correlations_and_unconfirmed_decisions_fail_the_run() {
    for scenario in [
        "gate-no-decision",
        "gate-preflight-error",
        "gate-unconfirmed",
        "gate-mismatch",
        "gate-deny-success",
        "gate-repeat",
        "gate-repeat-request",
    ] {
        let (output, events) = execute(scenario, "block", RULES, "test");
        assert_eq!(output.status.code(), Some(2), "{scenario}");
        assert_eq!(
            events.last().unwrap()["data"]["control_outcome"],
            "failed",
            "{scenario}"
        );
    }
}

#[test]
fn unrelated_extension_dialog_is_never_automatically_approved() {
    let (output, events) = execute("gate-foreign-dialog", "block", RULES, "test");
    assert_eq!(output.status.code(), Some(2));
    assert!(!events.iter().any(|e| e["kind"] == "gate_decision"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["kind"] == "rpc_command_written")
            .count(),
        2
    );
    assert_eq!(events.last().unwrap()["data"]["control_outcome"], "failed");
}

#[test]
fn block_requires_gate_and_gate_cannot_be_combined_with_abort() {
    for (action, gate, message) in [
        ("block", false, "requires --tool-gate"),
        ("abort", true, "record or block"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_inhibitor-lab"))
            .args([
                "run",
                "--format",
                "pi-rpc",
                "--rules",
                "unused.json",
                "--prompt",
                "test",
                "--on-trigger",
                action,
            ])
            .args(if gate { &["--tool-gate"][..] } else { &[][..] })
            .arg("--")
            .arg(env!("CARGO_BIN_EXE_mock-agent"))
            .arg("repeat")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}
