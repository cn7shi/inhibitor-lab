use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, BufRead};

pub mod pi;

/// Provider-independent facts. Failure is reported by the tool, not inferred from text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    ToolCompleted {
        tool_call_id: String,
        tool_name: String,
        failed: bool,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventFilter {
    kind: EventKind,
    failed: bool,
    tool_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventKind {
    ToolCompleted,
}

impl EventFilter {
    fn accepts(&self, event: &AgentEvent) -> bool {
        match (&self.kind, event) {
            (
                EventKind::ToolCompleted,
                AgentEvent::ToolCompleted {
                    tool_name, failed, ..
                },
            ) => {
                *failed == self.failed
                    && self.tool_name.as_ref().is_none_or(|name| name == tool_name)
            }
        }
    }
}

/// One observed stream. Stderr does not inherently mean failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceFilter {
    #[default]
    Any,
    Stdout,
    Stderr,
}

impl SourceFilter {
    fn accepts(self, source: Source) -> bool {
        matches!(
            (self, source),
            (Self::Any, _) | (Self::Stdout, Source::Stdout) | (Self::Stderr, Source::Stderr)
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    #[serde(default)]
    source: SourceFilter,
    contains: Option<String>,
    event: Option<EventFilter>,
    threshold: u64,
}

struct RuleState {
    rule: Rule,
    matches: u64,
    fired: bool,
}

#[derive(Debug, Serialize)]
pub struct Trigger {
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_events: Option<u64>,
    pub threshold: u64,
}

impl RuleState {
    fn record_match(&mut self) -> Option<Trigger> {
        self.matches += 1;
        if self.matches < self.rule.threshold {
            return None;
        }
        self.fired = true;
        Some(Trigger {
            rule_id: self.rule.id.clone(),
            matched_lines: self.rule.contains.as_ref().map(|_| self.matches),
            matched_events: self.rule.event.as_ref().map(|_| self.matches),
            threshold: self.rule.threshold,
        })
    }
}

pub struct RuleEngine {
    states: Vec<RuleState>,
}

impl RuleEngine {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let config: Config = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let mut ids = HashSet::new();
        for rule in &config.rules {
            if rule.id.trim().is_empty() || !ids.insert(rule.id.clone()) {
                return Err("rule ids must be nonempty and unique".into());
            }
            if rule.contains.is_some() == rule.event.is_some()
                || rule.contains.as_ref().is_some_and(String::is_empty)
                || rule.event.as_ref().is_some_and(|filter| {
                    filter
                        .tool_name
                        .as_ref()
                        .is_some_and(|name| name.trim().is_empty())
                })
                || rule.threshold == 0
            {
                return Err(format!(
                    "rule {} requires exactly one of nonempty contains or event, and threshold >= 1",
                    rule.id
                ));
            }
        }
        Ok(Self {
            states: config
                .rules
                .into_iter()
                .map(|rule| RuleState {
                    rule,
                    matches: 0,
                    fired: false,
                })
                .collect(),
        })
    }

    /// A matching line counts once. A rule fires once per process run.
    pub fn observe(&mut self, source: Source, line: &str) -> Vec<Trigger> {
        let mut triggers = Vec::new();
        for state in &mut self.states {
            if state.fired
                || !state.rule.source.accepts(source)
                || !state
                    .rule
                    .contains
                    .as_ref()
                    .is_some_and(|text| line.contains(text))
            {
                continue;
            }
            if let Some(trigger) = state.record_match() {
                triggers.push(trigger);
            }
        }
        triggers
    }

    pub fn requires_agent_events(&self) -> bool {
        self.states.iter().any(|state| state.rule.event.is_some())
    }

    pub fn observe_event(&mut self, source: Source, event: &AgentEvent) -> Vec<Trigger> {
        let mut triggers = Vec::new();
        for state in &mut self.states {
            if state.fired
                || !state.rule.source.accepts(source)
                || !state
                    .rule
                    .event
                    .as_ref()
                    .is_some_and(|filter| filter.accepts(event))
            {
                continue;
            }
            if let Some(trigger) = state.record_match() {
                triggers.push(trigger);
            }
        }
        triggers
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedLine {
    pub text: String,
    pub truncated: bool,
    pub invalid_utf8: bool,
}

/// Retain at most `limit` bytes per line, draining the rest without unbounded allocation.
/// Truncated lines are reported to the caller, which must not match them as complete evidence.
pub fn read_line_bounded(
    reader: &mut impl BufRead,
    limit: usize,
) -> io::Result<Option<CapturedLine>> {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut seen = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            if !seen {
                return Ok(None);
            }
            break;
        }
        seen = true;
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(bytes.len());
        let keep = content_len.min(limit.saturating_sub(retained.len()));
        retained.extend_from_slice(&bytes[..keep]);
        truncated |= keep < content_len;
        let consumed = content_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if retained.last() == Some(&b'\r') {
        retained.pop();
    }
    let text = String::from_utf8_lossy(&retained);
    let invalid_utf8 = matches!(&text, std::borrow::Cow::Owned(_));
    Ok(Some(CapturedLine {
        text: text.into_owned(),
        truncated,
        invalid_utf8,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn source_filter_counts_lines_and_fires_once() {
        let mut engine = RuleEngine::from_json(
            r#"{"rules":[{"id":"repeat","source":"stderr","contains":"ERROR","threshold":2}]}"#,
        )
        .unwrap();
        assert!(engine.observe(Source::Stdout, "ERROR").is_empty());
        assert!(engine.observe(Source::Stderr, "ERROR ERROR").is_empty());
        assert!(engine.observe(Source::Stderr, "error").is_empty());
        assert_eq!(
            engine.observe(Source::Stderr, "ERROR")[0].matched_lines,
            Some(2)
        );
        assert!(engine.observe(Source::Stderr, "ERROR").is_empty());
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for input in [
            r#"{"rules":[{"id":"a","contains":"x","threshold":0}]}"#,
            r#"{"rules":[{"id":"a","contains":"","threshold":1}]}"#,
            r#"{"rules":[{"id":"a","contains":"x","threshold":1},{"id":"a","contains":"y","threshold":1}]}"#,
            r#"{"rules":[{"id":"a","contains":"x","threshold":1,"typo":true}]}"#,
            r#"{"rules":[{"id":"a","threshold":1}]}"#,
            r#"{"rules":[{"id":"a","contains":"x","event":{"kind":"tool_completed","failed":true},"threshold":1}]}"#,
            r#"{"rules":[{"id":"a","event":{"kind":"tool_completed","failed":"true"},"threshold":1}]}"#,
        ] {
            assert!(RuleEngine::from_json(input).is_err());
        }
    }

    #[test]
    fn event_rule_counts_failures_only_and_keeps_text_rules_separate() {
        let mut engine = RuleEngine::from_json(
            r#"{"rules":[{"id":"failures","event":{"kind":"tool_completed","failed":true,"tool_name":"read"},"threshold":2}]}"#,
        ).unwrap();
        let event = |name: &str, failed| AgentEvent::ToolCompleted {
            tool_call_id: "call".into(),
            tool_name: name.into(),
            failed,
        };
        assert!(engine.observe(Source::Stdout, "ERROR").is_empty());
        assert!(
            engine
                .observe_event(Source::Stdout, &event("read", false))
                .is_empty()
        );
        assert!(
            engine
                .observe_event(Source::Stdout, &event("bash", true))
                .is_empty()
        );
        assert!(
            engine
                .observe_event(Source::Stdout, &event("read", true))
                .is_empty()
        );
        let triggers = engine.observe_event(Source::Stdout, &event("read", true));
        assert_eq!(triggers[0].matched_events, Some(2));
        assert_eq!(triggers[0].matched_lines, None);
        assert!(
            engine
                .observe_event(Source::Stdout, &event("read", true))
                .is_empty()
        );
    }

    #[test]
    fn oversized_line_is_drained_and_last_unterminated_line_is_kept() {
        let mut reader = Cursor::new(b"123456789\nnext\r\nlast");
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(CapturedLine {
                text: "12345".into(),
                truncated: true,
                invalid_utf8: false
            })
        );
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(CapturedLine {
                text: "next".into(),
                truncated: false,
                invalid_utf8: false
            })
        );
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(CapturedLine {
                text: "last".into(),
                truncated: false,
                invalid_utf8: false
            })
        );
        assert_eq!(read_line_bounded(&mut reader, 5).unwrap(), None);
    }

    #[test]
    fn valid_unicode_is_distinguished_from_replaced_invalid_bytes() {
        let mut valid = Cursor::new("\u{fffd}\u{2028}text\n".as_bytes());
        let line = read_line_bounded(&mut valid, 100).unwrap().unwrap();
        assert_eq!(line.text, "\u{fffd}\u{2028}text");
        assert!(!line.invalid_utf8);
        assert!(read_line_bounded(&mut valid, 100).unwrap().is_none());
        let mut invalid = Cursor::new(b"\xff\nnext\n");
        assert!(
            read_line_bounded(&mut invalid, 100)
                .unwrap()
                .unwrap()
                .invalid_utf8
        );
        assert!(
            !read_line_bounded(&mut invalid, 100)
                .unwrap()
                .unwrap()
                .invalid_utf8
        );
    }
}
