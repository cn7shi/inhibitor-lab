//! Read-only journal diagnosis. Eligibility is evidence consistency, never authorization.
use crate::gate::{COMMAND, PROTOCOL};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LINE: usize = 1024 * 1024;
const MAX_EVENTS: usize = 100_000;
type Check<T> = Result<T, String>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    schema_version: u64,
    run_id: String,
    sequence: u64,
    at_unix_ms: u64,
    kind: String,
    data: Value,
}

#[derive(Serialize)]
struct Report {
    report_version: u8,
    status: &'static str,
    run_id: Option<String>,
    format: Option<String>,
    linkage: Option<Value>,
    event_count: usize,
    triggers: Vec<Value>,
    gate_closed_sequence: Option<u64>,
    confirmed_blocked_tools: u64,
    finish: Option<Value>,
    eligible_for_manual_restart: bool,
    reasons: Vec<String>,
    limitations: [&'static str; 3],
}

impl Default for Report {
    fn default() -> Self {
        Self {
            report_version: 1,
            status: "invalid",
            run_id: None,
            format: None,
            linkage: None,
            event_count: 0,
            triggers: Vec::new(),
            gate_closed_sequence: None,
            confirmed_blocked_tools: 0,
            finish: None,
            eligible_for_manual_restart: false,
            reasons: Vec::new(),
            limitations: [
                "这里只核对日志内部证据，不认证作者；缺少旧规则快照，不能重算完整规则条件。",
                "已经批准的工具可能产生效果，需要另行核对；工具开始事件不能证明文件变化。",
                "满足条件不等于已获人工许可；关联只核对本日志字段，不认证外部记录；本命令不会启动程序。",
            ],
        }
    }
}

fn require(condition: bool, message: &str) -> Check<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string<'a>(data: &'a Value, key: &str) -> Check<&'a str> {
    data[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("缺少或无效的字符串字段 {key}"))
}

fn number(data: &Value, key: &str) -> Check<u64> {
    data[key]
        .as_u64()
        .ok_or_else(|| format!("缺少或无效的整数字段 {key}"))
}

fn boolean(data: &Value, key: &str) -> Check<bool> {
    data[key]
        .as_bool()
        .ok_or_else(|| format!("缺少或无效的布尔字段 {key}"))
}

fn earlier<'a>(events: &'a [Event], at: usize, data: &Value, key: &str) -> Check<&'a Event> {
    let seq = number(data, key)?;
    require(
        seq > 0 && seq <= at as u64,
        "事件引用必须指向本运行中已存在的更早记录",
    )?;
    Ok(&events[(seq - 1) as usize])
}

fn raw(events: &[Event], at: usize, data: &Value) -> Check<Value> {
    let event = earlier(events, at, data, "raw_event_sequence")?;
    require(
        event.kind == "output"
            && event.data["source"] == "stdout"
            && event.data["truncated"] == false
            && event.data["invalid_utf8"] == false,
        "原始证据必须是完整有效的 stdout output",
    )?;
    serde_json::from_str(string(&event.data, "text")?)
        .map_err(|_| "原始 Pi 证据不是有效 JSON".into())
}

