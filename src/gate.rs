use crate::Journal;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io;

pub const PROTOCOL: &str = "inhibitor-tool-gate/v1";
pub const COMMAND: &str = "inhibitor-gate-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tool_call_id: String,
    tool_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Applied {
    protocol: String,
    tool_call_id: String,
    tool_name: String,
    allowed: bool,
}

struct Decision {
    tool_name: String,
    allowed: bool,
    applied: bool,
    sequence: u64,
}

#[derive(Default)]
pub struct ToolGate {
    pub ready: bool,
    pub closed_by: Option<u64>,
    pub blocked_count: u64,
    decisions: HashMap<String, Decision>,
    request_ids: HashSet<String>,
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512
}

impl ToolGate {
    pub fn close(&mut self, trigger: u64, journal: &mut Journal) -> io::Result<()> {
        if self.closed_by.is_none() {
            self.closed_by = Some(trigger);
            journal.emit(
                "gate_closed",
                json!({"trigger_sequence": trigger, "reset": "new_run_only"}),
            )?;
        }
        Ok(())
    }

    pub fn observe(
        &mut self,
        value: &Value,
        raw: u64,
        journal: &mut Journal,
    ) -> io::Result<Option<Value>> {
        if value["type"] != "extension_ui_request" {
            return Ok(None);
        }
        if value["method"] == "confirm" && value["title"] == PROTOCOL {
            if !self.ready {
                return Err(io::Error::other("tool gate request before readiness"));
            }
            let id = value["id"]
                .as_str()
                .filter(|id| valid_name(id))
                .ok_or_else(|| io::Error::other("invalid gate request ID"))?;
            let request: Request = serde_json::from_str(
                value["message"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("missing gate request body"))?,
            )?;
            if !valid_name(&request.tool_call_id)
                || !valid_name(&request.tool_name)
                || !self.request_ids.insert(id.to_owned())
                || self.decisions.contains_key(&request.tool_call_id)
            {
                return Err(io::Error::other("invalid or repeated gate tool/request ID"));
            }
            let allowed = self.closed_by.is_none();
            let sequence = journal.emit("gate_decision", json!({"request_id": id, "tool_call_id": request.tool_call_id, "tool_name": request.tool_name, "allowed": allowed, "raw_event_sequence": raw, "trigger_sequence": self.closed_by}))?;
            self.decisions.insert(
                request.tool_call_id,
                Decision {
                    tool_name: request.tool_name,
                    allowed,
                    applied: false,
                    sequence,
                },
            );
            return Ok(Some(
                json!({"type": "extension_ui_response", "id": id, "confirmed": allowed}),
            ));
        }
        if value["method"] == "notify" {
            let message = value["message"].as_str().unwrap_or_default();
            let Ok(body) = serde_json::from_str::<Value>(message) else {
                return Ok(None);
            };
            if body["protocol"] != PROTOCOL {
                return Ok(None);
            }
            let applied: Applied = serde_json::from_value(body)?;
            let Some(decision) = self.decisions.get_mut(&applied.tool_call_id) else {
                return Err(io::Error::other("gate applied an unknown decision"));
            };
            if applied.protocol != PROTOCOL
                || applied.allowed != decision.allowed
                || applied.tool_name != decision.tool_name
            {
                return Err(io::Error::other("gate applied a mismatched decision"));
            }
            if !decision.applied {
                decision.applied = true;
                if !decision.allowed {
                    self.blocked_count += 1;
                }
                journal.emit("gate_applied", json!({"tool_call_id": applied.tool_call_id, "tool_name": applied.tool_name, "allowed": applied.allowed, "decision_sequence": decision.sequence, "raw_event_sequence": raw}))?;
            }
        }
        Ok(None)
    }

    /// Denied calls produce synthetic errors in Pi; they are not executed-tool failures.
    pub fn blocked_result(&self, value: &Value) -> io::Result<bool> {
        if value["type"] != "tool_execution_end" {
            return Ok(false);
        }
        let decision = value["toolCallId"]
            .as_str()
            .and_then(|id| self.decisions.get(id))
            .ok_or_else(|| io::Error::other("tool result has no gate decision"))?;
        if !decision.applied || value["toolName"] != decision.tool_name {
            return Err(io::Error::other(
                "tool result has no matching applied gate decision",
            ));
        }
        if !decision.allowed && value["isError"] != true {
            return Err(io::Error::other(
                "a denied tool unexpectedly reported success",
            ));
        }
        Ok(!decision.allowed)
    }

    pub fn complete(&self) -> bool {
        self.ready && self.decisions.values().all(|decision| decision.applied)
    }
}
