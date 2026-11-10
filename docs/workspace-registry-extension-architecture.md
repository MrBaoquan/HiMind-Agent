# 工作区登记扩展：架构方案（插件侧管理，暂不做 DSH 投影）

> 目标：让 AI 在对话中判断用户意图，适时把「某个目录属于哪个项目」登记下来；
> 登记表由插件自带的窗口管理。
>
> **本轮范围**：只到「插件存数据 + 技能判意图 + 插件 UI 管理」。
> **不投影到 DSH rail**（侧边栏工作区分组）——结论留在 3.1，本轮不做。

## 1. 结论

| 层 | 归谁 | 具体内容 |
| --- | --- | --- |
| 数据与解析 | 新插件 `com.himind.workspace-registry` | 项目树、路径绑定、别名、父子/聚合关系、技术栈与入口清单；纯确定性文件读写 |
| 意图判断 | 新技能 `workspace-context` | 什么时候 resolve、什么时候只记候选、什么时候必须问用户 |
| 管理界面 | 同一插件的视图窗口 | 项目树、路径绑定、候选收件箱 |

**不建议**把 work-log 的 `projects.json` 原样搬出来。日志库不该背全生态的项目树；
两者用稳定 ID 单向链接即可。

一句话职责划分：

- **work-log**：我做了什么、花了多久。
- **workspace-registry**：这个路径是什么项目、它属于谁、该用什么命令。

不做 DSH 投影带来三个好处：不动 `BUILTIN_AI_SESSIONS` 这条进程内状态、
不引入 rail tombstone 这类「用户删了要不要重建」的规则、也不需要
`workspace/create` + `workspace/rename` 的两步标题协商。
代价是：**登记了不会自动出现在 DSH 侧边栏**，用户仍按 DSH 自己的方式加分组。


## 2. 现状核查

### 2.1 模型自己就能看到当前目录，插件看不到

DSH 的 `working-directory` 服务把
`Current working directory: "<abs path>"` 作为**必需的**系统提示片段注入
（`dsh-working-directory/lib/index.js` 的 `contextText`，
`ctx.systemPrompt.context({ required: true })`）。所以：

- 技能不需要「猜」当前目录——模型上下文里本来就有；
- 但**能力处理器**看不到：MCP 服务是同一 profile 下的全局单例
  （`src/mcp_ipc.rs:374` `ensure_service_running`），环境变量来自第一个拉起它的桥，
  第二个工作区的插件调用会读到别人的 `HIMIND_AI_WORKSPACE`
  （`src/extension_workspace.rs:382` 注释已写明这个限制）；
- 从插件窗口调用时（`invoke_plugin_view_capability`，`src/app/commands.rs:7373`），
  处理器跑在 Tauri 进程，那里通常根本没有这个变量。

结论：**`workspace_root` 一律由调用方显式传入**，插件内部的「默认值」只作提示。
MVP 因此不需要改协议、改单例、改环境变量白名单。

### 2.2 Windows 上 DSH 沙箱不支持额外可写根

`dsh-sandbox-local`（Runtime 0.2.2-rc.1）在 `windowsAclRunnerArgv` 里明确抛错：

> Windows ACL confinement does not support additional writable directories

而 `sandbox-policy` 的 `workspaceRoot` 取自 **会话创建时的 cwd**
（`dsh-sandbox-policy/lib/index.js`，`resolve()` 用 `session.header.cwd`），
Windows 上 `workspace-write` 由受限令牌 + 工作区 ACL 兑现
（`dsh-sandbox-local/lib/index.js` `windowsAclRunnerArgv`）。
于是有两条直接结论：

1. 「把另一个项目的目录加成当前会话的可写目录」在 Windows 上没有实现路径；
2. `cd` 到工作区外**读得到、写不了**：文件工具被 fs 沙箱按范围拒绝
   （`dsh-fs-sandbox/lib/index.js` `checkedTarget` → `FS_SANDBOX_DENIED`），
   命令同样跑在只有当前工作区写权限的受限令牌下。
   模型很容易把 `FS_SANDBOX_DENIED` 误读成「路径写错了」而反复重试。

要真正在另一个目录里写代码，只有两条路：
**换一个以该目录为 cwd 的会话**（Agent 已支持：一个目录一个 DSH 进程），
或者**走 Agent 自己的能力**（能力处理器跑在 Agent 进程里，不受 DSH 沙箱约束，
但仍受审批与 `workspace_root` 校验约束）。

