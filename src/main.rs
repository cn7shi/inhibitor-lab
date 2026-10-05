use inhibitor_lab::{CapturedLine, RuleEngine, Source, Trigger, pi::PiAdapter, read_line_bounded};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod control;
mod gate;
use control::{Action, RpcControl};

const HELP: &str = r#"inhibitor-lab: capture process output and emit explicit rule triggers

Usage:
  inhibitor-lab run --rules <rules.json> [--format text|pi-json|pi-rpc] [--out <events.jsonl>] -- <program> [args...]

RPC options (pi-rpc only):
  --prompt <text>                 Required single prompt sent over stdin
  --on-trigger record|abort|block Default: record
  --tool-gate                     Require the explicit Pi tool-gate.js extension
  --timeout-ms <1..86400000>       Total deadline, default: 30000
  --control-timeout-ms <1..86400000> Control/shutdown deadline, default: 5000

Text and pi-json modes record triggers only. RPC abort is cooperative cancellation.
The block action requires --tool-gate. Gated runs support record or block, and check extension readiness before sending a prompt.
Exit 3 means a clean controlled stop (abort or gate closure); inspect control_outcome and blocked_tool_count for evidence.
Exit 2 means capture/adapter/control failure. Previously admitted concurrent tools are not revoked by gate closure.
Logs default to runs/<run-id>.jsonl. Existing output files are never overwritten."#;

struct Options {
    rules: PathBuf,
    output: Option<PathBuf>,
    command: Vec<OsString>,
    format: &'static str,
    prompt: Option<String>,
    action: Action,
    tool_gate: bool,
    timeout: Duration,
    control_timeout: Duration,
}

fn parse_options() -> Result<Option<Options>, String> {
    let mut args = env::args_os().skip(1);
    match args.next().as_deref() {
        None => return Ok(None),
        Some(value) if value == "--help" || value == "-h" => return Ok(None),
        Some(value) if value == "run" => {}
        _ => return Err("expected the run command; use --help".into()),
    }
    let mut rules = None;
    let mut output = None;
    let mut format = None;
    let mut prompt = None;
    let mut action = Action::Record;
    let mut tool_gate = false;
    let mut timeout = Duration::from_secs(30);
    let mut control_timeout = Duration::from_secs(5);
    let mut rpc_options = HashSet::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            let command: Vec<_> = args.collect();
            if command.is_empty() {
                return Err("a program is required after --".into());
            }
            let format = format.unwrap_or("text");
            if format == "pi-rpc" {
                if prompt
                    .as_ref()
                    .is_none_or(|text: &String| text.trim().is_empty())
                {
                    return Err("--format pi-rpc requires a nonempty --prompt".into());
                }
                if action == Action::Block && !tool_gate {
                    return Err("--on-trigger block requires --tool-gate".into());
                }
                if tool_gate && action == Action::Abort {
                    return Err("--tool-gate currently supports record or block actions".into());
                }
            } else if !rpc_options.is_empty() {
                return Err(
                    "--prompt, --on-trigger, --tool-gate and deadline options require --format pi-rpc".into(),
                );
            }
            return Ok(Some(Options {
                rules: rules.ok_or("--rules is required")?,
                output,
                command,
                format,
                prompt,
                action,
                tool_gate,
                timeout,
                control_timeout,
            }));
        }
        if arg == "--tool-gate" {
            if !rpc_options.insert(arg) {
                return Err("duplicate option: --tool-gate".into());
            }
            tool_gate = true;
            continue;
        }
        if arg == "--format" {
            if format.is_some() {
                return Err("duplicate option: --format".into());
            }
            format = Some(match args.next().as_deref() {
                Some(value) if value == "text" => "text",
                Some(value) if value == "pi-json" => "pi-json",
                Some(value) if value == "pi-rpc" => "pi-rpc",
                _ => return Err("--format requires text, pi-json or pi-rpc".into()),
            });
            continue;
        }
        if [
            "--prompt",
            "--on-trigger",
            "--timeout-ms",
            "--control-timeout-ms",
        ]
        .iter()
        .any(|key| arg == *key)
        {
            if !rpc_options.insert(arg.clone()) {
                return Err(format!("duplicate option: {}", arg.to_string_lossy()));
            }
            let value = args
                .next()
                .ok_or("missing option value")?
                .into_string()
                .map_err(|_| "RPC option must be UTF-8")?;
            match arg.to_str().unwrap_or_default() {
                "--prompt" => prompt = Some(value),
                "--on-trigger" => {
                    action = match value.as_str() {
                        "record" => Action::Record,
                        "abort" => Action::Abort,
                        "block" => Action::Block,
                        _ => return Err("--on-trigger requires record, abort or block".into()),
                    }
                }
                _ => {
                    let ms: u64 = value
                        .parse()
                        .map_err(|_| "deadline must be an integer in 1..86400000 ms")?;
                    if !(1..=86_400_000).contains(&ms) {
                        return Err("deadline must be in 1..86400000 ms".into());
                    }
                    if arg == "--timeout-ms" {
                        timeout = Duration::from_millis(ms);
                    } else {
                        control_timeout = Duration::from_millis(ms);
                    }
                }
            }
            continue;
        }
        let target = if arg == "--rules" {
            &mut rules
        } else if arg == "--out" {
            &mut output
        } else {
            return Err(format!("unknown option: {}", arg.to_string_lossy()));
        };
        if target.is_some() {
            return Err(format!("duplicate option: {}", arg.to_string_lossy()));
        }
        *target = Some(PathBuf::from(args.next().ok_or("missing option value")?));
    }
    Err("use -- to separate the program and its arguments".into())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

