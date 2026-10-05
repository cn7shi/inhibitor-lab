# 恢复前只读诊断 R1 验证（2026-10-05）

基线 `786aa3e`；本阶段新增 `inspect`，没有实现 `resume`。原始证据在本机忽略目录 `runs/inspect-review-2026-10-05/`，本报告保存可提交的摘要。

## 使用与结论

```powershell
cargo build --locked --bins
.\target\debug\inhibitor-lab.exe inspect --log runs/<某次实验>/events.jsonl
.\target\debug\inhibitor-lab.exe inspect --log runs/<某次实验>/events.jsonl --format json
```

默认中文摘要。JSON 的 `report_version: 1` 描述报告格式；`status` 为 `valid`、`invalid` 或 `incomplete`。报告包含规则、阈值、触发和证据序号、最后一次匹配的工具结果、准入关闭序号、扩展确认拒绝数、日志终态、原因与限制。若结构解析失败，不能保证有部分运行摘要；关联检查失败时可能保留已检查前缀，但 `status` 不会变成 valid。

退出 0 表示诊断完成且满足所检查的结构 / 关联契约，**不表示可以直接启动**。只有完整结束的 `pi-rpc + block` 门禁运行才可能得到 `eligible_for_manual_restart: true`；这个字段仍不是人工许可。结构错误、未知版本、读取失败、超限或未结束返回 2。有效的 record、abort 和失败收尾可以返回 0，但继续条件为 false。

检查覆盖版本、运行身份、序号、首尾、向前引用、归一化与原文、规则引用及计数上界、就绪与提示、决定与应用确认、拒绝结果、空闲与收尾，以及终态数量和完整性。旧日志不含规则快照，只能检查计数是否可能、证据是否对应，不能重算完整过滤条件。

## 回归与修复

- 新增 12 项 Rust 命令行测试，多项遍历故障变体：有效 block、关闭但无后续调用、record、abort、失败退出、截断 / 损坏 / 空日志、未知版本、混合运行、缺失 / 向前 / 错类引用、伪终态、缺少应用或关闭证据、提前输入关闭、内容不符、超限、缺失文件、非法参数。
- 每次诊断前后比较日志字节及目录条目。把程序名改成含写文件命令的字符串，诊断仍只读，未创建标记文件；带终端控制字符的规则名被转义。
- 首轮 10 / 11 通过：把两次工具结果的阈值和计数都改为 999 后，初版仍给出 eligible。先保存失败 `threshold-before-fix.log`，再增加按引用位置统计的证据数量上界；修复后 11 / 11 通过。不伪称已重新执行原规则。
- 委托复核发现输入关闭没有检查与正常收尾的先后关系；新增第 12 项测试，替换一条早期写入记录为输入关闭后初版错误认可，失败保存在 `shutdown-before-fix.log`。修复为提前输入关闭标记不健康，不能据此认可正常 block 收尾；同时让就绪 / 中止确认引用对应 RPC 响应的同一条原文。最终 12 / 12 通过。
- clippy 首次指出一处布尔条件可简化；按建议修改后通过。没有压制告警。
- 全量本地 Rust 测试 43 项通过；格式和 clippy 通过。具体输出在 `rust-tests.log`（最终 43 项）、`inspect-tests.log`（首轮修复后 11 项）、`clippy.log`。
- 现有 Node 测试 14 项通过（`node-tests.log`）。文本模式从实际 mock 输出生成日志后，诊断有效且继续条件为 false（`text-events.jsonl` / `text-inspection.json`）。五份本阶段文档的 74 个本地链接、代码围栏和差异检查通过（`doc-check.json`）。

## 真实 Pi 与已有日志

固定 Pi 1.0.3、Scoop Node 24.20.0、确定性本机模型回复。`check:boundaries` 已加入诊断验证，因此现有 Windows CI 会持续覆盖；没有配置真实模型。

| 新实验 | 诊断结果 | 确认拒绝 | 日志继续条件 | 原日志字节 |
| --- | --- | --- | --- | --- |
| 同批 record · `pi-boundary-batch-record-8g7Wpg` | valid | 0 | false | 不变 |
| 同批 block · `pi-boundary-batch-block-50TEGk` | valid | 2 | true | 不变 |
| 嵌套 record · `pi-boundary-nested-record-ULP27A` | valid | 0 | false | 不变 |
| 嵌套 block · `pi-boundary-nested-block-fkM0Bx` | valid | 2 | true | 不变 |

脚本同时保留原有真实文件效果、提前放行、父子调用关系断言，并核对诊断中的运行 ID、触发证据序号和确认拒绝数。每个实验目录增加 `inspection.json`，脚本输出在 `pi-boundaries.log`。收尾时序修复后，用最终构建重读这四份真实日志，诊断及原文不变复核全部通过（`final-pi-inspections.json`）；没有为重复诊断再次启动 Pi。

另读取七份既有真实 Pi 日志并比较读取前后 SHA-256：全部未改变。准入 block / 同批 block / 嵌套 block 分别给出 1 / 2 / 2 次确认拒绝并满足条件；准入 record、RPC abort、Pi JSON 不满足条件。早期 Node 崩溃日志 `pi-rpc-abort-bL8ytq` 缺少后续才加入的 `blocked_tool_count`，明确报字段缺失并返回 2，没有将缺失计数默认为零。见 `existing-logs.json`。

## OpenCodeReview 委托复核

使用本机 OCR 1.12.12，`OCR_NO_UPDATE=1`，无需配置 OCR 模型 API。

- `delegate preview` 覆盖 4 个代码文件：`src/main.rs`、`src/inspect.rs`、`tests/inspect.rs`、`integrations/pi/check-boundaries.mjs`；Markdown 手工核对。
- `delegate rule` 返回 Rust 与 JavaScript 两组规则，保存为 `preview.json` / `rules.json`。
- 当前助手核查输入边界、索引和引用、借用生命周期、失败默认不可继续、未执行日志内容、只读文件打开、控制字符转义、测试清理及子进程检查期限；发现并修复上述收尾时序误判，复核后没有未解决的阻断项。资源常量属于本阶段明确的输入契约，不把通用“禁止硬编码”建议机械当成缺陷。
- OCR 提供范围和规则，判断由当前助手执行；不是独立第二模型复核，不是零缺陷证明。

## 限制与下一步

- 只支持当前日志字段契约、普通文件、32 MiB 文件、1 MiB 单行及 100,000 条记录。较老日志即使版本号也是 1，缺少关键字段仍拒绝；暂不做隐式迁移。
- 打开文件的大小和修改时间用于发现读取期间变化，不是防篡改机制或锁定快照。未做确定性的并发写入 / 替换、磁盘耗尽、系统崩溃或大负载压力验证。
- 不能认证日志作者或阻止同权限者伪造完整证据链，不能证明外部文件效果或后代进程状态。工具开始事件不等于实际执行证明。
- 本阶段没有执行人工恢复。R2 应设计新配置、原因、明确确认、父日志 / 配置摘要和单次消费关联记录，再进入 R3 新运行及 R4 双轮验证。
