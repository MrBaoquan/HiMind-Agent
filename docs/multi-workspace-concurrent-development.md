# Multi-Workspace Concurrent Development

HiMind AI（DSH）会话可以同时打开多个工作目录，每个目录各自开发一个拓展。
本文说明这条链路为什么成立、边界在哪里，以及怎么验证。

## 一次会话，一个工作目录

打开拓展工作区时，Agent 用该目录单独启动一个 DSH 实例：

| 每目录独立的东西 | 位置 | 作用 |
| --- | --- | --- |
| DSH home | `profiles/<profile>/runtimes/deepseek-harness/homes/<hash>` | 会话目录、存储、凭据都按目录隔离 |
| Agent 入口端口 | `21600 + hash(home) % 1200` | 每个目录一个浏览器 origin |
| overlay | `homes/<hash>/agent.<hash>.patch.yml` | 把 `HIMIND_AI_WORKSPACE` 钉在该目录 |
| Host 进程 | 一个 node 进程 | 会话、工具调用、模型请求 |

关键点：**一个 origin 一个 localStorage**。DSH 用 `dsh.sessions.current` 记住
当前会话、用 `dsh.workspace.view.v5` 记住 rail 展开状态，两者都按 origin 存，
所以两个目录天然不会共用「当前工作区」。

## 并发开发到底卡在哪里

曾经的表现是两个入口都跑进同一个目录（后打开的那个赢）。真实原因不在
overlay，也不在会话落盘的 `cwd`（两者实测都正确），而在 DSH 前端的首屏兜底：

`@deepseek-ai/dsh-client-ui-workspace/lib/client.js` 的 `reconcile()` 只在
`sessions.current === undefined` 时才用 `recentWorkspace(workspaces, sessions)`
自动连一个工作区，而被选中的是 **updatedAt 最新的那个**。每个入口都是全新
origin、`sessions.current` 都为空，于是两条会话同时去抢同一个「最近工作区」，
谁后写盘谁赢。

修复方式是不让前端去猜：Agent 在起会话时本来就会为目录 `workspace/create` +
`session/create` 采纳一条会话，现在把这条会话的 `sessionId` 一起写进首屏预设，
让入口开局就落在自己目录上（`src/app/builtin_ai_proxy.rs` 的
`rail_view_preset_script`）。

写入是有条件的：

- 只在 `dsh.sessions.current` 为空时写，用户在该 origin 里手动切过会话就保留用户选择；
- 与已有的 rail 展开预设共用同一个首屏脚本，仍然运行在 DSH bundle 之前。

## 首调用为什么慢：Windows ACL 沙箱的一次性成本

Windows 上 DSH 用受限令牌跑命令：工作区根目录先拿到一条能力 SID 的写权限 ACE，
再由继承铺满整棵目录树。ACE 一旦落地就是常驻的（跨会话复用），命中之后这条路
只剩一次 DACL 读，几十毫秒；但**第一次**落地要对每个子目录逐个传播：

| 工作区 | 规模 | 第一次铺 ACE |
| --- | --- | --- |
| himind-extensions | 14,355 文件 / 516 MB | 2.1 s |
| himind-short-video | 135,440 文件 / 4.0 GB | 21.0–24.9 s |

而用户在 DSH 里真正感受到的是这条（同一会话的两条工具调用，取自会话日志
`session.v3.jsonl.zstd`，见本次实测 `--F-WebProjects-himind-short-video--`）：

| 事件间隔 | 场景 | 耗时 |
| --- | --- | --- |
| 02:00:16.025 → 02:07:59.775 | `tool/call pwsh` → `tool/result`，ACE 尚未落地 | 463.8 s |
| 02:14:01.760 → 02:14:03.014 | 同会话第二次 `tool/call pwsh` → `tool/result` | 1.25 s |

按设计这笔钱算在「第一次用到工作区」的那一刻，也就是用户的第一条命令里，
表现出来就是 HiMind AI 卡在工具调用上几分钟。

### 预热

Agent 把这次传播提前到会话启动前后，在后台线程里付掉（`src/app/sandbox_warmup.rs`）：

| 触发点 | 时机 | 行为 |
| --- | --- | --- |
| 打开/准备一个 HiMind AI 会话 | 会话真正启动之前 | 预热该会话的工作区 |
| Agent 启动 | 启动约 8 秒后 | 顺序预热工作区绑定与最近会话，最多 8 个 |

预热调用厂商自带的 `dsh-sandbox-windows-acl` 模块，用同一条能力 SID、同一个
`standing` 语义把 ACE 铺到工作区根——不复制 SID 派生与 ACE 形状的约定，也不
制造第二个真相。厂商模块自己的 `withPathLock` 是跨进程文件锁，预热与真实调用
撞上只是排队，不会写出两条互相覆盖的 DACL。

边界：预热失败不影响会话。缺 `node.exe` 或 ACL 模块（例如 `HIMIND_DSH_EXECUTABLE`
指向开发构建）就是「少了一次加速」，只记一行日志；非 Windows 平台直接跳过
（bwrap / Landlock / Seatbelt 没有这个一次性成本）。