### 2.3 模型看到的工具是「白名单投影」，加一条能力不等于模型看得见

`tools/list` 默认只给 bootstrap 9 条（`src/mcp.rs:37`），其余要靠
`HIMIND_MCP_DEFAULT_ACTIVATE` 预激活或 `system.capability.activate` 激活
（`src/mcp.rs:100`）。当前预激活清单只有 `media.image.generate`
（`src/runtime/deepseek_harness.rs:58`）。要让配套技能在第一轮就能调用登记能力，
必须把能力 ID 追加进这份清单。

### 2.4 技能是「会话启动时快照」，装完不会热生效

`himind-skills` 目录由 `ensure_himind_skill_adapter` 在启动时整目录重建
（`src/runtime/deepseek_harness.rs:4079`），overlay 里写的是 `watch: false`
（`src/runtime/deepseek_harness.rs:3561`）。所以：

- 技能必须**在会话启动前**已安装并被判定 ready，否则模型根本看不到它；
- 现成的正确做法已经存在：保存指令选择后只停本工作区会话
  （`src/app/commands.rs:605`），下次进入时带着新技能启动。

配套技能因此不能「先聊着，聊到一半再装」。

### 2.5 插件窗口能调什么，由两处白名单决定

- 能力调用：`invoke_plugin_view_capability`（`src/app/commands.rs:7373`）
  只放行「插件自己声明的能力」或视图 `external_capabilities` 命中的外部能力；
- 外部能力前缀写死在 `VIEW_EXTERNAL_CAPABILITY_PREFIXES`
  （`src/capability/plugin.rs:3994`）：`media. audio. visual. asset. render.`；
- 同一份校验函数也被 `capability_dependencies` 复用（`src/capability/plugin.rs:2439`），
  即插件后端走 broker 调外部能力时受同一张表约束。

**这意味着**：任何 `workspace.*` / `ai.*` 能力，插件 UI 和插件后端今天都调不到。
本方案的 MVP 不扩这张表——**插件 UI 只调本插件自己的能力**。

顺带一个既有但与本方案无关的边界：`capabilities/default.json` 只把
`core:event:default` 授给 `main` 和 `settings` 两个窗口，插件视图窗口不在里面；
所以插件页面**不要依赖 Tauri 事件**，用能力调用 + 轮询或一次性读取。

## 3. 目标架构

```
   ┌──────────────┐        ┌──────────────────┐        ┌────────────────────┐
   │ AI 对话       │  MCP   │ 插件              │  读/写  │ 注册表文件          │
   │ 技能 workspace│───────▶│ workspace-registry│◀──────▶│ (插件数据目录)      │
   │ -context      │capability  resolve/upsert  │        │ projects/workspaces │
   └──────────────┘        └──────────────────┘        │ /candidates         │
                                  ▲                     └────────────────────┘
                                  │ invoke_plugin_view_capability
                           ┌──────┴───────┐
                           │ 插件视图窗口  │
                           │ 项目树/绑定/  │
                           │ 候选收件箱    │
                           └──────────────┘
```

- **写入点唯一**：注册表只由插件进程写；
- **读取点两个**：技能（通过能力）+ 插件窗口（通过能力）；
- **没有内核侧后台循环，没有 DSH RPC**。

### 3.1 为什么不做 DSH rail 投影（保留结论供以后复用）

调研过，可行，但本轮不做。真要做的话有这几条事实需要提前知道
（都已在 Runtime 0.2.2-rc.1 上核对过）：

- `workspace/create` 是唯一入口，按路径幂等，已存在返回 `created: false`；
  只接受**已存在且是目录**的路径（`dsh-workspace` 的 `create` 先 `realpath` 再 `stat`）；
- 未给标题时**标题就是目录名**（`defaultWorkspaceTitle`），
  要中文名得再调 `workspace/rename({workspaceId, title})`；
- `workspace/follow` 是增量流，新建分组**立即出现**；但「默认展开」依赖
  Agent 在页面 HTML 注入的 `dsh.workspace.view.v*` 预设
  （`src/app/builtin_ai_proxy.rs:252`），已打开的页面不会因新建分组改变展开状态；
- 这些调用必须带**本会话的 DSH 地址 + 短时会话 cookie**
  （`src/app/builtin_ai_proxy.rs:809` `call_api`），而它们只存在于
  Tauri 主进程的 `BUILTIN_AI_SESSIONS`（`src/app/ui.rs:94`）里。

