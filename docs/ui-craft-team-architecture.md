# UI 专业打磨团队：多 Agent 协作架构

日期：2026-11-07
状态：设计 + 可运行包

## 1. 为什么需要这个团队

现状的问题不在「不会打磨」，而在**打磨不可持续**：

- 一份 UI 审查报告（`docs/ui-review-gateway-profiles.md`）写完就停在那里，
  下次改同一片区域时没人重新读它。
- 审查依赖某个人的记忆和手感，换人即断档。
- `check:ui` 有 26 项自检，但它们只防回归，不评判「够不够好」。
- 打磨与审查由同一个人做，等于自评——发现不了自己的盲区。

所以这个团队的核心不是「更多角色」，而是**把审查与实现分开，并把结论沉淀成可重放的东西**。

## 2. 团队构成

| 角色 | 职责 | 关键约束 |
|---|---|---|
| `ui.auditor` | 信息架构审查：导航层级、页面结构、任务流、命名与文案 | 只输出问题清单，不改代码 |
| `ui.craft` | 工艺审查：间距、圆角、描边、阴影、字阶、命中区、动效 | 只输出问题清单，不改代码 |
| `ui.a11y` | 无障碍审查：对比度、键盘路径、焦点、读屏语义 | 只输出问题清单，不改代码 |
| `ui.polisher` | 按**已确认**的清单改代码 | 不许顺手重构，不许改清单外的东西 |
| `ui.verifier` | 跑真实检查：`check:ui`、DOM 断言、窄窗复验 | 读真实输出，不臆测 |
| `ui.guard` | 守门：依据 diff 与检查结果判断是否达标 | `tool_policy: none`，读不到实现方自述 |

前三个与后三个之间有一道**人工确认**：审查产出问题清单，用户决定改哪些。
这避免了「AI 自作主张改了一堆你没让它改的东西」。

`ui.guard` 用 `tool_policy: none` 是有意的：它只能看代码与检查结果，
不能看 `ui.polisher` 的推理过程，否则会变成自我确认。

## 3. 流程

```
SCOPE（圈定范围：哪些页面/组件）
  ↓
CONFIRM-SCOPE ──────────────── 审批闸门 ①
  ↓
AUDIT（三路并行委派）
  ├── IA 审查      ui.auditor
  ├── 工艺审查     ui.craft
  └── 无障碍审查   ui.a11y
  ↓
CONFIRM-FIXES ──────────────── 审批闸门 ②：用户勾选要改哪几条
  ↓
POLISH-LOOP（最多 3 轮）
  ├── POLISH       ui.polisher
  ├── VERIFY（两路并行委派）
  │   ├── 跑 check:ui + DOM 断言   ui.verifier
  │   └── 窄窗/真机复验            ui.verifier
  └── GATE         ui.guard  ── passed=true 则退出循环
  ↓
DELIVER ────────────────────── 审批闸门 ③：确认交付
```

三处用到了刚建的能力：

- **`delegate` 步骤**：AUDIT 与 VERIFY 是并行委派，各起独立子运行。
  三个审查角色同时在跑，互不等待。
- **`loop` + `exit_when`**：POLISH → VERIFY → GATE 形成一个有退出条件的环，
  最多 3 轮，避免无限打磨。
- **`roles` 配置**：每个角色的 brief 作为独立段落拼进提示词，
  同一 Runtime 按角色行事。

## 4. 任务入口

用户从哪发起这个团队，决定了它有多少实际使用频率。目前有五条路，成熟度不同：

| 入口 | 现状 | 说明 |
|---|---|---|
| **工作流页** | ✅ 可用 | 选包 → 填工作区 → 跑。这是当前唯一完整路径 |
| **MCP（AI 客户端）** | 🟡 能力在，包未投影 | Claude Code / Codex 可通过 `himind.*` 能力发起；但工作流包还没做成 MCP 可调用的入口 |
| **Task Center** | ❌ 缺 | 只能「查看运行」，无法「发起任务」。这是最该补的入口 |
| **定时计划** | ✅ 可用 | `scheduler` 已支持按 preset 触发，适合「每周体检」 |
| **Dashboard** | 🟡 见 `docs/architecture-agent-core-dashboard-optional.md` | 作为组织级下发入口，独立模式不依赖 |

我的判断：**该优先补 Task Center 入口**。用户看见任务的地方不能发起任务，
是当前最明显的断层。

## 5. 多 Agent 怎么落地

「多 Agent」在这个包里体现为三层，每一层都能在台账里查到：

### 5.1 角色（`roles`）

```json
"roles": {
  "ui.craft": {
    "label": "工艺审查",
    "brief": "只审查间距、圆角、描边、阴影、字阶与命中区。不评价功能是否正确。",
    "tool_policy": "default"
  }
}
```

