# 多 Agent 协作架构成熟化规划

日期：2026-11-07
状态：规划（非实现）

## 0. 当前基线

已落地并验证的：

| 能力 | 状态 | 证据 |
|---|---|---|
| 角色语义 | ✅ 已提交 `8dd3e63` | `WorkflowPackage.roles` + `LocalRunStep.role_id` |
| 角色影响执行 | ✅ 已部署 | provider / prompt / tool_policy 覆盖，brief 拼前缀 |
| 委派（并行子运行） | ✅ 已部署 | `kind: "delegate"`，真并发（Barrier 测试） |
| 委派与返工统计分离 | ✅ 已提交 | `delegated_runs` vs `rework_runs` |
| 协作可视 | ✅ 已部署 | 角色链、子运行明细、并行委派标记 |
| 运行期收紧覆盖 | ✅ 已提交 | `execution_overrides`（加审批 / 降轮数） |
| 团队工作流包 | ✅ 两个 | `software-delivery` v1.1.0、`ui-craft-team` v1.0.0 |

已识别但**未实现**的缺口，按影响面排序：

| # | 缺口 | 影响 |
|---|---|---|
| G1 | 循环轮数不可配置（`max_rounds` 是死字段） | 用户无法调整打磨强度 |
| G2 | 无 `workflow.run.list`，MCP 无运行历史 | AI 客户端跨会话找不到任务 |
| G3 | 台账不存角色对话内容 | 事后无法回看判断依据 |
| G4 | 审批卡片无依据 | 盲签 |
| G5 | 无暂停 / 单步重试 | 只能取消重来 |
| G6 | 审查结论不落盘 | 跨运行无追踪 |
| G7 | Task Center 不能发起任务 | 看见的地方不能操作 |
| G8 | `interaction_request.schema` 未渲染 | 反馈只能写自然语言 |

## 1. 三条主线

这八个缺口不是并列的。它们沿着三条线：

```
线 A：可配置性 ── G1                          「让用户调得动」
线 B：可追溯性 ── G2 G3 G4 G6                 「让事实留得住」
线 C：可操作性 ── G5 G7 G8                    「让用户接得住」
```

三条线的成熟标志不同：

- **线 A**：用户能在启动时表达意图，且不越过包声明的边界
- **线 B**：任何一次运行，事后都能回答「谁在什么时候依据什么做了什么判断」
- **线 C**：运行卡住时，用户能精确干预而不是推倒重来

## 2. 线 A：可配置性

### 2.1 轮数配置的正确做法

现在的问题不是"没做"，而是**做了个壳**。正确链路：

```
包内声明        "loop": { "max_iterations": 3, "max_iterations_limit": 8 }
                    ↓
启动表单        用户选 1..=limit 之间的值（字段 option_source 从包读）
                    ↓
执行覆盖        input.execution_overrides["POLISH-LOOP"].max_iterations = N
                    ↓
校验            只允许 ≤ limit，且 ≤ 已声明值？
```

**关键决策：上限放宽到包声明的 `max_iterations_limit`，而不是当前的已声明值。**

现在的限制是 `rounds > loop_config.max_iterations` 就拒绝。这意味着包的 `max_iterations: 3` 同时也是天花板。既然要让用户调，就得区分两个概念：

- `max_iterations`：默认轮数
- `max_iterations_limit`：硬上限（包作者声明的成本边界）

用户可以在 `[1, limit]` 内自由选，超出限制才拒绝。

### 2.2 其他该开放但不该无边界的

| 项 | 可配置 | 边界 |
|---|---|---|
| 循环轮数 | ✅ | ≤ 包声明 limit |
| 单步超时 | ✅ | ≤ 包声明 timeout_limit |
| 审批闸门 | 只能加，不能撤 | 保留现有限制 |
| Runtime provider | 只能在 supported_runtimes 内换 | 防止跑到未声明的 provider |
| 并发分支数 | ❌ 由包的 branches 决定 | 改分支数等于改结构 |
| 角色 brief | ❌ | 改 brief 等于改包内容 |

**判据**：能配置的是「这次怎么跑」，不能配置的是「跑什么」。前者是运行参数，后者是产品定义。

## 3. 线 B：可追溯性

这条线最重要，因为它决定了这套系统能不能被信任。

### 3.1 三层事实模型

现在只有第一层。要补齐另两层：

