# inhibitor-lab

本地 Agent 错误治理的独立实验工程，使用 Rust 从头搭建。

当前阶段只验证一条链路：**启动进程 → 获取输出 → 匹配显式规则 → 记录触发事件**。

## 当前能力

- 启动指定程序，并发读取 stdout 和 stderr。
- 将采集到的文本转发到对应终端输出流。
- 按输出来源、区分大小写的包含字符串和累计命中行数匹配规则。
- 每条规则在一次运行中最多触发一次，动作固定为记录 `rule_triggered`。
- 写入逐行 JSON 事件，保留运行 ID、事件序号、采集时间和触发依据。
- 子进程继续运行，正常采集结束后保留它的退出码。

规则完全由配置决定；`stderr` 本身不代表错误。当前不判断错误语义，也不执行自动熔断、重试、恢复或修改模型上下文。

## 运行实验

需要 Rust 2024 edition 工具链；本地已用 Rust 1.96.0 验证。

在 PowerShell 中进入这个项目目录，执行：

```powershell
cargo build --bins
.\target\debug\inhibitor-lab.exe run --rules examples/rules.json -- .\target\debug\mock-agent.exe repeat
```

默认把事件写入 `runs/<run-id>.jsonl`，启动时会显示具体路径。示例规则匹配 stderr 中的 `ERROR`，累计到第 2 行时触发一次；第 3 行仍会被采集，模拟程序正常完成。

将最后的 `repeat` 换成：

| 场景 | 预期 |
| --- | --- |
| `normal` | 采集正常输出，stderr 的普通诊断不触发规则 |
| `repeat` | 3 行匹配输出，只触发 1 次，进程继续完成 |
| `fail` | 保留子进程退出码 7；未达到阈值，不触发规则 |
| `long-line` | 超长行截断并标记，仍能采集后续没有换行的末行 |

也可以指定自己的程序：

```powershell
.\target\debug\inhibitor-lab.exe run --rules examples/rules.json --out runs/my-run.jsonl -- your-agent argument1 argument2
```

输出文件已存在时会拒绝运行，保留已有记录。被启动的程序继承工作目录、环境变量和标准输入；命令通过参数直接启动，不经过额外的 shell。规则相对路径和输出相对路径都基于当前工作目录。

## 规则

```json
{
  "rules": [
    { "id": "example", "source": "stderr", "contains": "ERROR", "threshold": 2 }
  ]
}
```

- `id`：非空、唯一。
- `source`：`stdout`、`stderr` 或 `any`，省略时为 `any`。
- `contains`：非空字符串，区分大小写。
- `threshold`：正整数，累计匹配行数；一行出现多次关键词只计一次。
- `rules: []`：只采集，不触发规则。

每次运行使用独立计数。目前没有时间窗口、跨运行累计或“无进展”识别。

## 事件记录

事件类型为 `run_started`、`output`、`rule_triggered`、`run_finished`，异常时会记录 `spawn_failed` 或 `capture_failed`。

`rule_triggered.data.matched_event_sequence` 指向本次触发对应的输出事件。两个流各自保序，跨流序号表示采集顺序；时间戳表示采集时间，不承诺是子进程内的实际发生时间。

每行最多保留 64 KiB，超长部分会被读完并丢弃，事件标记 `truncated: true`，该行不参与规则判断。文本按 UTF-8 解码，无效字节替换为替代字符。终端转发会规范化换行，因此本版本面向文本输出程序。事件队列最多缓存 256 行，写入缓慢时会对采集施加背压。

当前尚未接入本地日志文件跟随、MCP 代理、交互式终端、进程树熔断和人工恢复。若子进程将输出管道交给后台后代进程，采集可能需要等待这些后代关闭管道。

## 验证

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

集成测试实际启动模拟子进程，检查输出完整性、规则触发、证据关联、退出码和已有日志保护。

## 后续实验

1. 验证真实 Agent 的结构化事件可以映射到统一记录。
2. 增加本地日志来源并处理重复事件。
3. 加入 stdio MCP 调用代理，验证调用前规则触发。
4. 在采集和触发链路稳定后讨论熔断动作及恢复方式。