struct Journal {
    file: File,
    run_id: String,
    sequence: u64,
}

impl Journal {
    fn emit(&mut self, kind: &str, data: Value) -> io::Result<u64> {
        self.sequence += 1;
        let event = json!({
            "schema_version": 1,
            "run_id": self.run_id,
            "sequence": self.sequence,
            "at_unix_ms": now_ms(),
            "kind": kind,
            "data": data
        });
        serde_json::to_writer(&mut self.file, &event)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(self.sequence)
    }
}

fn record_triggers(
    journal: &mut Journal,
    triggers: Vec<Trigger>,
    sequence: u64,
    control: &mut Option<RpcControl>,
    control_failure: &mut Option<String>,
) -> io::Result<usize> {
    let count = triggers.len();
    for trigger in triggers {
        let action = control
            .as_ref()
            .map_or("record", |control| control.action.label());
        let trigger_sequence = journal.emit(
            "rule_triggered",
            json!({"trigger": trigger, "matched_event_sequence": sequence, "action": action}),
        )?;
        if let Some(control) = control
            && let Err(error) = control.trigger(trigger_sequence, journal)
        {
            *control_failure = Some(error.to_string());
        }
        eprintln!(
            "[inhibitor-lab] rule triggered: {} ({} matches)",
            trigger.rule_id,
            trigger
                .matched_events
                .or(trigger.matched_lines)
                .unwrap_or_default()
        );
    }
    Ok(count)
}

enum Capture {
    Line(Source, CapturedLine),
    Error(Source, String),
    Closed,
    CommandWritten(Value),
    InputError(String),
    InputClosed,
}

