# Agent 项目树与工作区

2026-11-19 更新。本文描述当前实现；项目树由 Agent 内核提供，工作日志是消费方。

## 职责

- `com.himind.workspace-registry` 是内核内置能力，不由外部插件覆盖。
- 项目和子项目都是逻辑节点，使用稳定 ID 与单父 `parent_id` 表达层级。
- 目录是物理绑定，可绑定到任一项目节点；一个项目可以有多个目录。
- Agent 管理名称、别名、层级、目录与通用工程上下文，不保存工作日志或看板的外部身份字段。
- 会话来源适配器只读客户端历史，通过项目树解析工作目录的归属。

工作日志读取项目树、目录解析与会话活动能力，保存日志、工作状态、工作项、计划和报告。日志中的 `project_id` 直接引用 Agent 项目 ID。名称用于展示，不能替代身份；展项等业务分类只属于日志自身。

## 数据

注册表位于 Agent 数据根目录的 `data/workspace-registry.json`。使用同目录 `.lock` 的跨进程锁、原子替换及 `.bak` 备份。

```json
{
  "schema_version": "workspace_registry.v1",
  "projects": [
    {"id":"prj-platform","name":"设备平台","kind":"aggregate","status":"active"},
    {"id":"prj-editor","name":"设备编辑器","kind":"subproject","status":"active","parent_id":"prj-platform"}
  ],
  "workspaces": [
    {"id":"wsp-editor","project_id":"prj-editor","path":"F:/Projects/DeviceEditor","role":"root","source":"manual","confidence":"exact"}
  ],
  "candidates": []
}
```

项目还可以携带 `aliases`、`description`、`color`、`context`、创建和更新时间。通用上下文包含技术栈、命令、文档、规则和备注。

目录 `role` 为 `root`、`worktree`、`mirror`、`release`、`source`、`build`。目录来源为 `manual`、`import`、`manifest`、`git`、`session`、`scan`。路径推断与已确认绑定分开保存；工作树和镜像目录不自动成为新的项目身份。

## 能力

| 能力 | 作用 |
| --- | --- |
| `workspace.registry.resolve` | 根据目录或项目引用解析项目、祖先和工程上下文 |
| `workspace.registry.context.get` | 根据目录提供可消费的项目上下文快照 |
| `workspace.registry.project.list` | 读取项目树及目录绑定快照 |
| `workspace.registry.project.upsert` | 创建或更新通用项目及目录绑定 |
| `workspace.registry.project.delete` | 按项目约束删除项目 |
| `workspace.registry.path.bind` | 维护目录绑定 |
| `workspace.registry.candidate.list` | 读取待确认候选 |
| `workspace.registry.candidate.accept` | 将已确认候选落实为项目或绑定 |
| `workspace.registry.validate` | 检查项目树及目录约束 |

父子项目调整还由 Agent 原生项目树操作提供，保持未变目录的稳定身份。创建和修改可使用 `create_only`、`expected_revision` 限制同名误修改与过期写入。

## 消费方约束

1. 工作日志只调用项目读取和目录解析能力，不提供项目新增、改名、合并、层级或路径管理入口。
2. `workspace-business.json` 只保存以 Agent 项目 ID 为键的日志业务状态，不复制项目名称、别名、层级和目录。
3. 未匹配的目录或有歧义的名称保持未归属。显式项目 ID 无效时返回错误，不按名称猜测另一身份。
4. 项目改名和目录重新绑定不自动重写历史日志。查询可按当前项目名称展示，保存的历史事实保持不变。
5. Agent 不可用时返回错误，不回退到日志私有项目登记表。
6. 旧资料的一次性交接是维护操作：先备份，再按已核实的稳定映射更新引用。它不进入 Agent 模型、工作日志 UI 或日常能力合同。

## 验证

Rust 覆盖项目树约束、层级调整、目录冲突、版本检查、原子写入及能力注册。工作日志覆盖只读 broker 合同、同名项目隔离、历史归属、日志与业务状态、父子范围汇总及并行活动统计。浏览器场景验证按项目 ID 选择、手工记录和保存后的刷新。

构建与本机部署使用 [分层验证策略](validation-policy.md)，安装后核对实际文件、进程、健康和相关能力。
