//! Preparation and explicit confirmation only. No process creation or start consumption in R2.
use crate::evidence_file::{create, digest, read};
use crate::inspect::{ParentEvidence, recovery_snapshot};
use inhibitor_lab::RuleEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CONFIG_LIMIT: u64 = 1024 * 1024;
const PLAN_LIMIT: u64 = 2 * 1024 * 1024;
type Check<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewRun {
    schema_version: u8,
    reason: String,
    prompt: String,
    rules: PathBuf,
    cwd: PathBuf,
    program: PathBuf,
    args: Vec<String>,
    timeout_ms: u64,
    control_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    parent_log: PathBuf,
    parent: ParentEvidence,
    config_path: PathBuf,
    config_sha256: String,
    rules_sha256: String,
    new_run: NewRun,
    rules_snapshot: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u8,
    kind: String,
    recovery_id: String,
    planned_run_id: String,
    prepared_at_unix_ms: u64,
    plan_path: PathBuf,
    policy: Value,
    inputs: Inputs,
}

fn policy() -> Value {
    json!({"format":"pi-rpc", "tool_gate":true, "on_trigger":"block", "counter_reset":"new_run"})
}

fn now_ms() -> Check<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时钟早于 Unix 时间起点")?
        .as_millis()
        .try_into()
        .map_err(|_| "系统时间超出范围".into())
}

fn require(condition: bool, message: &str) -> Check<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn canonical(path: &Path) -> Check<PathBuf> {
    fs::canonicalize(path).map_err(|e| format!("无法解析所需路径：{e}"))
}

fn resolved(base: &Path, path: &Path, directory: bool) -> Check<PathBuf> {
    require(!path.as_os_str().is_empty(), "配置路径不能为空")?;
    let path = canonical(&base.join(path))?;
    let metadata = fs::metadata(&path).map_err(|_| "无法读取配置路径属性")?;
    require(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "配置路径类型不符",
    )?;
    Ok(path)
}

fn inputs(parent: &Path, config: &Path) -> Check<Inputs> {
    let parent_log = canonical(parent)?;
    let parent = recovery_snapshot(&parent_log)?;
    let config_path = canonical(config)?;
    let bytes = read(&config_path, CONFIG_LIMIT)?;
    let mut new_run: NewRun =
        serde_json::from_slice(&bytes).map_err(|_| "新运行配置结构无效或含未知字段")?;
    require(new_run.schema_version == 1, "未知新配置版本")?;
    require(
        !new_run.reason.trim().is_empty()
            && new_run.reason.len() <= 4096
            && !new_run.reason.contains('\0'),
        "继续原因必须为 1..4096 字节且不含 NUL",
    )?;
    require(
        !new_run.prompt.trim().is_empty()
            && new_run.prompt.len() <= 65536
            && !new_run.prompt.contains('\0'),
        "提示必须为 1..65536 字节且不含 NUL",
    )?;
    require(
        new_run.args.len() <= 256
            && new_run
                .args
                .iter()
                .all(|a| a.len() <= 32768 && !a.contains('\0')),
        "程序参数数量、长度或内容无效",
    )?;
    require(
        (1..=86_400_000).contains(&new_run.timeout_ms)
            && (1..=86_400_000).contains(&new_run.control_timeout_ms),
        "期限必须为 1..86400000 毫秒",
    )?;
    let base = config_path.parent().ok_or("配置必须有父目录")?;
    new_run.cwd = resolved(base, &new_run.cwd, true)?;
    new_run.program = resolved(base, &new_run.program, false)?;
    new_run.rules = resolved(base, &new_run.rules, false)?;
    let rule_bytes = read(&new_run.rules, CONFIG_LIMIT)?;
    let rules_text = std::str::from_utf8(&rule_bytes).map_err(|_| "规则必须是有效 UTF-8")?;
    RuleEngine::from_json(rules_text).map_err(|_| "新规则未通过规则引擎校验")?;
    let rules_snapshot: Value = serde_json::from_slice(&rule_bytes).map_err(|_| "无效规则 JSON")?;
    require(
        rules_snapshot["rules"]
            .as_array()
            .is_some_and(|rules| !rules.is_empty()),
        "人工继续必须显式提供至少一条规则",
    )?;
    Ok(Inputs {
        parent_log,
        parent,
        config_path,
        config_sha256: digest(&bytes),
        rules_sha256: digest(&rule_bytes),
        new_run,
        rules_snapshot,
    })
}

fn encode(value: &impl Serialize) -> Check<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn receipt_path(plan: &Path) -> PathBuf {
    let mut name = plan.as_os_str().to_os_string();
    name.push(".confirmed.json");
    name.into()
}

fn identity(path: &Path, prepared_at_unix_ms: u64, inputs: &Inputs) -> Check<String> {
    // This binds the audit identity to its anchor and reviewed content, not to a human identity.
    let bytes = encode(
        &json!({"plan_path":path,"prepared_at_unix_ms":prepared_at_unix_ms,"inputs":inputs}),
    )?;
    Ok(format!("recovery-{}", &digest(&bytes)[..24]))
}

