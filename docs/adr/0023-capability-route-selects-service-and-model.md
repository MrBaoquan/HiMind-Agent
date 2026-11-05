# 能力路由的选择单位是「服务 + 模型」

Status: accepted (2026-11-05)

## Context

ADR 0016 已经把能力执行的契约定成一个信封：

```text
request:  capability, service, model, input, provider_options, constraints
plan:     primary + ordered same-capability fallbacks + candidate reasons
result:   capability, service, adapter, model, execution_mode,
          request_id, provider_request_id, usage, artifacts, warnings, attempts
```

`model` 与 `service` 是并列字段，路由计划也要能解释"为什么选了这个模型"。

实现停在了 `service` 这一级：

- `capability_route_plan(capability, requested_service)`（`src/store/ai_services.rs:1054`）
  的入参只有服务，没有模型；
- `PersistedAIServiceSelection`（`:185-200`）用三个互不相通的字段表达路由：
  `active_service_id`（对话主选）、`media_services`（媒体主选）、
  `capability_fallbacks: BTreeMap<String, Vec<String>>`（备用顺序，只有服务 ID）；
- 计划输出的 `fallbacks` / `attempts` 连 `model` 字段都没有（`:1231-1234`）。

后果是三处真实故障：

**一、一个服务的多个模型无法区分。** `CustomAIService.models: Vec<String>` 可以列出
任意多个对话模型（实机存在 28 个模型的服务），但在能力路由里 `selected.model`
恒等于单值 `service.model`（`:1123`、`:1195`）。用户在会话里能换模型，那个选择
存在 DSH 的 settings.yaml，不进能力路由，也不参与备用顺序。

**二、备用切换会串模型。** `with_service()`（`src/app/ai_capability_bus.rs:297-302`）
替换 `service` 但保留请求体里的 `model`。主选是 `doubao-seed-1-6`、降到
OpenCode 时仍带着 `doubao-seed-1-6` 去请求，通常直接报错。这是个静默失败。

**三、媒体能力只有一个模型槽。** `MediaModelBinding`（`ai_services.rs:108-113`）
是 `{adapter, model, execution_mode}`，生图/改图/视频/TTS/转写各只能填一个模型。
"某个服务有几个生图模型、路由想用第二个"在产品里没有表达方式。

**四、`vision.describe` 的计划与执行不一致。** 计划走 chat 分支报 `service.model`
（`:1120-1125`、`:1192-1197`），实际执行优先用 `media["vision.describe"].model`
（`src/app/vision_describe.rs:46-49`），前端能力判定又要求显式 vision 绑定
（`frontend/src/pages/aiServiceCapabilities.ts:66-69`）。路由表上显示的模型不是
真正会用的那个。

ADR 0020 已经证明"路由项携带模型"在本仓库是可行且必要的：`GatewayModelRoute`
的 `upstream_model`（`src/store/gateway_profiles.rs:24-30`）就是这个形状，还配套了
`service_has_model` / `validate_routes` 校验。但那套是给**外部客户端**用的；
能力路由服务的是 HiMind 自身，两者语义不同（ADR 0020「Capability Route 继续只服务
HiMind 自身能力」）。所以不是复用，而是把同一条原则补到能力路由这一侧。

## Decision

### 一、路由项是一等对象

```rust
/// 一条能力路由项：在某个服务上使用某个模型。
/// `model` 为空表示跟随该服务在这个能力上的默认模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct CapabilityRouteEntry {
    pub service_id: String,
    #[serde(default)]
    pub model: String,
}
```

持久化从三个平铺字段收敛为一张有序表：

```rust
struct PersistedAIServiceSelection {
    #[serde(default)]
    schema_version: u32,                       // 2
    #[serde(default)]
    capability_routes: BTreeMap<String, Vec<CapabilityRouteEntry>>,
}
```

`Vec` 的下标 0 是主选，其余按用户排定的顺序是备用。一个能力只有一张表，
"主选"与"备用"不再用两套字段表达。

### 二、`model` 为空表示跟随服务默认

空字符串不是"没配置"，而是一个明确的语义：**跟随该服务在此能力上的默认模型**。

| 能力 | 默认模型来源 |
| --- | --- |
| `chat` | `service.model` |
| 媒体能力 | `media[capability].model`，`image.generate` 回落 `image_model` |

这样迁移后旧行为逐字不变（旧数据全部迁移成 `model: ""`），且用户改服务默认模型时，
未显式选择过模型的路由会跟着变——这符合"跟随"的直觉。

计划输出必须把这个区别显示出来，不能让人猜：

```json
{ "service_id": "ark-doubao", "model": "doubao-seed-1-6", "model_source": "service_default" }
```

`model_source` 取 `"explicit"` 或 `"service_default"`。

### 三、候选的原子是 (服务, 模型)

`schema_version` 升到 `himind.ai-capability-route.v2`。`candidates` 从"每服务一条"
改为"每个可用的 (服务, 模型) 一条"：

