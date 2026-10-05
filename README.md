# inhibitor-lab

本地 Agent 错误治理的独立实验工程，使用 Rust 从头搭建。

当前阶段验证：**启动进程 → 获取输出 → 转换工具结果 → 匹配显式规则 → 记录触发事件**。

## 当前能力

- 启动指定程序，并发读取 stdout 和 stderr。
- 将采集到的文本转发到对应终端输出流。
- 按输出来源、区分大小写的包含字符串和累计命中行数匹配规则。
- 通过 `--format pi-json` 接收 Pi 事件，将工具结束结果转换为统一事件，按工具报告的成功/失败计数；同一调用 ID 去重。
- 每条规则在一次运行中最多触发一次，动作固定为记录 `rule_triggered`。
- 写入逐行 JSON 事件，保留运行 ID、事件序号、采集时间和触发依据。
- 子进程继续运行，正常采集结束后保留它的退出码。

规则完全由配置决定；`stderr` 本身不代表错误。当前不判断错误语义，也不执行自动熔断、重试、恢复或修改模型上下文。

开发过程见 [阶段记录](docs/development-log.md)、[决策记录](docs/decisions.md)，证据摘要见 [Pi 本地验证](docs/verification/pi-local.md)、[可靠性自查](docs/verification/self-review.md) 和 [OCR 委托审查](docs/verification/ocr-delegate.md)。

## Pi 本地验证（不需要模型账号）

沿用 Scoop 安装的 Node.js；Pi 是项目内的 npm 开发依赖，固定为 `1.0.3`，不进行全局安装。需要 Node.js >= 22.19。

```powershell
npm ci --prefix integrations/pi --ignore-scripts --no-audit --no-fund --cache .npm-cache
cargo build --locked --bins
npm --prefix integrations/pi run check:local
```

这会启动真实 Pi CLI 和本机确定性模型测试服务。Pi 的真实 `read` 工具依次读取两个不存在的文件和一个内容包含 `ERROR` 的正常文件。验收：3 次工具结果、2 次失败、1 次规则触发；触发后仍执行第 3 次读取，进程正常退出。

测试服务只监听 `127.0.0.1` 的临时端口，模型回复由脚本固定生成；Pi 使用独立配置目录、离线模式且只启用 `read` 工具。验证不会调用付费模型，也不需要登录。它验证接入链路，不代表已验证真实模型的行为。

每次运行创建 `runs/pi-local-<随机后缀>/`，保存 `events.jsonl`、Pi 标准输出、诊断输出和 `summary.json`。脚本自动检查结果和证据关联，失败返回非零退出码，30 秒超时后清理本次启动的进程。运行文件保留在本机，不进入 Git。

配置真实模型后，可在终端使用下面的接入形式（这部分尚未进行真实模型验证）：

```powershell
.\target\debug\inhibitor-lab.exe run --format pi-json --rules examples/pi-rules.json -- node integrations/pi/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js --mode json --tools read --no-session "读取指定的两个测试文件，然后结束"
```

Windows 下直接通过 Node 启动 Pi 的入口文件，避免依赖 `.cmd` 启动脚本。模型登录或环境变量由 Pi 自己管理，凭据不要写入规则文件或提交到仓库。

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
- `event`：结构化条件，与 `contains` 二选一；需启用 `--format pi-json`。
- `threshold`：正整数，文本规则累计匹配行数，事件规则累计匹配事件数；一行出现多次关键词只计一次。
- `rules: []`：只采集，不触发规则。

每次运行使用独立计数。目前没有时间窗口、跨运行累计或“无进展”识别。

两次工具失败的规则如下；可增加 `"tool_name": "read"` 限定工具。不配置工具名则统计所有工具的累计失败，并不要求连续失败或相同错误。

```json
{
  "rules": [
    { "id": "two-tool-failures", "event": { "kind": "tool_completed", "failed": true }, "threshold": 2 }
  ]
}
```

Pi 适配器仅解析 stdout 中的 `tool_execution_end`，严格要求非空 `toolCallId` / `toolName` 和布尔型 `isError`。结果正文中的 `ERROR`、stderr 和模型自述均不用于判断事件失败。其它带 `type` 的合法事件只保留原文。同一调用 ID 的重复结果不重复计数；若工具名或失败标记冲突，则记录适配错误。去重表随一次进程运行结束释放，内存占用随工具调用数增长。

## 事件记录

事件类型为 `run_started`、`output`、`agent_event`、`rule_triggered`、`run_finished`，异常时会记录 `spawn_failed`、`capture_failed` 或 `adapter_error`。

`rule_triggered.data.matched_event_sequence` 指向本次触发对应的输出或统一事件；统一事件的 `data.raw_event_sequence` 再指向原始输出。文本规则记录 `matched_lines`，事件规则记录 `matched_events`。两个流各自保序，跨流序号表示采集顺序；时间戳表示采集时间，不承诺是子进程内的实际发生时间。

每行最多保留 64 KiB，超长部分会被读完并丢弃，事件标记 `truncated: true`，该行不参与规则判断。文本按 UTF-8 解码，无效字节替换为替代字符，同时标记 `invalid_utf8: true`。普通文本模式维持替换解码后的匹配；Pi stdout 中的无效编码行不参与任何规则匹配。终端转发会规范化换行，因此本版本面向文本输出程序。事件队列最多缓存 256 行，写入缓慢时会对采集施加背压。

Pi stdout 的无效 UTF-8、无效 JSON、缺失必填字段、冲突结果或截断记录会生成 `adapter_error`；后续记录继续采集，但结束时 `adapter_complete: false`，启动器返回 2。`run_finished.exit_code` / `success` 仍描述子进程本身，需同时检查 `capture_complete` 和 `adapter_complete`。大体积工具结果或汇总事件可能超过当前 64 KiB 上限，此时不会宣称采集适配完整。

当前尚未接入本地日志文件跟随、MCP 代理、交互式终端、进程树熔断和人工恢复。若子进程将输出管道交给后台后代进程，采集可能需要等待这些后代关闭管道。

## 验证

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
npm --prefix integrations/pi test
npm --prefix integrations/pi run check:local
```

15 项 Rust 测试覆盖文本采集、规则、Pi 结果去重、假阳性排除、损坏输入、证据关联、退出码和已有日志保护；9 项 Node 测试检查半条日志下的进程清理、输出写入失败、超时及错误保留。另有 `check:local` 启动真实 Pi 验证完整链路，Windows CI 执行全部验证。

## 后续实验

1. 配置用户选择的模型服务，验证真实模型运行时的事件。
2. 单独验证 RPC 中止与执行前拦截，并明确它们的控制边界。
3. 增加本地日志来源与 MCP 观测，处理不同来源之间的重复事件。
