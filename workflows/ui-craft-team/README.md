# UI 专业打磨团队

三路并行审查 → 人工勾选整改项 → 打磨循环 → 守门验收。

架构说明见 `docs/ui-craft-team-architecture.md`。

## 两个入口

| 入口 | 用途 |
|---|---|
| `full` | 完整流程：审查后打磨并交付 |
| `audit` | 只审查，不改代码；适合定期体检 |

## 团队

| 角色 | 职责 | 工具 |
|---|---|---|
| `ui.auditor` | 信息架构：导航、结构、任务流、命名 | 只读审查 |
| `ui.craft` | 工艺：间距、圆角、阴影、字阶、命中区 | 只读审查 |
| `ui.a11y` | 无障碍：对比度、键盘、焦点、语义 | 只读审查 |
| `ui.polisher` | 按确认清单改代码 | 可写 |
| `ui.verifier` | 跑真实检查，贴原始输出 | 可执行 |
| `ui.guard` | 依据 diff 与结果判达标 | **无工具** |

`ui.guard` 用 `tool_policy: none` 是刻意的：它读不到实现方的推理过程，
只能看代码与检查结果，避免变成自我确认。

## 三处用到的新能力

**并行委派**（`kind: "delegate"`）：AUDIT 与 VERIFY 各起独立子运行，
三个审查员同时在跑。台账里 `delegated_runs` 累加，`rework_runs` 保持 0。

**守门循环**（`kind: "loop"` + `exit_when`）：`POLISH → VERIFY → GATE`
最多 3 轮，GATE 输出 `passed: true` 才退出。

**角色覆盖**（`roles`）：brief 作为独立段落拼进提示词，
同一 Runtime 按角色行事。

## 判据来自已有资产

不重复造轮子：

| 已有 | 用在 |
|---|---|
| `npm run check:ui`（26 项） | VERIFY/CHECKS |
| `check:text-layout` | VERIFY/LAYOUT |
| `check:design-tokens` | 工艺与无障碍的量化依据 |
| `ui-craft` / `software-ui-spec` Skill | 审查员方法论 |

## 已知局限

- 检查命令由模型执行，非确定性；VERIFY 要求贴原始命令与退出码，GATE 交叉验证
- 无真机截图，视觉问题依赖 DOM 断言
- 无视觉回归，改 A 弄坏 B 不易发现
- 审查结论不落盘，跨运行无追踪（规划中的 `ui-audit-ledger.json`）