因为最后一条，它**只能**放在 Tauri 进程的后台循环里，不能做成能力
（`--mcp-service` 进程里那张表是空的；而插件窗口调用又跑在 Tauri 进程，
同一条能力两种语义）。本轮因此跳过，等真的有「登记完就想去侧边栏看见」的需求再加。

## 4. 数据模型

沿用「两张表 + 候选」：`projects`（逻辑节点）、`workspaces`（物理路径）、
`candidates`（待确认）。要点：

```json
{
  "schema_version": "workspace_registry.v1",
  "projects": [
    { "id": "prj-himind-ext", "name": "HiMind 扩展仓库", "kind": "aggregate",
      "parent_id": "", "aliases": ["himind-extensions"],
      "context": { "stack": ["Go"], "commands": [], "docs": [], "rules": [] },
      "links": { "work_log_project_id": "pj-..." } }
  ],
  "workspaces": [
    { "id": "wsp-work-log", "project_id": "prj-work-log",
      "path": "F:\\WebProjects\\himind-extensions\\plugins\\work-log",
      "role": "root", "source": "manual", "confidence": "exact" }
  ],
  "candidates": [
    { "path": "...", "suggested_project": "work-log", "suggested_parent": "prj-himind-ext",
      "confidence": "inferred", "source": "scan", "seen_at": "..." }
  ]
}
```

- `parent_id` 单父树，同时表达「物理嵌套」和「逻辑父子」；
- `role`：`root / worktree / mirror / release / docs`，worktree 不再被误当成新项目；
- `source`：`manual / import / manifest / git / session / scan`；
- `confidence`：`exact / inferred`，只有 `exact` 才允许自动绑定；
- `links` 只存跨系统稳定 ID，不复制工时、优先级、下一步。

写盘：原子写 + `.bak`；**跨进程文件锁**。每个能力调用都是一个新的短生命周期
插件进程，`sync.Mutex` 跨不了进程——work-log 的 `lockRoot`
（`plugins/work-log/storage.go:21`）就是进程内互斥，只能防同进程重入，
两个并发工具调用仍会丢更新。新插件要用 `workspaces.json.lock` + OS 文件锁
（Windows `LockFileEx`）。

## 5. 能力清单

命名空间显式声明 `capability_namespace: "workspace.registry"`（首段 `workspace`
本来就在 ADR 0119 领域表里，但多段前缀必须在清单登记，见
`src/capability/plugin.rs:683`）。全部 ID ≤ 48 字符、≤ 5 段。
`resolve`、`bind`、`accept` 不在内核动作词表里，但这是**插件命名空间内的动词**，
与既有 `work-log.project.resolve` / `.claim` / `.assign` 同一先例。

| 能力 | 作用 | 风险 |
| --- | --- | --- |
| `workspace.registry.resolve` | 路径/别名 → 项目 + 祖先 + 子项 + 同级 + 上下文 | read_only |
| `workspace.registry.context.get` | 生成 ≤500 字的 agent 可读 brief | read_only |
| `workspace.registry.project.list` | 树/搜索 | read_only |
| `workspace.registry.project.upsert` | 新建或修改项目、父级、上下文 | local_write |
| `workspace.registry.project.delete` | 删除；有子项或路径时默认拒绝 | local_write（R3，走审批） |
| `workspace.registry.path.bind` | 绑定/解绑路径、role、source | local_write |
| `workspace.registry.candidate.list` | 列候选 | read_only |
| `workspace.registry.candidate.accept` | 候选落成项目/绑定 | local_write |
| `workspace.registry.validate` | 环、冲突、别名重复、失效路径 | read_only |
| `workspace.registry.import` / `.export` | 与 work-log 项目登记互导，支持 dry-run | local_write / read_only |

「打开链接 / 在资源管理器定位」**不需要**自建能力：插件视图可以直接调内核的
`open_external_url` 和 `open_folder` 两个 Tauri 命令
（`src/app/ui.rs:470`、`src/app/ui.rs:726`）——`media-downloader`
的视图就是这么用的（`plugins/media-downloader/ui/app.js:269`）。

注意：`workspace.delete` 已被审批策略当作文件删除
（`src/approval/policy.rs:32`），所以删除能力必须带 `.project.` 段，
避免语义撞车。同理不要叫 `workspace.registry.candidate.delete`——
用 `candidate.dismiss`（不删数据，只打标）。

