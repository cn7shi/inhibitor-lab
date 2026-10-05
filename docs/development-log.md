# 阶段与开发记录

记录目的：保留“为什么做、改了什么、如何验证、还缺什么”。Git 保存代码快照，本文件保存阶段过程，`decisions.md` 保存决策，`runs/` 保存本机运行证据。

## 环境准备（2026-10-05 补记）

- 用户要求优先通过 `D:\DevEnvironment\scoop` 管理开发工具，并要求安装完成后先停下讨论。
- 已将 Scoop 的 Rust 安装关联为 rustup 默认工具链，提供 cargo、fmt 和 clippy；Node.js / npm 沿用 Scoop 安装。
- MSVC 构建工具与 Windows SDK 使用微软官方安装程序，属于 Scoop 之外的组件，安装方式已告知用户。
- 安装后曾完成独立 Rust 编译验证；旧 Inhibitor 的业务代码没有因此改动。
- 此后与用户讨论新方向，才创建独立 `inhibitor-lab`。本节来自会话中已完成动作的补记；不宣称存在当时未保存的完整安装日志。

## 阶段 0：文本采集与显式规则（2026-10-05 补记）

本节是在用户要求持续记录后，依据已有代码、提交和测试补记，不是当时逐步记录的完整开发过程。

- 目标：从原项目独立建立实验；先获取信息并触发简单规则，暂不做智能判错。
- 交付：Rust 启动器、stdout/stderr 采集、包含字符串与累计阈值规则、JSONL 日志、模拟子进程、Windows CI。
- 代码依据：初始提交 `cd582be05992c6d6e31e0ac4071868d58ff48b5d`。
- 当时验证：3 个单元测试、4 个集成测试；fmt、test、clippy 通过。初始 GitHub CI：[run 37306778136](https://github.com/cn7shi/inhibitor-lab/actions/runs/37306778136)。
- 边界：规则只记录，不停止子进程；未接真实 Agent、MCP 或日志文件跟随。

## 阶段 1：Pi 事件接入（2026-10-05，本地验收完成）

### 目标与验收

- 用户选择 Pi 作为第一个接入对象，并选择“先完成本地验证，模型稍后配置”。
- 启动真实 Pi CLI，捕获结构化工具结果并转换为统一事件。
- 同一次运行中，两次不同工具调用明确返回失败，只触发一次规则；成功结果中的 `ERROR` 文本不视为工具失败。
- 原始输出、统一事件和规则触发应能通过事件序号追溯。
- 本阶段只记录触发；RPC 中止与执行前拦截属于后续阶段。

### 决策与实施记录

1. 检查原型：现有规则按文本行匹配，尚无 Pi JSON 解析。工作区初始干净。
2. 检查环境：Scoop Node.js 24.17.0 / npm 11.13.0 可用；未发现 Pi 命令、Pi 登录文件或常用模型服务的环境变量凭据。检查只记录存在性，不读取或输出密钥。
3. 核对上游文档和 npm：固定 `@earendil-works/pi-coding-agent` 为 `1.0.3`，在 `integrations/pi` 内由 npm 管理，依赖安装禁用生命周期脚本。用户已获知该软件包不由 Scoop 直接管理。
4. 增加持续记录约定（`AGENTS.md`）、本阶段记录和决策记录。
5. 在 `src/pi.rs` 实现 Pi 工具结果解析与调用 ID 去重；在 `src/lib.rs` 增加统一事件与事件条件；在 `src/main.rs` 接入格式选择、转换、证据关联及适配完整性标记。
6. 保留已有文本规则；事件规则使用显式失败标记，达到阈值后只记录一次。无效 Pi 输入继续收集，但明确标记不完整并返回 2，详见 D005。
7. 在 `integrations/pi/check-local.mjs` 加入真实 Pi 本地实验：独立实验目录、独立配置、只启用读取工具、固定模型回复；验证结束关闭服务，证据留在 `runs/`。
8. 扩展 Rust 测试，并将固定版本 Pi 本地实验加入 Windows CI；README 补充安装、复现步骤与实际限制。
9. 完成代码与差异检查，未发现需要进一步修复的验证失败。旧项目工作区保持干净；依赖缓存和原始运行日志均被忽略于 Git。

### 验证与证据

- `cargo fmt --check`：通过。
- `cargo test --locked`：6 项单元测试、7 项进程集成测试通过。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo build --locked --bins`：通过。
- `npm --prefix integrations/pi run check:local`：通过，真实 Pi 产生 3 个工具结果，2 次失败，1 次触发；成功读取发生在触发之后，退出码为 0。
- `git diff --check`：通过；Windows 换行提示不影响检查结果。
- 原始证据：`runs/pi-local-y5Wcrm/`；可提交摘要：[Pi 本地验证](verification/pi-local.md)。
- 远程 CI：工作流包含上述 Rust 检查与真实 Pi 本地实验；运行状态与完整输出保存在对应 Git 提交的 [GitHub Actions](https://github.com/cn7shi/inhibitor-lab/actions) 中。

### 尚未完成的下一阶段

- 用户的真实模型服务尚未配置，真实模型试跑尚未执行。
- RPC 中止、执行前阻止和恢复逻辑尚未实现；本阶段没有把“记录触发”描述为“已经熔断”。
- 下一阶段开始前继续先写目标、验收与决策，随后记录编码和验证结果。
