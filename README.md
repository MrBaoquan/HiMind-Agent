# HiMind Agent

HiMind Agent 是可独立安装和运行的 Windows 客户端，提供本机 AI、Skill、插件、MCP、工程、远程设备与受控能力网关。Dashboard 是通过标准业务对接协议接入的可选增强 Provider，不是 Agent 的启动依赖。

## 仓库与构建

环境要求：Rust stable、Node.js 22 和 PowerShell 7。构建 Agent、前端与官方客户端适配器：

```powershell
./scripts/build.ps1
```

生成便携发布包：

```powershell
./scripts/package.ps1
```

安装器需要 NSIS。GitHub 安装器默认 `independent`；Dashboard 分发必须显式选择 `connected`：

```powershell
./scripts/build-installer.ps1 -ReleaseDirectory ./artifacts/0.3.37
./scripts/build-installer.ps1 -ReleaseDirectory ./artifacts/0.3.37 -DefaultMode connected
```

业务协议说明见 [业务对接协议](docs/business-integration-protocol.md)，仓库与分发边界见 [仓库边界](docs/repository-boundaries.md)。

## 安装与本地应用服务

正式安装使用 Tauri NSIS 用户级安装器，安装根目录固定为 `%LOCALAPPDATA%\HiMindAgent`。安装后的可执行文件位于 `current\himind-agent.exe`，状态位于 `data\agent-state.json`，更新器使用 `previous` 和 staging 目录完成切换与失败回退。不要直接运行从浏览器下载的裸 Agent 可执行文件；裸 exe 仅用于开发或诊断。

Windows 桌面使用时优先运行本地应用服务模式：

```powershell
cargo build --release
.\target\release\himind-agent.exe --local-app --local-port 18181
```

该模式会启动系统托盘图标和 `http://127.0.0.1:18181` 本地服务。Agent 默认以 `Independent` 模式运行，保留本机 AI、技能、插件、MCP、工程和远程设备能力，不启动组织控制面 Worker；从 Dashboard 分发的安装器会在首次安装时预置 `Connected` 模式。Connected 模式额外启用 Dashboard Worker 和内置 Dashboard Provider。两种模式共享同一套 Agent 协议、Capability Gateway 和本机实现。

运行模式持久化在 Agent 状态目录旁的 `agent-preferences.json`，模式切换必须重启进程，避免同一进程内出现服务边界不一致。Independent 不是离线模式，仍可访问 GitHub、第三方 AI Provider 和原生 DSH 网络服务；组织调度、策略、审计、组织商城和 Dashboard AI 服务属于 Connected 模式的可选控制面能力。

本地服务当前接口：

