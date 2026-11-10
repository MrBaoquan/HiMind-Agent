# AI 客户端适配与代理网关成熟度方案

日期：2026-11-10
状态：方案待评审
相关：ADR [0101](../../docs/decisions/0101-agent-loopback-inference-gateway.md)、[0113](../../docs/decisions/0113-agent-inference-gateway-and-injection-matrix.md)、[0114](../../docs/decisions/0114-client-asset-sync-and-verification.md)、[0116](../../docs/decisions/0116-ai-service-client-registration-lifecycle.md)、[0122](../../docs/decisions/0122-ai-client-desired-state-synchronization.md)、[0123](../../docs/decisions/0123-ai-service-api-key-pool.md)

## 0. 结论先行

**不建议对齐 magpie 的功能面，建议只补它已经验证过的四个工程缺口。**

对标结论按证据分三层：

1. **已达标、且强于 magpie 的部分**——Codex、Claude Desktop、DeepSeek Harness、OpenCode 四个客户端的适配深度；协议互译；写入校验分层；能力观测预降级；直连风险闸门。这些不需要再做。
2. **真实缺口、值得补**——字段级漂移检测、HTTP 传输层健壮性、会话亲和、适配器版本回归夹具。这四项是 magpie 用 1184 个 gateway 测试 + 412 个 agent 测试换来的经验，himind 目前靠 181 个测试 + 代码注释记录同类知识。
3. **明确不补**——80+ 客户端广度、订阅账号复用、请求级 PII 脱敏、局域网/远程网关、金额计价。前三项与本仓库既有 ADR 的边界冲突，后两项与产品形态冲突，理由见第 5 节。

## 1. 现状盘点（带文件与行号证据）

### 1.1 客户端适配覆盖

| 面 | 覆盖 | 事实源 |
| --- | --- | --- |
| 模型服务导入 | 15 个 | `known_adapters()`（`src/app/ai_provider_import.rs:475`） |
| MCP 注册 | 21 个 JSON 目标 + Agent 内建 | `src/app/mcp_targets.rs` |
| 技能投放 | 17 个目录客户端 + 2 个宿主 | `src/skill/clients.rs` |

模型服务 15 个分为两类：

- 手写适配器 9 个：`vscode`、`cc-switch`、`codex`、`workbuddy`、`kimi-code`、`qwen-code`、`claude-code`、`claude-desktop`、`opencode`（`ai_provider_import.rs:459-467`）。每个都承载该客户端专有的写入语义，不共享实现。
- 声明式适配器 6 个：`continue`、`aider`、`crush`、`qoder`、`qoder-cn`、`zcode`（`ProviderConfigAdapter`，`ai_provider_import.rs:6555`）。一条 `AiProviderTargetDefinition` 数据行等于一个客户端，新增不需要写代码。

### 1.2 四个重点客户端的实际深度

| 客户端 | 关键实现 | 已处理的版本约束 |
| --- | --- | --- |
| Codex | `import_codex`（`:3635`）、`build_codex_models_json` | `wire_api="chat"` 已移除必须拒写；`base_instructions` 与 `experimental_supported_tools` 是 0.150 目录解析必需字段；`max`/`ultra` 档位会让整份目录解析失败；`model_catalog_json` 引用缺失与「沿用内置目录」要区分 |
| Claude Desktop | `claude_desktop_active_dir`（`:5128`）、`third_party_enabled_in`（`:5152`）、`persisted_deployment_mode`（`:5167`）、`owned_entry_id`（`:5440`） | 3P 配置库与 1P 目录切换、library meta 归属、模型过滤器只认 id |
| DeepSeek Harness | `src/runtime/deepseek_harness.rs`（7417 行） | 完整 Runtime 生命周期：下载、验签、版本清单、profile 覆盖、`cordis.patch.yml` 增量、MCP/Skill 注入、进程管理 |
| OpenCode | `import_opencode`、`OPENCODE_NPM_*` 三个包常量 | `@ai-sdk/openai-compatible` / `@ai-sdk/openai` / `@ai-sdk/anthropic` 按协议选定；`OPENCODE_CONFIG` 可重定位 |

