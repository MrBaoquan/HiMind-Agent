# Runtime Shell 权限与用户授权

## 用户体验与权限约定

用户可以在创建协作任务时，或在执行计划确认前选择 **完全访问**。确认计划后，HiMind AI 的执行命令按当前 Windows 用户权限运行，后续无需逐次确认，也不要求用户修改目录 ACL。已有任务因沙箱环境暂停时，弹窗提供“授予完全访问并继续”；该选项不依赖权限检查完成。

完全访问采用 Runtime 已有的 `danger-full-access` / `approval=never`，不为磁盘目录添加 FullControl，也不自动提升管理员权限。操作系统本来就拒绝当前用户访问的资源，仍可能拒绝；这不应被描述为需要用户修复 Agent 沙箱。任务目标、范围与交付验收仍有效；只读审查和独立验收保持只读。

此版本由内置 `himind.builtin` 执行器兑现该模式。其他执行器必须实现可验证的对应契约后才能开放，不静默忽略权限选择。

## 统一入口与状态

UI 与 MCP 共享 MissionStore 和宿主实现，不通过操作 UI 或改数据库推进生产任务。

| 入口 | 行为 |
|---|---|
| 创建任务 `workspace.permission_mode` | `workspace_write` 或 `full_access`；随任务计划确认后生效 |
| `mission.permission.set` | 仅允许执行前修改；更新计划摘要并使旧确认失效 |
| `runtime.workspace.check` | 只检查，不修改权限；返回当前目录诊断及指纹 |
| `runtime.workspace.resolve`，`action=full_access` | 经宿主用户授权后切换本任务权限并恢复；无需 ACL 指纹或修复 |
| 同能力 `repair` / `retry` | 高级隔离选项；按最新指纹补充最小权限或重新验证 |
| 同能力 `files_only` | 禁用专家命令工具与间接命令工具，仍保留宿主必需检查 |

运行中的权限切换始终绑定工作区、任务与已确认计划，通过宿主 R3 授权，不能由模型提交 `user_approved=true` 自行批准。执行前的模式设置不是执行授权，仍须确认更新后的计划。

沙箱环境失败时，任务进入 Waiting，并显示“命令环境待处理”，不会伪装成正在执行。新任务在创建尝试和消耗执行轮次之前检查；崩溃后恢复的排队任务也复用检查。普通 resume 或增加预算不能绕过尚未处理的环境阻塞。权限选择及恢复结果持久化；取消、计划变化和预算耗尽分别处理。

## 根因一：grantWrite / SetNamedSecurityInfoW / Win32 5

复现时当前用户拥有目录，实际可打开 WRITE_DAC，但 WRITE_OWNER 返回 Win32 5。UAC 过滤令牌中的 Administrators 组为 deny-only；所有者隐式权限与 Modify 不包含 WRITE_OWNER。Runtime 合并配置 DACL 和 Low 完整性标签时需要该权限，因此普通文件可写不代表能配置沙箱。

首选产品路径是用户直接授予任务完全访问，无需修复 ACL。保留隔离的高级路径由 `scripts/runtime-workspace-permissions.ps1` 执行：默认只检查；明确 `-Repair` 时仅为当前用户在选定根目录添加不继承的 WRITE_OWNER（0x80000）。保留所有者、拒绝项、继承与 SACL，先备份 DACL；恢复只在备份后的预期状态匹配时执行，避免覆盖后续变更。磁盘根、重解析路径、Agent 自身目录及其上层目录不做 ACL 扩权。

修复权限后还须通过真正的受限进程、管道子进程和 Runtime 所选 PowerShell 自检；仅 ACL 修改成功不算 Shell 已恢复。失败继续暂停并记录修复及备份证据。

## 根因二：受限 Node 管道子进程 spawn EPERM

Windows NPFS 在 CreateNamedPipe 未传显式安全描述符时，没有保留受限令牌 TokenDefaultDacl 中的 capability SID。Node/libuv 创建管道后，受限进程打开另一端失败，与工作区 WRITE_OWNER 是两个独立问题。

修复位于上层仓库 `scripts/runtime/patches/node-restricted-pipe-dacl.patch`：仅对实际受限令牌读取有效线程/进程的默认 DACL，显式传入管道创建。覆盖子进程 stdio 与命名管道接受路径；缺少 DACL 或查询失败时失败关闭，不把 Everyone 或用户 SID 塞进限制 SID 列表，也不改变普通进程行为。

`build-patched-node.ps1` 校验官方源码 SHA256、应用补丁、构建并记录工具链与二进制/补丁哈希。新 Runtime 使用新的不可变版本及签名；不热修改已安装的签名包。声明此补丁的 Runtime 会将内置 Node 放在命令 PATH 首位；使用 Agent 自有 TEMP 命名空间避免共享系统 TEMP 中遗留的 ACL 锁文件干扰。

## 验证与发布

已本地部署 Runtime：`0.2.2-rc.1.himind.1`，引擎 `0.2.2-rc.1`，Node `v24.22.2`；配套 Agent `0.3.92 local`。

- 包 SHA256：`255a00a18a013e10c03e1e9152492a00a808e18fe97bf34cf172c7faee4fbe97`。
- Node SHA256：`a926552584d75ed132d1dd571259262263b3e5b203ea02b5d2933acdbdb1d1e2`。
- 默认 `node --test` 子进程隔离、同步/异步管道、fork IPC、PowerShell7 调用均通过，未使用 `--test-isolation=none`。
- 工作区可写、工作区外拒写、只读拒写及外部哨兵文件保持不变均通过。
- 完全访问使用真实 SandboxPwshExecutor 验证：工作区外测试文件写入、管道子进程通过，沙箱 ACL 调用数为 0。
- Mission 权限计划确认、恢复持久化、取消/预算边界、MCP 授权及模态 UI 均有定向验证。
- Runtime 源包验证、版本检查与 Office DOCX→PDF 冒烟通过。
- 2026-11-12 安装版 MCP stdio 桥接、权限能力目录及只读诊断通过；原已完成 Mission 的状态、计划、任务、用量和更新时间一致。
- 安装版真实执行器完全访问通过，默认 `node --test` 为7/7，5个原交付文件哈希不变；Runtime状态为ready并报告修订包版本。

本地部署及安装版复验结果记录在 `.himind/long-tasks/20261111-runtime-shell-permissions/review.md`。原已完成的真实 Mission 不重新执行，不修改其计划、状态或交付文件。

显式指定另一份未修补的外部 Node 时，该二进制自身仍可能发生受限管道错误；普通 `node` 命令会使用带补丁的内置版本。

## 宿主版本与恢复兼容

Runtime 发布包修订号和引擎CLI版本分开校验：发布清单可声明 `engine_version`，未声明时兼容历史的同版本规则。status/probe 对匹配的已安装可执行文件报告包版本，并保留引擎版本信息，防止修订包被误判为旧版本。

MCP 发现记录同时校验进程仍存活及启动时间；Windows 已退出但仍有句柄的进程不能让旧管道一直被判定可用。相应回归保留退出进程的句柄，证明旧记录会被丢弃并允许宿主恢复服务。