1. `GET /health`：本地服务、原生文件夹选择、打开文件夹、远控客户端唤起和登录状态能力。
2. `GET /pick-folder`：打开 Windows 原生文件夹选择器。
3. `GET /open-folder?path=`：调用 Windows Explorer 打开指定路径。
4. `POST /remote-connect`：接收一次性远控工具、设备码、验证码；这是运维工作台用户主动点击的一键直连动作，不进入审批队列，优先复用或唤起本机向日葵 / ToDesk。连接参数支持 `HIMIND_SUNLOGIN_CLI` / `HIMIND_SUNLOGIN_ARGS`、`HIMIND_TODESK_CLI` / `HIMIND_TODESK_ARGS`，并兼容旧的 `PROJECT_DASHBOARD_*` 变量。
5. `GET /remote-clients`、`POST /remote-clients/detect`：读取或重新检测本机向日葵 / ToDesk。没有已保存路径时，Agent 会依次检查运行中进程、注册表、开始菜单、标准安装目录和系统 `PATH`，并把首个有效程序写入 `agent-state.json` 同目录下的 `agent-remote-clients.json`，作为该机器的默认路径。
6. `POST /remote-clients/configure`、`GET /pick-remote-client`：手动选择并保存客户端程序。手动路径优先于自动发现，失效时也不会被静默覆盖；管理员仍可用 `HIMIND_*_CLI` 环境变量作最高优先级覆盖。
7. `GET /login-status`：返回本机 Agent 是否已保存可复用的内网登录。
8. `POST /login`：由 Dashboard 本地 Agent 卡片提交内网账号密码，保存到本机当前用户配置。
9. `POST /logout`：清除本机 Agent 已保存的内网登录。
10. `GET /open-login`：打开内网页面，供用户手工查看或确认登录状态。
11. `GET /capabilities`：返回 Capability Gateway 已注册的内置能力描述、风险等级和参数 Schema。
12. `POST /capabilities/invoke`：通过 Capability Gateway 调用受控能力，当前已接入健康状态、登录状态、打开文件夹、工程状态/打开工程、远程连接、插件列表、插件 Manifest 查询、已声明插件能力调用，以及 Connected 模式下的项目/展项 CRUD、人员配置、展项关联和工作区检出等 Dashboard 业务能力。能力描述包含 `availability`：`local`、`network_service` 或 `control_plane`；Independent 会隐藏并拒绝最后一类能力。
13. `GET /plugins`：扫描 `%LOCALAPPDATA%/ProjectDashboardAgent/plugins/` 并返回本机插件 Registry 状态。
14. `GET /plugins/manifest?plugin_id=`：返回指定插件的 Manifest、能力和权限摘要。
15. `POST /plugins/install|update|uninstall|enable|disable`：已预留本地接口，当前在 Distribution 策略与制品校验接入前返回 `not_implemented`。
16. `GET /ai-provider-import/status`：读取 VS Code、CC Switch 和 WorkBuddy 的 HiMind AI 导入状态。
17. `POST /ai-provider-import/cancel`：取消指定客户端的 HiMind AI 导入；WorkBuddy 和 CC Switch 会在变更前创建备份，VS Code 由官方扩展清除 SecretStorage 凭据。

Agent 主窗口已提供“本机插件”页：左侧选择当前设备已安装插件，右侧查看该插件的本机状态、错误、功能页面、权限和已注册能力，并可打开独立插件窗口、创建桌面快捷方式或打开插件目录；页面通过 Tauri 命令直接读取本机注册表和 Gateway，避免主窗口 WebView 再 fetch 自身 `127.0.0.1` 服务。插件和 Skill 可从本地包或固定 GitHub ref 导入，Independent 不依赖组织商城；组织安装策略、审批、分发和审核仍由 Connected 模式的控制面负责。

Agent 主窗口前端采用 React + TypeScript + Vite：`frontend/src/` 按 Shell、通用组件、页面和服务层组织，生产构建输出到 `frontend/dist` 并由 Tauri 打包。界面使用 Lucide 标准图标和适配 `900 × 640` 默认窗口的紧凑桌面控制台布局；低于 `740px` 时侧栏收为图标栏，本机插件页采用列表/详情主从布局，日志在自身容器内滚动。

MCP 接入由统一 Registry、目标适配和真实探测组成。Registry 继续读取 `himind-ai-mcp.json`，但 DSH、Agent MCP、CLI 和 Tauri 都通过同一模型访问；环境变量、请求头和疑似密钥参数不会出现在公开快照中。除 Codex、GitHub Copilot、WorkBuddy 外，面板和 CLI 还可注册 Claude、VS Code、Cursor、Windsurf、Qoder、ZCode、Gemini CLI、OpenCode、Kimi Code、Kiro、Qwen Code、Trae 等客户端。目标配置写入前保留备份，损坏配置必须显式选择重建；需要保留客户端专有配置格式的目标只提供手动配置片段，不参与批量写入。可用命令为 `himind-agent mcp list|targets|inspect|plan|apply|apply-all|remove|remove-all|test`，批量操作默认只处理已检测且支持自动配置的客户端，并逐目标返回成功、失败和跳过明细。