```
第 1 层：执行事实（已有）
  RuntimeEvent: step_started / tool_completed / approval_requested / error
  → 回答「发生了什么、什么时候」

第 2 层：判断依据（缺）
  StepEvidence: 该步骤看到了什么输入、引用了哪些文件、产出了什么结论
  → 回答「依据什么做的判断」

第 3 层：协作上下文（缺）
  HandoffRecord: 角色之间传了什么、谁把什么交给了谁
  → 回答「分工边界在哪」
```

**为什么第 2 层不能靠"存 agent 对话"实现**：对话内容非结构化、体积大、
且不同 Runtime 格式不同（Codex 是 JSONL、builtin 是纯文本、ACP 是另一套）。
存对话等于把 Runtime 的实现细节写进台账。

正确的做法是**结构化证据**：

```json
{
  "step_id": "AUDIT_IA",
  "role_id": "ui.auditor",
  "inputs": {
    "files_read": ["frontend/src/pages/SettingsPage.tsx"],
    "artifacts": [],
    "scope": "设置窗口 AI 连接页"
  },
  "outputs": {
    "issues": [
      { "id": "IA-1", "location": "...", "symptom": "...", "rationale": "..." }
    ]
  },
  "runtime_facts": {
    "provider": "personal.codex",
    "model": "deepseek-v4.1-flash-1017",
    "duration_ms": 182000,
    "tokens": { "input": 12000, "output": 3000 }
  }
}
```

这样第 2 层可查询、可对比、可跨运行聚合——**这正是 G6（审查台账）的基础**。

### 3.2 运行可发现性（G2）

新增能力：

```
workflow.run.list   → 按 workflow_id / status / 时间区间列出运行（默认最近 20 条）
workflow.run.search → 按标题、角色、错误片段检索
```

同时补 MCP 投影：

- `resources/list` 增加 `himind://run/{run_id}` 与 `himind://run/latest/{workflow_id}`
- `workflow.run.get` 的输出里带上 `workflow_id` 与 `title`，让客户端能自己组织索引

**但要先回答一个问题**：AI 客户端应不应该看到所有历史？

我的判断是**按会话边界暴露**，而不是全量。理由：全量历史会污染客户端的上下文预算，
而且跨项目的历史对当前会话没价值。做法是：

```
workflow.run.list 默认只返回「当前 agent + 最近 7 天 + 未归档」
```

### 3.3 审批要有依据（G4）

审批卡片聚合第 2 层证据：

```
┌─ 创建 PR ──────────────────────────┐
│ 角色：交付                       R4 外部写 │
├────────────────────────────────────┤
│ 变更摘要                            │
│   3 个文件 · +124 / -18             │
│   前端设置页增加协议族选择          │
├────────────────────────────────────┤
│ 测试结论                            │
│   check:ui 26/26 通过               │
│   check:text-layout 通过            │
├────────────────────────────────────┤
│ 审查意见（ui.guard，无工具权限）    │
│   passed: false                     │
│   剩余：窄窗 400px 下页签换行溢出   │
├────────────────────────────────────┤
│      [批准并继续]  [拒绝]           │
└────────────────────────────────────┘
```

数据全都有，只差组装。**这是投入产出比最高的一项**。

## 4. 线 C：可操作性

### 4.1 暂停与单步重试（G5）

**暂停**：现在的 `LocalRunStatus::Waiting` 同时表达「等审批」「等反馈」。
新增 `pause_kind` 字段区分：

```rust
pub(crate) struct LocalRun {
    // …
    pub status: LocalRunStatus,        // 仍是 Waiting
    pub pause_kind: RunPauseKind,      // User / Approval / Feedback / External
}

enum RunPauseKind { None, User, Approval, Feedback, External }
```

**为什么不新增状态值**：新增枚举变体会让旧台账反序列化失败。
`Waiting` 语义本来就是"停住等人"，只是没说等谁。

暂停的语义边界：
- 只在**步骤边界**生效，不在步骤执行中打断（打断需要 kill Runtime 进程，成本高且不可控）
- 暂停状态下 resume 从下一个未落定步骤继续

**单步重试**：

```
retry_step(run_id, step_id):
  1. 校验 step 状态 ∈ {failed, succeeded, skipped}
  2. 重置该 step 为 Pending，清空 started_at/finished_at/error/attempt
  3. 清除该 step 的 approval 记录（需重新批准）
  4. 沿 depends_on 反向传播：所有下游步骤若已落定，同样重置
  5. run.status = Queued
```

