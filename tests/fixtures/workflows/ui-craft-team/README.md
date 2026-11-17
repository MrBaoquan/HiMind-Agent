# UI 专业打磨团队

闭环：**审查 → 计划 → 人工确认 → 按角色委派优化 → 独立验证 → 守门验收**。

`full` 运行完整闭环；`audit` 只运行审查并在审查出口结束。没有整改项时计划置 `has_tasks=false`，优化循环跳过。

## 角色

| 角色 | 职责 |
|---|---|
| `ui.auditor` / `ui.craft` / `ui.a11y` | 并行审查结构、工艺、无障碍，输出带源码位置与证据的 JSON findings |
| `ui.planner` | 合并重复问题，分配稳定 task id、负责角色、文件范围、验收与验证条件 |
| `ui.implementer.ia` / `.craft` / `.a11y` / `.shared` | 只执行用户批准计划中分配给自己的 task id |
| `ui.verifier` | 独立运行检查，并按 task id 逐项核对差异和验收条件 |
| `ui.guard` | 无工具，只根据批准计划、实施回执、验证报告核对 task id 完成情况 |

确认卡直接展示 PLAN 输出中的摘要和任务明细。批准事件绑定 PLAN 的不可变运行输出；拒绝则不进入优化。确认后的计划经父步骤上下文传给每个委派子运行；Loop 反馈也会带入下一轮。任务类别分支顺序执行，因为它们共享同一工作树。验证分支可并行只读。任何计划项未被验证通过，GATE 都不得返回通过。

## 运行环境

默认使用 `himind.builtin`。启动时可为 POLISH 委派步骤选择 `himind.builtin`、已接入的 `acp.*` 或包声明的其它 provider，并输入对应模型别名：

- HiMind 路由：`himind:<service_id>:<model>`
- ACP 会话模型：`acp:<provider>:<model>`
- Codex / GitHub Copilot CLI：`cli:codex:<model>` / `cli:github-copilot:<model>`

审查与无工具守门仍固定内置 Runtime，以保持隔离边界。外部 ACP 是否能访问工作区取决于该客户端实现；ACP 工具权限由其 profile 策略控制。

## 判据与验证

- 审查判据：`docs/ui-review-profile.json`
- UI 自检：`npm run check:ui`、`npx tsc --noEmit`、`npx vite build`
- 窄窗 / 工艺：`check:text-layout`、`check:design-tokens`
- 运行结果 schema：`schemas/`

检查没有实际运行、命令失败或 task id 缺少验证证据时，不得报告通过。
