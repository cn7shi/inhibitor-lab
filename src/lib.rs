use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, BufRead};

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
    contains: String,
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
    pub matched_lines: u64,
    pub threshold: u64,
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
            if rule.contains.is_empty() || rule.threshold == 0 {
                return Err(format!(
                    "rule {} requires nonempty contains and threshold >= 1",
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
                || !line.contains(&state.rule.contains)
            {
                continue;
            }
            state.matches += 1;
            if state.matches >= state.rule.threshold {
                state.fired = true;
                triggers.push(Trigger {
                    rule_id: state.rule.id.clone(),
                    matched_lines: state.matches,
                    threshold: state.rule.threshold,
                });
            }
        }
        triggers
    }
}

/// Retain at most `limit` bytes per line, draining the rest without unbounded allocation.
/// Truncated lines are reported to the caller, which must not match them as complete evidence.
pub fn read_line_bounded(
    reader: &mut impl BufRead,
    limit: usize,
) -> io::Result<Option<(String, bool)>> {
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
    Ok(Some((
        String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    )))
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
        assert_eq!(engine.observe(Source::Stderr, "ERROR")[0].matched_lines, 2);
        assert!(engine.observe(Source::Stderr, "ERROR").is_empty());
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for input in [
            r#"{"rules":[{"id":"a","contains":"x","threshold":0}]}"#,
            r#"{"rules":[{"id":"a","contains":"","threshold":1}]}"#,
            r#"{"rules":[{"id":"a","contains":"x","threshold":1},{"id":"a","contains":"y","threshold":1}]}"#,
            r#"{"rules":[{"id":"a","contains":"x","threshold":1,"typo":true}]}"#,
        ] {
            assert!(RuleEngine::from_json(input).is_err());
        }
    }

    #[test]
    fn oversized_line_is_drained_and_last_unterminated_line_is_kept() {
        let mut reader = Cursor::new(b"123456789\nnext\r\nlast");
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(("12345".into(), true))
        );
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(("next".into(), false))
        );
        assert_eq!(
            read_line_bounded(&mut reader, 5).unwrap(),
            Some(("last".into(), false))
        );
        assert_eq!(read_line_bounded(&mut reader, 5).unwrap(), None);
    }
}