结论：这四项的适配深度不弱于 magpie，DSH 一项显著更强（magpie 只写 `llm-pi-ai` 一行 provider，himind 管整个 Runtime 生命周期）。

### 1.3 网关能力现状

`src/app/inference_gateway.rs` 6450 行，`InferenceGateway::start`（`:354`）。

已实现：

- 入站三协议：`/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/v1/models`（`path_protocol`，`:624`）。
- 三向互译：`anthropic_openai.rs`（801 行）、`anthropic_responses.rs`（1652 行）、`chat_responses.rs`（1031 行）。
- per-binding 令牌、`x-himind-surface` 表面识别、Claude Desktop 中性别名（`claude_desktop_model_alias`）。
- 同请求路由故障转移（`send_with_route_failover`，`:1008`）：401/403/408/429/5xx/网络错误切换，400/422 不切换。
- 路由冷却 60s（`src/store/ai_route_health.rs`）、能力观测 7 天过期预降级。
- 资源边界：入站 64 MiB、出站 24 MiB、连接 32、单客户端并发 8。
- 用量台账 `ai-usage-local.jsonl`（`src/store/local_usage.rs:24`）。

### 1.4 已验证的缺口

以下每条都在代码里核实过，不是推测。

#### 缺口 A：漂移检测只比摘要，不读客户端文件

`desired_state_for_binding`（`ai_provider_import.rs:1182`）只计算 `desired_revision` 并与 `applied_revision` 比对，**全过程不读客户端配置**。后果：

- 用户手工把 Codex 的 `model_provider` 改回去，或别的工具（cc-switch、企业脚本）覆写了 `[model_providers.himind]`，状态仍显示 `synced`。
- `SYNC_STATE_CONFLICT` 在 `:527` 声明，全文件只有这一处出现——**从未被赋值**。`conflict` 是一条永远走不到的状态。
- `SYNC_STATE_FAILED` 只在 `mark_binding_sync_failure`（`:2575`）里出现，也只覆盖显式同步失败，不含外部改动。
- ADR 0122 P1 写的「Agent 重启时执行一次 drift 检查，只标记不写入」未实现：`refresh_all_binding_sync_states`（`:1358`）的调用点只有 `save_ai_service`、`clone_ai_service` 和 profile 变更（`commands.rs:2905`、`:3562`、`:3612`），启动路径 `app/ui.rs:235` 没有调用。

这是当前实现里**唯一会让用户误以为仍在用平台模型、实际早已失效的静默失效点**。

#### 缺口 B：HTTP 入站不支持 chunked 请求体

`read_request`（`:539`）只读 `content-length`，然后把 body 截断到该长度。全文件 `transfer-encoding` 出现次数为 **0**。

后果：使用 `Transfer-Encoding: chunked` 发请求的客户端（.NET `HttpClient` 的流式上传、部分 Java/Node 客户端）会被解析成空 body 或错误 body，表现为上游 400 或模型名解析失败，错误发生在网关内部、客户端只看到莫名其妙的失败。

同类问题：所有响应固定 `connection: close`（`:4092`、`:4309`），长会话下每轮重建 TCP，Claude Code / Codex 这类高频小请求的客户端会有额外延迟，并且部分客户端对 `close` 的复用假设不同。

#### 缺口 C：无会话亲和

`send_with_route_failover` 按候选顺序选路，候选顺序由健康状态决定。同一会话的连续请求可能落到不同上游账号/路由，**提示词缓存全部失效**。magpie 明确把「对话留在回答它的那个账号上」作为禁止跨账号的规则（gateway-routing 子系统文档），himind 没有对应机制。

这一项即使不引入多账号，在「一个 Profile 同模型配多条上游」时也有直接收益。

#### 缺口 D：无协议面扩展（Gemini / Ollama shim）

- Gemini 入站缺失。`ai_service_templates.rs:285` 里 `gemini_native` 是**被显式拒绝**的未知 `api_mode`。
- Ollama 兼容 shim 缺失（`/api/version`、`/api/tags`、`/api/show`）。部分客户端（VS Code Copilot 的自定义端点、Visual Studio）只接受「Ollama 服务器」形态的自定义端点，没有 shim 就接不进来。

#### 缺口 E：适配器缺少版本回归夹具