fn prepare(parent: &Path, config: &Path, output: &Path) -> Check<i32> {
    let name = output.file_name().ok_or("计划输出需要文件名")?;
    let parent_dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let plan_path = canonical(parent_dir)?.join(name);
    require(
        !plan_path.try_exists().map_err(|_| "无法检查计划路径")?,
        "计划文件已存在，不会覆盖",
    )?;
    require(
        !receipt_path(&plan_path)
            .try_exists()
            .map_err(|_| "无法检查确认路径")?,
        "该位置已存在确认记录，请核对后选择新计划位置",
    )?;
    let prepared_at_unix_ms = now_ms()?;
    let inputs = inputs(parent, config)?;
    let recovery_id = identity(&plan_path, prepared_at_unix_ms, &inputs)?;
    let plan = Plan {
        schema_version: 1,
        kind: "recovery_plan".into(),
        planned_run_id: format!("{recovery_id}-child"),
        recovery_id,
        prepared_at_unix_ms,
        plan_path,
        policy: policy(),
        inputs,
    };
    let bytes = encode(&plan)?;
    require(
        bytes.len() as u64 <= PLAN_LIMIT,
        "生成计划超过 2 MiB，请缩小配置 / 规则",
    )?;
    create(&plan.plan_path, &bytes)?;
    println!(
        "计划已保存：{}\n恢复 ID：{}\n当前仅准备，尚未确认或启动。",
        json!(plan.plan_path),
        plan.recovery_id
    );
    Ok(0)
}

fn verify(plan: &Plan, path: &Path) -> Check<()> {
    require(
        plan.schema_version == 1 && plan.kind == "recovery_plan" && plan.policy == policy(),
        "计划版本、类型或控制策略无效",
    )?;
    require(
        plan.plan_path == path,
        "计划位置已变化；复制或改名不能复用同一计划，请重新准备",
    )?;
    let suffix = plan
        .recovery_id
        .strip_prefix("recovery-")
        .ok_or("无效恢复 ID")?;
    require(
        suffix.len() == 24
            && suffix.bytes().all(|b| b.is_ascii_hexdigit())
            && plan.recovery_id == identity(path, plan.prepared_at_unix_ms, &plan.inputs)?
            && plan.planned_run_id == format!("{}-child", plan.recovery_id),
        "无效恢复或预留运行 ID",
    )?;
    require(
        inputs(&plan.inputs.parent_log, &plan.inputs.config_path)? == plan.inputs,
        "父日志、新配置、路径或规则与计划不同，请重新准备并确认",
    )
}

fn confirm(path: &Path, input: &mut impl BufRead, output: &mut impl Write) -> Check<i32> {
    let path = canonical(path)?;
    let bytes = read(&path, PLAN_LIMIT)?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(|_| "计划 JSON 无效或不完整")?;
    verify(&plan, &path)?;
    let receipt = receipt_path(&path);
    require(
        !receipt.try_exists().map_err(|_| "无法检查确认记录")?,
        "该计划已有确认或占位记录，不能重复确认",
    )?;
    // JSON presentation preserves exact arguments while escaping terminal control characters.
    writeln!(output, "请核对父运行原因和新任务配置：\n{}\n新运行将重置规则计数和准入状态。当前阶段仅保存确认，不启动程序。\n输入 CONFIRM {} 并换行以确认；其它输入或 EOF 取消。",
        serde_json::to_string_pretty(&plan).map_err(|e| e.to_string())?, plan.recovery_id).map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    input
        .take(1025)
        .read_until(b'\n', &mut answer)
        .map_err(|_| "读取确认输入失败")?;
    let exact = format!("CONFIRM {}", plan.recovery_id);
    let accepted = answer.len() <= 1024
        && answer.last() == Some(&b'\n')
        && std::str::from_utf8(&answer)
            .ok()
            .is_some_and(|s| s.trim_end_matches(['\r', '\n']) == exact);
    if !accepted {
        writeln!(output, "已取消，未保存确认，也未启动程序。").map_err(|e| e.to_string())?;
        return Ok(3);
    }
    let plan_sha256 = digest(&bytes);
    require(
        canonical(&path)? == path && digest(&read(&path, PLAN_LIMIT)?) == plan_sha256,
        "计划在确认期间变化，请重新准备",
    )?;
    verify(&plan, &path)?;
    let confirmation = json!({
        "schema_version":1,"kind":"recovery_confirmation","status":"confirmed_not_started",
        "recovery_id":plan.recovery_id,"planned_run_id":plan.planned_run_id,
        "parent_run_id":plan.inputs.parent.run_id,"parent_sha256":plan.inputs.parent.sha256,
        "config_sha256":plan.inputs.config_sha256,"rules_sha256":plan.inputs.rules_sha256,
        "plan_path":path,"plan_sha256":plan_sha256,"confirmed_at_unix_ms":now_ms()?,
    });
    // The check above is only for usability. This atomic create arbitrates competing confirmers.
    create(&receipt, &encode(&confirmation)?)?;
    writeln!(
        output,
        "确认已保存：{}\n状态：已确认、尚未启动。",
        json!(receipt)
    )
    .map_err(|e| e.to_string())?;
    Ok(0)
}

pub fn command(mut args: impl Iterator<Item = OsString>) -> Check<i32> {
    let mode = args.next().ok_or("recovery 需要 prepare 或 confirm")?;
    let allowed: &[&str] = if mode == "prepare" {
        &["--parent", "--config", "--out"]
    } else if mode == "confirm" {
        &["--plan"]
    } else {
        return Err("recovery 只支持 prepare 或 confirm".into());
    };
    let mut options = HashMap::new();
    while let Some(arg) = args.next() {
        let key = allowed
            .iter()
            .find(|key| arg == **key)
            .ok_or("recovery 存在未知选项")?;
        let value = args.next().ok_or("recovery 选项缺少路径")?;
        require(
            options.insert(*key, PathBuf::from(value)).is_none(),
            "recovery 选项重复",
        )?;
    }
    require(
        options.len() == allowed.len(),
        "recovery 缺少必需选项；参阅 --help",
    )?;
    if mode == "prepare" {
        prepare(
            &options["--parent"],
            &options["--config"],
            &options["--out"],
        )
    } else {
        confirm(
            &options["--plan"],
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        )
    }
}
