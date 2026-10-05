# Pi 本地验证证据（2026-10-05）

## 验证对象

- Rust 启动器和 Pi JSON 适配器。
- 真实 `@earendil-works/pi-coding-agent` 1.0.3 CLI、真实 `read` 工具。
- Scoop Node.js 24.17.0 / npm 11.13.0，Rust 1.96.0，Windows。
- 模型回复由仅监听本机的确定性测试服务产生；没有使用真实模型或模型凭据。

## 结果

| 项目 | 实际结果 |
| --- | --- |
| 读取 `missing-a.txt` | Pi 工具报告失败，原始结果含 `ENOENT` |
| 读取 `missing-b.txt` | Pi 工具报告失败，累计到 2 次 |
| 规则触发 | 恰好 1 次，动作 `record` |
| 读取 `present.txt` | 成功，正文含 `ERROR`，未增加失败计数 |
| 运行结束 | 子进程和启动器退出码均为 0 |
| 采集与适配 | `capture_complete: true`、`adapter_complete: true` |

首次通过的本机证据目录：`runs/pi-local-y5Wcrm/`（忽略于 Git，保留在本机）。

| 关联 | 事件序号 |
| --- | --- |
| 原始工具结束输出 | 15、27、40 |
| 对应统一工具事件 | 16、28、41 |
| 规则触发 | 29 |
| 触发引用的统一事件 | 28 |
| 该统一事件引用的原始输出 | 27 |

序号对应这次运行，不作为固定协议；脚本每次根据实际事件检查关联。

## 自动检查

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
npm --prefix integrations/pi run check:local
```

- 6 项 Rust 单元测试、7 项进程集成测试通过。
- 本地 Pi 验证通过：3 个工具结果，2 次失败，1 次触发，4 次本机模型接口请求。
- 检查覆盖正常结果中的错误文本、重复结果、冲突结果、缺失/无效字段、截断输入和触发依据关联。
- GitHub CI 的同一检查已纳入工作流；本轮远程运行结果见阶段记录。

## 未验证与限制

- 真实模型是否遵循任务及不同供应商的行为，尚未验证。
- 本阶段不验证 RPC 中止、执行前阻止、MCP 或进程树熔断。
- 超过 64 KiB 的记录会被标记不完整；本次案例未触及大结果存储设计。