**关键约束**：只重置「该步骤 + 它的下游」，不碰无关分支。
一个委派扇出里，重试 AUDIT 不应影响 PREPARE-PR 之外的东西。

### 4.2 Task Center 直接操作（G7）

现在只有「查看运行」。补：

| 状态 | 行内操作 |
|---|---|
| running | 暂停 |
| waiting (user) | 恢复 |
| waiting (approval) | 批准 / 拒绝 |
| failed | 重试失败步骤 |
| 任意 | 取消 |

**注意**：不需要把工作流页的所有功能搬过来，只需要「卡住时最常用的那几个」。

### 4.3 结构化反馈（G8）

`interaction_request.schema` 已经存在但只以 JSON 原样展示。渲染成表单：

```json
{
  "type": "object",
  "properties": {
    "selected_issues": {
      "type": "array",
      "items": { "enum": ["IA-1", "CRAFT-3", "A11Y-2"] },
      "title": "要整改的问题"
    },
    "note": { "type": "string", "title": "补充说明" }
  }
}
```

→ 渲成勾选框列表 + 文本框。用户点选而不是写自然语言。

**价值**：反馈变成可统计的数据，而不是需要模型重新解析的文本。

## 5. 实施顺序

按「依赖关系 + 投入产出比」：

### 阶段 1：让已有事实可见（低风险，高回报）

| 项 | 缺口 | 预估 |
|---|---|---|
| 1.1 审批卡片聚合依据 | G4 | 1 轮 |
| 1.2 `max_rounds` 接通执行覆盖 | G1 | 1 轮 |
| 1.3 Task Center 行内暂停/恢复/重试 | G5 G7 | 2 轮 |

**为什么先做这些**：全部是"读已有数据 + 调已有 API"，不动状态机。

### 阶段 2：让历史可追溯（中风险）

| 项 | 缺口 | 预估 |
|---|---|---|
| 2.1 `StepEvidence` 结构化证据 | G3 | 2 轮 |
| 2.2 `workflow.run.list` + MCP 投影 | G2 | 2 轮 |
| 2.3 审查台账 `ui-audit-ledger.json` | G6 | 2 轮 |

**2.1 是本规划里最重要的一项**。没有它，2.2 暴露的只是"步骤成功/失败"，
2.3 也无从建立——台账要有"问题 id"可追踪，而问题来自结构化证据。

### 阶段 3：让反馈结构化（中风险）

| 项 | 缺口 | 预估 |
|---|---|---|
| 3.1 JSON Schema → 表单渲染 | G8 | 2 轮 |

### 阶段 4：状态机改造（高风险）

| 项 | 缺口 | 预估 |
|---|---|---|
| 4.1 `pause_kind` + 暂停/恢复 | G5 | 2 轮 |
| 4.2 单步重试与下游传播 | G5 | 2 轮 |

**为什么放最后**：这两项改 `LocalRun` 契约与 runner 主循环，
一旦出错影响所有运行。前面阶段积累的测试与证据是它们的安全网。

## 6. 明确不做

写清楚不做，避免以后反复讨论：

| 项 | 原因 |
|---|---|
| 存 agent 原始对话 | 非结构化、体积大、各 Runtime 格式不同；用结构化证据替代 |
| 自由 DAG 编辑器 | 会绕过包签名与 dry-run；工作流是制品不是画布 |
| 跨组织任务投递 | 属于 Dashboard 控制面职责，独立模式不承担 |
| 运行时换 Runtime provider | 会改变执行语义且难以复现；换 provider 应新起 run |
| 自动重跑失败的历史运行 | 会让台账里的运行不再可信；重跑必须显式发起 |

## 7. 成熟度的判断标准

不看"做了多少功能"，看这三个问题能否回答：

1. **用户能不能在不改包的前提下，调整这次怎么跑？**
   → 阶段 1 完成后：能（轮数、审批、重试）
2. **三天后回看，能不能说清当时为什么那么做？**
   → 阶段 2 完成后：能（结构化证据 + 运行可发现）
3. **运行卡住时，用户能不能精确干预？**
   → 阶段 4 完成后：能（暂停、单步重试）

三项全绿，才叫「成熟的协作架构」。现在：第 1 项部分满足，第 2、3 项不满足。