同一个工作区可能以 `\\?\F:\dir`（工作区绑定的规范形式，长路径安全）和 `F:\dir`
（最近会话的形式）两种写法到达。去重键先把两者归一到同一把键，避免同一棵树被
两条线程同时铺 ACL——实测那种并发会把 21 秒的批量预热拖成 24.9 秒，并按倍数放大
磁盘写入。

### 预热前后的首调用

| 工作区 | 预热前 | 预热后 |
| --- | --- | --- |
| himind-short-video | 463.8 s | 1.38 s |
| himind-extensions | 2.1 s（ACE 本已常驻） | 2.51 s |

两条会话并发跑同一条任务，总耗时 15.0 s，界面各自显示「用时 3 秒 / 4 秒」
（`.tmp-dsh-probe/out/v5-warmup.json`）。

## 创作链路的前置门槛：AI 扩展开发工具版本

并发本身不依赖任何插件，但**用 AI 写拓展**要先过一道版本门槛：预检会要求
`com.himind.extension-development-tools` 不低于 `1.4.0`
（`src/capability/service.rs` 的 `extension_tools_plugin_outdated`）。低于门槛时
预检返回 `state=blocked`，技能约定是**诚实停止**、不写工程文件，于是会话看起来
「跑完了但什么都没产出」。

这个门槛和并发无关，但很容易被误读成并发不可用，所以：

- preflight 的 remediation 会带上「本机哪个来源能拿到达标版本」，不再只说版本太低
  （`authoring_upgrade_hint()`，同样是给模型和用户看的定位线索）；
- 同一次预检里由老插件派生的阻塞（`extension_tool_missing`、
  `authoring_skill_contract_mismatch`）会在 remediation 末尾指回这个根因，
  否则字面上看像是「Skill 装错了」，用户会去重装 Skill 而始终升不上去；
- 组织通道长期发低版本、而本地/GitHub 来源有高版本时，升级会被**不可变性守卫**
  拦下（同版本不同内容不允许覆盖），此时只能卸载重装或发布更高版本；
- 验证脚本把门槛镜像成一个显式断言（`$Script:AuthoringToolsMinimum`），
  达不到就在 2 秒内抛错，并说明「这是环境门槛，不是并发架构问题」。

### 组织通道的闭环实测

「发布 → 组织策略 → Agent 自动安装 → 并发创作」四段一起走过一遍，2026-09-27 在生产
通道实测：

| 环节 | 证据 |
| --- | --- |
| 发布 | 提交 `pluginsub_0d785ba927790f50` 审批通过，release `distrel_9fcaf57c47c231fb` = 1.4.2 published，artifact `distart_816df159170144c3`，sha256 `bf175d10…340` |
| 组织策略 | 该资产 `desired-state` 变为 `desired_version=1.4.2 / intent=required / management=organization_managed / source=organization` |
| Agent 安装 | 生产 home 的 `plugins/com.himind.extension-development-tools/current/plugin.json` = 1.4.2，`extension.lock.json` 里的 artifact_id 与 sha256 就是上面这一份，包内 `checksums.sha256` 逐条校验通过 |
| 真实调用 | 安装后的入口 `extension.environment.preflight` 返回 `ready=true`；`scripts/verify-multisession-dsh.ps1` 以生产 home 跑出 24/24 |

授权（`data/agent-preferences.json` 的 mode）是这条链路上唯一的用户侧开关：
**取消授权即停止对接**，Agent 不再拉取组织策略，也就不会再自动升级——低版本会
原地停住，看起来像「组织通道发不出新版本」。反向的坑同样存在：授权按钮要求当前
处于对接状态，而取消授权会把 mode 打回 independent，于是「取消授权」之后
再点「授权」必然以 `control_plane_required` 失败。现在 `start_dashboard_authorization`
会先切回对接状态再发起设备授权（`src/app/commands.rs` 的
`ensure_dashboard_mode_for_authorization`），两条控制互为反向操作。

多工作台补充（ADR 0008）：设备授权按工作台连接分别保存，`mode` 只决定控制面
Worker 是否运行。在「设置 → 账号 → HiMind 账号」里对当前连接取消授权会把 `mode`
打回 independent，但其它连接仍保留自己的授权记录，切回去即可继续对接。

## 使用方式

在拓展工作区里对每个要并行开发的拓展点「用 AI 开发」，每个拓展会打开自己的
HiMind AI 窗口。各窗口可以同时发任务，互不阻塞；rail 里各自保留自己的会话记录。

工作区页面本身是**只读的上下文**：顶部说清「默认工作区是哪个仓库」，列表按工作区
分组，每个分组头有「用 AI 开发」直接开一条绑定该目录的会话。会话按目录寻址
（`start_builtin_ai_session(workspace_root)`），不再读「当前工作区」这个全局单值——
那正是并发时 A 会话把 B 会话的目录传下去的原因。

