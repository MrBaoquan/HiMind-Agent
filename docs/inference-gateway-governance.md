# 本机推理网关治理基线

日期：2026-11-09
状态：已落地（随代码演进）

## 1. 信任边界

本机推理网关（`src/app/inference_gateway.rs`）只监听 `127.0.0.1`，服务单用户桌面 Agent。
客户端只持有本机令牌，真实 Provider Key 留在 Agent；网关按绑定/Profile 聚合多个上游。

## 2. 直连与网关模式

- **gateway 模式**：客户端配置指向 `http://localhost:<port>`，由网关做协议互译、兼容降级、用量记账。
- **direct 模式**：客户端配置写入真实 Provider 地址与凭据，绕过网关；本机用量不计入台账。

富工具客户端（Codex、Claude Code 等）在 direct 模式下无法获得网关的以下保护：

1. `function_call` / `function_call_output` 历史降级；
2. 工具结果图片/文件移除；
3. `namespace` / `web_search` 扩展工具移除；
4. 400/422 诊断与重试；
5. 本机 request id 与用量归集。

## 3. 直连风险闸门

`src/app/ai_provider_import.rs` 的 `direct_compatibility_warning()` 对富工具客户端始终给出直连风险。
`require_direct_risk_acknowledgement()` 构成后端闸门：

- 风险非空且未显式确认时，`disable_gateway_binding()` 在**改写任何客户端配置之前**返回错误；
- 确认后，绑定簿记写入：
  - `direct_risk_acknowledged_at`
  - `direct_risk_reason`
- 事件日志记录 `直连风险已确认`，作为审计事实。

前端在切换到 direct 前读取 `AIProviderImportStatus.direct_warning` 并弹确认框；确认后才传 `allowDirectRisk=true`。
“停网关并全部解绑”是用户显式整体动作，会记录风险但不再逐客户端二次确认。

## 4. 资源边界

- 入站 body 默认 64 MiB（环境变量 `HIMIND_AGENT_GATEWAY_INBOUND_MIB` 可覆盖，范围 32–512 MiB）。
- 出站 body 预算 24 MiB；fallback 改写后会再次检查，超出则拒绝而不是继续发送。
- 非流式上游响应最多 32 MiB，超出返回 502，不再静默截断成非法 JSON。
- 上游错误体最多读取 1 MiB，避免错误页拖垮内存。
- 图片解码前检查像素数，超过 4000 万像素只保留原图引用，不做解码预览。
- 连接上限 32；单客户端并发请求上限 8（`MAX_INFLIGHT_PER_CLIENT`），超限返回 429 并计入失败用量。
- 长流式请求会持续占用并发槽，防止一个失控客户端挤占其他客户端。

## 5. 可观测性

每个请求生成 `req_himind_<millis>_<seq>`，并写入：

- `ai-usage-local.jsonl` 的 `request_id` 字段；
- 上游失败、兼容诊断、翻译失败日志；
- 兼容诊断的 `gateway_request_id` 提示。

用量聚合额外提供 `failures`、`failure_rate`、`avg_latency_ms`、`p95_latency_ms` 和 `recent_failures`（最近 20 条失败，含 request id、工具、模型、状态、延迟）；前端本机用量面板支持按工具/模型/服务切换分组，展示失败率、P95 延迟、失败列和最近失败列表。

## 6. 运行时能力观测

`ai_capability_observations.rs` 记录“兼容回退后上游成功”的事实：

- key = Provider（未知 Provider 带 host）+ 协议 + 能力名；
- 至少累计 2 次成功回退，后续请求才会提前降级，避免一次偶发 400 造成永久降级；
- 观测 7 天后过期，Provider 修复后不会永久被预降级；
- 预降级处理 Responses 入口的 `reasoning_history` / `tool_result_media` / `tool_history` / `unsupported_tools`；
- 观测写入 `ai-capability-observations.json`，最多 128 条；
- 状态接口返回 `capability_observations`，日志记录 `能力观测` 与 `能力观测预降级`。

## 7. 凭据脱敏

- `GatewayBinding` 不再派生 `Debug`，自定义 `Debug` 把 `api_key` 与 `token` 输出为 `<redacted>`。
- `redact_message` 增加 `x-api-key:`、`api-key:`、`authorization:` 标记，并识别裸 `sk-...` 令牌。

## 8. 多路由健康切换

- 同一 `exposed_model` 允许配置多条上游路由；“别名 + 服务 + 上游模型”完全相同仍会被拒绝。
- `ai_route_health` 在进程内记录路由健康，键为服务（凭据）+ 规范化端点 + 上游模型 + 协议，不把 API Key 写入状态：
  - 401/403/408/429/5xx 或网络失败 → 冷却 60 秒；
  - 2xx → 立即恢复；不同账号或协议即使共享端点/模型也不会互相冷却。
- `gateway_bindings` 在同模型组内按健康状态稳定排序，健康路由优先；
  发送阶段还会在同一请求内按候选顺序切换（见第 10 节）。
- 这是快速失败切换，不持久化；重启后重新探测。

## 9. 媒体缓存回收

- 存储页提供「清理推理媒体缓存」：保留最近 30 天且最多 2000 个文件。
- 只删除 `media-inbox` 下的自动外置原图/预览，并同步清理
  `capability = inference.media-normalize` 的登记记录。
- 用户生成的其它媒体（`image.generate` 等）不受影响。

## 10. Responses 协议兼容方言语义

上游即使声称支持 Responses，也可能对历史 `input[].type = "reasoning"` 返回泛化 400。网关只在真实 4xx 后先移除这些不可重放的推理条目并重试，保留普通消息、function call/output、工具定义和顶层 reasoning 配置；这属于协议收敛，不按 Provider 名称分支。详见 [ADR 0034](adr/0034-inference-protocol-and-route-isolation.md)。

## 11. 同请求路由切换

- 首次写出前，网关会按候选顺序尝试同一上游协议的路由：直通路径要求 Provider 与上游模型一致；Responses→Chat 转译路径允许不同服务、账号和上游模型，并为每个候选独立转换、映射模型和执行兼容规则。跨线协议的备用仍不会接收另一种协议已序列化的负载。
- 401/403/408/429/5xx 或网络失败会切换到下一条候选；成功响应、400/422 不会切换；
- 每条候选最多进行一次瞬时 5xx 重试，外层不会重新遍历整组候选，因此总尝试范围固定。
- 所有候选都失败时才把最后一个失败返回客户端。
- 路由健康状态是进程内状态，Agent 重启后从健康开始。
- 能力观测是保守预降级；若 Provider 行为再次变化，7 天后观测过期并重新探测。

流式翻译器（Responses/Anthropic/Chat 四向）区分成功、失败与截断：

- 上游流内 `error` / `response.failed` 必须向客户端发出失败事件；
- 失败后不得再合成 `response.completed`；
- `finish_reason=length` 映射为 `response.incomplete`；
- 失败请求按 502 记用量，不按 HTTP 200 记成功；
- Chat SSE 在完整事件边界上已出现非空 `finish_reason` 时，允许上游随后直接 EOF，无需额外 `[DONE]`；半截 JSON、未完成事件或空终态仍然失败。

## 6. 验收

```powershell
cargo test --locked app::inference_gateway::tests
cargo test --locked direct_risk
cargo test --locked rich_tool_direct_override_has_explicit_warning
npm run build   # frontend
```

后续计划见 `docs/multi-agent-maturity-roadmap.md` 与网关 P0/P1 清单。
