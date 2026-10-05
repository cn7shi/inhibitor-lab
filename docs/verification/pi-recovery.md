# R4：真实 Pi 两轮恢复对照

日期：2026-10-05。基线 `426514a`。本阶段新增 [check-recovery-runs.mjs](../../integrations/pi/check-recovery-runs.mjs)、npm `check:recovery` 和 Windows CI 步骤；Rust 启动、规则及门禁代码未修改。

## 实验问题与控制条件

验证实际 `recovery start` 是否能从已关闭的 Pi A 创建关联的新 Pi B，以及 B 是否仍受同一规则约束。Pi 1.0.3、Scoop Node 24.20.0，使用真实内置 `read` / `write` 和准入扩展，模型回复由本机回环服务固定提供，不使用真实模型账号。

两组各使用隔离目录；每组 A、B 共用工作目录、规则副本、提示和工具调用 ID，但配置目录独立、禁用会话持久化。固定服务都依次要求读取 `input-a.txt`、`input-b.txt`，再写 `effect.txt`。没有人为延迟。首个模型请求必须只有新提示和零条工具历史，后续历史数量与步骤严格对应。

两组 A 开始时目录为空：两次真实 ENOENT 触发规则，之后的写入被扩展拒绝。实验确认文件未产生，并只读诊断父运行满足人工继续条件。准备 / 确认使用测试输入，不宣称真人身份认证。

## 最终实际结果

| 组与运行 | 真实工具结果 | 触发 / 确认拒绝 | 启动器退出 / 控制结果 | 实际文件 |
| --- | --- | --- | --- | --- |
| repaired · A | 两次读取失败 | 1 / 1 | 3 / `blocked` | 无 `effect.txt` |
| repaired · B | 两次读取、一次写入成功 | 0 / 0 | 0 / `completed` | 内容精确为 `recovery-proof` 加换行 |
| unchanged · A | 两次读取失败 | 1 / 1 | 3 / `blocked` | 无 `effect.txt` |
| unchanged · B | 两次读取失败 | 1 / 1 | 3 / `blocked` | 仍无 `effect.txt` |

四个 Pi 进程本身均正常退出 0，采集和适配完整，没有强制清理。启动器退出 3 表示正常受控关闭。准入造成的合成拒绝单独记录，不作为第三次工具失败计数。

repaired 组的输入由**实验操作者在 A 结束后、确认 B 之前补齐**。两份输入内容摘要在 B 前后相同，唯一新增文件为预期输出。unchanged 组始终为空。这里证明新任务在修正条件后可以完成，不能把补齐文件描述为系统自动定位或修复根因。

未修正组 B 的准入序号为 25、42、62，先批准两个读取；归一化结果为 30、47，第二次失败产生触发 48，第三次准入拒绝并关联该触发。A 使用相同工具 ID，但 B 仍重新识别和累计，证明该次实际运行没有继承 A 的去重、计数或已关闭门禁状态。

## 关联、拒绝与留档核对

- A、B 运行 ID 不同，B 与计划预留 ID 一致；首条仍为含 PID 的 `run_started`，第二条 `run_linked` 回指 A，包含父、计划、确认摘要与计数重置策略。
- Node crypto 独立检查父、配置、规则、计划、确认、启动与结果关联；结果 PID、返回码及终态与子日志一致。`summary.json` 另保存八份关键证据的 SHA-256，便于后续读取核对，不依赖测试服务仍在运行。
- repaired 组首次确认后故意修改配置提示：旧计划启动返回 2，没有占位、子日志、结果或模型请求，计划 / 确认原文不变。保留旧计划，在 `review-fresh/` 重新准备确认原本需要的任务，随后才启动 B。没有改写旧计划或删除启动占位来重试。
- 两组完成后再次调用同一计划 `start`，均返回 2；父日志、计划、确认、启动、子日志、结果字节与工作区快照均不变，模型请求数也不增加。
- `inspect` 诊断两个父日志与两个子日志有效；repaired B 正常完成，不在首版从阻断运行继续的范围；unchanged B 再次关闭，满足日志继续条件，但仍需新的明确计划和确认。

## 证据位置与复现

最终证据目录（Git 忽略的本机文件）：

| 场景 | 目录 | 父 / 子运行 ID |
| --- | --- | --- |
| repaired | `runs/pi-recovery-repaired-e2Uzya/` | `1791212987702-41732` / `recovery-669c679762cb66818b392fdb-child` |
| unchanged | `runs/pi-recovery-unchanged-UDhyPJ/` | `1791212989219-42044` / `recovery-a44f572483d1534f1f35f7c0-child` |

每组有 `parent.jsonl`、父 / 子诊断、输出与错误流、`summary.json`、工作区、规则副本、Pi 配置和恢复计划配套记录。`review-summary.json` 保留 R2 取消 / 确认 / 重复确认摘要；repaired 的 `stale-attempt.json` 保留配置变化拒绝事实。实验结束关闭本地模型服务，不要直接启动这些历史计划；重新运行实验会建立新服务和新目录。

首次两组也通过，保留在 `pi-recovery-repaired-KZMDTf/` 和 `pi-recovery-unchanged-bFjiJl/`；之后为补齐摘要、拒绝尝试留档和结果路径断言再次执行，最终两组通过。没有观察到需修复的 Rust 功能回归，不虚构失败记录。

在独立仓库根目录复现：

```powershell
npm ci --prefix integrations/pi --ignore-scripts --no-audit --no-fund --cache .npm-cache
cargo build --locked --bins
npm --prefix integrations/pi run check:recovery
```

依赖安装及构建仅在需要时执行。实验自己检查预期退出 3；两组验收通过时脚本整体返回 0。

## 检查与审查

- 新脚本 Node 语法检查、Node 14 项测试、两组真实 Pi 恢复对照通过，输出在 `runs/pi-recovery-review-2026-10-05/` 的 `first-run.log`、`final-run.log`、`node-tests.log`。
- Rust 本轮未改，未重复本地全量 Rust；基线 62 项及全部旧 Pi 对照的 [CI 已通过](https://github.com/cn7shi/inhibitor-lab/actions/runs/37329144214)。本次提交的 Windows CI 执行 62 项 Rust、14 项 Node、全部旧实验和新增 R4，提交后核对远端结果。
- OCR 1.12.12 禁止自动更新，`delegate preview` / `delegate rule` 覆盖新 JS 脚本、package.json、CI 三个文件 / 三组规则；当前助手复核异步服务与进程、超时清理、错误传播、固定凭据、摘要和断言、依赖及工作流权限。不是独立第二模型或 OCR 自带模型审查。
- 复核补充：错误证据本身保存失败时用 `AggregateError` 同时保留原始失败；未进行磁盘耗尽注入，不将这条异常分支标为实测通过。固定模型协议参数属于实验条件，未机械套用通用规则中的禁止硬编码建议。
- README、D016、阶段记录和统一教程 C10 人工核对；修正 README 沿用 54 项的旧数量。文档链接、围栏、十课编号及差异检查另留 `doc-check.json`。没有安装系统工具或新增依赖。

## 适用边界

已验证首版 R1–R4 的本地确定性闭环。真实模型决策、自动修复、原会话恢复、多个并发恢复任务、同批提前批准的撤销、任意后代进程、系统断电 / 磁盘耗尽仍不在这两组证据内。R3 的模拟进程故障回归继续保留，不能将其等同于所有真实 Pi 故障已注入。摘要检测变化，不认证操作者或防御同权限整套改写，也没有锁定程序、脚本、环境和整个目录。
