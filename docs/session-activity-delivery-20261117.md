# 会话活动服务与工作日志接入交付记录

2026-11-17 完成隔离源码实施及合成回归。变更尚未提交、推送或部署，生产安装版没有因此更新。

## 工作树与恢复基线

两仓库分支均为 `codex/session-activity-20261117`。

| 仓库 | G 盘工作树 | 基线提交 |
| --- | --- | --- |
| Agent | `G:/HiMindAgentWorktrees/session-activity-20261117/himind-agent` | `06d14cec398a86f7dc5d4858443d2adabc0f1c5e` |
| Extensions | `G:/HiMindAgentWorktrees/session-activity-20261117/himind-extensions` | `34bfe2a032a9f40bb5725e644704650f64a9c2f2` |

全部实施位于这两个工作树；F 盘原有 UI 证据、媒体功能及迁移方案文档保持原状。回滚可依据以上基线审查或丢弃本分支的对应变更，不操作 F 盘既有改动。任务恢复文件与原始验证输出保存在 Agent 工作树 `.himind/long-tasks/20261117-session-activity/`，不作为发行制品。

## 验收

下表证据路径均相对此任务目录；通过表示本次源码和合成验证范围通过，不代表生产环境验收。

| 标准 | 结果 | 证据与评审结论 |
| --- | --- | --- |
| A1 隔离与恢复 | 通过 | `isolation-delivery.json`；两仓库分支/基线与 F 盘原始状态核对一致；task/state/progress/handoff 完整 |
| A2 共享服务与范围 | 通过 | `cargo-test-delivery.log`；UI 与网关共用 session_service，4 个只读能力，项目/后代/全部/未归属，保留实际子项目 ID 和别名 |
| A3 活动事实与可恢复读取 | 通过 | 同上；时区半开窗口、页预算、游标重启/失效、页内更改、来源失败、身份歧义、ACP 加密时间兼容、DSH 跨进程认证 |
| A4 日志兼容与幂等 | 通过 | `go-test-delivery.log`、`cargo-test-delivery.log`；原生键/跨日补记/重复写入/锁定/证据保留/未迁来源/隐私/重叠估算/人工零工时 |
| A5 回归与构建 | 通过 | Rust 82/82；cargo check --bins、前端构建、工作区模型 14 条断言、三个 Go 包测试及 vet、日志 UI JS 语法检查通过 |
| A6 合同与交付一致 | 通过 | `extension-validation-delivery.log`；两插件及工作流验证通过；真实素材和候选条目经对应 Schema 验证；版本/依赖/UI 缓存版本一致 |

## 变更清单

- Agent：新增 `src/session_service.rs`、`src/session_service/query.rs`、`src/session_service/host.rs`，Tauri 历史命令改为委托；五来源继续复用已有只读适配器。
- 网关/MCP：注册 `workspace.session.sources/list/messages/activity`，能力版本 `1.0.0`，响应 `session_history.v1`；沿用动态激活、审批与每次调用 broker。
- ACP：新轮次保存可选 user_at/assistant_at；旧加密会话无需迁移，未知时间保持未知。DSH 发布当前用户保护的宿主引用，读取运行中合法 Runtime。
- Extensions：新增 Go broker SDK 与 Agent 素材兼容层；collector `0.8.0`、work-log `0.18.0`、daily-work-log `0.8.0`；同步素材/条目 Schema、来源声明和 UI 缓存版本。
- 日志：保留 source/native session/date 键、证据和 workspace_project_id；自动工时显式标记估算，去除本批 Agent 区间重叠，人工修订支持 0，旧未迁来源继续采集。
- 不创建调度器、持久活动索引或完整 t3code 控制台；后续运行/审批/取消接入现有 LocalRun/Mission/Runtime。

## 验证命令与结果

| 检查 | 结果 / 输出文件 |
| --- | --- |
| `cargo check --locked --offline --bins` | exit 0；`cargo-check-delivery.log`；仓库仍有编译警告 |
| `cargo test --locked --offline --bin himind-agent session_ -- --test-threads=1` | 82 passed，0 failed；`cargo-test-delivery.log` |
| `go test ./sdk/capability ./plugins/work-log-collector ./plugins/work-log` | 三包通过；`go-test-delivery.log` |
| `go vet ./sdk/capability ./plugins/work-log-collector ./plugins/work-log` | exit 0；`go-vet-delivery.log` |
| `npm run build`（frontend） | 通过；`frontend-build.log`，保留已有 chunk 大小提示 |
| `npm run check:workspace-project-model`（frontend） | 14 assertions passed；`frontend-workspace-model.log` |
| 插件与工作流源码验证 | 三项目均 valid；`extension-validation-delivery.log` |
| `node --check plugins/work-log/ui/app.js`；两仓库 `git diff --check` | exit 0；Git 仅提示既有 CRLF/LF 转换 |

跨仓测试通过 `HIMIND_HISTORY_EXTENSIONS_FIXTURE`、`HIMIND_HISTORY_COLLECTOR_FIXTURE`、`HIMIND_HISTORY_WORKLOG_FIXTURE` 指定本次源码构建的 Go 可执行文件。真实 broker → Gateway → 合成 Codex 文件 → collector → work-log 生成 10 分钟活动估算，保留证据，重复写入仍只有一条；使用 session-digest Schema 和 entry.upsert 输入 Schema 校验真实结果。未读取其它私人会话正文，也未改真实项目登记。

## 已知边界与发布要求

- DSH 需要对应运行中的宿主；旧 ACP 缺消息时间时无法可靠回填活动日。usage 尚未统一投影，不填伪造的零用量。
- 当前是有界读取和可恢复游标；停止拉取即可取消后续页，单次同步来源读取只具软预算。海量历史应缩小窗口或回退兼容采集；持久增量索引另行实施。
- 工时属于估算；同批 Agent 会话可扣重，旧来源缺少同等区间事实，不能承诺跨全部客户端无重叠。证据引用不保证客户端记录永久保存。
- 生产真实客户端覆盖、原生运行态和日志编辑交互未做人工验收。本次没有新增工作区页面，不沿用早期合成 UI 验收来宣称生产通过。
- 发布前按实际构建的新版插件生成 artifact、市场索引和工作流依赖锁；现有已发布哈希保持原状。能力发现是服务可用性的依据，Agent 版本字符串未据此伪造升级。

合同详情见 [会话与活动公共服务](session-activity-service.md)；Extensions 侧见另一工作树 `docs/work-log-agent-session-integration.md`。
