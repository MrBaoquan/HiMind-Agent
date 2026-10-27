# 本机 AI 网关聚合 Profile 与模型路由

Status: accepted (2026-10-27)

## Context

现有本机推理网关已经支持环回监听、每客户端令牌、真实凭据隔离、OpenAI Chat、OpenAI Responses、Anthropic Messages、SSE 流式和本机用量台账。

当前绑定模型是“一个客户端对应一个 AI 服务”。客户端配置里的 Base URL、协议和模型列表都来自同一个上游服务，因此无法把一个客户端同时暴露给多个已添加服务，也无法在不重新分发配置的情况下切换服务模型。

本需求需要把“AI 服务连接”和“客户端模型入口”拆开：

- AI Service 仍表示一个真实上游连接、协议、模型目录和凭据；
- Gateway Profile 表示用户选择的一组服务与模型，以及对客户端暴露的稳定模型别名；
- Client Binding 表示某个 AI 客户端使用某个 Profile 和独立本机令牌；
- Capability Route 继续只服务 HiMind 自身能力，不用于外部 AI 客户端。

## Decision

在现有 `inference_gateway` 上增加 Gateway Profile，不新建第二套网关。

### Data model

```text
GatewayProfile
  id
  name
  entry_protocol          openai-chat | openai-responses | anthropic  # 旧版兼容主协议
  entry_protocols[]       openai-chat | openai-responses | anthropic
  default_model           exposed model id
  enabled
  routes[]
    exposed_model         stable model id returned by /v1/models
    service_id            custom AI service id
    upstream_model        model id sent to the upstream
    enabled
```

一个 Profile 可以同时声明多个 `entry_protocols`。`entry_protocol` 只保留为旧数据和旧客户端的兼容主协议；读取旧配置时若 `entry_protocols` 为空，会自动回填为 `[entry_protocol]`。保存时协议去重，第一项写回兼容主协议。

Profile 不保存 API Key。网关解析绑定时才从 `ai_services` 读取服务和 DPAPI 凭据。第一版只允许引用本机自定义 AI Service；工作台托管服务继续走原有单服务绑定。

### Client binding

一个 Gateway Profile token 可对应多条内存路由。每个客户端仍获得独立令牌，令牌先定位客户端和 Profile，再由请求体中的 `model` 选择具体路由。

同一 Profile 绑定多个客户端时，每个客户端使用自己原生入口协议：Codex 选择 `openai-responses`，Claude Code / Claude Desktop 选择 `anthropic`，OpenCode 等通用客户端选择第一项可用协议（通常为 `openai-chat`）。客户端绑定记录实际选择的协议，而不是盲用 Profile 主协议。

旧版“单服务网关绑定”继续可读、可用。没有 Profile 的旧绑定按服务模型目录展开为兼容路由。

### Aggregated model catalog

`GET /v1/models` 不再依赖上游模型接口。网关返回当前 Profile 中启用路由的聚合目录：

```json
{
  "object": "list",
  "data": [
    { "id": "deepseek/deepseek-v4-pro", "object": "model" },
    { "id": "yundu/gpt-6.1-sol", "object": "model" }
  ]
}
```

模型别名为稳定唯一标识。默认建议 `service_id/upstream_model`，保存时校验冲突。

### Request routing

```text
request path -> client-facing protocol
token        -> client + profile
body.model   -> exposed model route
route        -> upstream service + upstream model + protocol
```

网关在转发前把请求体中的模型名改写为 `upstream_model`，在响应中把模型名改写回 `exposed_model`。流式响应每一帧执行相同映射。

### Protocol matrix (P1)

| 客户端入口 | 直通上游 | 转换上游 |
| --- | --- | --- |
| OpenAI Chat | OpenAI Chat | 暂无 |
| OpenAI Responses | OpenAI Responses | OpenAI Chat |
| Anthropic Messages | Anthropic Messages | OpenAI Chat、OpenAI Responses |

多协议 Profile 不再要求每条模型路由兼容所有入口，而是按客户端投影：