```json
{
  "service_id": "...", "display_name": "...", "model": "...",
  "model_source": "explicit | service_default",
  "adapter": "...", "execution_mode": "sync",
  "eligible": true, "eligibility": "selected | fallback | candidate",
  "reason": "explicit_request | configured_default | ...",
  "credential_present": true
}
```

`attempts` 与 `fallbacks` 都带 `model`，形状一致。执行时每个尝试用自己的模型。

候选集合的来源统一为"服务在该能力下的模型目录"：对话是
`dedupe([service.model] + service.models)`，媒体是
`dedupe([media[cap].model] + media[cap].models)`。

### 四、媒体能力也能声明多个模型

```rust
pub(crate) struct MediaModelBinding {
    pub adapter: String,
    pub model: String,                // 默认模型
    #[serde(default)]
    pub models: Vec<String>,          // 该能力下的可选模型；留空则只有 model 一个候选
    #[serde(default)]
    pub execution_mode: Option<MediaExecutionMode>,
}
```

`MediaModelBinding` 带 `deny_unknown_fields`。本决策接受"旧版本 Agent 读到新数据
会失败"，因此这个属性继续保留，只作为字段名的严格校验，不再承担前向兼容职责。

### 五、执行按尝试取模型

```rust
fn with_attempt(input: &Value, service_id: &str, model: &str) -> Value
```

替换 `with_service`，显式写入该尝试自己的模型；`model` 为空时删掉请求体里的
`model` 键，交回服务绑定解析。`preflight` 增加 `model` 形参，只校验该尝试的模型
是否存在，而不是回落到 `service.model`。

这条同时修掉"主选模型串到备用服务"的静默失败。

### 六、显式模型失效时不静默降级

服务被编辑后，路由项里钉死的模型可能已不在模型目录里。此时该候选
`eligible: false`、`reason: "model_unavailable"`，**不进入 `attempts`**。
路由状态若因此没有可执行项，则 `state: "missing"` 并在 `reason` 说明。

不允许"模型找不到就换成服务默认模型悄悄继续"——那会让用户以为自己在用 A 模型。

### 七、`vision.describe` 与执行对齐

`vision.describe` 的默认模型来源改为 `media["vision.describe"].model`，缺失时回落
`service.model`。路由计划、`preflight`、`vision_describe.rs` 三处必须给出同一个答案。

### 八、硬迁移

旧的 `active_service_id` / `image_service_id` / `media_services` / `capability_fallbacks`
从持久化结构中删除。读取时若检测到旧字段，一次性转换为 `capability_routes` 并写回
新结构；此后不再维护旧字段。

降级到旧版本 Agent 会丢失路由配置，这是本决策显式接受的成本。

`public_snapshot()` 与 `media_provider_catalog()` 不再导出任何平铺旧键：前端已全部
改为读 `capability_routes`，保留派生投影只会留下第二个事实源。

### 九、读入口保留，写入口只留一条

`active_id()` / `media_service_id()` 的签名不变，内部改为读 `capability_routes` 的
第一项。它们有 20 多处调用（`builtin_ai_router`、`commands.rs`、`vision_describe`、
`local_*` 等），保留签名把影响面从几十处收敛到一处。

写入口**只保留** `set_capability_routes()`。`set_active()` / `set_media_service()`
只作为内部测试辅助保留（`#[cfg(test)]`），非测试构建里不存在，因此不可能被生产
路径误用。它们内部退化成 [`set_primary_route`]——只换主选、把主选的 `model` 置空
（换服务后旧模型通常不再适用）。以下入口已删除，因为它们无法表达模型，保留就
等于给「丢掉模型」留后门：

- `set_capability_fallbacks()` / 命令 `set_ai_capability_fallbacks`
  （写备用项时只能填服务 ID）；
- `capability_fallbacks()`（只返回服务 ID）；
- `set_image_service()` / `image_service_id()` / `resolve_image_service()`
  （`image.generate` 的旧别名，已由 `set_media_service` / `media_service_id` /
  `resolve_media_service` 覆盖）；
- 命令 `set_active_ai_service` / `set_image_ai_service`。

对外暴露的能力 `ai.service.image.set` 同样带上了 `model` 形参（空值表示跟随服务
默认模型），内部改走 `set_capability_routes()`。否则远端插件仍能通过这条能力把用户
钉住的媒体模型擦成服务默认——旧入口换了个壳继续存在。

`set_active_ai_service` 原先还会在切换后同步 DSH 模型目录并广播
`himind:ai-service-changed`。这个副作用已并入 `set_capability_routes`：写对话
路由时比较写入前后的 (服务, 模型)，真变了才重新激活 DSH；清空主选则停掉本机
路由。<br>DSH 侧的"当前模型"也必须来自路由项而不是 `service.model`，
所以 `builtin_ai_router::active_model_alias()` 改为解析对话路由的第一项；
否则用户钉了具体模型，DSH 仍会走服务默认模型。