`ai_provider_import.rs` 有 121 个 `#[test]`，`inference_gateway.rs` 有 60 个，全仓库 1744 个 Rust 测试。但适配器的版本知识目前是**代码注释**（例如 Codex 0.150 的三条约束、`max` 档位会让目录解析失败）。没有一处「拿一份真实版本的配置文件，断言写入结果」的夹具。

magpie 的做法是每个客户端一套 `*_test.go`（`internal/agent/` 412 个测试文件），配合真实版本样本。himind 缺这一层，所以客户端升级后只能等用户报错。

#### 缺口 F（次要）：路由健康键不含凭据

`route_key(base_url, upstream_model)`（`ai_route_health.rs:33`）只由地址和模型组成。同一地址同一模型配多把 Key 时，一把 Key 被限流会把另一把也标记为冷却。ADR 0123 的 Key 池在平台侧，Agent 侧当前是单 Key，所以现在不触发；一旦 P2 引入多 Key 就必须一起改。

## 2. 差距对比表（与 magpie 逐项）

| 能力 | magpie | himind 现状 | 判断 |
| --- | --- | --- | --- |
| 客户端数 | 80+ | 15 | 不追 |
| 本机网关协议面 | Chat / Responses / Messages / Gemini + Ollama shim + Codex 目录端点 | Chat / Responses / Messages | 补 Gemini + Ollama |
| 协议互译 | 四向 | 三向 | 达标 |
| HTTP 传输 | net/http（chunked / keep-alive / TLS 全套） | 自研最小 HTTP，仅 content-length | 补 chunked，可选 keep-alive |
| 漂移检测 | 字段级（`applied.json` 对逐字段 `Check`） | 修订摘要，不读文件 | 补 |
| 会话亲和 | 有（按会话 id / 首消息摘要） | 无 | 补 |
| 路由策略 | 11 种（smart/order/rotate/usage/pace/weight/free/fast/drain/cheap/expiring） | 健康排序 + failover | 抽象接口，先做 3 种 |
| 账号池 | 核心能力 | 无（ADR 0123 在平台侧） | 不做 |
| 请求级 PII 脱敏 | 有 | 仅日志脱敏 | 不做 |
| 网关中间件 | JS 钩子 | 无 | 不做 |
| WSL 孪生 | 完整（NAT/桥接、符号链接、休眠态） | 无 | 按需 |
| 用量成本计价 | 有 | 明确不做（ADR 0113 §4） | 维持 |
| 写入校验分层 | 无 | `file_written` / `file_verified` / `client_load_verified` | himind 更强 |
| 直连风险闸门 | 无 | 有（`ai_provider_import.rs` §3） | himind 更强 |
| 兼容降级 + 能力观测 | 无 | 有（`ai_compatibility.rs` 2764 行） | himind 更强 |

## 3. 分阶段方案

每个阶段独立可交付、独立验收。P0 与 P1 互不依赖，可并行。

### P0：字段级漂移检测（最高优先）

**目标**：客户端配置文件被外部改动时，状态从 `synced` 变为 `conflict`，并给出具体字段。

**设计**

1. `AIClientAdapter` 增加两个方法（带默认实现，避免一次性改 15 个适配器）：

```rust
/// 适配器自己认为「由我托管」的字段当前落盘值。键名是适配器私有约定。
fn applied_fields(&self, options: &Options) -> Result<Value, String> {
    Err("not implemented".into())
}

/// 从当前落盘值里读出托管字段，与 applied_fields 同构。
fn current_fields(&self, options: &Options) -> Result<Value, String> {
    Err("not implemented".into())
}
```

2. `AIProviderImportBinding` 增加一个字段（旧簿记缺省兼容）：

```rust
/// 最近一次成功写入后，托管字段的落盘快照。不含凭据，只含可比较的非秘密值。
#[serde(default, skip_serializing_if = "Option::is_none")]
applied_fields: Option<Value>,
```

3. `desired_state_for_binding` 在算完 `desired_revision` 之后追加一段比对：

