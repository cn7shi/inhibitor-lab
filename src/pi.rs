use crate::AgentEvent;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResult {
    tool_call_id: String,
    tool_name: String,
    is_error: bool,
}

/// One adapter per process run. Only stdout protocol records are passed here.
#[derive(Default)]
pub struct PiAdapter {
    completed: HashMap<String, AgentEvent>,
}

impl PiAdapter {
    pub fn observe(&mut self, line: &str) -> Result<Option<AgentEvent>, String> {
        let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Pi record requires a string type")?;
        if kind != "tool_execution_end" {
            return Ok(None);
        }
        let result: ToolResult =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        if result.tool_call_id.trim().is_empty() || result.tool_name.trim().is_empty() {
            return Err("Pi tool result requires nonempty toolCallId and toolName".into());
        }
        let event = AgentEvent::ToolCompleted {
            tool_call_id: result.tool_call_id.clone(),
            tool_name: result.tool_name,
            failed: result.is_error,
        };
        if let Some(previous) = self.completed.get(&result.tool_call_id) {
            return if previous == &event {
                Ok(None)
            } else {
                Err(format!(
                    "conflicting tool result for {}",
                    result.tool_call_id
                ))
            };
        }
        self.completed.insert(result.tool_call_id, event.clone());
        Ok(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_tool_results_are_normalized_and_duplicates_are_ignored() {
        let mut adapter = PiAdapter::default();
        for line in [
            r#"{"type":"message_end","message":{"text":"ERROR","isError":true}}"#,
            r#"{"type":"future_event","isError":true}"#,
        ] {
            assert_eq!(adapter.observe(line).unwrap(), None);
        }
        let line = r#"{"type":"tool_execution_end","toolCallId":"a","toolName":"read","isError":false,"result":{"content":[{"text":"ERROR"}]}}"#;
        assert!(matches!(
            adapter.observe(line).unwrap(),
            Some(AgentEvent::ToolCompleted { failed: false, .. })
        ));
        assert_eq!(adapter.observe(line).unwrap(), None);
        assert!(adapter.observe(&line.replace("false", "true")).is_err());
    }

    #[test]
    fn malformed_records_cannot_become_evidence_and_do_not_poison_later_records() {
        let mut adapter = PiAdapter::default();
        for line in [
            "not json",
            "{}",
            "[]",
            r#"{"type":"tool_execution_end","toolCallId":"a","toolName":"read"}"#,
            r#"{"type":"tool_execution_end","toolCallId":"a","toolName":"read","isError":"true"}"#,
        ] {
            assert!(adapter.observe(line).is_err());
        }
        assert!(adapter.observe(r#"{"type":"tool_execution_end","toolCallId":"a","toolName":"read","isError":true}"#).unwrap().is_some());
    }
}
