# AI 客户端来源与原生账号

AI 服务管理可复用的连接和模型；客户端接入负责把这些来源提供给具体工具。原生订阅在客户端中登录，Agent 不接管账号令牌。图像/音频等插件能力继续使用 Agent 的服务与能力路由。

- Codex：工具接入 → 展开 Codex → 使用 ChatGPT 账号 → 核对计划 → 应用。重启共用此配置目录的 Codex，在客户端核对账号、来源和模型。配置切换成功不代表已登录或订阅可用。
- Claude Code：使用原生账号会清除预览列出的用户级 API/端点/模型覆盖；启动新会话并核对 /status、按需 /login。项目、环境及组织策略可能仍影响最终来源。
- OpenCode：添加模型来源不会覆盖默认模型。在客户端 /connect 登录、/models 选择，或在 Agent 中选择已登记模型并点击“设为默认模型”。移除 Agent 来源时，只还原仍指向 Agent 的默认选择。
- 恢复分发前配置：需要可用快照，恢复结果可能是另一个第三方来源。没有快照时，使用移除来源，不把保守清理说成历史恢复。

MCP 工具接入与模型来源独立；切换原生账号无需断开 Agent MCP。原生切换会解除模型同步绑定，后台更新不会重新接管。已保存 AI 服务保留，可再次分发。

MCP 使用同一能力网关：先调用 ai.client.status 读取 source.capabilities，再调用 ai.client.source.plan，确认计划后将返回的 revision 作为 expected_revision 传给 ai.client.source.apply。action 支持 native_account、restore_previous、set_default；set_default 的 model 为已登记目录中的完整模型 ID。模型 ID 可以包含斜杠。只有接受当前计划中的漂移变更时才传 accept_drift=true。旧计划、未知参数、不支持的操作、损坏配置会拒绝写入。

状态分别展示登记、托管、配置来源；auth_status=unknown 和 effective_status=not_verified 表示没有认证或实际会话生效证据。环境提示只显示变量名，不显示密钥；不会修改全局环境或组织设置。当前源操作绑定检测到的配置路径，不提供任意路径写入或通用多账号中心。

## 实施与验证（2026-11-17）

本次基于 `3f2b1606f49868319ce0767137ad3263fc22b761` 在独立 worktree 实现，架构约定见 [ADR 0033](adr/0033-capability-driven-ai-client-source-lifecycle.md)。

| 验证 | 结果与边界 |
| --- | --- |
| Rust `app::ai_provider_import::` | 150 通过，0 失败、0 忽略；包含隔离配置的原生切换、历史恢复、旧绑定迁移、失败回退，以及 UI/MCP 上下文通过同一网关计划实际写入夹具配置 |
| Rust `capability::service::tests::ai_client` | 3 通过，0 失败、0 忽略；客户端目录、既有契约和新来源能力契约 |
| TypeScript / Vite | 最终类型检查与生产前端构建通过；保留既有 chunk 大小提示 |
| 浏览器 | 8 组通过；运行真实 React 工具接入面板，Tauri 返回为测试替身，不是已安装版或官方订阅验收 |
| 相邻页面检查 | 服务路由、聚合入口 UI、设置页信息架构和服务新增流程通过 |
| 格式与差异 | 本次 Rust 改动块无格式差异，`git diff --check` 通过；3 个既有大文件仍有与本次改动无关的格式差异 |

原始证据保存在本地 `.himind/long-tasks/20261117-ai-client-source-lifecycle/evidence/`，`validated-inputs.json` 记录源码、工具及日志身份。最终使用 `rust-adapters-current.log`、`rust-gateway-current.log`、`browser-current.log`、`frontend-build-current.log`；初次网关测试编译错误、缺浏览器、失败注入夹具的锁文件冲突、旧文案断言失败均保留原记录，对应问题已修正并复验。早期 `cargo check --locked --bins` 通过；随后对最终 Rust 实现及调用方重新编译并执行上述定向测试，不将早期全 bin 检查写成最终版本重跑。

验证未切换用户实际账号，未启动或重启生产客户端，未部署或安装。当前实现完成的是客户端配置与接入流程；官方账号登录、会员权益和实际会话来源仍需在客户端核对。没有执行与本次来源管理无关的全量发布或协作任务验收。
