# 2026-11-17 Agent main 功能整合记录

本次在主工作区 `F:/WebProjects/项目看板/himind-agent` 的 `main` 整合审查发现的四条本地功能分支。起点为 `e50580034e2dc81e03809d8db4f14a10a420a248`。使用普通 merge 保留各分支历史，未改写提交，未推送或部署安装版。

## 纳入内容

| 功能分支 | 纳入提交 | 结果 |
| --- | --- | --- |
| `codex/20261116-collaboration-architecture-optimization` | `06d14ce` 及其七项功能/修复提交 | 专家组队、统一需求入口、有界并行、隔离候选整合、复核与暂停恢复、执行现场可视化进入 main |
| `codex/20261117-ai-service-onboarding` | `129450d` | 自身 patch 已由 `f9e609f` 等效迁入；补齐历史关系，保留 main 后续桌面布局、文案和测试修正 |
| `codex/20261116-plugin-ai-service-selection` | `53c41e0` | 插件按项目选择服务/模型、宿主路由校验、选择器与客户端示例进入 main |
| `codex/session-activity-20261117` | `d59ffeb` | 原 worktree 中 19 项未提交文件先按字节快照留存，再提交并合入：共享会话/活动服务、四个 MCP 能力、ACP 时间及宿主读取、适配器与合同文档 |

合并提交依次为 `8c3adf7`、`04dea7b`、`d5e393b`、`867aa03`。本次恢复 ADR-0029 索引；原插件 ADR-0030 与主分支 onboarding ADR-0030 重号，插件记录调整为 ADR-0031 并同步引用。

会话活动的 Extensions 消费侧属于独立的 `himind-extensions` 仓库。本次用其已有候选源码构建 Go 夹具验证 Agent 的 broker 链路，没有提交或发布该仓库，也没有生成插件发行制品。上级项目仓库及其独立未提交改动不属于本次 Agent 分支整合范围。

## 本轮验证

| 检查 | 结果与边界 |
| --- | --- |
| `cargo check --locked --offline --bins` | 合并后四个二进制编译检查通过；保留现有编译警告 |
| Agent Rust 测试覆盖 | 2,204 项注册用例通过，23 项沿用显式 ignored；组合首轮结果、定向分批复跑及最后 24 项复跑，**不是同一轮全绿执行** |
| MCP / launcher / updater 测试 | 分别 10 / 21 / 24 项通过 |
| `npm run check:ui`、`npm run build` | 前端检查、TypeScript 和生产构建通过 |
| 协作创建浏览器回归 | 需求入口、固定工作流可选、推荐与手动优先、可选职责、专家/执行器/服务/模型选择、弹窗、未确认不执行通过 |
| 执行现场浏览器回放 | 对已有真实 Store/ledger 确定性夹具快照重新验证当前组件；三轨并行及等待/复核/完成投影通过，不宣称本轮新增安装版真实模型执行 |
| 桌面及 AI 服务浏览器回归 | 桌面布局/滚动/状态 9 项、服务引导 9 组、插件选择器 7 场景通过 |
| 会话历史浏览器回归 | 36 项检查通过；按用户已明确范围移除 200% CSS 缩放检查，保留正常尺寸与窄屏覆盖 |
| 插件客户端 | 3 项测试通过 |
| 会话活动跨仓链路 | 使用候选源码编译的 collector/work-log，通过真实本地 broker、Gateway 和合成 Codex 历史验证素材 Schema、10 分钟活动估算及重复写入幂等 |

原测试清单有一项重复的 `#[test]` 注册：`aggregated_models_payload_uses_exposed_aliases`；2,227 项注册对应 2,226 个唯一名称。覆盖记录保留注册数量和逐项来源，没有把重复注册算成两个不同场景。

## 宿主环境与验收调整

F 盘旧构建目录的可执行文件继承 Low integrity。独立探针证明它们写普通临时目录会收到 Windows error 5，写仓库内目录可以成功。改用仓库内短临时目录后多数用例恢复，但部分扩展/Skill 测试会正确拒绝 Agent 数据目录；其中 Git 探测也会发现上层仓库。最终 24 项在主工作区源码生成的 G 盘独立构建产物及独立临时目录下全部通过，没有放宽业务路径或权限保护。

仅修正两项 Rust 测试：审计锁等待断言对齐已有三秒 best-effort 超时并保留上限约束；完整性诊断以只读系统文件与内存 SACL 验证普通/Low 标记，避免测试为了构造条件修改真实 ACL。上述改动均位于 `#[cfg(test)]` 模块，复跑期间生产源码、依赖和工具没有变化。

原始日志、源码快照、二进制 SHA-256、各轮选择与结果、逐项覆盖来源见主工作区忽略目录：

`.himind/reviews/20261117-all-features-main-integration/`

核心证据为 `baseline.json`、`validation-coverage.json`、`rust-retry-manifest.json`、`rust-final-retry.json` 及 `cleanup-result.json`。旧失败日志保留用于说明环境原因；不将其改写成通过。

## 分支与 worktree 收尾

在确认四条功能分支均成为 main 的祖先后，使用 `git branch -d` 删除本地引用；被 worktree 使用的分支先在原提交 detach，保留全部文件与证据。没有删除 worktree 目录，没有删除远程分支。main 最终提交、删除前分支 SHA、worktree 前后状态由 `cleanup-result.json` 记录。

这次交付确认本地 Agent main 的源码整合与上述验证。安装版仍需另行部署才能获得本次合并的能力。