MVP 落地上表前 9 条（`import` / `export` 放以后，需先定迁接口径）。

## 6. 「择机自动添加」的决策阶梯

这是最容易做坏的部分。规则不是「让模型自己决定」，而是**先划好档位，模型只选档**。

| 档 | 条件 | 行为 | 需要确认 |
| --- | --- | --- | --- |
| L0 复用 | `resolve` 命中 | 直接用规范名与上下文，静默 | 否 |
| L1 记候选 | 未命中，但能从 `.git` / `.himind/project.json` / `extensions.json` / 目录名推断 | 写 `candidates`，回复里一行提示「已记为候选」 | 否 |
| L2 自动绑定 | 未命中，但已登记项目的别名/basename 与路径末段精确一致，且该路径尚无绑定 | `path.bind(confidence=exact)`，带 undo 记录 | 否 |
| L3 新建节点 | 需要新项目、改 `parent_id`、合并、删除、迁移路径 | 先出计划再执行 | **是** |

硬性边界（写进 SKILL.md，不靠模型自觉）：

1. **不自动开新会话、不自动 `cd`。** 跨目录改代码不是登记问题——用户在 Agent 里
   为该目录开会话即可（一个目录一个 DSH 进程）。技能最多把路径原样报给用户。
2. **不自动改 `AGENTS.md`。** 规则正文属于用户。
3. 一次会话最多触发一次自动登记；`resolve` 失败一律静默降级，不打断任务。
4. 候选默认 14 天过期；同一路径反复出现 3 次以上才升级为「建议确认」。
5. 未命中就未命中：不得为了「有个结果」现造一个项目。
6. 登记只影响**命名与上下文**，不影响会话权限；不要暗示「登记后就能写这里」。

触发点（技能里写死，别让模型自由发挥）：

- 任务开始时（含「继续上次那个」）；
- 用户第一次给出绝对路径或项目别名时；
- 话题从 A 项目切到 B 项目时；
- 写工作日志、启动开发/交付工作流之前。

## 7. 插件 UI 管理

视图贡献用 `plugin_navigation` + `quick_access: true`，与 work-log 一致
（插件注册表 → 快捷入口的过滤逻辑见 `frontend/src/main.tsx:1731`）；
窗口内通过 `invoke_plugin_view_capability` 调本插件能力，插件身份与数据目录用
`get_plugin_view_context`（`src/app/commands.rs:7251`）。

**注意**：这个命令返回的 `workspace_root` 来自当前进程环境
（`src/app/commands.rs:7266` → `extension_projects::current_workspace(None)`），
和 2.1 说的是同一条不可信链路。插件 UI 把它当**展示提示**即可，
真正的「当前项目」由用户在窗口里选。

四个区域，MVP 只做前三个：

1. **项目树**：拖拽改父子（只改 `parent_id`，不改磁盘）、聚合/子项目标记、别名编辑；
2. **路径绑定**：绑定/解绑、role 下拉、`exact / inferred` 标签、失效路径高亮；
3. **候选收件箱**：接受 → 建 project 或 bind，忽略 → 打标不再提示；
4. （不做）**在 AI 中打开**：需要新增内核能力并扩外部能力白名单，见 10 节。

选目录用 `pick_workspace_directory` 这个 Tauri 命令
（`src/app/commands.rs:7292`，`media-downloader` 与 `media-library` 的视图已在用）；
它不经过能力网关，也不受 `external_capabilities` 限制。

不做成第二个 work-log：不显示工时、不显示下一步、不做日志编辑。点项目就是
「这个路径属于谁」，其余交给对应扩展。

## 8. 落地步骤

### 8.1 插件（`himind-extensions` 仓库）

1. **骨架**：`plugins/workspace-registry`，Go + `process-jsonrpc-stdio`，
   `capability_namespace: "workspace.registry"`；视图 `ui/index.html`
   （`plugin_navigation` + `quick_access: true`）。
2. **存储**：`<HIMIND_PLUGIN_DATA_ROOT>/workspaces.json`，原子写 + `.bak`；
   **跨进程文件锁**（`workspaces.json.lock` + `LockFileEx`），
   不能用 `sync.Mutex`（见 4 节）。