| 客户端入口 | 可进入该客户端目录的上游 |
| --- | --- |
| OpenAI Chat | OpenAI Chat |
| OpenAI Responses | OpenAI Responses、OpenAI Chat |
| Anthropic Messages | Anthropic Messages、OpenAI Chat、OpenAI Responses |

保存规则：

- 每条模型路由至少兼容一个已选客户端协议；
- 每个已选客户端协议至少有一个兼容模型；
- 启用的 Profile 中，每个协议至少有一个启用中的兼容模型；
- 绑定客户端时只注册该客户端兼容的模型；
- 运行时按令牌解析后也只返回该客户端兼容的模型路由。

Profile 保存时即拒绝当前矩阵之外的入口/上游组合；运行期仍保留协议不兼容错误，任何情况下都不得静默更换协议或模型。Anthropic ↔ Responses 的请求、非流式响应、命名事件 SSE、图片和函数工具转换参考 cc-switch 的成熟实现；web search、thinking signature、prompt cache 等高级能力不在本轮自动等价。

### Credentials and security

- 网关只监听环回地址。
- 每个客户端使用独立令牌。
- 真实 API Key 只存在于 Agent 内存和 DPAPI 存储。
- Profile 只引用本机自定义 AI Service，不引用远端托管凭据。
- 删除被 Profile 引用的服务必须被阻止。

### Failure policy

第一版不自动跨模型降级。上游连接失败、429、5xx 或响应语义错误都直接回传可诊断错误，避免用户选择 A 模型却无提示运行 B 模型。

### Observability

本机用量记录至少保留：

- client；
- profile_id；
- exposed_model；
- service；
- upstream_model；
- protocol；
- stream；
- status；
- latency_ms；
- token 用量。

旧台账字段保持兼容，新增字段使用 serde 默认值。

## UI

AI 工具接入页新增“聚合模型入口”区域：

- 创建、编辑、启停和删除 Profile；
- 选择本机服务与模型；
- 勾选一个或多个入口协议，并实时显示该组合兼容的上游；
- 服务下拉保留不兼容项文字说明，禁止静默隐藏或自动替换；
- 配置默认模型和唯一别名，模型过多时折叠为前 3 个；
- 单个操作显示进行中状态，窄窗下表单和模型行切换为单列；
- 显示每个 Profile 的模型数量与健康状态；
- 客户端模型来源可选择 Profile；
- 客户端显示当前 Profile、入口协议和聚合模型数量。

AI Service、Gateway Profile、Capability Route 使用不同命名，避免“路由”一词同时表示三件事。

## Compatibility

- 现有直连与单服务网关绑定不改写、不失效。
- 旧 `ai-services.json` 和 `ai-provider-import-bindings.json` 字段保持可读。
- 缺少 `entry_protocols` 的旧 `ai-gateway-profiles.json` 读取时自动回填，不改写原文件也可继续工作。
- 同一 Profile 同时服务多个客户端协议，不要求复制成一 Profile 一协议。
- Profile 删除前检查客户端绑定；仍被引用时拒绝删除。
- 模型目录始终来自本机持久化路由，不因上游 `/v1/models` 不可用而整体消失。

## Rejected alternatives

- **把所有已添加服务自动合成一个池。** 用户无法区分暴露范围和凭据边界，模型名冲突也不可控。
- **每个客户端仅切回“活动服务”。** 客户端无法在同一入口内自主选择多个模型。
- **复用 Capability Route 作为客户端路由。** 两者选择主体、失败语义和观测口径不同。
- **自动跨模型降级。** 会改变回答、工具行为和成本，第一版必须显式选择。
- **另起一个独立网关进程。** 会重复令牌、端口、凭据和客户端注入逻辑，并增加生命周期故障面。

## Consequences

- `/v1/models` 从透明转发变成 Agent 自己生成聚合并结果。
- 内存中的 GatewayBinding 从“一个服务”细化为“一条模型路由”。
- 用量台账需要区分公开模型和真实上游模型。
- 客户端是否出现模型选择器仍由客户端能力决定；网关只能保证模型目录和路由可用。
- 同一 Profile 可以混合不同上游协议的模型；客户端只会看到并调用与自身入口协议兼容的那一部分模型，避免“目录可见但请求失败”。

