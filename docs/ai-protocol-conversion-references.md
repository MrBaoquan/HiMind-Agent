# AI 协议转换与客户端模型过滤参考

日期：2026-10-27  
目标：为 Anthropic Messages ↔ OpenAI Responses 以及多客户端模型注册确定可复用边界。

## 参考实现

### cc-switch

- 仓库：https://github.com/farion1231/cc-switch
- 审阅提交：`dd81bc95bdf48bf218d348c59058196afdcff4a0`（v4.0.21 同期）
- 许可：MIT
- 关键文件：
  - `src-tauri/src/proxy/providers/transform_responses.rs`
  - `src-tauri/src/proxy/providers/streaming_responses.rs`
  - `src-tauri/src/proxy/providers/transform_codex_anthropic.rs`

采用的结构：

- Anthropic system → Responses `instructions`；
- message content 中的 `tool_use` / `tool_result` 提升为顶层 `function_call` / `function_call_output`；
- Anthropic image/document → Responses `input_image` / `input_file`；
- `max_tokens` → `max_output_tokens`，小于 16 的探测请求抬到 16；
- 函数工具显式写 `strict: false`，保留 Anthropic 可选参数语义；
- Responses output message/function_call → Anthropic text/tool_use；
- Responses 命名 SSE 事件转换为 Anthropic `message_start`、`content_block_start`、`content_block_delta`、`content_block_stop`、`message_delta`、`message_stop`；
- 非法/未完成函数参数按响应状态决定报错或回落为空对象。

暂不照搬的高级能力：

- hosted web search；
- Anthropic thinking signature / reasoning encrypted content 跨协议回放；
- prompt cache 字段与供应商专属扩展。

这些能力需要独立语义桥接，不能在主路径中静默丢弃或伪装成等价。

### cc-desktop-switch

- 仓库：https://github.com/lonr-6/cc-desktop-switch
- 审阅提交：`1fb673c59ea72a9297eda9d2553a1a40b7fd32e7`
- 关键文件：
  - `backend/api_adapters.py`
  - `backend/proxy.py`

采用的产品策略：

- Claude Desktop 只展示明确映射、且兼容的模型槽位；
- 不把兼容性未知的模型全部注册给客户端；
- 拒绝未映射路由，不做静默默认模型回退；
- OpenAI Responses 曾被列为暂不自动导入项，说明仅做配置切换不足以覆盖 Responses 上游；必须单独实现协议转换。

## HiMind 采用方案

1. Anthropic Messages 入口支持 OpenAI Chat 与 OpenAI Responses 两种 GPT 上游。
2. Gateway Profile 不再要求所有模型兼容所有入口；每个模型进入 Profile 前必须兼容至少一个已选客户端协议。
3. 每个已选客户端协议至少需要一个启用中的兼容模型。
4. 客户端绑定时按入口协议过滤模型目录；运行时解析令牌后也只返回该客户端兼容的路由。
5. UI 逐协议展示可用上游，并在每条模型路由上标明会注册到哪些客户端。

## 未纳入本轮

- Responses 入口 → Anthropic 上游；
- Chat 入口 → Responses 上游；
- Anthropic thinking 签名跨协议回放；
- hosted web search 与供应商缓存计费字段。
