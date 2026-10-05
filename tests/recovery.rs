use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_inhibitor-lab");
const MOCK: &str = env!("CARGO_BIN_EXE_mock-agent");

fn expect_code(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
    original_parent: Vec<u8>,
    rules: PathBuf,
    config: PathBuf,
    plan: PathBuf,
    receipt: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "inhibitor-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("config")).unwrap();
        fs::create_dir(root.join("work")).unwrap();
        let parent = root.join("parent.jsonl");
        let rules = root.join("rules.json");
        let config = root.join("config/new-run.json");
        let plan = root.join("plan.json");
        let receipt = root.join("plan.json.confirmed.json");
        fs::write(&rules, include_str!("../examples/pi-rules.json")).unwrap();
        let output = Command::new(BIN)
            .args(["run", "--format", "pi-rpc", "--rules"])
            .arg(&rules)
            .arg("--out")
            .arg(&parent)
            .args([
                "--prompt",
                "fixture",
                "--tool-gate",
                "--on-trigger",
                "block",
                "--timeout-ms",
                "3000",
                "--control-timeout-ms",
                "500",
                "--",
                MOCK,
                "gate-block",
            ])
            .output()
            .unwrap();
        expect_code(&output, 3);
        // If recovery accidentally executes this configured command, it creates a visible marker log.
        let new_run = json!({"schema_version":1,"reason":"已检查失败原因，准备新任务","prompt":"新的隔离实验任务",
            "rules":"../rules.json", "cwd":"../work", "program":BIN,
            "args":["run","--rules",rules,"--out",root.join("accidental-execution.jsonl"),"--",MOCK,"normal"],
            "timeout_ms":30000,"control_timeout_ms":5000});
        fs::write(&config, serde_json::to_vec_pretty(&new_run).unwrap()).unwrap();
        let original_parent = fs::read(&parent).unwrap();
        Self {
            root,
            parent,
            original_parent,
            rules,
            config,
            plan,
            receipt,
        }
    }

    fn prepare(&self) -> Output {
        Command::new(BIN)
            .args(["recovery", "prepare", "--parent"])
            .arg(&self.parent)
            .arg("--config")
            .arg(&self.config)
            .arg("--out")
            .arg(&self.plan)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn prepared(&self) -> Value {
        expect_code(&self.prepare(), 0);
        read_json(&self.plan)
    }

    fn token(&self) -> String {
        format!(
            "CONFIRM {}\n",
            read_json(&self.plan)["recovery_id"].as_str().unwrap()
        )
    }

    fn confirm(&self, answer: &[u8]) -> Output {
        let mut child = confirmation(&self.plan).spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(answer);
        child.wait_with_output().unwrap()
    }

    fn unchanged(&self) {
        assert_eq!(fs::read(&self.parent).unwrap(), self.original_parent);
        assert!(!self.root.join("accidental-execution.jsonl").exists());
        assert_eq!(fs::read_dir(self.root.join("work")).unwrap().count(), 0);
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
                .starts_with("inhibitor-recovery-")
        );
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn update_json(path: &Path, edit: impl FnOnce(&mut Value)) {
    let mut value = read_json(path);
    edit(&mut value);
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn confirmation(path: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(["recovery", "confirm", "--plan"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn start_command(path: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(["recovery", "start", "--plan"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

impl Fixture {
    fn start_setup(&self, scenario: &str) -> Value {
        update_json(&self.config, |value| {
            value["program"] = json!(MOCK);
            value["args"] = json!([scenario, "mark-start"]);
            value["timeout_ms"] = json!(3000);
            value["control_timeout_ms"] = json!(500);
        });
        let plan = self.prepared();
        expect_code(&self.confirm(self.token().as_bytes()), 0);
        plan
    }

    fn start(&self) -> Output {
        start_command(&self.plan).output().unwrap()
    }

    fn sidecar(&self, suffix: &str) -> PathBuf {
        self.root.join(format!("plan.json{suffix}"))
    }

    fn child_events(&self) -> Vec<Value> {
        fs::read_to_string(self.sidecar(".run.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn inspect_child(&self) -> Output {
        Command::new(BIN)
            .args(["inspect", "--log"])
            .arg(self.sidecar(".run.jsonl"))
            .args(["--format", "json"])
            .output()
            .unwrap()
    }

    fn no_start(&self) {
        assert!(!self.sidecar(".start.json").exists());
        assert!(!self.sidecar(".run.jsonl").exists());
        assert!(!self.sidecar(".result.json").exists());
        assert!(!self.root.join("work/start-markers.jsonl").exists());
    }
}

#[test]
fn confirmed_start_links_real_child_resets_counts_and_can_block_again() {
    let fixture = Fixture::new();
    let plan = fixture.start_setup("gate-block");
    let receipt = fs::read(&fixture.receipt).unwrap();
    let plan_bytes = fs::read(&fixture.plan).unwrap();
    expect_code(&fixture.start(), 3);
    let events = fixture.child_events();
    assert_eq!(events[0]["kind"], "run_started");
    assert!(events[0]["data"]["pid"].as_u64().unwrap() > 0);
    assert_eq!(events[1]["kind"], "run_linked");
    assert_eq!(
        events[1]["data"]["parent_run_id"],
        plan["inputs"]["parent"]["run_id"]
    );
    assert!(events.iter().all(|e| e["run_id"] == plan["planned_run_id"]));
    let results: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "agent_event")
        .collect();
    assert_eq!(results.len(), 2);
    let trigger = events
        .iter()
        .find(|e| e["kind"] == "rule_triggered")
        .unwrap();
    assert_eq!(
        trigger["data"]["matched_event_sequence"],
        results[1]["sequence"]
    );
    let decisions: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "gate_decision")
        .collect();
    assert_eq!(decisions.len(), 3);
    assert_eq!(decisions[0]["data"]["allowed"], true);
    assert_eq!(decisions[1]["data"]["allowed"], true);
    assert_eq!(decisions[2]["data"]["allowed"], false);
    let summary = read_json(&fixture.sidecar(".result.json"));
    assert_eq!(summary["status"], "run_finished");
    assert_eq!(summary["launcher_exit_code"], 3);
    assert_eq!(summary["finish"]["control_outcome"], "blocked");
    assert_eq!(summary["finish"]["trigger_count"], 1);
    assert_eq!(fs::read(&fixture.parent).unwrap(), fixture.original_parent);
    assert_eq!(fs::read(&fixture.plan).unwrap(), plan_bytes);
    assert_eq!(fs::read(&fixture.receipt).unwrap(), receipt);
    let inspected = fixture.inspect_child();
    expect_code(&inspected, 0);
    let report: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(report["linkage"], events[1]["data"]);
    assert_eq!(report["eligible_for_manual_restart"], true);
    // A new explicit plan can use this child as its parent; this is not an automatic retry.
    let next = Command::new(BIN)
        .args(["recovery", "prepare", "--parent"])
        .arg(fixture.sidecar(".run.jsonl"))
        .arg("--config")
        .arg(&fixture.config)
        .arg("--out")
        .arg(fixture.root.join("next.json"))
        .output()
        .unwrap();
    expect_code(&next, 0);
    assert_eq!(
        read_json(&fixture.root.join("next.json"))["inputs"]["parent"]["run_id"],
        plan["planned_run_id"]
    );
}

#[test]
fn successful_start_uses_selected_cwd_and_preserves_all_records_on_replay() {
    let fixture = Fixture::new();
    fixture.start_setup("gate-success");
    expect_code(&fixture.start(), 0);
    let result = read_json(&fixture.sidecar(".result.json"));
    assert_eq!(result["finish"]["control_outcome"], "completed");
    assert_eq!(result["finish"]["trigger_count"], 0);
    let marker = fixture.root.join("work/start-markers.jsonl");
    assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 1);
    let paths = [
        fixture.plan.clone(),
        fixture.receipt.clone(),
        fixture.parent.clone(),
        fixture.sidecar(".start.json"),
        fixture.sidecar(".run.jsonl"),
        fixture.sidecar(".result.json"),
        marker,
    ];
    let before: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    expect_code(&fixture.start(), 2);
    assert_eq!(
        paths
            .iter()
            .map(|p| fs::read(p).unwrap())
            .collect::<Vec<_>>(),
        before
    );
    expect_code(&fixture.inspect_child(), 0);
}

#[test]
fn two_start_processes_consume_confirmation_only_once() {
    let fixture = Fixture::new();
    fixture.start_setup("gate-success");
    let first = start_command(&fixture.plan).spawn().unwrap();
    let second = start_command(&fixture.plan).spawn().unwrap();
    let mut codes = [
        first.wait_with_output().unwrap().status.code().unwrap(),
        second.wait_with_output().unwrap().status.code().unwrap(),
    ];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    assert_eq!(
        fs::read_to_string(fixture.root.join("work/start-markers.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        read_json(&fixture.sidecar(".result.json"))["status"],
        "run_finished"
    );
}

#[test]
fn unconfirmed_cancelled_or_changed_inputs_never_start() {
    for changed in [
        "missing",
        "cancelled",
        "parent",
        "config",
        "rules",
        "plan",
        "confirmation",
        "extra-field",
        "partial",
    ] {
        let fixture = Fixture::new();
        fixture.start_setup("gate-success");
        match changed {
            "missing" | "cancelled" => {
                fs::remove_file(&fixture.receipt).unwrap();
                if changed == "cancelled" {
                    expect_code(&fixture.confirm(b"no\n"), 3);
                }
            }
            "parent" => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&fixture.parent)
                    .unwrap()
                    .write_all(b"\n")
                    .unwrap();
            }
            "config" => update_json(&fixture.config, |v| v["prompt"] = json!("unreviewed")),
            "rules" => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&fixture.rules)
                    .unwrap()
                    .write_all(b" ")
                    .unwrap();
            }
            "plan" => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&fixture.plan)
                    .unwrap()
                    .write_all(b" ")
                    .unwrap();
            }
            "confirmation" => update_json(&fixture.receipt, |v| {
                v["rules_sha256"] = json!("f".repeat(64))
            }),
            "extra-field" => update_json(&fixture.receipt, |v| v["extra"] = json!(true)),
            "partial" => fs::write(&fixture.receipt, b"{").unwrap(),
            _ => unreachable!(),
        }
        expect_code(&fixture.start(), 2);
        fixture.no_start();
    }
}

#[test]
fn existing_start_output_or_result_including_partial_records_prevent_execution() {
    for suffix in [".start.json", ".run.jsonl", ".result.json"] {
        let fixture = Fixture::new();
        fixture.start_setup("gate-success");
        let path = fixture.sidecar(suffix);
        fs::write(&path, b"{").unwrap();
        let output = fixture.start();
        expect_code(&output, 2);
        if suffix == ".start.json" {
            assert!(String::from_utf8_lossy(&output.stderr).contains("结果未知"));
        }
        assert_eq!(fs::read(path).unwrap(), b"{");
        assert!(!fixture.root.join("work/start-markers.jsonl").exists());
    }
}

#[test]
fn spawn_failure_is_consumed_and_control_failure_is_not_success() {
    for failure in ["spawn", "control"] {
        let fixture = Fixture::new();
        if failure == "spawn" {
            let program = fixture.root.join("not-executable.txt");
            fs::write(&program, b"not a program").unwrap();
            update_json(&fixture.config, |v| v["program"] = json!(program));
            fixture.prepared();
            expect_code(&fixture.confirm(fixture.token().as_bytes()), 0);
        } else {
            fixture.start_setup("gate-missing");
        }
        expect_code(&fixture.start(), 2);
        let result = read_json(&fixture.sidecar(".result.json"));
        assert_eq!(result["launcher_exit_code"], 2);
        if failure == "spawn" {
            assert_eq!(result["status"], "spawn_failed");
            assert_eq!(fixture.child_events()[0]["kind"], "spawn_failed");
            assert!(result["pid"].is_null());
        } else {
            assert_eq!(result["status"], "run_finished");
            assert_eq!(result["finish"]["control_outcome"], "failed");
        }
        let before = fs::read(fixture.sidecar(".result.json")).unwrap();
        expect_code(&fixture.start(), 2);
        assert_eq!(fs::read(fixture.sidecar(".result.json")).unwrap(), before);
    }
}

#[test]
fn invalid_or_misplaced_lineage_is_rejected() {
    let fixture = Fixture::new();
    fixture.start_setup("gate-block");
    expect_code(&fixture.start(), 3);
    let original = fixture.child_events();
    for defect in ["id", "hash", "self-parent", "extra", "missing", "late"] {
        let mut events = original.clone();
        match defect {
            "id" => events[1]["data"]["recovery_id"] = json!("wrong"),
            "hash" => events[1]["data"]["plan_sha256"] = json!("bad"),
            "self-parent" => events[1]["data"]["parent_run_id"] = events[1]["run_id"].clone(),
            "extra" => events[1]["data"]["unknown"] = json!(true),
            "missing" => {
                events.remove(1);
            }
            "late" => events.swap(1, 2),
            _ => unreachable!(),
        }
        for (index, event) in events.iter_mut().enumerate() {
            event["sequence"] = json!(index + 1);
        }
        let text = events.iter().map(|e| format!("{e}\n")).collect::<String>();
        fs::write(fixture.sidecar(".run.jsonl"), text).unwrap();
        expect_code(&fixture.inspect_child(), 2);
    }
}

#[test]
fn broken_output_after_spawn_records_runner_failure_without_retry() {
    let fixture = Fixture::new();
    fixture.start_setup("gate-success");
    let mut process = start_command(&fixture.plan).spawn().unwrap();
    // Closing the actual reader makes the launcher's first forwarded RPC output fail.
    drop(process.stdout.take());
    expect_code(&process.wait_with_output().unwrap(), 2);
    let result = read_json(&fixture.sidecar(".result.json"));
    assert_eq!(result["status"], "runner_failed");
    assert!(result["pid"].as_u64().unwrap() > 0);
    assert!(result["finish"].is_null());
    assert!(result["error"].is_string());
    assert_ne!(
        fixture.child_events().last().unwrap()["kind"],
        "run_finished"
    );
    expect_code(&fixture.start(), 2);
    assert_eq!(
        fs::read_to_string(fixture.root.join("work/start-markers.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

// Wait for the actual confirmation prompt, without sleeps or production test hooks.
struct Waiting {
    child: Option<Child>,
    reader: Option<JoinHandle<Vec<u8>>>,
}

impl Waiting {
    fn new(path: &Path) -> Self {
        let mut child = confirmation(path).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (ready, waiting) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut line = String::new();
            while stdout.read_line(&mut line).unwrap() != 0 {
                bytes.extend_from_slice(line.as_bytes());
                if line.starts_with("输入 CONFIRM ") {
                    let _ = ready.send(());
                }
                line.clear();
            }
            bytes
        });
        let result = Self {
            child: Some(child),
            reader: Some(reader),
        };
        waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("confirmation prompt not reached");
        result
    }

    fn answer(&mut self, answer: &[u8]) {
        self.child
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .unwrap()
            .write_all(answer)
            .unwrap();
    }

    fn finish(mut self) -> Output {
        let mut output = self.child.take().unwrap().wait_with_output().unwrap();
        output.stdout = self.reader.take().unwrap().join().unwrap();
        output
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn plan_and_confirmation_bind_parent_new_inputs_and_reserved_child_without_starting() {
    let fixture = Fixture::new();
    let plan = fixture.prepared();
    assert_eq!(plan["kind"], "recovery_plan");
    assert_eq!(plan["policy"]["on_trigger"], "block");
    assert_eq!(plan["policy"]["tool_gate"], true);
    assert_eq!(
        plan["inputs"]["new_run"]["cwd"],
        json!(fs::canonicalize(fixture.root.join("work")).unwrap())
    );
    assert_eq!(plan["inputs"]["parent"]["confirmed_blocked_tools"], 1);
    let bytes = fs::read(&fixture.plan).unwrap();
    let output = fixture.confirm(fixture.token().as_bytes());
    expect_code(&output, 0);
    let displayed = String::from_utf8(output.stdout).unwrap();
    assert!(
        displayed.contains("已检查失败原因")
            && displayed.contains("rules_snapshot")
            && displayed.contains("新运行将重置")
    );
    let receipt = read_json(&fixture.receipt);
    assert_eq!(receipt["status"], "confirmed_not_started");
    assert_eq!(receipt["recovery_id"], plan["recovery_id"]);
    assert_eq!(receipt["planned_run_id"], plan["planned_run_id"]);
    assert_eq!(receipt["parent_run_id"], plan["inputs"]["parent"]["run_id"]);
    assert_eq!(receipt["parent_sha256"], plan["inputs"]["parent"]["sha256"]);
    assert_eq!(receipt["config_sha256"], plan["inputs"]["config_sha256"]);
    assert_eq!(receipt["rules_sha256"], plan["inputs"]["rules_sha256"]);
    assert_eq!(receipt["plan_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(fs::read(&fixture.plan).unwrap(), bytes);
    fixture.unchanged();
}

#[test]
fn cancellation_eof_wrong_confirmation_and_long_input_leave_no_receipt() {
    let fixture = Fixture::new();
    fixture.prepared();
    for answer in [
        vec![],
        b"yes\n".to_vec(),
        b"CONFIRM wrong-id\n".to_vec(),
        fixture.token().trim_end().as_bytes().to_vec(),
        vec![b'x'; 2048],
    ] {
        expect_code(&fixture.confirm(&answer), 3);
        assert!(!fixture.receipt.exists());
        fixture.unchanged();
    }
    // Cancellation does not consume the plan; explicit later confirmation is allowed.
    expect_code(&fixture.confirm(fixture.token().as_bytes()), 0);
}

#[test]
fn stale_sources_are_rejected_before_confirmation() {
    for source in ["parent", "config", "rules"] {
        let fixture = Fixture::new();
        fixture.prepared();
        let path = match source {
            "parent" => &fixture.parent,
            "config" => &fixture.config,
            _ => &fixture.rules,
        };
        let mut bytes = fs::read(path).unwrap();
        bytes.push(b' '); // Even a content-preserving JSON whitespace edit changes the reviewed bytes.
        fs::write(path, bytes).unwrap();
        expect_code(&fixture.confirm(fixture.token().as_bytes()), 2);
        assert!(!fixture.receipt.exists());
        assert!(!fixture.root.join("accidental-execution.jsonl").exists());
    }
}

#[test]
fn changes_while_the_user_is_reviewing_invalidate_the_exact_confirmation() {
    for source in ["parent", "config", "rules", "plan"] {
        let fixture = Fixture::new();
        fixture.prepared();
        let token = fixture.token();
        let mut pending = Waiting::new(&fixture.plan);
        let path = match source {
            "parent" => &fixture.parent,
            "config" => &fixture.config,
            "rules" => &fixture.rules,
            _ => &fixture.plan,
        };
        let mut bytes = fs::read(path).unwrap();
        bytes.push(b' ');
        fs::write(path, bytes).unwrap();
        pending.answer(token.as_bytes());
        expect_code(&pending.finish(), 2);
        assert!(!fixture.receipt.exists());
        assert!(!fixture.root.join("accidental-execution.jsonl").exists());
    }
}

#[test]
fn concurrent_confirmations_create_exactly_one_receipt_and_replay_fails() {
    let fixture = Fixture::new();
    fixture.prepared();
    let token = fixture.token();
    let mut first = Waiting::new(&fixture.plan);
    let mut second = Waiting::new(&fixture.plan);
    first.answer(token.as_bytes());
    second.answer(token.as_bytes());
    let mut codes = [
        first.finish().status.code().unwrap(),
        second.finish().status.code().unwrap(),
    ];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    let receipt = fs::read(&fixture.receipt).unwrap();
    expect_code(&fixture.confirm(token.as_bytes()), 2);
    assert_eq!(fs::read(&fixture.receipt).unwrap(), receipt);
    fixture.unchanged();
}

#[test]
fn copied_and_edited_plans_cannot_reuse_the_original_confirmation_identity() {
    let fixture = Fixture::new();
    fixture.prepared();
    let original = fs::read(&fixture.plan).unwrap();
    let copy = fixture.root.join("copy.json");
    fs::write(&copy, &original).unwrap();
    let output = confirmation(&copy).output().unwrap();
    expect_code(&output, 2);
    assert!(!fixture.root.join("copy.json.confirmed.json").exists());
    // Updating only the location must not let a copied plan reuse the same recovery ID.
    update_json(&copy, |plan| {
        plan["plan_path"] = json!(fs::canonicalize(&copy).unwrap())
    });
    let mut copied_confirmation = confirmation(&copy).spawn().unwrap();
    let _ = copied_confirmation
        .stdin
        .take()
        .unwrap()
        .write_all(fixture.token().as_bytes());
    expect_code(&copied_confirmation.wait_with_output().unwrap(), 2);
    for key in ["new_run", "rules_snapshot", "parent"] {
        fs::write(&fixture.plan, &original).unwrap();
        update_json(&fixture.plan, |plan| match key {
            "new_run" => plan["inputs"][key]["reason"] = json!("未在原配置中指定的原因"),
            "parent" => plan["inputs"][key]["run_id"] = json!("foreign-run"),
            _ => plan["inputs"][key]["rules"][0]["threshold"] = json!(999),
        });
        expect_code(&fixture.confirm(fixture.token().as_bytes()), 2);
        assert!(!fixture.receipt.exists());
    }
}

#[test]
fn existing_plan_partial_receipt_and_output_collision_are_never_overwritten() {
    let fixture = Fixture::new();
    fixture.prepared();
    let plan = fs::read(&fixture.plan).unwrap();
    expect_code(&fixture.prepare(), 2);
    assert_eq!(fs::read(&fixture.plan).unwrap(), plan);
    fs::write(&fixture.receipt, b"{partial").unwrap();
    expect_code(&fixture.confirm(fixture.token().as_bytes()), 2);
    assert_eq!(fs::read(&fixture.receipt).unwrap(), b"{partial");
    let output = Command::new(BIN)
        .args(["recovery", "prepare", "--parent"])
        .arg(&fixture.parent)
        .arg("--config")
        .arg(&fixture.config)
        .arg("--out")
        .arg(&fixture.parent)
        .output()
        .unwrap();
    expect_code(&output, 2);
    fixture.unchanged();
}

#[test]
fn unsupported_parent_and_missing_finish_cannot_be_prepared() {
    for incomplete in [false, true] {
        let fixture = Fixture::new();
        let mut events: Vec<Value> = String::from_utf8(fixture.original_parent.clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if incomplete {
            events.pop();
        } else {
            events.last_mut().unwrap()["data"]["control_outcome"] = json!("completed");
        }
        let text: String = events.iter().map(|e| format!("{e}\n")).collect();
        fs::write(&fixture.parent, text).unwrap();
        expect_code(&fixture.prepare(), 2);
        assert!(!fixture.plan.exists());
    }
}

#[test]
fn invalid_configuration_rules_and_paths_fail_before_any_plan_is_created() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.config).unwrap();
    for (key, value) in [
        ("schema_version", json!(9)),
        ("reason", json!(" ")),
        ("prompt", json!("")),
        ("timeout_ms", json!(0)),
        ("control_timeout_ms", json!(86400001)),
        ("program", json!("missing.exe")),
        ("cwd", json!("../rules.json")),
        ("args", json!(["bad\u{0}argument"])),
        ("env", json!({"KEY":"not-allowed"})),
    ] {
        fs::write(&fixture.config, &original).unwrap();
        update_json(&fixture.config, |config| config[key] = value);
        expect_code(&fixture.prepare(), 2);
        assert!(!fixture.plan.exists());
    }
    fs::write(&fixture.config, original).unwrap();
    for rules in [
        r#"{"rules":[]}"#,
        r#"{"rules":[{"id":"bad","threshold":0,"contains":"x"}]}"#,
        "broken",
    ] {
        fs::write(&fixture.rules, rules).unwrap();
        expect_code(&fixture.prepare(), 2);
        assert!(!fixture.plan.exists());
    }
    fixture.unchanged();
}

#[test]
fn oversized_inputs_and_unknown_cli_options_fail_without_new_files() {
    let fixture = Fixture::new();
    let config = fs::read(&fixture.config).unwrap();
    fs::File::create(&fixture.config)
        .unwrap()
        .set_len(1024 * 1024 + 1)
        .unwrap();
    expect_code(&fixture.prepare(), 2);
    assert!(!fixture.plan.exists());
    fs::write(&fixture.config, config).unwrap();
    fixture.prepared();
    fs::File::create(&fixture.plan)
        .unwrap()
        .set_len(2 * 1024 * 1024 + 1)
        .unwrap();
    expect_code(&fixture.confirm(b"yes\n"), 2);
    assert!(!fixture.receipt.exists());
    for args in [
        vec!["recovery"],
        vec!["recovery", "resume"],
        vec!["recovery", "prepare"],
        vec!["recovery", "confirm", "--plan", "none", "--yes"],
        vec!["recovery", "confirm", "--plan", "none", "--plan", "other"],
    ] {
        expect_code(&Command::new(BIN).args(args).output().unwrap(), 2);
    }
    fixture.unchanged();
}

#[test]
fn terminal_control_characters_in_reviewed_inputs_are_escaped() {
    let fixture = Fixture::new();
    update_json(&fixture.config, |config| {
        config["reason"] = json!("\u{1b}[2J\nspoofed output")
    });
    fixture.prepared();
    let output = fixture.confirm(b"cancel\n");
    expect_code(&output, 3);
    assert!(!output.stdout.contains(&0x1b));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\\u001b")
    );
    fixture.unchanged();
}
