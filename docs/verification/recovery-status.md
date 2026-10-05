# R5：只读恢复状态与完整完成证据

日期：2026-10-05；基线 `c93f324`。本轮实现 `recovery status --plan <path> [--format text|json]`，同步总报告和教程；没有新增依赖或安装工具。

## 判定契约

状态命令从原计划规范位置读取计划及 `.confirmed.json`、`.start.json`、`.run.jsonl`、`.result.json`。不执行记录中的程序，不按记录里任意输出路径寻找结果，不写入、删除或重试。报告固定 `read_only: true`、`authorizes_start: false`。

| 状态 | 判定条件概要 | 退出码 |
| --- | --- | --- |
| `awaiting_confirmation` | 有效计划，当前来源匹配，没有确认、启动或结果 | 0 |
| `awaiting_start` | 确认与计划一致，当前来源匹配，尚未观察到启动记录 | 0 |
| `stale_inputs` | 尚无启动占位，当前来源已变化或不可读 | 2 |
| `completed` | 结果、占位、身份、PID、关联及完整协议完成证据一致 | 0 |
| `blocked` | 上述关联一致，完整结果为 `blocked` 或 `gate_closed` | 0 |
| `failed` | 核对到未创建、创建失败、运行器异常或完整失败收尾 | 0 |
| `outcome_unknown` | 占位 / 结果未写完、占位后缺结果，或查看期间记录变化 | 2 |
| `invalid_evidence` | 结构、身份、位置、摘要或实际日志与结果矛盾 | 2 |

`source_check` 独立说明当前父日志、新配置、规则和路径是否仍与计划匹配。历史链完整但配置后来被删除，保留已核对终态并提示来源变化。已知失败返回 0 表示报告确定，不能被上层当成任务成功；缺结果也不能靠子日志末行代替。`pid` 只是日志记录，没有存活探测。

五份文件前后有界读取比较，观察到变化转为未知；计划上限 2 MiB，小记录各 64 KiB，子日志 32 MiB，子日志还复用诊断的行数和行长限制。它不是锁、原子目录快照或身份认证；没有文件不证明从未执行。

## 实际发现并修复的问题

新增回归将实际成功恢复日志缩减为 `run_started`、`run_linked`、`run_finished` 三行，重排连续序号，保留原结果文件。修复前状态错误地返回 `completed` / 0，测试要求 `invalid_evidence` / 2，因此测试退出 101。原因是原 `inspect::validate` 对关闭分支有完整协议约束，对正常完成没有对应约束。

修复要求 `completed` 具备 Pi RPC 格式、正常退出、采集 / 适配完整、无异常或关闭、提示接受、settled、shutdown、输入关闭、所有控制响应与决定应用；关联恢复还必须有门禁就绪。恢复状态使用 `recovery_child` 验证调用者已读取的同一份字节，避免摘要与另一次读取混用。修复后该回归与所有既有诊断 / 控制测试通过。

修复前证据：`runs/recovery-status-review-2026-10-05/completion-before-fix.log`。这证明校验缺口和修复，不代表日志获得了防伪认证。

## 验证结果

| 检查 | 实测结果 |
| --- | --- |
| Rust 全套 | 72 项通过：库 7、采集 8、控制 16、诊断 12、恢复 29 |
| 新增恢复状态回归 | 10 项；八状态、只读、来源与历史分离、缺失 / 半文件、位置复制、超限、CLI 错误、关联篡改、缺协议过程 |
| 故障分类 | 实际不可执行程序、门禁缺失、关闭输出管道读端均报告失败；不释放占位或重试 |
| `not_spawned` 分类 | 合成结果与空 / 缺日志接受，冲突非空子日志拒绝；不是实际崩溃或精确竞态注入 |
| Node | 原有 14 项全部通过 |
| 真实 Pi 两轮 | repaired / unchanged 两组共四个 Pi 进程通过；B 运行中均报告未知，结束分别为完成 / 再次关闭 |
| 准备、取消与确认 | 真实 Pi 父运行后分别为待确认、取消后仍待确认、确认后待启动；不启动模型 |
| 旧确认与修改配置 | 拒绝启动且无占位，状态为 `stale_inputs`；新计划重新确认后成功运行 |
| 只读效果 | 状态前后记录字节、实际文件及模型请求数量断言通过；父原文不变 |
| 并发 / 嵌套旧实验 | batch / nested × record / block 四组通过，仍准确暴露同批已批准工具不能撤销 |
| 格式与静态检查 | `cargo fmt --check`、`cargo clippy --locked --all-targets -- -D warnings`；构建通过 |

在仓库根复现本轮相关检查（Pi 依赖按 README 安装）：

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
npm --prefix integrations/pi test
npm --prefix integrations/pi run check:recovery
npm --prefix integrations/pi run check:boundaries
```

证据在忽略目录，提交的是此摘要：

- `runs/recovery-status-review-2026-10-05/`：修复前失败、最终 Rust / Node / clippy、Pi 对照、OCR 规则与范围、文档检查；提交后保存 `ci-run.json`。
- `runs/pi-recovery-repaired-DozONi/`：父 `1791214489715-37784`，子 `recovery-45995bc2fec2be1cdc5d2bad-child`。
- `runs/pi-recovery-unchanged-cw62sZ/`：父 `1791214493820-40228`，子 `recovery-330ef3d28a06a078ff333bc8-child`。
- 两组各保存 `live-status.json`、`final-status.json`、`summary.json`；repaired 的 `review/stale-status.json` 保存来源变化；各 `review-summary.json` 保存准备 / 确认状态。

本轮本地真实 Pi 对照首次执行通过；没有编造环境故障。模型仍为本机固定服务，实验结束服务关闭，不能直接启动历史计划。既有 Windows [CI 工作流](../../.github/workflows/check.yml) 已包含所有实验，本轮无需增加 CI 步骤；对应提交结果在 [Actions](https://github.com/cn7shi/inhibitor-lab/actions) 查看。基线 `c93f324` 的 [CI 已通过](https://github.com/cn7shi/inhibitor-lab/actions/runs/37331206344)。

## OCR 委托复核及边界

使用 OCR 1.12.12 的 `delegate preview` / `delegate rule`，禁止自动更新；覆盖 7 个 Rust / JavaScript 文件、2 组规则。当前助手核对有界读取、类型校验、空 / 半文件、摘要与路径、终态判定、异常分类、同步只读调用和实验服务的关系。Markdown 由人工事实核对与本地链接检查补足。OCR 不单独调用模型，本轮不是第二个独立审查者。

报告中的错误 / 路径以 JSON 或转义文本显示，不把日志文字当指令；当前来源复核与历史证据分开，避免配置后来变化抹除已完成事实。未新增确认权限，没有改动执行循环或门禁控制语义。

没有确定性注入恰好发生在两次快照之间的变化，也没有模拟系统断电 / 磁盘耗尽；实际运行中查询覆盖了“已占位、尚无结果”。运行器失败只核对有效启动前缀，不能推导所有子孙进程停止。摘要不抵御同权限整套改写，程序、脚本、环境、完整工作目录与服务可用性没有绑定。真实模型与任务质量均未验证。