Agent Skills 分发与 MCP 注册是两套独立适配器，共享同一客户端注册表。新建 Skill 使用 `agent-skills` 可移植能力标识，历史上声明 Codex、GitHub Copilot 或 WorkBuddy 的标准 `SKILL.md` 也按 Agent Skills 兼容包处理；仅声明 `himind-ai` 的内部 Skill 不会外部分发。Agent 只向本机已检测到的目录型客户端同步，当前覆盖 HiMind AI、Codex、GitHub Copilot、WorkBuddy、Claude、Qoder、ZCode、Cursor、Windsurf、Trae、CodeBuddy、Antigravity、Gemini CLI、OpenCode、Kimi Code、Kiro 与 Qwen Code。客户端路径、支持级别和 MCP 入口均由注册表集中维护，新增客户端不需要复制同步流程。

启用的个人 MCP 会话会在 Agent MCP 中以 `mcp.<server>.<tool>` 聚合暴露，stdio 服务在 Agent 生命周期内复用并在配置变化或故障后重连；HTTP 服务沿用 Streamable HTTP 和会话请求头。DSH 已通过原生配置层接收个人 MCP，因此不会在 Agent 桥接中重复展示。单个下游服务失败不会影响 Agent 自有能力。

通知反馈分为两级：主窗口普通操作结果使用右上角最多 4 条的自动消失通知；手动审批通过独立 `390 × 280` 置顶窗口处理。审批接口会返回 `remaining_seconds`，由 Rust 按真实请求存活时间计算，弹窗与主窗口审批列表均据此显示剩余时间，避免轮询刷新后倒计时重置。

本地 HTTP 已按 ADR 0022 启用 Origin/Host 信任边界：浏览器默认只能从当前 `--api` 的 Dashboard Origin 调用，响应不再返回通配 CORS；无 Origin 的本机脚本保持兼容。额外可信 Dashboard Origin 通过 `HIMIND_AGENT_ALLOWED_ORIGINS` 配置。自更新下载必须与当前 Dashboard API 同源，并在替换前验证完整 SHA-256。

Agent 的 Dashboard 用户身份使用 OAuth 设备授权和轮换 refresh token，不依赖浏览器持续在线。设备 credential 与 refresh token 通过当前 Windows 用户 DPAPI 保护，access token 只保存在内存；设备 credential 默认每 30 天自动轮换。主窗口会显示当前 Dashboard 代表用户，并提供浏览器设备授权、撤销和在线验证。“AI 接入”页首批支持 Codex、GitHub Copilot 与 WorkBuddy 的 MCP 配置、备份合并和连接自检。已安装 Agent 由稳定 launcher 接收 `--mcp` 并转发到同版本的 `himind-agent-mcp.exe` console companion；开发目录和直接配置则使用同目录 companion，避免把 Windows GUI 子系统的 Agent 当作 stdio 进程。MCP 配置不得写入 Dashboard session、Agent credential 或 OAuth token。

本机已提供示例插件 `demo-multi-cap`（位于 `%LOCALAPPDATA%/ProjectDashboardAgent/plugins/demo-multi-cap`），声明 `demo.echo`、`demo.time`、`demo.stats` 三项能力，用于验证一个插件对应一组能力、能力合并到 Gateway 和通过 `/capabilities/invoke` 调用的端到端链路。

开发模式可直接执行：

```powershell
cargo run -- --local-app --local-port 18181
```

## Worker 调试模式

```powershell
cargo run -- --api http://localhost:8080
```

连接 Docker runtime Dashboard：

```powershell
cargo run -- --api http://localhost:18080
```

常驻模式会持续心跳并拉取任务。如果 Dashboard runtime 重启导致旧 Agent ID 失效，Agent 会自动重新注册并继续运行。

只执行一次心跳和任务轮询：

```powershell
cargo run -- --api http://localhost:8080 --once
```

runtime 单轮调试：

```powershell
cargo run -- --api http://localhost:18080 --once
```

默认扫描根目录：

1. F:\U3DProjects
2. F:\Project Released Files
