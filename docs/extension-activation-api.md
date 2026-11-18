# 本地候选激活 API

Agent 1.5.0 扩展开发内置插件提供四个本地生命周期能力，激活能力契约版本为 `1.0.0`，通过 Capability Gateway 对 MCP、CLI 和本地 HTTP 暴露。插件和 Workflow 激活复用桌面端事务，不需要发布到工作台或读取分发凭据。

| 能力 | 用途 | 审批 |
| --- | --- | --- |
| `extension.plugin.candidate.activate` | 将已测试、已确认的插件候选激活到 SHA 寻址开发槽 | 普通本地写操作审批 |
| `extension.workflow.candidate.activate` | 将已测试、已确认的 Workflow 候选及依赖锁安装到开发槽 | 普通本地写操作审批 |
| `extension.activation.list` | 读取当前 Profile 的活动/部分激活账本 | 只读 |
| `extension.activation.rollback` | 按当前活动哈希和来源工作区回滚插件或 Workflow 到上一开发版 | 普通本地写操作审批 |

这些能力属于 Agent Core 的候选生命周期，和已有 `candidate.save/test/confirm` 一样，不归入扩展开发工具的免审批白名单。实际审批服从当前用户选择的 Profile 策略。启用一个本地插件可能改变整个 Profile 的运行能力，`workspace_root` 绑定的是候选来源，不能把它理解成运行时仅在该工作区可见。

## 插件与工作流调用

1. 保存候选，取得 `id`、`version`、`candidate_sha256` 和来源工作区。插件通过 `extension.plugin.candidate.save(package_path)`；Workflow 通过 `extension.workflow.candidate.save(workspace_root, source_root)`。
2. 调用 `extension.test`，随后调用同类型 `extension.<kind>.candidate.confirm`。MCP 的确认只确认候选，不激活。
3. 调用 `extension.<kind>.candidate.activate`，四个字段全部必填：

```json
{
  "id": "<候选 id>",
  "version": "<候选版本>",
  "candidate_sha256": "<上一步返回的 64 位 SHA-256>",
  "workspace_root": "<候选记录中的绝对来源目录>"
}
```

来源目录必须与候选记录指向同一个实际目录，接受解析到同一位置的路径别名。插件导入 `.hmpkg` 时，该目录通常为包所在目录；不能把上级聚合仓库当作相同身份。不能传 Profile、任意安装目录、`force` 或绕过测试的标记。

入口验证候选身份、实际包哈希、测试和确认状态。插件从冻结包重新解包，避免测试目录内容变化后被意外激活。Workflow 同时校验测试报告关联的哈希，并通过原有安装器验证依赖锁。候选确认前也会核对冻结 Workflow 包本身；候选包变化不能继续使用旧测试结果。

返回 `draft` 与 `activation`；后者包含当前 Profile、来源目录、当前和前一候选哈希及运行面可见状态。调用成功后可用 `extension.activation.list` 检查账本、`system.capability.search` 检查插件能力、`workflow.catalog.describe` 检查工作流开发版。MCP 客户端在注册表变化后应重新激活所需工具。

## 回滚

`extension.activation.rollback` 必须传 `kind`（`plugin` / `workflow`）、`id`、`workspace_root` 和 `candidate_sha256`。这里的哈希是**当前**活动候选的哈希，回滚目标由其 `previous_sha256` 决定。旧活动哈希或来源工作区不匹配时拒绝操作，避免撤回另一会话刚激活的版本。

重复激活同一候选保留原 `previous_sha256`，不把“上一版本”改成当前自身。激活和回滚沿用事务账本与恢复行为；它们不会提交审核、写发布状态或自动启动 Workflow。

## 桌面端兼容

插件桌面激活仍经过 `plugin_authoring`，也从冻结候选包物化。Workflow 桌面端原有“确认并激活”操作保留，通过同一 Workflow 激活/回滚模块执行；MCP 的 `confirm` 与 `activate` 仍分开。

## 验证与部署

使用隔离 Agent Home 和合成候选验证 API、MCP Schema、正常审批、候选状态/身份/哈希/工作区拒绝、开发版可见性、重复激活和回滚。构建产物仅为本地开发候选；正式服务需要部署包含这些注册项的 Agent 并重启服务，才会在能力目录发现它们。添加源码 API 本身不会改变正在运行的生产 Agent 或激活其候选。
