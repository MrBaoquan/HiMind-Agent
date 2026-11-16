# 插件 AI 服务与模型选择

Agent 提供服务目录、模型选择解析和执行；插件拥有项目偏好、音色与业务参数。
本目录是可复用客户端和界面库，图片/语音示例为接入参考，不是已安装的业务插件。
需要包含 ADR 0030 的 Agent 构建；现有旧安装版不会自动获得这些资源。

## 在插件窗口接入

视图清单声明所需能力（精确到使用的能力，不必声明 `media.*`）：

```json
{
  "external_capabilities": [
    "media.provider.catalog", "media.route.plan",
    "media.image.generate", "media.audio.speech", "media.audio.voice.list"
  ]
}
```

Agent 在有效、已启用的插件视图下提供以下模块，不需要 CDN：

```js
const sdk = new URL('/<plugin-id>/<view-id>/_himind/ai-v1/', location.href);
const { createAiClient, viewTransport } = await import(new URL('client.mjs', sdk));
const { mountServiceSelector } = await import(new URL('service-selector.mjs', sdk));
const client = createAiClient({
  pluginId: '<plugin-id>', projectId: currentProject.id, invoke: viewTransport(),
});
const picker = mountServiceSelector(container, {
  client, capability: 'media.image.generate', filters: { execution_mode: 'sync' },
  onChange(state) {
    submit.disabled = !state.ready;
    // 立即清空依赖旧服务/模型的音色等选择；异步预检完成会再次通知。
    if (state.targetChanged) selectedVoice = null;
  },
});
await picker.loaded;
// 默认使用调用覆盖，其次项目偏好、插件偏好，最后 Agent 默认。
const result = await client.execute('media.image.generate', { prompt: '山间小屋' }, picker.value);
console.log(result.actual, result.attempts, result.usage, result.artifacts);
```

完整可运行视图片段见 [example.html](example.html)。将其复制到自己的插件 UI
目录并声明上述能力；插件身份由宿主窗口校验，SDK 参数不能冒充另一个插件。
SDK 文件也可打包进入插件，在其他客户端通过注入 `invoke(capability, input)` 复用。
宿主预检始终是最终依据，浏览器控件就绪不代表上游已连接健康。

## 公共能力契约

```json
{
  "capability": "media.image.generate",
  "selection": { "mode": "pinned", "service_id": "image-a", "model": "image-two" },
  "fallback": "none",
  "selection_source": "project",
  "payload": { "prompt": "一座山间小屋" }
}
```

将以上参数传给 `media.route.plan`：本地解析与可选输入预检，不产生 Provider 请求。
执行时调用 `media.image.generate` 等能力，移除 `capability` 与 `payload` 外壳，
把业务字段放在顶层。`selection` 三种模式：

| 模式 | 参数 | 语义 |
| --- | --- | --- |
| `agent_default` | 无额外字段 | 执行时使用 Agent 该能力路由 |
| `service_default` | `service_id` | 使用该服务能力绑定的默认模型 |
| `pinned` | `service_id`, `model` | 使用指定模型，失效时报不可用 |

`fallback=none/configured`。跟随 Agent 默认时默认使用 configured；显式服务/模型
选择默认使用 none。只有明确允许 configured 才会尝试已配置的同能力备用路由。
备用模型取备用路由自己的值。此策略会收紧旧显式 service 调用的自动回退行为。

兼容旧 `service: "custom:<id>"` / `model`。只有 service 表示该服务默认模型；
只有 model 表示固定当前解析出的主选服务上的该模型。非空旧字段与 selection 混用报错，
未知 mode、字段、空固定模型均报错。预检与执行调用同一解析器，预检不预留路由；
要防止默认变化，请使用具体固定选择。

`media.provider.catalog` 过滤参数：

```json
{
  "capability": "media.audio.speech",
  "execution_mode": "sync",
  "required_features": ["text_to_speech"],
  "ready_only": true
}
```

能力 ID 同时接受 `media.image.generate` 与 `image.generate`；视觉使用
`media.ai.describe` / `vision.describe`。过滤仅依赖本机已知适配器元数据。
绑定的 `models` 是配置的可选模型，`eligible` 是本机配置资格；`health.state=unknown`
表示这里没有进行真实上游测试。`metadata_scope=adapter` 表示 limits/features
不是逐模型认证，也不代表当前价格。视觉协议元数据标注为 protocol，模型是否支持图片仍需真实验证。按能力过滤的目录不返回 API Key 或服务 URL；空参数的旧调用保留原目录字段以兼容存量插件。
配置为 stream 的同步适配器不能作为可执行流式服务展示。

计划包含 `requested/selection_source/selected/attempts/fallback_policy/billable=false`。
执行结果保留原媒体结果并增加 `actual`、尝试历史、回退标记、用量与产物。
异步视频返回提交结果与 job_id，后续状态继续使用 media.job.*；提交成功不等于生成完成。

## 后端、多客户端与批处理

后端在插件根清单 `capability_dependencies` 中声明同样的能力。
Node 后端可以使用相同客户端：

```js
const { createAiClient, brokerTransport } = await import('./client.mjs');
const client = createAiClient({
  pluginId: '<plugin-id>', storage: myProjectPreferenceStorage, projectId,
  invoke: brokerTransport({
    url: process.env.HIMIND_CAPABILITY_BROKER_URL,
    token: process.env.HIMIND_CAPABILITY_BROKER_TOKEN,
  }),
});
const pinned = await client.pin('media.audio.speech');
// 保存 pinned 到批任务记录，再让所有项使用它；重启后复用同一选择。
const result = await client.execute('media.audio.speech', { text: '你好' }, pinned);
```

其他语言通过 ADR 0024 的 loopback Broker 调用同样的 JSON 契约；MCP 和 Workflow
也可直接调用公共能力。Broker Token 不得传到 UI。客户端身份由宿主/传输决定，
selection_source 仅记录偏好来源。

SDK 内置 localStorage 偏好按插件、项目和能力隔离；保存失败会显示错误。
需要多设备同步的插件自行存储项目偏好并注入 storage，不在 Agent 再建一份服务配置。
SDK 不会自动更新全局路由，也不会把失效偏好悄悄改成默认值。

音色不是语音模型。`client.voices(target)` 根据选中的语音服务查询音色，只有支持目录的
适配器才发起查询；否则返回 `state=unsupported`。音色目录查询可能访问 Provider。
服务/模型变化时清除音色，重新选择后再执行。示例使用服务默认音色，不携带旧音色。

内置视觉批任务现在保存服务和模型，恢复时保持固定。早期没有固定模型记录的作业
需要用户重新创建；不能推测历史模型然后声称恢复了原执行目标。

## 验证

本次交付的覆盖、证据来源和限制见 [VALIDATION.md](VALIDATION.md)。

```text
node --test integrations/plugin-ai/client.test.mjs
node integrations/plugin-ai/selector.browser.mjs
cargo test --bin himind-agent plugin_ai
cargo test --bin himind-agent ai_selection::tests
cargo test --bin himind-agent ai_capability_bus::tests
```

浏览器检查支持 `PLAYWRIGHT_MODULE` 与 `CHROMIUM_EXECUTABLE`，不写已安装插件。
它运行真实 SDK 与示例界面，宿主传输使用内存替身，覆盖选择/偏好/失效/竞态/断线恢复。
Rust 回环 HTTP 测试检查真实图片与音频适配器请求里的模型以及落盘产物；公共 Gateway
测试验证目录/计划与非法参数，插件权限测试验证查询授权不会顺带授予执行和管理权限。
这些结果不等同于商业 Provider、已安装第三方插件或最终安装包验收。
