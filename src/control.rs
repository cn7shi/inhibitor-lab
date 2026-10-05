use crate::gate::{self, ToolGate};
use crate::{Capture, Journal};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ChildStdin;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Record,
    Abort,
    Block,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Abort => "abort",
            Self::Block => "block",
        }
    }
}

pub struct RpcControl {
    writer: Option<SyncSender<Value>>,
    pending: HashMap<String, String>,
    deadline: Instant,
    phase_deadline: Option<Instant>,
    grace: Duration,
    pub action: Action,
    pub gate: Option<ToolGate>,
    pending_prompt: Option<String>,
    pub trigger_sequence: Option<u64>,
    first_trigger_sequence: Option<u64>,
    pub confirmed: bool,
    pub closing: bool,
    prompt_accepted: bool,
    settled: bool,
}

impl RpcControl {
    pub fn new(
        mut stdin: ChildStdin,
        sender: SyncSender<Capture>,
        timeout: Duration,
        grace: Duration,
        action: Action,
        tool_gate: bool,
    ) -> Self {
        let (writer, commands) = sync_channel::<Value>(4);
        thread::spawn(move || {
            for command in commands {
                let result = (|| -> io::Result<()> {
                    serde_json::to_writer(&mut stdin, &command)?;
                    stdin.write_all(b"\n")?;
                    stdin.flush()
                })();
                let event = match result {
                    Ok(()) => Capture::CommandWritten(command["id"].clone()),
                    Err(error) => {
                        let _ = sender.send(Capture::InputError(error.to_string()));
                        return;
                    }
                };
                if sender.send(event).is_err() {
                    return;
                }
            }
            drop(stdin);
            let _ = sender.send(Capture::InputClosed);
        });
        Self {
            writer: Some(writer),
            pending: HashMap::new(),
            deadline: Instant::now() + timeout,
            phase_deadline: None,
            grace,
            action,
            gate: tool_gate.then(ToolGate::default),
            pending_prompt: None,
            trigger_sequence: None,
            first_trigger_sequence: None,
            confirmed: false,
            closing: false,
            prompt_accepted: false,
            settled: false,
        }
    }

    fn send(
        &mut self,
        command: &str,
        message: Option<&str>,
        journal: &mut Journal,
    ) -> io::Result<()> {
        let id = format!("inhibitor-{command}");
        let mut value = json!({"id": id, "type": command});
        if let Some(message) = message {
            value["message"] = json!(message);
        }
        journal.emit(
            "rpc_command_requested",
            json!({"id": id, "command": command, "trigger_sequence": self.trigger_sequence}),
        )?;
        self.writer
            .as_ref()
            .ok_or_else(|| io::Error::other("RPC input already closed"))?
            .try_send(value)
            .map_err(io::Error::other)?;
        self.pending.insert(id, command.to_owned());
        Ok(())
    }

    pub fn prompt(&mut self, message: &str, journal: &mut Journal) -> io::Result<()> {
        if self.gate.is_some() {
            self.pending_prompt = Some(message.to_owned());
            self.send("get_commands", None, journal)
        } else {
            self.send("prompt", Some(message), journal)
        }
    }

    pub fn trigger(&mut self, sequence: u64, journal: &mut Journal) -> io::Result<()> {
        self.first_trigger_sequence.get_or_insert(sequence);
        if self.action == Action::Block {
            return self
                .gate
                .as_mut()
                .ok_or_else(|| io::Error::other("block action requires tool gate"))?
                .close(sequence, journal);
        }
        if self.action != Action::Abort || self.trigger_sequence.is_some() {
            return Ok(());
        }
        if self.closing {
            return Err(io::Error::other("rule triggered after RPC shutdown began"));
        }
        self.trigger_sequence = Some(sequence);
        self.phase_deadline = Some(Instant::now() + self.grace);
        self.send("clear_queue", None, journal)
    }