```
current = adapter.current_fields()
applied = binding.applied_fields
若 current 与 applied 有差异：
    → 区分两类差异
      (a) 只缺托管键 / 托管键值被改 → reason_codes += client_fields_modified → sync_state = conflict
      (b) 托管段整体消失 → reason_codes += client_managed_segment_removed → sync_state = conflict
      (c) 文件无法解析 → reason_codes += client_config_unparseable → sync_state = blocked
    用户在自己的其它键上新增内容不进入比较（适配器只返回托管字段）。
```

4. 新增原因码常量，并接进 `AIProviderSyncPlan.reason_codes` 与前端 `sync_state` 判断（`frontend/src/pages/AiToolsPanel.tsx:129` 已有 `conflict` 分支，只需后端真的产出它）。

5. 启动时执行一次只读检查：在 `app/ui.rs:235` 网关启动之后调用 `refresh_all_binding_sync_states`，**只标记不写入**（ADR 0122 §5 的原意）。

6. 同步行为：`sync_binding` 遇到 `conflict` 默认拒绝写入，返回「客户端配置已被外部修改」，由 UI 提供「覆盖外部修改」显式按钮，二次确认后才走 `replace: true` 路径。

**改动面**

| 文件 | 改动 |
| --- | --- |
| `src/app/ai_provider_import.rs` | trait 方法、binding 字段、比对逻辑、原因码、`sync_binding` 拦截 |
| 15 个适配器 | 各自实现 `applied_fields` / `current_fields`（声明式的 6 个可在 `ProviderConfigAdapter` 统一实现） |
| `src/app/ui.rs` | 启动检查调用 |
| `frontend/src/pages/AiToolsPanel.tsx`、`AiProvidersPanel.tsx` | conflict 提示与「覆盖外部修改」确认 |
| `frontend/src/services/agentApi.ts` | 类型补 `conflict` 原因码 |

**验收**

- 手工把 Codex `config.toml` 的 `model_provider` 改成 `openai`，状态变为 `conflict`，原因码 `client_fields_modified`，差异指出 `model_provider`。
- 删除 `[model_providers.himind]` 整段，原因为 `client_managed_segment_removed`。
- 在 `config.toml` 里新增 `notify` 数组（用户自己的键），状态保持 `synced`。
- 把 `config.toml` 改成非法 TOML，状态为 `blocked` 而不是崩溃。
- 未实现 `current_fields` 的适配器行为与今天完全一致（默认实现返回错误时视为「无漂移信息」，不改变 `synced`）。
- 启动后不做任何写入（用文件 mtime 断言）。

**工作量**：中。核心逻辑集中在一个函数，主要成本是 15 个适配器各自提取托管字段。建议先做 codex、claude-code、claude-desktop、opencode、zcode 五个，其余走默认实现后续补。

**落地记录（2026-11-10）**

已实现，与本节设计的差异与取舍：

- `managed_fields` 与 `current_managed_fields` **共用同一套提取函数**（`codex_managed_fields_from_document`、`read_claude_env_managed_fields`、`claude_desktop_entry_managed_fields`、`opencode_current_managed_fields`、`provider_config_managed_fields`）。「我写了什么」直接取写入后的落盘结果，因此两侧天然同构，不会因为维护两份期望而误报。
- 「用户自己的键」由 `segments` + 键清单共同界定：`segments` 缺失报 `client_managed_segment_removed`，键清单只含 HiMind 写过的键。
- 凭据两侧都用 `<secret>` 占位符。单测 `drift_detail_never_prints_secrets` 抓到过一次真实泄漏（差异说明把明文打了出来），已修为「已变更」而不打印值。
- 启动检查放在 `app/ui.rs` 网关启动之后，用 `catch_unwind` 包住，任何失败都不阻断启动。
- 声明式适配器只实现了 ZCode（双文件、三个规则数组，结构最复杂）；Continue / Aider / Crush / Qoder 返回 `Unsupported`，不参与比对。
- 前端：`AiToolsPanel` 在 `conflict` 时先弹确认框（展示 `drift_detail`），确认后才带 `accept_drift` 调用；行内也显示差异摘要。

测试：`app::ai_provider_import::tests` 130 个全绿（新增 9 个），`capability::service::tests` 51 个全绿（冻结契约已按有意变更更新）。

### P1：网关传输层与协议面