fn capture(reader: impl Read, source: Source, sender: SyncSender<Capture>) {
    let mut reader = BufReader::new(reader);
    loop {
        match read_line_bounded(&mut reader, 64 * 1024) {
            Ok(Some(line)) => {
                if sender.send(Capture::Line(source, line)).is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(error) => {
                let _ = sender.send(Capture::Error(source, error.to_string()));
                break;
            }
        }
    }
    let _ = sender.send(Capture::Closed);
}

/// Clean up the direct child if the collector itself fails. Full process-tree control is future work.
struct ManagedChild(Child);

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn run(options: Options) -> Result<i32, Box<dyn std::error::Error>> {
    let pi_json = options.format != "text";
    let pi_rpc = options.format == "pi-rpc";
    let mut rules = RuleEngine::from_json(&fs::read_to_string(&options.rules)?)?;
    if rules.requires_agent_events() && !pi_json {
        return Err("event rules require --format pi-json or pi-rpc".into());
    }
    let mut adapter = pi_json.then(PiAdapter::default);
    let mut adapter_complete = true;
    let run_id = format!("{}-{}", now_ms(), std::process::id());
    let output = options
        .output
        .unwrap_or_else(|| PathBuf::from(format!("runs/{run_id}.jsonl")));
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut journal = Journal {
        file: OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?,
        run_id,
        sequence: 0,
    };
    eprintln!("[inhibitor-lab] event log: {}", output.display());
    let program = &options.command[0];
    let spawn_result = Command::new(program)
        .args(&options.command[1..])
        .stdin(if pi_rpc {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawn_result {
        Ok(child) => ManagedChild(child),
        Err(error) => {
            journal.emit(
                "spawn_failed",
                json!({"program": program.to_string_lossy(), "error": error.to_string()}),
            )?;
            return Err(error.into());
        }
    };
    journal.emit(
        "run_started",
        json!({"program": program.to_string_lossy(), "pid": child.0.id(), "format": options.format}),
    )?;
    let stdout = child.0.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.0.stderr.take().ok_or("missing stderr pipe")?;
    let (sender, receiver) = sync_channel(256);
    let mut control = if pi_rpc {
        Some(RpcControl::new(
            child.0.stdin.take().ok_or("missing stdin pipe")?,
            sender.clone(),
            options.timeout,
            options.control_timeout,
            options.action,
            options.tool_gate,
        ))
    } else {
        None
    };
    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || capture(stdout, Source::Stdout, stdout_sender));
    let stderr_thread = thread::spawn(move || capture(stderr, Source::Stderr, sender));
    let mut capture_failed = false;
    let mut trigger_count = 0;
    let mut control_failure = None;
    let mut forced_cleanup = false;
    let mut closed_streams = 0;
    let mut input_closed = false;
    if let Some(control) = &mut control {
        control.prompt(
            options.prompt.as_deref().ok_or("missing RPC prompt")?,
            &mut journal,
        )?;
    }
    loop {
        if control.as_ref().is_some_and(RpcControl::expired) && control_failure.is_none() {
            control_failure = Some("RPC run or control/shutdown deadline exceeded".into());
        }
        if let Some(error) = &control_failure {
            journal.emit(
                "control_failed",
                json!({"error": error, "cleanup_scope": "direct_child"}),
            )?;
            if child.0.try_wait()?.is_none() {
                child.0.kill()?;
            }
            forced_cleanup = true;
            capture_failed = true; // The deadline can interrupt draining; do not claim complete output.
            break;
        }
        if control
            .as_ref()
            .is_some_and(|control| !control.closing || input_closed)
            && closed_streams == 2
            && child.0.try_wait()?.is_some()
        {
            break;
        }
        let captured = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(captured) => captured,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match captured {
            Capture::Line(
                source,
                CapturedLine {
                    text,
                    truncated,
                    invalid_utf8,
                },
            ) => {
                let sequence = journal.emit(
                    "output",
                    json!({"source": source, "text": text, "truncated": truncated, "invalid_utf8": invalid_utf8}),
                )?;
                let display_text = if truncated {
                    format!("{text} [truncated]")
                } else {
                    text.clone()
                };
                match source {
                    Source::Stdout => writeln!(io::stdout().lock(), "{display_text}")?,
                    Source::Stderr => writeln!(io::stderr().lock(), "{display_text}")?,
                }
                if truncated {
                    if pi_json && source == Source::Stdout {
                        adapter_complete = false;
                        journal.emit("adapter_error", json!({"adapter": "pi", "raw_event_sequence": sequence, "error": "truncated Pi record; skipped"}))?;
                        if pi_rpc {
                            control_failure = Some("truncated RPC record".into());
                        }
                    }
                    continue;
                }
                if pi_json && source == Source::Stdout && invalid_utf8 {
                    adapter_complete = false;
                    journal.emit("adapter_error", json!({"adapter": "pi", "raw_event_sequence": sequence, "error": "invalid UTF-8 Pi record; skipped"}))?;
                    if pi_rpc {
                        control_failure = Some("invalid UTF-8 RPC record".into());
                    }
                    continue;
                }
                if source == Source::Stdout
                    && let Some(control) = &control
                {
                    match control.blocked_result(&text) {
                        Ok(true) => {
                            journal.emit("tool_blocked_result", json!({"raw_event_sequence": sequence, "counted_as_tool_failure": false}))?;
                            continue;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            control_failure = Some(error.to_string());
                            journal.emit(
                                "gate_error",
                                json!({"raw_event_sequence": sequence, "error": error.to_string()}),
                            )?;
                            continue;
                        }
                    }
                }
                trigger_count += record_triggers(
                    &mut journal,
                    rules.observe(source, &text),
                    sequence,
                    &mut control,
                    &mut control_failure,
                )?;
                if source == Source::Stdout
                    && let Some(adapter) = &mut adapter
                {
                    match adapter.observe(&text) {
                        Ok(Some(event)) => {
                            let normalized = journal.emit("agent_event", json!({"adapter": "pi", "raw_event_sequence": sequence, "event": event}))?;
                            trigger_count += record_triggers(
                                &mut journal,
                                rules.observe_event(source, &event),
                                normalized,
                                &mut control,
                                &mut control_failure,
                            )?;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            adapter_complete = false;
                            journal.emit("adapter_error", json!({"adapter": "pi", "raw_event_sequence": sequence, "error": error}))?;
                            if pi_rpc {
                                control_failure = Some("invalid RPC record".into());
                            }
                        }
                    }
                }
                if source == Source::Stdout
                    && control_failure.is_none()
                    && let Some(control) = &mut control
                    && let Err(error) = control.observe(&text, sequence, &mut journal)
                {
                    control_failure = Some(error.to_string());
                }
            }
            Capture::Error(source, error) => {
                capture_failed = true;
                journal.emit("capture_failed", json!({"source": source, "error": error}))?;
                if pi_rpc {
                    control_failure = Some("RPC output capture failed".into());
                }
            }
            Capture::Closed => closed_streams += 1,
            Capture::CommandWritten(id) => {
                journal.emit("rpc_command_written", json!({"id": id}))?;
            }
            Capture::InputClosed => {
                input_closed = true;
                journal.emit("rpc_input_closed", json!({}))?;
            }
            Capture::InputError(error) => {
                control_failure = Some(format!("RPC stdin failed: {error}"))
            }
        }
    }
    // Deadline failures detach readers: a descendant holding a pipe must not defeat the deadline.
    for reader in [stdout_thread, stderr_thread]
        .into_iter()
        .filter(|_| !forced_cleanup)
    {
        if reader.join().is_err() {
            capture_failed = true;
            journal.emit("capture_failed", json!({"error": "reader thread panicked"}))?;
        }
    }
    let status = child.0.wait()?;
    if let Some(control) = &control
        && control_failure.is_none()
    {
        if !control.closing {
            control_failure = Some("Pi exited before RPC completion was confirmed".into());
        } else if !status.success() || capture_failed || !adapter_complete {
            control_failure =
                Some("Pi shutdown or output collection failed after protocol completion".into());
        }
        if control_failure.is_some() {
            journal.emit("control_failed", json!({"error": control_failure}))?;
        }
    }
    let aborted = control.as_ref().is_some_and(|control| control.confirmed);
    let gate_closed = control
        .as_ref()
        .and_then(|control| control.gate.as_ref())
        .is_some_and(|gate| gate.closed_by.is_some());
    let blocked_count = control
        .as_ref()
        .and_then(|control| control.gate.as_ref())
        .map_or(0, |gate| gate.blocked_count);
    let control_outcome = if control.is_none() {
        "not_enabled"
    } else if control_failure.is_some() || !status.success() {
        "failed"
    } else if aborted {
        "aborted"
    } else if gate_closed {
        if blocked_count > 0 {
            "blocked"
        } else {
            "gate_closed"
        }
    } else {
        "completed"
    };
    journal.emit("run_finished", json!({"exit_code": status.code(), "success": status.success(), "trigger_count": trigger_count, "capture_complete": !capture_failed, "adapter_complete": adapter_complete, "control_outcome": control_outcome, "forced_cleanup": forced_cleanup, "blocked_tool_count": blocked_count}))?;
    if !adapter_complete {
        eprintln!("[inhibitor-lab] Pi input was incomplete or invalid; see adapter_error events");
    }
    Ok(
        if capture_failed || !adapter_complete || control_outcome == "failed" {
            2
        } else if aborted || gate_closed {
            3
        } else {
            status.code().unwrap_or(1)
        },
    )
}

fn main() {
    let result = match parse_options() {
        Ok(None) => {
            println!("{HELP}");
            return;
        }
        Ok(Some(options)) => run(options),
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("[inhibitor-lab] {error}");
            std::process::exit(2);
        }
    }
}
