//! Read-only recovery evidence aggregation. A report is never a start authorization.
use super::*;
use serde::de::DeserializeOwned;

const SMALL_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    AwaitingConfirmation,
    AwaitingStart,
    StaleInputs,
    Completed,
    Blocked,
    Failed,
    OutcomeUnknown,
    InvalidEvidence,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::AwaitingConfirmation => "待确认",
            Self::AwaitingStart => "已确认，尚未观察到启动记录",
            Self::StaleInputs => "当前来源已变化或不可读",
            Self::Completed => "已正常完成",
            Self::Blocked => "已再次关闭准入",
            Self::Failed => "已记录失败",
            Self::OutcomeUnknown => "结果未知，可能仍在运行",
            Self::InvalidEvidence => "证据无效或相互矛盾",
        }
    }
    fn exit_code(self) -> i32 {
        if matches!(
            self,
            Self::StaleInputs | Self::OutcomeUnknown | Self::InvalidEvidence
        ) {
            2
        } else {
            0
        }
    }
    fn next_action(self) -> &'static str {
        match self {
            Self::AwaitingConfirmation => "核对计划后另行明确确认；本报告不是许可。",
            Self::AwaitingStart => "启动命令仍会再次复核来源；本报告不保证程序或模型服务可用。",
            Self::StaleInputs => "核对来源后重新准备和确认；不要修改旧证据来复用许可。",
            Self::Completed => "核对任务的实际效果；协议完成不等于任务质量合格。",
            Self::Blocked => "查看触发原因与已有影响；继续工作需要新的计划和确认。",
            Self::Failed | Self::OutcomeUnknown | Self::InvalidEvidence => {
                "保留证据，人工核对进程与实际影响；不要删除占位或直接重试同一计划。"
            }
        }
    }
}

