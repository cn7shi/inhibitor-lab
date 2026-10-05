# inhibitor-lab

本地 Agent 错误治理的独立实验工程，使用 Rust 从头搭建。

当前阶段验证：**启动进程 → 获取输出 → 转换工具结果 → 匹配显式规则 → 记录触发 → 可选 Pi RPC 中止或工具准入拦截**。

## 当前能力

- 启动指定程序，并发读取 stdout 和 stderr。
- 将采集到的文本转发到对应终端输出流。
- 按输出来源、区分大小写的包含字符串和累计命中行数匹配规则。
- 通过 `--format pi-json` 接收 Pi 事件，将工具结束结果转换为统一事件，按工具报告的成功/失败计数；同一调用 ID 去重。
- 每条规则在一次运行中最多触发一次，默认记录 `rule_triggered`；显式启用 Pi RPC 中止时，首次触发启动一次控制流程。
- 显式加载 Pi 工具准入扩展时，每次工具调用等待放行；`block` 动作在规则触发后拒绝新的准入请求。
- 写入逐行 JSON 事件，保留运行 ID、事件序号、采集时间和触发依据。
- 观察模式让子进程继续运行并保留其退出码；RPC 模式额外记录控制结果。

规则完全由配置决定；`stderr` 本身不代表错误。当前不判断错误语义。Pi RPC 可请求中止，配合自有扩展可关闭新工具准入；已经放行的并发工具不能撤销。尚不提供重试、人工恢复或上下文修复。

开发过程见 [阶段记录](docs/development-log.md)、[决策记录](docs/decisions.md)，证据摘要见 [Pi 本地验证](docs/verification/pi-local.md)、[可靠性自查](docs/verification/self-review.md)、[OCR 委托审查](docs/verification/ocr-delegate.md)、[RPC 中止实验](docs/verification/pi-rpc.md) 和 [工具准入实验](docs/verification/pi-gate.md)。

## Pi 本地验证（不需要模型账号）

沿用 Scoop 管理的 Node.js，本地与 CI 使用 **24.20.0**；Pi 是项目内的 npm 开发依赖，固定为 `1.0.3`，不进行全局安装。Windows 的 Node 24.17.0 曾在 RPC 中止后的进程退出阶段崩溃，升级后的对照结果见 RPC 报告。

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

## Pi RPC 中止实验

```powershell
npm --prefix integrations/pi run check:rpc
```

同一脚本依次运行仅记录和中止两种模式，使用相同的本机固定模型回复，在第三次工具调用前留出一秒响应窗口。仅记录模式完成三次读取；中止模式在两次失败后清空任务队列、请求中止、查询会话空闲状态，然后关闭 stdin 请求 Pi 正常退出。当前已验证第三次读取在这个受控中止实验中没有发生。

这属于合作式事后中止。真实运行可能在收到中止命令前已开始下一次工具调用；RPC 模式用 `tool_started_after_trigger` 记录这种情况。中止无法撤销已完成动作，也不保证终止任意后代进程。

独立运行形式如下，模型服务需先自行配置；这个形式尚未与真实模型联调：

```powershell
.\target\debug\inhibitor-lab.exe run --format pi-rpc --rules examples/pi-rules.json --prompt "读取指定的两个测试文件，然后结束" --on-trigger abort -- node integrations/pi/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js --mode rpc --no-session --no-extensions --tools read
```

- `--format pi-rpc`：启动器独占子进程 stdin，一次发送一个 `--prompt`。它不是透传交互式会话。
- `--on-trigger record|abort|block`：默认 `record`；`abort` 请求中止，`block` 需要下面的工具准入扩展。
- `--timeout-ms`：整个 RPC 实验期限，默认 30000；`--control-timeout-ms`：控制流程及随后关闭进程的各自期限，默认 5000。均接受 1 到 86400000 毫秒。
- 自然完成须等 `agent_settled`（或提示被直接处理），`agent_end` 本身不够；所有命令响应按 ID 与命令名关联。
- 正常完成返回子进程退出码；**确认中止且 Pi 正常退出，启动器返回 3**。采集、协议、控制、关闭异常返回 2。超过期限会尝试清理直接子进程，并标记失败；不会把强制清理当作中止成功。

`check:rpc` 会验证启动器的预期退出码 3，因此整个实验脚本成功时仍返回 0，适合在 CI 中运行。

## Pi 工具准入实验

```powershell
npm --prefix integrations/pi run check:gate
```

该实验使用真实 Pi 的 `read` / `write` 工具和本机固定模型回复，依次完成两次失败读取，再尝试写入隔离目录里的 `effect.txt`。两组都没有额外等待：仅记录组成功写出文件；拦截组关闭准入，扩展确认拒绝第三次调用，文件不存在，Pi 正常退出。详细证据见 [实验报告](docs/verification/pi-gate.md)。

模型配置完成后的接入形式如下，真实模型联调仍待完成：

```powershell
.\target\debug\inhibitor-lab.exe run --format pi-rpc --rules examples/pi-rules.json --prompt "读取两个指定的测试文件，然后写入 effect.txt" --tool-gate --on-trigger block -- node integrations/pi/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js --mode rpc --no-session --no-extensions --extension integrations/pi/tool-gate.js --tools read,write
```

