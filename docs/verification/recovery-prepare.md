# 人工继续准备与确认 R2（2026-10-05）

基线 `9c99341`。本阶段实现计划及唯一确认记录，状态为 `confirmed_not_started`；没有接入新进程启动、启动消费或 `run_linked`。原始证据在忽略目录 `runs/recovery-review-2026-10-05/`。

## 接口与数据

```powershell
.\target\debug\inhibitor-lab.exe recovery prepare --parent runs/<父实验>/events.jsonl --config examples/recovery-config.example.json --out runs/recovery-plan.json
.\target\debug\inhibitor-lab.exe recovery confirm --plan runs/recovery-plan.json
```

先按实际需要修改示例中的继续原因、任务、路径与参数。示例是 Windows / 本机 Scoop 布局，不是已经配置好的模型服务。`prepare` 校验结构、规则和文件存在性；不保证目标程序能启动、扩展就绪或模型可用。配置的 `rules` / `cwd` / `program` 相对路径以配置文件所在目录解析；`args` 保持原字符串，将来相对新工作目录解释，不从旧日志补出任何命令。

确认时完整展示父运行触发 / 关闭依据、确认拒绝数、新提示、继续原因、规则快照、目录、程序 / 参数、期限、固定控制策略及预留运行 ID。输入屏幕显示的 `CONFIRM recovery-...` 并换行才保存。确认文件路径为计划原规范路径追加 `.confirmed.json`，不是新运行日志。环境变量不写入计划；模型密钥应继续使用模型自身配置，不放进会持久化的提示或参数。

| 情况 | 退出码 | 文件结果 |
| --- | --- | --- |
| 准备成功 | 0 | 新计划，未确认 / 未启动 |
| 明确确认成功 | 0 | 唯一 `confirmed_not_started` 记录 |
| 取消、错误文本、EOF（含无换行的完整确认文本） | 3 | 不写确认，可再次主动确认原计划 |
| 文件 / 配置无效、内容变化、重复确认 | 2 | 不覆盖已有证据 |

父日志原字节 SHA-256 与诊断共用同一次有界读取。计划另绑定配置文件原字节、规则原字节及规范化配置，并嵌入父证据和规则快照。恢复 ID 由计划位置、准备时间、全部输入内容派生；它是审计标识，不是认证口令。确认记录再保存计划原字节 SHA-256、各摘要、父运行、预留子运行和确认时间。

同一父运行允许明确准备新的计划；同一份计划必须在原位置确认。复制 / 改名会失败，单改副本内部位置也不能复用原 ID。并发确认最终通过 `create_new` 仲裁，写入后 `sync_all`。失败留下的占位或半文件不自动覆盖。

## 回归、失败与修复

新增 11 项 Rust 进程测试，部分遍历多个输入变体：

- 正常准备、相对路径、新参数展示、父子身份 / 摘要关联；旧日志和计划原字节不变。
- 取消、EOF、错误 ID、仅输入 yes、超长输入、终端控制字符转义。
- 父日志、配置、规则提前变化，以及等待确认时修改这三者或计划本身；以真正的确认提示作为同步点，不使用固定睡眠。
- 两个进程均停在确认提示后提交，仅一个返回 0，另一个返回 2；后续重复确认不改已有记录。
- 计划副本 / 内部字段修改、已有输出 / 半确认文件、输出路径碰撞、非法父终态、缺少结束、无效规则 / 路径 / 配置、未知选项和超限。
- 新配置指向一个若被执行就会写标记日志的测试命令；准备和确认全过程没有该标记，工作目录无新文件。没有把 R2 的预留 ID 说成已运行子进程。

首轮 11 项通过后，复核扩展了副本案例：复制计划并只改 `plan_path` 后，初版仍可按原 ID 确认，预期 2、实际 0。证据 `copied-anchor-before-fix.log`。修复为生成和校验恢复 ID 时绑定计划位置、准备时间和输入快照；扩展回归通过。没有将该修复概括为防御同权限恶意重写整套证据。

本地完整 Rust **54 项**通过（原 43 + 新 11），格式、clippy 和构建通过。见 `rust-tests.log` / `clippy.log`；`recovery-tests-first.log` 保留扩展案例之前的首轮结果。

既有 Node **14 项**测试通过（`node-tests.log`）。五份本阶段文档的 89 个本地链接、代码围栏与八个课程编号核对通过（`doc-check.json`）。配置示例用实际父日志准备成功，随后取消返回 3，没有确认文件（`example-prepare.log` / `example-cancel.log`）。