**目标**：让「本来能连」的客户端都能连上，且长会话不因为传输层细节失败。

#### P1-a 支持 chunked 请求体

`read_request` 增加 `transfer-encoding: chunked` 分支：按 chunk 长度逐段读，拼成 body，再走同一条 `route`。同时把 `expect: 100-continue` 与 chunked 的组合测到（当前只有 content-length 路径有测试）。

#### P1-b（可选）keep-alive

当前所有响应 `connection: close`。改成按请求头 `Connection: keep-alive` 复用连接，需要把 `route` 的返回从「写 stream」改成「产出响应供写出方决定是否关闭」。改动大、收益中等，**建议先不做**，只记录为已知限制。

#### P1-c Gemini 入站

新增 `path_protocol` 分支：

```
/v1beta/models/{model}:generateContent      -> gemini
/v1beta/models/{model}:streamGenerateContent-> gemini
/v1beta/models                               -> gemini models 列表
```

配一个 `gemini_translate.rs`：`contents` ↔ Chat `messages`，`generationConfig` ↔ `temperature` / `max_output_tokens`，`tools[].functionDeclarations` ↔ Chat `tools`，以及 `usageMetadata` ↔ `Usage`。上游可以是三种协议任意一种，走现有互译。

解锁：Gemini CLI 接非 Google 上游（ADR 0101 §10 B 档里唯一的阻塞项）。

#### P1-d Ollama shim

三个只读路由，把 `/v1/models` 的目录换个形状输出：

| 路由 | 返回 |
| --- | --- |
| `GET /api/version` | `{"version":"0.0.0-himind"}` |
| `GET /api/tags` | `{"models":[{"name":<exposed_model>,"model":<exposed_model>,"size":0,"details":{...}}]}` |
| `POST /api/show` | `{"capabilities":[...],"model_info":{...}}`，能力从 `known_model_metadata` 推导（`completion` 恒有，`tools`/`vision`/`thinking` 按元数据） |

然后对 `/api/chat` 找一个最小映射（可选；不做也能让客户端完成探测并走 `/v1`，取决于客户端行为，需要实测）。

解锁：只接受 Ollama 形态自定义端点的客户端。

#### P1-e 会话亲和

在 `GatewayBinding` 之上加一层「会话 → 上次使用的 binding id」的进程内映射：

- 会话键优先级：客户端专有头（WorkBuddy/CodeBuddy 的 `X-Conversation-ID`、Codex 的 `x-codex-window-id`）→ 请求体里可识别的会话字段 → 首条 user 消息的稳定摘要（magpie 同样做法）。
- 命中且该 binding 仍健康 → 排到候选首位；不健康 → 正常按健康排序，不阻塞。
- TTL 30 分钟，容量上限 512 条，LRU 淘汰。
- 这是一个**排序提示**，不是粘性锁；故障转移仍然生效。

**改动面**：`src/app/inference_gateway.rs`（`authorize_candidates` 之后、`send_with_route_failover` 之前插入排序）；新增 `src/store/session_affinity.rs`（进程内，不持久化）。

**验收**

- 同一会话连续 10 次请求全部落到同一 binding id（用两条同模型路由断言）。
- 该 binding 返回 429 后，下一次请求切到另一条，且切换后再连续请求粘在新 binding。
- 不带任何会话标识的请求行为与今天一致。
- 超过 TTL 后重新选择。

### P2：路由策略抽象 + 健康键修正

**目标**：把「健康排序 + failover」抽成可插拔策略，为将来的多 Key 留口，同时不改变现有默认行为。

1. 新增 trait：

```rust
pub(crate) trait RouteStrategy {
    fn id(&self) -> &'static str;
    fn order(&self, candidates: &[GatewayBinding], context: &RouteContext) -> Vec<usize>;
}
```

`RouteContext` 携带会话键、近 N 分钟用量（从 `local_usage` 聚合）、健康状态。

2. 内置三种：`health`（今天的默认）、`order`（按配置顺序）、`rotate`（轮转）。`GatewayProfile` 增加 `routing: Option<String>`，缺省 `health`。

3. 修正健康键：`route_key` 从 `base_url|model` 改为 `binding_id`（或 `base_url|model|credential_generation`）。当前单 Key 下行为等价，多 Key 时不再互相污染。