    pub fn observe(&mut self, text: &str, sequence: u64, journal: &mut Journal) -> io::Result<()> {
        let value: Value = serde_json::from_str(text)?;
        if let Some(gate) = &mut self.gate {
            if let Some(response) = gate.observe(&value, sequence, journal)? {
                self.writer
                    .as_ref()
                    .ok_or_else(|| io::Error::other("gate response after RPC input closed"))?
                    .try_send(response)
                    .map_err(io::Error::other)?;
            }
        } else if value["type"] == "extension_ui_request" && value["title"] == gate::PROTOCOL {
            return Err(io::Error::other("gate extension requires --tool-gate"));
        }
        match value["type"].as_str() {
            Some("agent_start" | "tool_execution_start") => {
                if self.gate.as_ref().is_some_and(|gate| !gate.ready) {
                    return Err(io::Error::other("Pi started work before gate readiness"));
                }
                if self.closing {
                    return Err(io::Error::other(
                        "Pi started work after idle/shutdown confirmation",
                    ));
                }
                self.settled = false;
                if value["type"] == "tool_execution_start" && self.first_trigger_sequence.is_some()
                {
                    journal.emit("tool_started_after_trigger", json!({"raw_event_sequence": sequence, "trigger_sequence": self.first_trigger_sequence, "tool_call_id": value["toolCallId"]}))?;
                }
            }
            Some("agent_settled") => {
                self.settled = true;
                journal.emit("rpc_settled", json!({"raw_event_sequence": sequence}))?;
            }
            Some("response") => {
                let Some(id) = value["id"].as_str() else {
                    return Ok(());
                };
                let Some(command) = self.pending.remove(id) else {
                    return Ok(());
                };
                journal.emit("rpc_response", json!({"id": id, "command": value["command"], "success": value["success"], "raw_event_sequence": sequence}))?;
                if value["command"] != command || value["success"] != true {
                    return Err(io::Error::other(format!(
                        "RPC {command} rejected or mismatched: {}",
                        value["error"]
                    )));
                }
                match command.as_str() {
                    "get_commands" => {
                        let present =
                            value["data"]["commands"]
                                .as_array()
                                .is_some_and(|commands| {
                                    commands.iter().any(|command| {
                                        command["name"] == gate::COMMAND
                                            && command["description"] == gate::PROTOCOL
                                            && command["source"] == "extension"
                                    })
                                });
                        if !present {
                            return Err(io::Error::other(
                                "required tool gate extension is missing or incompatible; no prompt sent",
                            ));
                        }
                        self.gate
                            .as_mut()
                            .ok_or_else(|| io::Error::other("unexpected gate readiness response"))?
                            .ready = true;
                        journal.emit(
                            "gate_ready",
                            json!({"protocol": gate::PROTOCOL, "raw_event_sequence": sequence}),
                        )?;
                        let message = self
                            .pending_prompt
                            .take()
                            .ok_or_else(|| io::Error::other("missing gated prompt"))?;
                        self.send("prompt", Some(&message), journal)?;
                    }
                    "prompt" => {
                        match value["data"]["disposition"].as_str() {
                            Some("started") => {}
                            Some("handled") => self.settled = true,
                            _ => {
                                return Err(io::Error::other(
                                    "single-prompt RPC requires started or handled disposition",
                                ));
                            }
                        }
                        self.prompt_accepted = true;
                    }
                    "clear_queue" => self.send("abort", None, journal)?,
                    "abort" => self.send("get_state", None, journal)?,
                    "get_state" => {
                        let state = &value["data"];
                        if state["isStreaming"] != false
                            || state["isCompacting"] != false
                            || state["pendingMessageCount"] != 0
                        {
                            return Err(io::Error::other(
                                "Pi has not confirmed an idle session with an empty queue",
                            ));
                        }
                        self.confirmed = true;
                        journal.emit("control_confirmed", json!({"action": "abort", "trigger_sequence": self.trigger_sequence, "raw_event_sequence": sequence, "evidence": "abort_response_and_idle_state"}))?;
                    }
                    _ => unreachable!("only locally created commands enter pending"),
                }
            }
            _ => {}
        }
        if !self.closing
            && self.prompt_accepted
            && self.pending.is_empty()
            && if self.trigger_sequence.is_some() {
                self.confirmed
            } else {
                self.settled
            }
        {
            if self.gate.as_ref().is_some_and(|gate| !gate.complete()) {
                return Err(io::Error::other(
                    "Pi settled with unconfirmed gate decisions",
                ));
            }
            journal.emit("rpc_shutdown_requested", json!({"reason": if self.confirmed { "abort_confirmed" } else { "agent_settled_or_prompt_handled" }}))?;
            self.writer.take();
            self.closing = true;
            self.phase_deadline = Some(Instant::now() + self.grace);
        }
        Ok(())
    }

    pub fn expired(&self) -> bool {
        let now = Instant::now();
        now >= self.deadline || self.phase_deadline.is_some_and(|deadline| now >= deadline)
    }

    pub fn blocked_result(&self, text: &str) -> io::Result<bool> {
        match &self.gate {
            Some(gate) => gate.blocked_result(&serde_json::from_str(text)?),
            None => Ok(false),
        }
    }
}