#[derive(Serialize)]
struct Report {
    report_version: u8,
    state: State,
    read_only: bool,
    authorizes_start: bool,
    plan_path: PathBuf,
    recovery_id: Option<String>,
    child_run_id: Option<String>,
    start_record_observed: bool,
    source_check: &'static str,
    result_status: Option<String>,
    pid: Option<u32>,
    finish: Option<Value>,
    diagnostics: Vec<String>,
    next_action: &'static str,
    limitations: [&'static str; 2],
}

impl Report {
    fn new(path: &Path) -> Self {
        Self {
            report_version: 1,
            state: State::InvalidEvidence,
            read_only: true,
            authorizes_start: false,
            plan_path: path.into(),
            recovery_id: None,
            child_run_id: None,
            start_record_observed: false,
            source_check: "not_checked",
            result_status: None,
            pid: None,
            finish: None,
            diagnostics: Vec::new(),
            next_action: State::InvalidEvidence.next_action(),
            limitations: [
                "文件摘要不认证作者；两次读取不是原子快照，记录缺失不证明从未执行。",
                "不探测 PID 存活、不启动程序；来源匹配不包含二进制、脚本、环境或服务可用性。",
            ],
        }
    }
}

#[derive(PartialEq, Eq)]
enum Record {
    Missing,
    Bytes(Vec<u8>),
    Unreadable(String),
}
impl Record {
    fn observed(&self) -> bool {
        !matches!(self, Self::Missing)
    }
    fn bytes(&self) -> Check<&[u8]> {
        match self {
            Self::Missing => Err("缺少所需证据文件".into()),
            Self::Bytes(bytes) => Ok(bytes),
            Self::Unreadable(error) => Err(error.clone()),
        }
    }
}

fn snapshot(path: &Path) -> [Record; 5] {
    [
        (path.to_owned(), PLAN_LIMIT),
        (receipt_path(path), SMALL_LIMIT),
        (sidecar(path, ".start.json"), SMALL_LIMIT),
        (sidecar(path, ".run.jsonl"), 32 * 1024 * 1024),
        (sidecar(path, ".result.json"), SMALL_LIMIT),
    ]
    .map(|(path, limit)| {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Record::Missing,
            Err(_) => Record::Unreadable("无法检查证据文件".into()),
            Ok(_) => {
                // Refuse directories/devices before opening. A dangling link is not an absent record.
                if !fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                    return Record::Unreadable("证据不可读或不是普通文件".into());
                }
                match read(&path, limit) {
                    Ok(bytes) => Record::Bytes(bytes),
                    Err(error) => Record::Unreadable(error),
                }
            }
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    schema_version: u8,
    kind: String,
    status: String,
    recovery_id: String,
    planned_run_id: String,
    plan_path: PathBuf,
    plan_sha256: String,
    confirmation_sha256: String,
    parent_run_id: String,
    parent_sha256: String,
    confirmed_at_unix_ms: u64,
    reserved_at_unix_ms: u64,
    output_path: PathBuf,
    result_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    schema_version: u8,
    kind: String,
    status: String,
    recovery_id: String,
    run_id: String,
    start_sha256: String,
    output_path: PathBuf,
    #[serde(deserialize_with = "Option::deserialize")]
    recorded_at_unix_ms: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pid: Option<u32>,
    launcher_exit_code: i32,
    #[serde(deserialize_with = "Option::deserialize")]
    finish: Option<Value>,
    #[serde(deserialize_with = "Option::deserialize")]
    error: Option<String>,
}

fn parse<T: DeserializeOwned>(record: &Record, label: &str) -> Check<T> {
    serde_json::from_slice(record.bytes()?)
        .map_err(|_| format!("{label}结构无效、未写完或包含未知字段"))
}

fn evaluate(path: &Path, records: &[Record; 5], report: &mut Report) -> Check<State> {
    let [
        plan_file,
        confirmation_file,
        claim_file,
        journal_file,
        result_file,
    ] = records;
    let plan: Plan = parse(plan_file, "计划")?;
    verify_plan(&plan, path)?;
    report.recovery_id = Some(plan.recovery_id.clone());
    report.child_run_id = Some(plan.planned_run_id.clone());
    match verify(&plan, path) {
        Ok(()) => report.source_check = "matches",
        Err(error) => {
            report.source_check = "changed_or_unavailable";
            report.diagnostics.push(format!("当前来源：{error}"));
        }
    }
    let plan_hash = digest(plan_file.bytes()?);
    let confirmation = if confirmation_file.observed() {
        Some(verified_confirmation(
            confirmation_file.bytes()?,
            &plan,
            path,
            &plan_hash,
        )?)
    } else {
        None
    };
    if !claim_file.observed() {
        require(
            !journal_file.observed() && !result_file.observed(),
            "缺少启动占位但已有子日志或结果，不能当作待启动",
        )?;
        return Ok(if report.source_check != "matches" {
            State::StaleInputs
        } else if confirmation.is_some() {
            State::AwaitingStart
        } else {
            State::AwaitingConfirmation
        });
    }
    let confirmation = confirmation.ok_or("启动占位缺少匹配确认，不能确定结果")?;
    let claim: Claim = match parse(claim_file, "启动占位") {
        Ok(claim) => claim,
        Err(error) => {
            report.diagnostics.push(error);
            return Ok(State::OutcomeUnknown);
        }
    };
    require(
        claim.schema_version == 1
            && claim.kind == "recovery_start"
            && claim.status == "start_reserved"
            && claim.recovery_id == plan.recovery_id
            && claim.planned_run_id == plan.planned_run_id
            && claim.plan_path == path
            && claim.plan_sha256 == plan_hash
            && claim.confirmation_sha256 == digest(confirmation_file.bytes()?)
            && claim.parent_run_id == plan.inputs.parent.run_id
            && claim.parent_sha256 == plan.inputs.parent.sha256
            && claim.confirmed_at_unix_ms == confirmation.confirmed_at_unix_ms
            && claim.output_path == sidecar(path, ".run.jsonl")
            && claim.result_path == sidecar(path, ".result.json"),
        "启动占位与计划、确认、父运行或固定输出路径不一致",
    )?;
    // Wall-clock values are records, not evidence of ordering or process liveness.
    let _ = claim.reserved_at_unix_ms;
    if !result_file.observed() {
        report
            .diagnostics
            .push("已有启动占位但没有结果；即使日志已有终态也不推断整体完成。".into());
        return Ok(State::OutcomeUnknown);
    }
    let result: Outcome = match parse(result_file, "启动结果") {
        Ok(result) => result,
        Err(error) => {
            report.diagnostics.push(error);
            return Ok(State::OutcomeUnknown);
        }
    };
    require(
        result.schema_version == 1
            && result.kind == "recovery_result"
            && result.recovery_id == plan.recovery_id
            && result.run_id == plan.planned_run_id
            && result.start_sha256 == digest(claim_file.bytes()?)
            && result.output_path == claim.output_path,
        "结果与启动占位、身份或输出路径不一致",
    )?;
    let _ = result.recorded_at_unix_ms;
    report.result_status = Some(result.status.clone());
    let failed = result.launcher_exit_code == 2
        && result.finish.is_none()
        && result.error.as_ref().is_some_and(|e| !e.trim().is_empty());
    if let Some(error) = &result.error {
        report
            .diagnostics
            .push(format!("结果记录中的错误：{error}"));
    }
    if result.status == "not_spawned" {
        require(failed && result.pid.is_none(), "未创建进程的结果字段矛盾")?;
        require(
            matches!(journal_file, Record::Missing) || journal_file.bytes()?.is_empty(),
            "声称未创建进程但存在非空子日志",
        )?;
        return Ok(State::Failed);
    }
    require(
        ["run_finished", "spawn_failed", "runner_failed"].contains(&result.status.as_str()),
        "未知启动结果状态",
    )?;
    if result.status != "run_finished" {
        require(failed, "失败结果字段矛盾")?;
    }
    let child =
        crate::inspect::recovery_child(journal_file.bytes()?, result.status != "runner_failed")?;
    require(
        child.run_id == plan.planned_run_id
            && child.pid == result.pid
            && child.program == plan.inputs.new_run.program.to_string_lossy(),
        "子日志身份、程序或 PID 与计划 / 结果不符",
    )?;
    if result.status == "spawn_failed" {
        require(
            child.pid.is_none() && child.spawn_error == result.error,
            "创建失败结果与子日志不一致",
        )?;
        return Ok(State::Failed);
    }
    require(
        child.pid.is_some()
            && child.linkage
                == Some(json!({
                    "recovery_id":plan.recovery_id, "parent_run_id":plan.inputs.parent.run_id,
                    "parent_sha256":plan.inputs.parent.sha256, "plan_sha256":plan_hash,
                    "confirmation_sha256":digest(confirmation_file.bytes()?), "counter_reset":"new_run",
                })),
        "子日志关联与本计划不符",
    )?;
    report.pid = child.pid;
    if result.status == "runner_failed" {
        // A startup prefix is not proof that the process or all of its descendants stopped.
        report
            .diagnostics
            .push("运行器已记录异常，子任务的最终效果仍需人工核对。".into());
        return Ok(State::Failed);
    }
    require(
        result.error.is_none() && result.finish.is_some() && result.finish == child.finish,
        "启动结果与实际子日志终态不一致",
    )?;
    let finish = result.finish.ok_or("缺少终态")?;
    let (state, code) = match finish["control_outcome"].as_str() {
        Some("completed") => (State::Completed, 0),
        Some("blocked" | "gate_closed") => (State::Blocked, 3),
        Some("failed") => (State::Failed, 2),
        _ => return Err("子运行终态不属于恢复的固定控制策略".into()),
    };
    require(
        result.launcher_exit_code == code,
        "结果返回码与控制终态矛盾",
    )?;
    report.finish = Some(finish);
    Ok(state)
}

fn diagnose(path: &Path) -> Report {
    let mut report = Report::new(path);
    let anchor = canonical(path).or_else(|_| {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok::<_, String>(canonical(parent)?.join(path.file_name().ok_or("计划需要文件名")?))
    });
    match anchor {
        Err(error) => report.diagnostics.push(error),
        Ok(path) => {
            report.plan_path = path.clone();
            let records = snapshot(&path);
            report.start_record_observed = records[2].observed();
            match evaluate(&path, &records, &mut report) {
                Ok(state) => report.state = state,
                Err(error) => report.diagnostics.push(error),
            }
            let after = snapshot(&path);
            if records != after {
                report.state = State::OutcomeUnknown;
                report.start_record_observed |= after[2].observed();
                report.result_status = None;
                report.pid = None;
                report.finish = None;
                report
                    .diagnostics
                    .push("记录在查看期间变化；请重新查看，当前报告不判定终态。".into());
            }
        }
    }
    report.next_action = report.state.next_action();
    report
}

pub(super) fn command(mut args: impl Iterator<Item = OsString>) -> Check<i32> {
    let mut path = None;
    let mut format = None;
    while let Some(arg) = args.next() {
        if arg == "--plan" && path.is_none() {
            path = Some(PathBuf::from(args.next().ok_or("--plan 需要路径")?));
        } else if arg == "--format" && format.is_none() {
            format = Some(match args.next().as_deref() {
                Some(value) if value == "text" => "text",
                Some(value) if value == "json" => "json",
                _ => return Err("status --format 只接受 text 或 json".into()),
            });
        } else {
            return Err("status 存在未知或重复选项".into());
        }
    }
    let report = diagnose(&path.ok_or("status 需要 --plan")?);
    if format == Some("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "恢复状态：{}\n计划：{}\n恢复 ID：{:?}\n子运行：{:?}\n已观察到启动占位：{}\n当前来源：{}",
            report.state.label(),
            json!(report.plan_path),
            report.recovery_id,
            report.child_run_id,
            report.start_record_observed,
            match report.source_check {
                "matches" => "与计划匹配",
                "changed_or_unavailable" => "已变化或不可读",
                _ => "尚未核对",
            }
        );
        if let Some(pid) = report.pid {
            println!("日志记录的子进程 PID：{pid}（未探测存活）");
        }
        if let Some(finish) = &report.finish {
            println!("核对后的终态：{finish}");
        }
        for detail in &report.diagnostics {
            println!("说明：{detail:?}");
        }
        println!("建议：{}\n本命令只读，不签发启动许可。", report.next_action);
        for limit in &report.limitations {
            println!("{limit}");
        }
    }
    Ok(report.state.exit_code())
}