**不做的部分**：`usage` / `pace` / `cheap` 这类需要额度窗口和价格数据的策略。himind 本机不掌握额度窗口（余额查询是手动刷新的一次性快照，`ai_service_balance.rs`），硬做会给出误导性路由。

**验收**：默认配置下所有现有 gateway 测试通过且候选顺序不变；显式配置 `routing: "order"` 时按 Profile 里的 routes 顺序。

### P3：适配器版本回归夹具

**目标**：客户端升级后，适配器错误在 CI 暴露，而不是等用户报错。

**结构**

```
src/app/testdata/adapters/
  codex/
    0.150.1/
      before/config.toml
      before/himind-models.json
      expected/config.toml
      expected/himind-models.json
      case.json            # 目标协议、服务模型列表、默认模型
  claude-code/
  claude-desktop/
  opencode/
  zcode/
```

测试驱动：读 `case.json` → 构造 `AIClientCredential` → 调用适配器的构建函数（`build_codex_config_toml` 等纯函数，不是 `import_*` 全链路）→ 与 `expected/` 逐字节比对。

优先补 4 个：**codex**（版本约束最多、最容易写坏）、**claude-desktop**（目录语义复杂）、**opencode**（npm 包选择）、**zcode**（双文件）。

新增客户端版本时的流程写进文档：收到客户端升级 → 用新版本生成一次真实配置 → 加入 `before/` → 修正适配器 → `expected/` 固化。

**工作量**：中。夹具本身不难，成本在每次采集真实样本。

### P4（按需）：WSL 孪生

**仅当**确有用户在 WSL 里跑 Codex / Claude Code 时再做。当前 `src/` 全仓库无任何 `wsl` 引用。

若要做，最小可行为：

- 枚举运行中的发行版（不主动启动已停止的）。
- 读出 `<distro>/.codex/config.toml`、`<distro>/.claude/settings.json`，走 `\\wsl.localhost\<distro>\...`。
- 网关需要额外监听 WSL 网络适配器地址（`vEthernet (WSL)`），否则发行版里的客户端连不上 `127.0.0.1:18181`。这一步有安全含义，必须单独评审：它把网关从「只监听环回」扩大到「监听一个虚拟网卡」，与 ADR 0101 §8 的边界不一致。
- 目标 id 采用 `<id>@wsl:<distro>`，与现有 `AIClientAdapter::id()` 的 `&'static str` 冲突，需要改成 `String`。

结论：**代价大、触及安全边界、需求未验证**。建议先记录为已知限制，不排期。

## 4. 明确不做的项与理由

| 项 | 理由 |
| --- | --- |
| 追平 80+ 客户端 | 剩余客户端多为海外小众 harness，每多一个都是「每次版本升级跟一次」的长期负债。ADR 0101 §10 已按 A/B/C/D 分档，B 档尚未做完，扩到 C/D 档没有收益。 |
| 订阅账号复用（读本机 Claude/ChatGPT 登录态） | ADR 0101 §7 已明确不做：会绕开平台计量与治理，与「组织分发 + 平台托管」的信任模型直接冲突。 |
| 请求级 PII 脱敏 | magpie 为此投入 81 个文件 + 本地 1.6 GB 模型。himind 已有日志脱敏（`redact_message`），覆盖了实际需要。 |
| 网关 JS 中间件钩子 | himind 的插件体系是 Capability 能力包，引入 Bun + JS 运行时是架构错配。需要「按 workspace 注入系统提示词」这类能力时，用 Rust 侧的策略钩子而不是通用 JS 沙箱。 |
| 局域网共享 / 远程网关 / WebDAV 同步 | 与「单用户桌面 Agent」定位冲突；局域网暴露直接违反 ADR 0101 §8、ADR 0022。 |
| 金额计价 | ADR 0113 §4 已决策：本机口径不显示金额（用户自己的 Key，金额只在服务商控制台有意义）。不要因为 magpie 有就加回来。 |
| 账号池 / 额度耗尽自动切换 | ADR 0123 把 Key 池放在平台侧（`ai-control`），Agent 侧只消费。重复在 Agent 实现会造成两套事实。 |
| 模型目录接 models.dev | 现有模型元数据来自工作台模板 + `known_model_metadata` 内置表，与服务分级和凭据绑定；换成公共目录会丢掉这些约束。可作为**补充**：内置表未命中时查询 models.dev 的上下文窗口与价格档，仅用于展示，不参与路由。 |