fn load(path: &Path) -> Check<(Vec<Event>, String)> {
    let bytes = crate::evidence_file::read(path, MAX_BYTES)?;
    require(!bytes.is_empty(), "日志为空")?;
    require(
        bytes.last() == Some(&b'\n'),
        "尾记录未以换行结束，日志可能截断或仍在写入",
    )?;
    let mut events: Vec<Event> = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        require(line.len() <= MAX_LINE, "单条日志超过 1 MiB 上限")?;
        require(events.len() < MAX_EVENTS, "日志超过 100000 条记录上限")?;
        let event: Event = serde_json::from_slice(line)
            .map_err(|_| format!("第 {} 行不是支持的 JSON 日志记录", events.len() + 1))?;
        require(event.schema_version == 1, "不支持的日志 schema_version")?;
        require(
            event.sequence == events.len() as u64 + 1,
            "事件序号必须从 1 连续递增",
        )?;
        require(
            !event.run_id.trim().is_empty()
                && events
                    .first()
                    .is_none_or(|first| first.run_id == event.run_id),
            "日志混入不同运行或运行 ID 为空",
        )?;
        require(event.data.is_object(), "事件 data 必须为对象")?;
        // Wall clocks can move backwards; only the sequence establishes ordering.
        let _ = event.at_unix_ms;
        events.push(event);
    }
    Ok((events, crate::evidence_file::digest(&bytes)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParentEvidence {
    pub run_id: String,
    pub sha256: String,
    pub triggers: Vec<Value>,
    pub gate_closed_sequence: u64,
    pub confirmed_blocked_tools: u64,
    pub finish: Value,
}

/// The diagnosis and hash refer to the very same bounded read, not two file opens.
pub(super) fn recovery_snapshot(path: &Path) -> Check<ParentEvidence> {
    let (events, sha256) = load(path)?;
    let mut report = Report::default();
    validate(&events, &mut report)?;
    require(
        report.eligible_for_manual_restart,
        "父日志不满足人工继续条件，请先运行 inspect 核对",
    )?;
    Ok(ParentEvidence {
        run_id: report.run_id.ok_or("父运行身份缺失")?,
        sha256,
        triggers: report.triggers,
        gate_closed_sequence: report.gate_closed_sequence.ok_or("父运行关闭证据缺失")?,
        confirmed_blocked_tools: report.confirmed_blocked_tools,
        finish: report.finish.ok_or("父运行终态缺失")?,
    })
}

struct Decision<'a> {
    sequence: u64,
    name: &'a str,
    allowed: bool,
    applied: bool,
}

fn validate(events: &[Event], report: &mut Report) -> Check<()> {
    let first = &events[0];
    report.run_id = Some(first.run_id.clone());
    report.event_count = events.len();
    require(
        first.kind == "run_started" || first.kind == "spawn_failed",
        "首条必须是启动或启动失败记录",
    )?;
    if first.kind == "spawn_failed" {
        require(events.len() == 1, "启动失败后存在意外记录")?;
        report.status = "valid";
        report
            .reasons
            .push("进程启动失败，没有可继续的已结束门禁运行".into());
        return Ok(());
    }
    require(
        !first.run_id.starts_with("recovery-")
            || events.get(1).is_some_and(|e| e.kind == "run_linked"),
        "恢复子运行缺少紧接启动的关联记录",
    )?;
    let format = string(&first.data, "format")?;
    require(
        ["text", "pi-json", "pi-rpc"].contains(&format),
        "未知采集格式",
    )?;
    string(&first.data, "program")?;
    require(number(&first.data, "pid")? > 0, "无效启动 PID")?;
    report.format = Some(format.into());
    let mut rule_ids = HashSet::new();
    let mut tool_ids = HashSet::new();
    let mut request_ids = HashSet::new();
    let mut decisions: HashMap<&str, Decision<'_>> = HashMap::new();
    let mut commands = HashMap::new();
    let mut responses = HashMap::new();
    let mut gate_ready = false;
    let mut closed_by = None;
    let mut prompt_accepted = false;
    let mut work_before_gate = false;
    let mut abort_confirmed = false;
    let mut settled = false;
    let mut shutdown = false;
    let mut input_closed = false;
    let mut unhealthy = false;
    let mut action = None;
    let mut available_lines = 0_u64;
    let mut evidence_totals = vec![(0_u64, 0_u64)];

    for (at, event) in events.iter().enumerate().skip(1) {
        let d = &event.data;
        let check = (|| -> Check<()> {
            // Every reference, including diagnostic/error events, must point backwards.
            for key in [
                "raw_event_sequence",
                "matched_event_sequence",
                "trigger_sequence",
                "decision_sequence",
            ] {
                if d.get(key).is_some_and(|v| !v.is_null()) {
                    earlier(events, at, d, key)?;
                }
            }
            match event.kind.as_str() {
                "run_linked" => {
                    require(
                        at == 1 && format == "pi-rpc",
                        "关联必须是 RPC 启动后的第二条记录",
                    )?;
                    crate::recovery::validate_link(d, &event.run_id)?;
                    report.linkage = Some(d.clone());
                }
                "output" => {
                    require(
                        ["stdout", "stderr"].contains(&string(d, "source")?),
                        "无效输出来源",
                    )?;
                    let truncated = boolean(d, "truncated")?;
                    let invalid = boolean(d, "invalid_utf8")?;
                    let text = d["text"].as_str().ok_or("输出 text 必须为字符串")?;
                    if !(truncated || format != "text" && d["source"] == "stdout" && invalid) {
                        available_lines += 1;
                    }
                    if format != "text" && d["source"] == "stdout" {
                        if truncated || invalid {
                            unhealthy = true;
                        } else if let Ok(value) = serde_json::from_str::<Value>(text) {
                            if matches!(
                                value["type"].as_str(),
                                Some("agent_start" | "tool_execution_start")
                            ) {
                                if !gate_ready {
                                    work_before_gate = true;
                                }
                                settled = false;
                                if shutdown {
                                    unhealthy = true;
                                }
                            }
                        } else {
                            unhealthy = true;
                        }
                    }
                }
                "agent_event" => {
                    let value = raw(events, at, d)?;
                    let normalized = &d["event"];
                    require(
                        d["adapter"] == "pi"
                            && normalized["kind"] == "tool_completed"
                            && value["type"] == "tool_execution_end",
                        "工具事件类型与原文不一致",
                    )?;
                    let id = string(normalized, "tool_call_id")?;
                    let name = string(normalized, "tool_name")?;
                    let failed = boolean(normalized, "failed")?;
                    require(
                        value["toolCallId"] == id
                            && value["toolName"] == name
                            && value["isError"] == failed,
                        "工具事件内容与原文不一致",
                    )?;
                    require(tool_ids.insert(id), "重复归一化工具调用")?;
                    if gate_ready {
                        require(
                            decisions
                                .get(id)
                                .is_some_and(|v| v.applied && v.allowed && v.name == name),
                            "工具完成没有对应的已确认放行",
                        )?;
                    }
                }
                "rule_triggered" => {
                    let evidence = earlier(events, at, d, "matched_event_sequence")?;
                    let t = &d["trigger"];
                    let id = string(t, "rule_id")?;
                    require(rule_ids.insert(id), "同一规则重复触发")?;
                    let threshold = number(t, "threshold")?;
                    require(threshold > 0, "触发阈值必须大于零")?;
                    let lines = t.get("matched_lines");
                    let count_key = if lines.is_some() {
                        "matched_lines"
                    } else {
                        "matched_events"
                    };
                    let (line_count, event_count) =
                        evidence_totals[(evidence.sequence - 1) as usize];
                    require(
                        threshold
                            <= if lines.is_some() {
                                line_count
                            } else {
                                event_count
                            },
                        "触发计数超过引用位置之前可用的证据总数",
                    )?;
                    require(
                        !(lines.is_some() && t.get("matched_events").is_some())
                            && number(t, count_key)? == threshold,
                        "触发计数与阈值不一致",
                    )?;
                    require(
                        evidence.kind
                            == if lines.is_some() {
                                "output"
                            } else {
                                "agent_event"
                            },
                        "规则引用的证据类型不符",
                    )?;
                    if lines.is_some() {
                        require(
                            evidence.data["truncated"] == false
                                && !(format != "text"
                                    && evidence.data["source"] == "stdout"
                                    && evidence.data["invalid_utf8"] == true),
                            "被跳过的输出不能作为文本触发证据",
                        )?;
                    }
                    let current_action = string(d, "action")?;
                    require(
                        ["record", "abort", "block"].contains(&current_action)
                            && action.is_none_or(|old| old == current_action),
                        "无效或混合的触发动作",
                    )?;
                    action = Some(current_action);
                    report.triggers.push(json!({"sequence":event.sequence,"rule_id":id,"threshold":threshold,
                        "matched_count":threshold,"action":current_action,"evidence_sequence":evidence.sequence,
                        "evidence_kind":evidence.kind,"raw_event_sequence":evidence.data.get("raw_event_sequence"),
                        "tool_event":evidence.data.get("event")}));
                }
                "gate_ready" => {
                    let value = raw(events, at, d)?;
                    require(
                        format == "pi-rpc"
                            && !gate_ready
                            && d["protocol"] == PROTOCOL
                            && value["type"] == "response"
                            && value["command"] == "get_commands"
                            && value["success"] == true
                            && value["id"].as_str().is_some_and(|id| {
                                responses.get(id) == d["raw_event_sequence"].as_u64().as_ref()
                            }),
                        "缺少有效准入就绪响应",
                    )?;
                    require(
                        value["data"]["commands"].as_array().is_some_and(|list| {
                            list.iter().any(|c| {
                                c["name"] == COMMAND
                                    && c["description"] == PROTOCOL
                                    && c["source"] == "extension"
                            })
                        }),
                        "就绪响应缺少扩展版本标记",
                    )?;
                    require(
                        !work_before_gate
                            && tool_ids.is_empty()
                            && !commands.values().any(|c| *c == "prompt"),
                        "准入就绪之前已经开始工作或发送提示",
                    )?;
                    gate_ready = true;
                }
                "gate_closed" => {
                    let trigger = earlier(events, at, d, "trigger_sequence")?;
                    require(
                        closed_by.is_none()
                            && trigger.kind == "rule_triggered"
                            && trigger.data["action"] == "block"
                            && d["reset"] == "new_run_only",
                        "关闭准入缺少唯一有效的 block 触发",
                    )?;
                    closed_by = Some(trigger.sequence);
                    report.gate_closed_sequence = Some(event.sequence);
                }
                "gate_decision" => {
                    let value = raw(events, at, d)?;
                    let body: Value = serde_json::from_str(string(&value, "message")?)
                        .map_err(|_| "无效准入请求正文")?;
                    let id = string(d, "tool_call_id")?;
                    let name = string(d, "tool_name")?;
                    let request = string(d, "request_id")?;
                    let allowed = boolean(d, "allowed")?;
                    require(
                        gate_ready
                            && value["type"] == "extension_ui_request"
                            && value["method"] == "confirm"
                            && value["title"] == PROTOCOL
                            && value["id"] == request
                            && body["tool_call_id"] == id
                            && body["tool_name"] == name,
                        "准入决定与原始请求不一致",
                    )?;
                    require(
                        !decisions.contains_key(id) && request_ids.insert(request),
                        "重复工具或准入请求 ID",
                    )?;
                    require(
                        allowed == closed_by.is_none() && d["trigger_sequence"] == json!(closed_by),
                        "准入决定与关闭状态不一致",
                    )?;
                    decisions.insert(
                        id,
                        Decision {
                            sequence: event.sequence,
                            name,
                            allowed,
                            applied: false,
                        },
                    );
                }
                "gate_applied" => {
                    let value = raw(events, at, d)?;
                    let body: Value = serde_json::from_str(string(&value, "message")?)
                        .map_err(|_| "无效准入确认正文")?;
                    let id = string(d, "tool_call_id")?;
                    let decision = decisions.get_mut(id).ok_or("准入确认缺少对应决定")?;
                    require(
                        !decision.applied
                            && d["decision_sequence"] == decision.sequence
                            && d["tool_name"] == decision.name
                            && d["allowed"] == decision.allowed
                            && value["type"] == "extension_ui_request"
                            && value["method"] == "notify"
                            && body["protocol"] == PROTOCOL
                            && body["tool_call_id"] == id
                            && body["tool_name"] == decision.name
                            && body["allowed"] == decision.allowed,
                        "准入确认与决定或原文不一致",
                    )?;
                    decision.applied = true;
                    if !decision.allowed {
                        report.confirmed_blocked_tools += 1;
                    }
                }
                "tool_blocked_result" => {
                    let value = raw(events, at, d)?;
                    let id = string(&value, "toolCallId")?;
                    require(
                        value["type"] == "tool_execution_end"
                            && value["isError"] == true
                            && d["counted_as_tool_failure"] == false
                            && decisions.get(id).is_some_and(|v| {
                                v.applied && !v.allowed && value["toolName"] == v.name
                            }),
                        "拒绝结果没有对应的已确认拒绝",
                    )?;
                }
                "rpc_command_requested" => {
                    require(format == "pi-rpc" && !shutdown, "无效 RPC 命令时序")?;
                    let id = string(d, "id")?;
                    require(
                        commands.insert(id, string(d, "command")?).is_none(),
                        "重复 RPC 命令 ID",
                    )?;
                }
                "rpc_command_written" => {
                    require(
                        d["id"].as_str().is_some_and(|id| commands.contains_key(id))
                            || d["id"].as_str().is_some_and(|id| request_ids.contains(id)),
                        "写入确认没有对应 RPC 请求",
                    )?;
                }
                "rpc_response" => {
                    let value = raw(events, at, d)?;
                    let id = string(d, "id")?;
                    let expected = commands.get(id).ok_or("RPC 响应没有对应请求")?;
                    require(
                        value["type"] == "response"
                            && value["id"] == id
                            && value["command"] == d["command"]
                            && value["success"] == d["success"]
                            && responses
                                .insert(id, number(d, "raw_event_sequence")?)
                                .is_none(),
                        "RPC 响应与原文不一致或重复",
                    )?;
                    if d["command"] != *expected || d["success"] != true {
                        unhealthy = true;
                    } else if *expected == "prompt" {
                        prompt_accepted = matches!(
                            value["data"]["disposition"].as_str(),
                            Some("started" | "handled")
                        );
                        if value["data"]["disposition"] == "handled" {
                            settled = true;
                        }
                    }
                }
                "rpc_settled" => {
                    require(
                        raw(events, at, d)?["type"] == "agent_settled",
                        "空闲证据与原文不一致",
                    )?;
                    settled = true;
                }
                "rpc_shutdown_requested" => {
                    require(!shutdown, "重复关闭 RPC")?;
                    if d["reason"] == "agent_settled_or_prompt_handled" {
                        require(
                            prompt_accepted && settled && commands.len() == responses.len(),
                            "缺少提示接受、空闲或 RPC 响应证据",
                        )?;
                    } else {
                        require(d["reason"] == "abort_confirmed", "未知 RPC 关闭原因")?;
                        require(abort_confirmed, "中止收尾缺少确认")?;
                    }
                    shutdown = true;
                }
                "rpc_input_closed" => {
                    require(!input_closed, "重复输入关闭记录")?;
                    // A writer can fail before shutdown. Preserve that diagnosis, but
                    // never use the early EOF as proof of a clean controlled stop.
                    if !shutdown {
                        unhealthy = true;
                    }
                    input_closed = true;
                }
                "control_confirmed" | "tool_started_after_trigger" => {
                    let trigger = earlier(events, at, d, "trigger_sequence")?;
                    require(trigger.kind == "rule_triggered", "控制记录未引用规则触发")?;
                    let value = raw(events, at, d)?;
                    if event.kind == "control_confirmed" {
                        require(
                            trigger.data["action"] == "abort"
                                && d["action"] == "abort"
                                && value["type"] == "response"
                                && value["command"] == "get_state"
                                && value["success"] == true
                                && value["data"]["isStreaming"] == false
                                && value["data"]["isCompacting"] == false
                                && value["data"]["pendingMessageCount"] == 0
                                && [
                                    "inhibitor-clear_queue",
                                    "inhibitor-abort",
                                    "inhibitor-get_state",
                                ]
                                .iter()
                                .all(|id| responses.contains_key(id))
                                && value["id"].as_str().is_some_and(|id| {
                                    responses.get(id) == d["raw_event_sequence"].as_u64().as_ref()
                                }),
                            "中止确认缺少命令响应和空闲证据",
                        )?;
                        abort_confirmed = true;
                    } else {
                        require(
                            value["type"] == "tool_execution_start"
                                && value["toolCallId"] == d["tool_call_id"],
                            "触发后开始记录与原文不一致",
                        )?;
                    }
                }
                "capture_failed" | "adapter_error" | "gate_error" | "control_failed" => {
                    unhealthy = true;
                }
                "run_finished" => {
                    require(at + 1 == events.len(), "终态之后仍有记录")?;
                    require(
                        number(d, "trigger_count")? == report.triggers.len() as u64
                            && number(d, "blocked_tool_count")? == report.confirmed_blocked_tools,
                        "终态计数与证据不一致",
                    )?;
                    let success = boolean(d, "success")?;
                    require(
                        d["exit_code"].is_null() || d["exit_code"].as_i64().is_some(),
                        "无效退出码",
                    )?;
                    require(success == (d["exit_code"] == 0), "退出码与成功标记不一致")?;
                    let capture_complete = boolean(d, "capture_complete")?;
                    let adapter_complete = boolean(d, "adapter_complete")?;
                    let complete = capture_complete && adapter_complete;
                    let forced = boolean(d, "forced_cleanup")?;
                    let outcome = string(d, "control_outcome")?;
                    require(
                        [
                            "not_enabled",
                            "completed",
                            "aborted",
                            "blocked",
                            "gate_closed",
                            "failed",
                        ]
                        .contains(&outcome),
                        "未知控制终态",
                    )?;
                    report.finish = Some(d.clone());
                    if matches!(outcome, "blocked" | "gate_closed") {
                        require(
                            format == "pi-rpc"
                                && gate_ready
                                && closed_by.is_some()
                                && action == Some("block"),
                            "声称关闭的终态缺少 block / 门禁证据",
                        )?;
                        require(
                            (outcome == "blocked") == (report.confirmed_blocked_tools > 0),
                            "关闭与实际拒绝终态不符",
                        )?;
                        require(
                            success
                                && complete
                                && !forced
                                && !unhealthy
                                && prompt_accepted
                                && settled
                                && shutdown
                                && input_closed
                                && decisions.values().all(|d| d.applied)
                                && commands.len() == responses.len(),
                            "声称正常关闭的终态缺少完整收尾证据",
                        )?;
                    } else {
                        report
                            .reasons
                            .push("首版继续条件仅接受完整结束的 pi-rpc block 门禁运行".into());
                    }
                    if !success || !complete || forced || unhealthy || outcome == "failed" {
                        report
                            .reasons
                            .push("运行失败、证据不完整或发生异常清理，需要先核对原因".into());
                    }
                }
                _ => return Err("未知或位置不合法的事件类型".into()),
            }
            Ok(())
        })();
        check.map_err(|message| format!("事件 {}：{message}", event.sequence))?;
        evidence_totals.push((available_lines, tool_ids.len() as u64));
    }
    if report.finish.is_none() {
        report.status = "incomplete";
        report
            .reasons
            .push("缺少 run_finished；运行可能未结束或日志截断".into());
    } else {
        report.status = "valid";
        report.eligible_for_manual_restart = report.reasons.is_empty();
    }
    Ok(())
}

fn inspect(path: &Path) -> Report {
    let mut report = Report::default();
    if let Err(error) = load(path).and_then(|(events, _)| validate(&events, &mut report)) {
        report.status = "invalid";
        report.eligible_for_manual_restart = false;
        report.reasons.push(error);
    }
    report
}

pub fn command(mut args: impl Iterator<Item = OsString>) -> Check<i32> {
    let mut path = None;
    let mut format = None;
    while let Some(arg) = args.next() {
        if arg == "--log" && path.is_none() {
            path = Some(PathBuf::from(args.next().ok_or("--log 需要路径")?));
        } else if arg == "--format" && format.is_none() {
            format = Some(match args.next().as_deref() {
                Some(value) if value == "json" => "json",
                Some(value) if value == "text" => "text",
                _ => return Err("inspect --format 只接受 text 或 json".into()),
            });
        } else {
            return Err(
                "inspect 存在未知或重复选项；用法：inspect --log <path> [--format text|json]"
                    .into(),
            );
        }
    }
    let report = inspect(&path.ok_or("inspect 需要 --log <path>")?);
    if format == Some("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        // Debug/JSON formatting escapes control characters from untrusted journal fields.
        println!(
            "诊断状态：{}\n运行：{:?}\n记录数：{}",
            report.status, report.run_id, report.event_count
        );
        for t in &report.triggers {
            println!(
                "规则 {}：计数 {}/{}，触发 #{}，依据 #{} ({})，原文 #{}",
                t["rule_id"],
                t["matched_count"],
                t["threshold"],
                t["sequence"],
                t["evidence_sequence"],
                t["evidence_kind"],
                t["raw_event_sequence"]
            );
        }
        if let Some(linkage) = &report.linkage {
            println!("父运行关联：{linkage}");
        }
        println!(
            "准入关闭序号：{:?}\n已确认拒绝：{} 次",
            report.gate_closed_sequence, report.confirmed_blocked_tools
        );
        if let Some(finish) = &report.finish {
            println!("日志终态：{finish}");
        }
        println!(
            "满足后续人工继续的日志条件：{}",
            if report.eligible_for_manual_restart {
                "是"
            } else {
                "否"
            }
        );
        for reason in &report.reasons {
            println!("原因：{reason:?}");
        }
        for limit in &report.limitations {
            println!("{limit}");
        }
    }
    Ok(if report.status == "valid" { 0 } else { 2 })
}