示例首轮在受限环境下无法规范化读取 Scoop Node 路径，返回 2，错误为“拒绝访问（os error 5）”，没有计划产生；允许该次工具链路径读取后重跑相同命令，准备及取消通过。这里是执行环境限制，不改程序绕过文件错误，也没有启动示例中的 Node / Pi。

## 真实 Pi 父运行验证

`check:boundaries` 保留原四组真实 Pi 1.0.3 对照，并在两组 block 父运行结束后调用 `check-recovery.mjs`。模型仍是本机固定回复，Node 为 Scoop 24.20.0。

| 父实验 | 结果 | 恢复 ID | 原文 |
| --- | --- | --- | --- |
| 同批 block · `pi-boundary-batch-block-517vns` | 取消 3 → 确认 0 → 重复 2 | `recovery-4bce4994ae43ad1f225b4088` | 不变 |
| 嵌套 block · `pi-boundary-nested-block-iJBubr` | 取消 3 → 确认 0 → 重复 2 | `recovery-1fce7a58682d73fcd46eb95a` | 不变 |

record 对照为 `pi-boundary-batch-record-c66EMO`、`pi-boundary-nested-record-n5KXNy`；这两组没有进入准备流程。每组保留既有准入和真实文件效果断言。两组确认均停留 `confirmed_not_started`，父日志 / 计划 / 既有确认不变，模型请求数没有增加，工作目录文件集合不变；Node `crypto` 独立核对父日志、配置、规则、计划的 SHA-256。新任务没有执行，这不是 R4 的两轮实际运行验证。

每个 block 实验目录包含 `recovery-config.json`、`recovery-plan.json`、`recovery-plan.json.confirmed.json` 与 `summary.json`；控制台输出在 `pi-boundaries.log`。现有 Windows CI 的边界脚本自动覆盖这条 R2 路径。

## OpenCodeReview 委托复核与依赖

本机 OCR 1.12.12，禁用自动升级。最终预览识别 9 个代码 / 配置文件；显式取得 Rust、JavaScript、JSON 示例、Cargo 及锁文件通用规则，共 5 组。锁文件不在预览支持扩展中，额外手工核对；Markdown 手工检查。原始输出 `preview.json` / `rules.json`。

当前助手按规则核查内容一致性、原子创建、并发与取消、I/O 失败保留、未执行参数、资源上限、控制字符转义及测试清理。上述副本身份问题已复现修复；没有新增未解决的阻断发现。OCR 只提供范围与规则，本轮不是独立第二模型复核。

新增 `sha2 = "=0.10.9"` Cargo 依赖，锁文件新增该包及 9 个传递包；使用 [RustCrypto 的 SHA-256 实现](https://docs.rs/sha2/0.10.9/sha2/)。没有安装系统环境或全局工具，Rust 继续由 Scoop 管理。唯一确认使用 [Rust `create_new` 的原子创建契约](https://doc.rust-lang.org/std/fs/struct.OpenOptions.html#method.create_new)；不自制哈希或靠“先查文件存在”代替并发仲裁。

## 限制与交接

- R2 的“只确认一次”不等于已经实现“只启动一次”。R3 必须重新复核所有摘要、原子创建启动占位，再接入 `run`；保留首条 `run_started` 的 PID 契约，随后记录 `run_linked`。
- 文件检查是按内容进行的时点复核，不锁定所有来源。确认后来源仍可能改变，未来启动前必须复核；不认证人类身份，不抵御同权限者重写全套证据。
- 不绑定程序二进制、脚本、环境变量或工作目录全部内容，也没有证明新程序具有 Pi RPC 能力；这些不等同于已审阅配置文件本身。真实模型配置仍未完成。
- 32 MiB 父日志、1 MiB 配置 / 规则、2 MiB 计划、1 KiB 确认输入；原因最多 4 KiB、提示最多 64 KiB、参数最多 256 个且单个最多 32 KiB。未知字段、空规则不接受。
- 未做断电 / 磁盘耗尽 / 多机共享目录故障注入；部分写入文件会保留，需人工核对后另起计划，不自动删除重试。未测试符号链接 / Junction 变更的全部平台行为。
- 启动失败、崩溃窗口、计数重置、再次熔断、真实两轮父子日志等验收留待 R3–R4。