## 5. 排期建议

| 阶段 | 内容 | 依赖 | 相对工作量 |
| --- | --- | --- | --- |
| **P0** ✅ | 字段级漂移检测（5 个重点适配器）—— 2026-11-10 已落地 | 无 | 中 |
| **P1-a** | chunked 请求体 | 无 | 小 |
| **P1-d** | Ollama shim | 无 | 小 |
| **P1-e** | 会话亲和 | 无 | 小 |
| **P1-c** | Gemini 入站 + 互译 | 无 | 中 |
| **P2** | 路由策略抽象 + 健康键修正 | 无（但建议在 P1-e 之后，避免改两遍候选排序） | 中 |
| **P3** | 适配器回归夹具（4 个） | 无 | 中 |
| **P4** | WSL 孪生 | 需求验证 + 安全评审 | 大 |

推荐顺序：**P0 → P1-a → P1-e → P1-d → P1-c → P3 → P2 → P4（按需）**。

P0 先行是因为它是唯一一处「用户零感知地失效」。P1-a 排第二是因为它修的是「连不上但你不知道为什么」。P1-e 要在 P2 之前，避免候选排序逻辑改两次。

## 6. 与既有 ADR 的关系

- **ADR 0122**：本方案 P0 是 0122 P1 里「启动 drift 检查」和「用户手工修改返回 `conflict` 而不是覆盖」两条验收项的具体实现。实施后 0122 的 P1 可标记完成。
- **ADR 0113**：P1-e 会话亲和与 P2 路由策略都是网关内部行为，不改变「入口统一、策略逐绑定」的决策，也不改变用量口径。
- **ADR 0101**：P1-c / P1-d 扩展的是「入口线格式」，0101 §5 已把协议翻译列为网关职责，属于范围内。P4 会触及 0101 §8 的环回边界，必须单独立项。
- **ADR 0116**：P0 的 `applied_fields` 与现有 `owned_snapshot` 是两件事——前者是「我写了什么」用于漂移比对，后者是「用户原来是什么」用于还原。两者都保留。

## 7. 风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| `applied_fields` 与适配器实际写入不同步 | 误报 `conflict` | 只在写入成功后由同一个函数产出，不单独维护；未实现 `current_fields` 的适配器默认不参与比对 |
| `conflict` 状态过多导致用户困惑 | 每次外部工具改动都告警 | 区分「托管字段被改」（conflict）与「用户新增自己的键」（不告警）；UI 只对 conflict 要求动作 |
| Gemini 互译遗漏字段 | 客户端拿到结构不对的响应 | 与现有三个 translator 同规格：流内 `error` 必须发出失败事件，失败后不得合成完成事件，`finish_reason` 映射完整 |
| Ollama shim 被客户端当成完整 Ollama | 客户端调用未实现的 `/api/chat` 失败 | 先只提供探测三件套，实测客户端行为后再决定是否补 `/api/chat` |
| 会话亲和在缓存与均衡之间取舍 | 多账号时负载不均 | 亲和只做排序提示、不阻塞故障转移；TTL 30 分钟；提供关闭开关 |
| 启动 drift 检查阻塞启动 | 启动变慢 | 异步执行，只写簿记不写客户端；首个状态查询之前完成 |

## 8. 验收总览

- `cargo test --locked app::ai_provider_import`：新增漂移用例全部通过，旧用例不变。
- `cargo test --locked app::inference_gateway`：chunked、Gemini、亲和、路由策略用例通过，现有 60 个不变。
- 手工验收：Codex / Claude Desktop / OpenCode / DSH 四个客户端在网关模式下各跑一轮真实会话，确认用量台账有记录、模型目录正确、重启客户端后仍指向网关。
- 手工验收：用 `curl -H "Transfer-Encoding: chunked"` 发一个 chat 请求，确认与 content-length 路径结果一致。