## 容量与边界

| 项 | 实测值 | 说明 |
| --- | --- | --- |
| 单 Host 进程常驻内存 | 约 210 MB | 2 个并发目录时两个 node 进程各约 210 MB |
| 端口范围 | 21600–22799 | 1200 个，按目录 hash 取模 |
| 同目录并发 | 支持 | 同一 origin 内可以开多条会话 |
| 首调用（工作区 ACE 未落地） | 4 GB 工作区约 21–25 s | 由启动预热在后台付掉，用户第一条命令只付几十毫秒 |

建议同时并发的目录数控制在 4 个以内（约 1 GB 常驻内存 + 模型请求并发），
更多目录仍可用，只是需要按机器内存评估。端口冲突时会退化成临时端口，
此时该目录的 origin 会变，首屏预设负责把 rail 展开状态和当前会话补回来。

## 验证

真机并发验证（两个目录各发一条真实任务，各自报告自己的工作目录）：

```powershell
$env:HIMIND_DSH_HOME = "$env:LOCALAPPDATA\HiMindAgent\profiles\development\runtimes\deepseek-harness\homes\interactive"
$env:HIMIND_TARGETS = "ext|43223|F:\WebProjects\himind-extensions|<token>;short|22488|F:\WebProjects\himind-short-video|<token>"
$env:HIMIND_REPLY_WAIT = "900000"
$env:HIMIND_STABLE_MS = "20000"
node .tmp-dsh-probe\concurrent-dev-run.mjs run v5-warmup
```

断言只看会话面板里的模型回复，不看左侧 rail（rail 里每个目录名都会出现一次，
拿整页文本断言会假阳性）。

会话日志是逐条工具调用耗时的一手证据（多 zstd 帧拼接，需要逐帧解压）：

```powershell
node .tmp-dsh-probe\session-timeline.mjs "$env:LOCALAPPDATA\HiMindAgent\profiles\development\runtimes\deepseek-harness\homes\interactive\sessions\--F-WebProjects-himind-short-video--\<session>\session.v3.jsonl.zstd" --tools
```

预热是否真的跑过，看 Agent 事件日志：

```powershell
Select-String -Path "$env:LOCALAPPDATA\HiMindAgent\profiles\development\logs\agent-events.jsonl" -Pattern "沙箱写权限预热"
```

单元测试覆盖首屏预设本身：

```powershell
cargo test --release --locked --features mcp-console rail_preset
cargo test --release --locked --features mcp-console sandbox_warmup
```

`rail_preset_pins_a_fresh_entry_to_its_own_session` 保证入口会被钉在自己的会话上，
`rail_preset_leaves_a_browser_that_already_chose_alone` 保证用户的选择不被覆盖。

## 端到端验证：两个工作区同时创作

`scripts/verify-multisession-dsh.ps1` 把这条链路跑成一次可重复的真机验证：
两个工作区各起一个 headless DSH 会话，并发发一条真实创作任务，然后按「工作区里的
文件、制品、会话记录」三方交叉断言。它不共享任何可变状态：草稿、开发注册表、
工程索引全部重定向到 `target/multisession-live/state`，不碰用户真实数据目录。

三个参数决定它验证的是哪条链路：

| 参数 | 作用 |
| --- | --- |
| `-AgentHome` | 会话以 `HIMIND_AGENT_HOME` 透传，保证「门槛检查」和「会话实际看到的插件」是同一份数据 |
| `-McpBinary` | 覆盖 MCP 伴生进程产物；`target\release` 被在跑的实例占用时改用 `target-verify\release` |
| `-AllowBlockedToolchain` | 工具链不达标时仍跑，只验并发与工作区隔离（创作类断言必然不成立） |

实测（2026-09-27，`target-verify\release` 产物，`profiles\development` 的 1.4.2 工具链）：

```powershell
pwsh -NoProfile -File scripts/verify-multisession-dsh.ps1 `
  -AgentHome "$env:LOCALAPPDATA\HiMindAgent\profiles\development" `
  -McpBinary "F:\WebProjects\项目看板\himind-agent\target-verify\release\himind-agent-mcp.exe"
```

| 断言 | 实测 |
| --- | --- |
| 两个会话执行区间真实重叠 | 72.0 s |
| 各自工作区生成了自己的插件工程 | 各 1 个 `plugin.json`，互不串目录 |
| 各自产出了自己的 `.hmpkg` | alpha `6e8c132b…` / beta `f12e667a…`，两份不同 |
| 会话记录里各有 `extension.test` 调用与测试记录 | alpha 4 次 / beta 1 次，各 7/7 check 通过 |
| 候选测试对象是自己的插件 | `com.himind.alpha-note-tool` / `com.himind.beta-note-tool` |

结果 24/24 通过，报告落在 `target/multisession-live/session-test-records.md`。
会话记录同时落在 DSH 自己的 home 里（`sessions/--<workspace-key>--/session-*/`），
所以在 HiMind AI 面板里能直接看到这两条会话。