### 十、与 Gateway Profile 的分工不变

Gateway Profile 继续负责"对外部客户端暴露哪些模型别名"（ADR 0020），
能力路由负责"HiMind 自身某个能力用哪个服务的哪个模型"。两者共享
`service_has_model` 这类校验思路，但不共享数据。

## Consequences

- 一个服务挂 28 个对话模型时，用户可以指定"对话用第 3 个"，并把它排进备用顺序。
- 备用顺序可以跨服务、跨模型：`ark/doubao-seed-1-6` → `opencode/kimi-k2`。
- 主选模型失败降级时，备用服务收到的是**它自己的**模型，不会串。
- 媒体能力获得与对话一致的模型选择能力。
- 路由计划可以解释"为什么是这个模型、它是不是显式指定"。
- `candidates` 数量会显著增加（按模型展开），UI 必须改成两级选择而不是平铺列表。
- 每行的控件不重复解析结果：不再单独占一列「解析结果」。服务下拉的兜底文案
  （生效模型、模型已不可用、执行方式）全部收进 `title`；模型下拉的空值项只写
  「跟随服务默认」。把最长的那串模型名塞进每个下拉的每个选项、或在旁边再列一遍，
  只会让控件在列宽下限处被裁掉；备用 chip 同理，完整值走 `title`。
- 服务只有一个模型时不渲染模型下拉：它只有一个「跟随服务默认」选项，没有选择权，
  只占一行高度。已有钉住的模型（含目录外的旧值）仍然渲染，否则用户没法清掉它。
- 备用顺序是状态而不是控件：只读展示，增删收进状态列的操作菜单。绝大多数行没有
  备用，为它常驻一个下拉会在每一行都占一格。
- 操作菜单里的候选按服务分组，组标题就是服务名。菜单宽度固定 280px：通用的 208px
  皮肤装不下最长的模型名（实测 28 个真实模型里有 2 个会被截断），菜单项文本统一加
  溢出省略，完整值走 `title` 与 `aria-label`。
- 路由表是四列。断点必须量表格自身（`container: ai-routes` 挂在 `.ai-routes-view`），
  不能量设置窗口内容列：`.ai-page` 的 `max-width: 880px` 把表格封在 880px 以内，
  而 `agent-main` 量的内容列在默认窗口是 764px、放大后 1168px，两者从不同步——旧的
  860px 断点在放大窗口时误判成四列，反而把备用 chip 挤到截断。
  四列的完整需求是 104+152+152+131 的列底限、3 个 10px 列间距、左右各 14px 行内边距
  与表格 1px 边框，合计 599px。默认设置窗口 996px 时表格实测 693px，仍在四列内，
  7 行合计 514px（旧两列 1211px）。
- 低于 599px 回落到「能力 + 状态一行 / 主选一行 / 备用一行」，实测 890px；旧的
  「能力整行 + 两列对开」是 1066px，三行并排一类更复杂的形态是 1292px，都更差。
  断点取 598px 而不是 585px：586–598px 这段看起来「没有横向溢出」，实际是末列吃掉了
  右内边距——实测 586/592/598px 时状态列右缘距表格边框只剩 2/8/14px（应为 15px），
  图标按钮贴住边框。`.ai-route-table` 有 `overflow: hidden`，这段越界量不出
  `scrollWidth`，所以必须在真实窗口量末列实际右缘，或在校验脚本里把行内边距与边框
  计入列需求。
- 能力说明最多两行并把完整句留在 `title`；备用 chip 拆成「序号 + 服务名」与模型名
  两行——单行放不下的最长模型名有 204px，两行至少让服务名读得全。
- 旧版本 Agent 无法读取新配置。这是硬迁移的既定成本。
- `ai-service-selection.json` 的结构在一次迁移后与旧版本不兼容。
- DSH 当前模型来自对话路由项，因此"钉住模型"对会话界面同样生效。

## Rejected alternatives

- **在 `capability_fallbacks` 旁边加一个平行的 `capability_models` 映射。**
  主选、备用、模型会变成三套独立表示，三者之间的对齐要靠约定维持。
  这正是当前问题的来源，不能再用同一形状扩展一次。

- **只给对话加模型选择，媒体维持单槽。**
  媒体能力同样存在"一个服务多个生图模型"的真实场景，
  会留下一半能力不能按模型路由的断层。

- **`model` 为空时回落到"该服务的第一个模型"。**
  与服务配置页显示的"默认模型"不一致，用户改默认模型不生效。

- **显式模型失效时静默回落到服务默认。**
  用户以为在用 A 模型，实际在用 B，属于不可观测的错误。

- **复用 Gateway Profile 的 `GatewayModelRoute`。**
  它对外的语义是"暴露给客户端的稳定模型别名"，混用会让内部路由
  继承外部入口的协议约束。