- `--tool-gate` 仅用于 `pi-rpc`，启动时先核对扩展版本标记；缺失时不发送业务提示。仅支持 `record` 或 `block`，不与 `abort` 混用。
- 扩展在执行前等待 Rust 的明确布尔放行，等待期限为 5 秒；超时、异常、取消或非 RPC 环境默认拒绝。Rust 只回答本项目的准入协议，不自动批准其它扩展的交互请求。
- 首次触发后，本次运行不再批准新的工具请求；只能开始新运行重置。Pi 收到拒绝会生成一个合成错误结果，该结果单独记录，不计入工具失败规则。
- `control_outcome: blocked` 表示已有扩展应用拒绝的证据；`gate_closed` 表示准入已关闭，但没有确认拒绝过后续调用。两者在 Pi 正常退出且采集完整时，启动器返回 3；协议或退出异常返回 2。
- 同批并发工具可能在规则触发前已全部获得放行，即使尚未实际执行也不能撤回。当前是合作式扩展协议，信任本地 Pi 与扩展，不是操作系统沙箱；嵌套与并发专项试验尚未完成。
- 无效工具名或参数可能在 Pi 准入钩子之前返回错误；当前会因缺少准入证据而结束运行，报告协议失败，不继续按普通工具失败计数。

Pi 的 `tool_execution_start` 出现在准入钩子之前。因此 `tool_started_after_trigger` 只能表示收到了开始事件，不能证明工具已经执行；本实验同时核对准入决定、扩展应用确认和实际文件效果。

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

输出文件已存在时会拒绝运行，保留已有记录。被启动的程序继承工作目录与环境变量；文本 / Pi JSON 模式继承标准输入，RPC 模式接管标准输入。命令通过参数直接启动，不经过额外的 shell。规则相对路径和输出相对路径都基于当前工作目录。

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
- `event`：结构化条件，与 `contains` 二选一；需启用 `--format pi-json` 或 `pi-rpc`。
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

RPC 增加 `rpc_command_requested`、`rpc_command_written`、`rpc_response`、`rpc_settled`、`control_confirmed`、`rpc_shutdown_requested`、`rpc_input_closed`、`control_failed` 与 `tool_started_after_trigger`。中止命令通过 `trigger_sequence` 关联触发，响应通过 `raw_event_sequence` 关联原始输出。`control_confirmed` 只证明协议中止与空闲状态；最终还须检查 `run_finished` 的退出和完整性信息。

准入模式增加 `gate_ready`、`gate_closed`、`gate_decision`、`gate_applied`、`tool_blocked_result` 和 `gate_error`。决定包含请求 ID、工具调用 ID、工具名、触发依据；应用确认通过 `decision_sequence` 回指决定。`rpc_command_written` 也记录准入回复写入，ID 对应 `gate_decision.request_id`。

`run_finished.control_outcome` 为 `not_enabled`、`completed`、`aborted`、`gate_closed`、`blocked` 或 `failed`；`blocked_tool_count` 为扩展确认应用拒绝的工具数，`forced_cleanup` 标记是否进入异常清理路径。`completed` 表示本次协议运行结束，不是模型任务质量判断。实际子进程的 `exit_code` / `success` 始终保留，不用启动器的受控停止退出码 3 覆盖。

`rule_triggered.data.matched_event_sequence` 指向本次触发对应的输出或统一事件；统一事件的 `data.raw_event_sequence` 再指向原始输出。文本规则记录 `matched_lines`，事件规则记录 `matched_events`。两个流各自保序，跨流序号表示采集顺序；时间戳表示采集时间，不承诺是子进程内的实际发生时间。

每行最多保留 64 KiB，超长部分会被读完并丢弃，事件标记 `truncated: true`，该行不参与规则判断。文本按 UTF-8 解码，无效字节替换为替代字符，同时标记 `invalid_utf8: true`。普通文本模式维持替换解码后的匹配；Pi stdout 中的无效编码行不参与任何规则匹配。终端转发会规范化换行，因此本版本面向文本输出程序。事件队列最多缓存 256 行，写入缓慢时会对采集施加背压。

Pi stdout 的无效 UTF-8、无效 JSON、缺失必填字段、冲突结果或截断记录会生成 `adapter_error`；Pi JSON 观察模式继续采集，结束时 `adapter_complete: false`，启动器返回 2。RPC 模式在协议损坏后进入失败清理，不再据此确认控制成功；准入模式可能先通过 `gate_error` / `control_failed` 报告无效输入或关联失败。`run_finished.exit_code` / `success` 仍描述子进程本身，需同时检查 `control_outcome`、`capture_complete` 和 `adapter_complete`。大体积工具结果或汇总事件可能超过当前 64 KiB 上限，此时不会宣称采集适配完整。

当前尚未接入本地日志文件跟随、MCP 代理、交互式终端、进程树熔断和人工恢复。观察模式可能等待后台后代关闭继承的输出管道；RPC 异常期限到达后不再无限等待这些读取线程，但不承诺已经停止后代。终端转发和日志仍为同步 I/O，期限不是系统级硬实时保证。

## 验证

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
npm --prefix integrations/pi test
npm --prefix integrations/pi run check:local
npm --prefix integrations/pi run check:rpc
npm --prefix integrations/pi run check:gate
```

30 项 Rust 测试覆盖文本采集、规则、Pi 结果去重、损坏输入、证据关联，以及 RPC 控制、准入确认、异常响应、超时、退出和竞态记录；13 项 Node 测试检查进程清理、输出写入失败、超时、错误保留及扩展拒绝行为。`check:local`、`check:rpc` 和 `check:gate` 启动真实 Pi 验证观察、中止及准入链路，Windows CI 执行全部验证。

## 后续实验

1. 配置用户选择的模型服务，验证真实模型运行时的事件。
2. 验证并发和嵌套调用的准入边界，再设计带明确证据的人工恢复。
3. 增加本地日志来源与 MCP 观测，处理不同来源之间的重复事件。
