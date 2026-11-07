# 软件交付（多角色）

一个从需求到 PR 的多角色交付流程。每个阶段由不同角色执行，角色配置写在
`workflow.json` 的 `roles` 里。

## 流程

```
ANALYZE（需求分析）
  ↓
CONFIRM（人工确认歧义）        ← 审批闸门
  ↓
IMPLEMENT（实现）
  ↓
VERIFY（测试 → 修复，最多 3 轮）
  ↓
REVIEW（代码审查，无工具）
  ↓
PREPARE-PR（整理 PR 说明）
  ↓
CREATE-PR（创建 PR）           ← 审批闸门
```

## 角色配置的作用

`roles` 里的每项都可以覆盖步骤自己的执行配置：

| 字段 | 作用 |
|---|---|
| `brief` | 作为独立段落拼在提示词前，让同一 Runtime 按角色行事 |
| `runtime_provider` | 覆盖步骤的 Runtime（必须在 `supported_runtimes` 内） |
| `prompt` | 覆盖步骤的提示词模板 |
| `tool_policy` | `none` 表示不挂载任何模型可见工具 |

本包里 `software.reviewer` 和 `software.delivery` 都用 `tool_policy: "none"`：
审查者因此读不到实现方的推理过程，只能依据代码与测试结果判断——这是
「代码审查」这个角色有意义的前提。

## 分段入口

`execution_policy` 是 `segmented`，可以从两个入口开始：

- `full`：从需求分析开始（默认）
- `implement`：已有明确需求，直接从实现开始

出口有两个：`reviewed`（审查通过）和 `pr_ready`（PR 说明就绪，默认）。

## 产物

本包不声明 Artifact：各阶段通过步骤输出与提示词上下文传递信息。
需要严格产物校验的包应在 `artifacts` 里声明 schema 并让步骤引用。
