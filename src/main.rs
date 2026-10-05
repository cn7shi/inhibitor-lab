use inhibitor_lab::{RuleEngine, Source, Trigger, pi::PiAdapter, read_line_bounded};
use serde_json::{Value, json};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "inhibitor-lab: capture process output and emit explicit rule triggers\n\nUsage:\n  inhibitor-lab run --rules <rules.json> [--format text|pi-json] [--out <events.jsonl>] -- <program> [args...]\n\nText rules match case-sensitive substrings. Pi JSON mode also normalizes tool results.\nTriggers are recorded only; the child keeps running. Malformed Pi stdout records cause exit 2 after capture.\nLogs default to runs/<run-id>.jsonl. Existing output files are never overwritten.";

struct Options {
    rules: PathBuf,
    output: Option<PathBuf>,
    command: Vec<OsString>,
    pi_json: bool,
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
    while let Some(arg) = args.next() {
        if arg == "--" {
            let command: Vec<_> = args.collect();
            if command.is_empty() {
                return Err("a program is required after --".into());
            }
            return Ok(Some(Options {
                rules: rules.ok_or("--rules is required")?,
                output,
                command,
                pi_json: format.unwrap_or(false),
            }));
        }
        if arg == "--format" {
            if format.is_some() {
                return Err("duplicate option: --format".into());
            }
            format = Some(match args.next().as_deref() {
                Some(value) if value == "text" => false,
                Some(value) if value == "pi-json" => true,
                _ => return Err("--format requires text or pi-json".into()),
            });
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
) -> io::Result<usize> {
    let count = triggers.len();
    for trigger in triggers {
        journal.emit(
            "rule_triggered",
            json!({"trigger": trigger, "matched_event_sequence": sequence, "action": "record"}),
        )?;
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
    Line(Source, String, bool),
    Error(Source, String),
}

fn capture(reader: impl Read, source: Source, sender: SyncSender<Capture>) {
    let mut reader = BufReader::new(reader);
    loop {
        match read_line_bounded(&mut reader, 64 * 1024) {
            Ok(Some((text, truncated))) => {
                if sender.send(Capture::Line(source, text, truncated)).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = sender.send(Capture::Error(source, error.to_string()));
                return;
            }
        }
    }
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
    let mut rules = RuleEngine::from_json(&fs::read_to_string(&options.rules)?)?;
    if rules.requires_agent_events() && !options.pi_json {
        return Err("event rules require --format pi-json".into());
    }
    let mut adapter = options.pi_json.then(PiAdapter::default);
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
        .stdin(Stdio::inherit())
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
        json!({"program": program.to_string_lossy(), "pid": child.0.id(), "format": if options.pi_json { "pi-json" } else { "text" }}),
    )?;
    let stdout = child.0.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.0.stderr.take().ok_or("missing stderr pipe")?;
    let (sender, receiver) = sync_channel(256);
    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || capture(stdout, Source::Stdout, stdout_sender));
    let stderr_thread = thread::spawn(move || capture(stderr, Source::Stderr, sender));
    let mut capture_failed = false;
    let mut trigger_count = 0;
    for captured in receiver {
        match captured {
            Capture::Line(source, text, truncated) => {
                let sequence = journal.emit(
                    "output",
                    json!({"source": source, "text": text, "truncated": truncated}),
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
                    if options.pi_json && source == Source::Stdout {
                        adapter_complete = false;
                        journal.emit("adapter_error", json!({"adapter": "pi", "raw_event_sequence": sequence, "error": "truncated Pi record; skipped"}))?;
                    }
                    continue;
                }
                trigger_count +=
                    record_triggers(&mut journal, rules.observe(source, &text), sequence)?;
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
                            )?;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            adapter_complete = false;
                            journal.emit("adapter_error", json!({"adapter": "pi", "raw_event_sequence": sequence, "error": error}))?;
                        }
                    }
                }
            }
            Capture::Error(source, error) => {
                capture_failed = true;
                journal.emit("capture_failed", json!({"source": source, "error": error}))?;
            }
        }
    }
    for reader in [stdout_thread, stderr_thread] {
        if reader.join().is_err() {
            capture_failed = true;
            journal.emit("capture_failed", json!({"error": "reader thread panicked"}))?;
        }
    }
    let status = child.0.wait()?;
    journal.emit("run_finished", json!({"exit_code": status.code(), "success": status.success(), "trigger_count": trigger_count, "capture_complete": !capture_failed, "adapter_complete": adapter_complete}))?;
    if !adapter_complete {
        eprintln!("[inhibitor-lab] Pi input was incomplete or invalid; see adapter_error events");
    }
    Ok(if capture_failed || !adapter_complete {
        2
    } else {
        status.code().unwrap_or(1)
    })
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