角色只改「怎么说、看什么」，不改路由。同一个 Runtime 可以依次扮演三个审查员。

### 5.2 委派（`delegate`）

AUDIT 一步拆成三条分支，各起一条**独立子运行**：

```
workflow_run_xxx                      ← 父运行
  ├─ …_delegate_AUDIT_IA              parent_kind = delegate
  ├─ …_delegate_AUDIT_CRAFT           parent_kind = delegate
  └─ …_delegate_AUDIT_A11Y            parent_kind = delegate
```

三条子运行并发跑，`min_succeeded: 3` 表示三个审查都成功才算这一步成功。
台账里 `delegated_runs` 会计 3 次，而 `rework_runs` 保持 0——这是分工不是返工。

### 5.3 汇总

父步骤的产出带完整分支结构：

```json
{
  "delegate_id": "AUDIT",
  "branches": [
    { "branch_id": "IA",    "role_id": "ui.auditor", "status": "succeeded", "output": { "issues": [...] } },
    { "branch_id": "CRAFT", "role_id": "ui.craft",   "status": "succeeded", "output": { "issues": [...] } },
    { "branch_id": "A11Y",  "role_id": "ui.a11y",    "status": "succeeded", "output": { "issues": [...] } }
  ],
  "succeeded": 3
}
```

下游的 `CONFIRM-FIXES` 直接从这里取三份清单合并展示给用户。

## 6. 「持续优化」到底怎么持续

这个词最容易变成空话。这里拆成三件可执行的事：

### 6.1 一轮内的持续：守门循环

`POLISH → VERIFY → GATE` 最多 3 轮。GATE 判 `passed=false` 就回到 POLISH。
这是**同一次运行内**的迭代，不需要人干预。

### 6.2 跨运行的持续：定时体检

`entrypoints: ["audit"]` 只做审查不改代码。配合 `scheduler` 每周跑一次，
产出的问题清单可以累积成 **UI 技术债台账**。

### 6.3 长期持续：审查结论要落盘

这是当前**最大的缺口**：

- `docs/ui-review-gateway-profiles.md` 那种文档是人工写的，AI 跑完不会更新它。
- 没有「上次审查发现的问题，这次是否修了」的追踪。

要真正持续，需要一个 `ui-audit-ledger.json`：记录每条问题的
`id / 发现时间 / 状态（open/fixed/wontfix）/ 复现位置`。
GATE 步骤除了判断本轮是否达标，还应该更新这个台账。

**这一条我建议单独做**，因为它需要新的存储与 UI，不是改个包能解决的。

## 7. 已知局限

诚实列出来，避免被当成"已完成"：

| 局限 | 影响 | 缓解 |
|---|---|---|
| 检查命令由模型执行，非确定性 | 模型可能跳过 `check:ui` 或误读输出 | VERIFY 分支要求贴出原始命令与退出码；GATE 交叉验证 |
| 无真机截图能力 | 视觉问题（溢出、错位）只能靠 DOM 断言 | 依赖现有 `check:text-layout` 等断言；真机复验留人工 |
| 无视觉回归 | 改 A 处弄坏 B 处不易发现 | `check:ui` 26 项覆盖已知回归面 |
| 审查标准靠 prompt | 不同 Runtime 判断尺度不同 | brief 写得足够具体；GATE 交叉验证 |
| delegate 内的角色不能声明 `runtime_provider` | 会覆盖分支自己的 provider | 本包的审查角色只声明 brief 与 tool_policy |

## 8. 与既有资产的关系

这个包**不重复造轮子**，而是编排已有能力：

| 已有资产 | 在团队里的位置 |
|---|---|
| `check:ui`（26 项脚本） | VERIFY 分支的判据 |
| `check-design-tokens` | 工艺审查的量化依据（对比度、字阶） |
| `check:text-layout` | 窄窗与截断问题的判据 |
| `docs/ui-review-gateway-profiles.md` | 审查报告格式的先例 |
| `ui-craft` / `software-ui-spec` Skill | 审查员的方法论来源 |
| `delegate` / `roles` / `loop` | 编排骨架 |

## 9. 落地顺序建议

1. **包跑通**（本次交付）：`workflows/ui-craft-team/`，可在工作流页发起
2. **Task Center 发起入口**：让「看见任务」的地方能「发起任务」
3. **审查台账**：`ui-audit-ledger.json` + 跨运行追踪
4. **定时体检**：`entrypoints: ["audit"]` 接 scheduler
5. **结构化反馈**：把 `interaction_request.schema` 渲染成表单，
   让用户在 CONFIRM-FIXES 勾选而不是写自然语言
