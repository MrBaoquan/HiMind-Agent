# Claude / OpenCode 协作任务执行器

Claude Agent（`acp.claude`）和 OpenCode（`acp.opencode`）通过 ACP stdio 接受 HiMind 协作任务。Claude 的执行进程是 Claude Agent SDK 的 ACP 适配器；Claude Desktop 仍是可以调用 HiMind MCP 的客户端，不承担后台任务进程。

## 接入

在已安装 Agent 的 Windows 主机执行：

```powershell
pwsh -NoProfile -File scripts/connect-acp-executors.ps1
```

可用 `-Executor claude` 或 `-Executor opencode` 单独接入。脚本保留已有 profile，不覆盖用户的路由、权限或认证环境。Claude 适配器固定到 `0.78.0`，安装在 Agent 的本地 runtime 目录，需要 Node.js 22+；安装只使用本次指定的 npm registry，不改变全局 npm 设置。OpenCode 优先使用 PATH，再探测桌面版附带的 CLI。

Claude 沿用用户的 Claude Code 设置、模型服务及登录。OpenCode 沿用自己的 provider/账号配置。HiMind 不复制这些客户端的密钥，也不替换其模型服务。

## 指派和执行

1. 创建协作任务，选择允许相应 ACP provider 的工作流。例如软件交付、UI 专业打磨团队已经声明这两个可选 provider。
2. 给职责选择专家，再选择 **Claude Agent** 或 **OpenCode**。服务和模型来自实际 ACP 会话返回的模型目录；客户端当前模型排在第一项。
3. 选择任务访问权限并确认计划。外部执行器负责实现，HiMind 宿主继续负责调度、必需检查、独立验收与任务状态。

UI 和 `mission.assignment.options` MCP 能力共用同一目录。目录会完成 `initialize` 和 `session/new`，不发送模型 prompt；协议探测最多等待 20 秒，缓存 60 秒，同时最多探测 4 个执行器。进程启动与退出另需少量时间。启动、协议或认证错误会显示在不可用原因中。目录可用表示能建立会话，服务余额、具体模型的授权和执行中的网络故障仍由真实执行确认。

目录探测使用 `npx` 启动适配器时，仅允许读取本机已有缓存；未安装或未缓存时提示先完成接入。这样打开指派窗口不会隐式联网下载软件，也不会被 npm registry 的下载重试拖住。正常任务执行保留原 profile 的行为，此设置不写入用户 npm 配置。

## 权限与预算

- 默认遵循 profile 的 `prompt` / `deny` / `allow_once` 策略，外部执行器的工具遵循其自身访问控制。
- 用户确认本任务的 **完全访问** 后，只在该任务执行期间把 ACP 权限请求自动选择为 `allow_once`；不修改 profile，也不持久修改客户端设置。按当前系统账户运行，不自动申请管理员权限。
- `files_only` 仍由 HiMind 内置执行器强制执行，不能把外部执行器标记为支持同等隔离。
- token 用量来自 ACP 的实际响应，使用其 `totalTokens` 对齐合计；缺少用量时不虚构。模型文本中自报的数字不作为预算证据。尚不推算费用；没有可信费用时，声明费用上限的任务仍会被预算检查阻止。
- 已保存、未指定服务和模型的历史 ACP 绑定继续沿用客户端默认模型。

## 可重复验证

`runtime::acp::tests::real_acp_executor_smoke` 是显式启用的真实测试，调用保存的 profile、注入 HiMind MCP、运行文件和命令工具，并由宿主独立运行断言。需要设置 `HIMIND_ACP_SMOKE_PROVIDER`、`HIMIND_ACP_SMOKE_MODEL`、`HIMIND_ACP_SMOKE_ROOT`；目标目录必须包含 `.himind-acp-smoke` 标记。它会产生真实模型用量，默认单元测试不会执行它。

本机验证记录保存在 `.himind/long-tasks/20261112-claude-opencode-executors/`，最终状态与证据见该目录的 `review.md`。

2026-11-12 已使用安装版 MCP 启动两个顺序步骤：Claude Agent 修复实现，OpenCode 运行测试并写独立审查记录。两个步骤均成功，宿主再次运行三项固定测试通过，测试文件与基线一致。已验证 Claude 模型 `deepseek-v4.1-flash-1017`、OpenCode 模型 `himind/gpt-yundu/gpt-6.1-sol`；该用例证明跨执行器 Workflow 调度与实际工具执行，不替代 Mission 的完整独立验收闭环。

Windows 开发检查发现，本机源码目录的低完整性标签会被编译出的测试 EXE 继承，导致其 OpenCode 子进程不能写正常用户目录的日志。应从安装版或正常完整性目录运行验收；验证时可将测试 EXE 的字节写入专用临时目录，避免继承原 EXE 的安全标签。无需放宽用户目录 ACL，也不应把此问题误判为客户端没有登录或模型权限不足。