3. **解析**：路径归一化单独实现，**不要照抄 work-log 的 `path.Clean`**——
   `//server/share` 会被它压成 `/server/share`，UNC 路径就坏了。
   要覆盖：`\`/`/`、盘符大小写、`\\?\` 前缀、尾斜杠、`.`/`..`、UNC；
   最长前缀匹配必须带路径边界（`A` 不匹配 `A2`）。
4. **能力**：第 5 节前 9 条。
5. **视图**：第 7 节前三个区域；`pick_workspace_directory` 选目录；
   一切都走 `invoke_plugin_view_capability`，不依赖 Tauri 事件（2.5）。

### 8.2 技能（`himind-extensions` 仓库）

`skills/workspace-context`，`supported_clients` 含 `agent-skills` 才会投影给
HiMind AI（`src/skill/clients.rs:482`）。正文写死第 6 节的档位与触发点，
并明确：

- 当前目录从**系统提示里的 `Current working directory`** 取（2.1），
  然后把它作为 `workspace_root` 显式传给能力；不要读环境变量、不要猜；
- 未命中只记候选，不阻断任务；
- 不暗示「登记后就能写这个目录」（2.2）。

### 8.3 Agent 内核（一处）

只有一处必须改：**预激活清单**。

`HIMIND_MCP_DEFAULT_ACTIVATED_CAPABILITIES`（`src/runtime/deepseek_harness.rs:58`）
追加 `workspace.registry.resolve,workspace.registry.context.get`，
让技能在第一轮 `tools/list` 里就看得见这两条。其余能力（写入类）
留给 `system.capability.search` → `activate` 按需激活，不必撑大默认工具面。

不改：MCP 协议帧、`InvocationContext`、插件进程环境变量、
`BUILTIN_AI_SESSIONS`、rail —— 都不在本轮范围。

### 8.4 测试

- 注册表单测：解析（UNC/大小写/`..`/尾斜杠）、最长前缀与边界、
  环检测、别名冲突、路径失效；
- 并发写：两个进程同时 upsert，断言不丢更新（这条最能证明文件锁真的在起作用）；
- `extension test` 跑一遍候选包：Manifest、命名空间、Capability ID 命名、
  依赖解析；
- 视图冒烟：窗口内增删改一次，重启 Agent 后数据仍在。

## 9. 风险与不做什么

| 风险 | 处理 |
| --- | --- |
| 每轮都 resolve，工具面被挤占 | 只在 6 节列出的触发点调用；一次会话一次自动登记 |
| 自动绑定绑错，把 A 的子项目挂到 B | 只有 `exact`（别名/末段精确一致、路径未占用）才进 L2；其余一律候选 |
| 注册表与 work-log 各写一份，名字漂移 | 只做链接 + 单向导入；work-log 侧不再扩展项目层级 |
| 并发写丢更新 | 跨进程文件锁（4 节），并有并发单测覆盖 |
| 模型把「登记了」理解成「可以写」 | SKILL.md 明写第 6 节第 6 条；能力返回值不带任何权限暗示 |
| 插件数据被备份/迁移覆盖 | 数据落在 `HIMIND_PLUGIN_DATA_ROOT`，与 Agent 备份同源；`export` 兜底 |

**本轮不做**：全盘后台扫描；跨机器同步；项目树多父（DAG）；
DSH rail 投影（3.1 留了结论）；「在 AI 中打开」按钮。

## 10. 以后可能要做的两件事

1. **「在 AI 中打开」按钮**：新增内核能力 `ai.workspace.start`
   （`ai` + `workspace` + 动作 `start`，三段且动作在内核词表内），
   只允许 `InvocationSource::Tauri` 调用；MCP 调用返回结构化错误而不是静默失败。
   实现即复用 `start_builtin_ai_session`（`src/app/commands.rs:1599`）。
   该**精确 ID** 要加进 `VIEW_EXTERNAL_CAPABILITY_PREFIXES`
   （`src/capability/plugin.rs:3994`）；加之前先把它改名为
   `EXTERNAL_CAPABILITY_PREFIXES`——它已经不只服务视图了。
2. **rail 投影**：3.1 的结论可以直接照做，但只在 Tauri 进程里，
   并带上「用户手动删了分组要不要重建」的 tombstone 规则。

## 11. 待定

**一个路径是否允许绑定多个项目？** 建议不允许，命中冲突就返回 `PATH_CONFLICT`，
让用户在窗口里显式移动。多绑定看着灵活，实际会让 `resolve` 变成「返回一个列表
让模型自己挑」，等于把歧义推给模型。

