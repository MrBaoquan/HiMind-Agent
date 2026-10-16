# HiMind Agent 安装与启动排查

这份文档用于处理三类现场问题：桌面快捷方式显示为白色/通用图标，点击快捷方式后没有窗口，以及启动过程中反复闪出的黑色控制台窗口。

## 已确认的故障链

### 1. “安装成功”与“窗口可用”之前没有形成同一个验收条件

安装器以前只验证 Runtime 导入，然后在完成页启动 launcher。Tauri 主窗口在 `tauri.conf.json` 中配置为 `visible: false`，窗口是否成功创建依赖 WebView2 和用户数据目录；如果 WebView2 环境创建失败，进程可能仍然有托盘图标，但用户看不到主窗口。

现在的判据是**「后台启动」而不是 `--show`**：桌面快捷方式、协议唤起和安装完成启动一律显示主窗口，只有登录自启显式带 `--hidden` 时才驻留托盘。

这条判据的方向很关键：自更新（工作台上的软件分发）只替换二进制、**不会重写桌面快捷方式**。0.3.69 曾把主窗口改成「只有 `--show` 才显示」，而 `--show` 只写在 0.3.69 安装器创建的快捷方式里，于是自更新上来的机器双击后进程在跑、窗口却永远不出现。默认显示消除了这类「升级即失联」的组合。

launcher 同时把登录自启项（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 的 `HiMindAgent`）同步成带 `--hidden` 的形式——只在自启项已存在时维护，用户没开「开机启动」时不会替用户创建。

主窗口缺失、显示失败和 `builder.run()` 失败都会写入 `%LOCALAPPDATA%\HiMindAgent\logs\agent-events.jsonl`，并弹出可见错误。

### 2. WebView2 用户数据目录是进程级独占资源

生产 Agent 使用 `%LOCALAPPDATA%\com.himind.agent`，其它 profile 使用自己的 `ebwebview` 目录，并在并行实例时按端口分目录。开发实例、旧进程、异常退出后的锁文件或安全软件造成的占用，都可能表现为“托盘在、窗口没有”。诊断包会记录实际目录。

### 3. 快捷方式图标由 NSIS 资源决定，不能只看 PNG

桌面快捷方式的图标来自安装器生成的 `himind-agent.ico`。Windows Explorer 对损坏的 ICO 可能直接显示白色通用图标，而 NSIS 仍然可以接受并创建快捷方式。构建脚本现在验证 ICO 头、每个目录条目、PNG 载荷边界和 16/24/32/48/64/256 多尺寸集合；缺失任一项会让安装器构建失败。

### 4. 启动错误以前容易丢失

安装版 Agent 是 Windows GUI 子系统，标准错误输出对双击用户不可见。现在 launcher 自身启动失败会弹窗；Agent UI 启动失败会记录错误并弹窗。诊断命令和诊断包不依赖 WebView，可以在白屏或无窗口时使用。主窗口由 setup 阶段显式创建，默认 WebView2 数据目录失败时会保留原目录并用隔离目录重试；若窗口对象创建成功但 15 秒内没有页面事件，启动看门狗会再次隔离重试。

### 5. 启动时反复闪出的黑窗口是控制台子进程

0.3.69 及更早的启动路径上有两个控制台窗口来源：

- **launcher 是控制台子系统程序。** Explorer 没有控制台，Windows 会先给它分配一个控制台窗口，launcher 只能在进程跑起来之后才 `FreeConsole`。用户看到的就是双击后先闪一个黑框，直到 Agent 窗口出现。
- **Agent 是 GUI 子系统程序，却在启动时按 7 条存储路径各起一次 `icacls.exe` 读完整性标签。** GUI 进程没有控制台，于是每个 icacls 子进程都分到一个自己的控制台窗口——「多轮黑窗口不停出现、消失」正是这 7 次探测。0.3.69 还把主窗口显示判据放在 `--show` 上，所以这台机器上「窗口最后才出现」的黑框期比正常更长。

0.3.70 的处理：

- launcher 改成 Windows **GUI 子系统**，桌面双击不再创建控制台窗口；`diagnostics`、`runtime`、`trust` 子命令和 MCP 隧道不受影响，因为它们读的是调用方交下来的 std 句柄（有句柄就继承，没有就退化成 `NUL`，避免 `println!` 在无效句柄上 panic）。
- 存储完整性级别改为直接读强制标签 SID，启动期不再启动任何 icacls；只有确实需要修复时才调用一次 `icacls /setintegritylevel`，并且带 `CREATE_NO_WINDOW`。
- launcher 派生的每个子进程都带 `CREATE_NO_WINDOW`：没有控制台的父进程若派生控制台子进程（MCP companion、Agent 内部的命令行助手），否则 Windows 会给它新开一个窗口。

启动路径之外还有同类泄漏，0.3.71 一并修掉：

- **每次启动 HiMind AI 会话都会闪黑窗口。** 工作区 DSH home 的 `profiles\node_modules` 目录联接、以及 `storages`/`sessions` 的共享联接都靠 `cmd.exe /D /C mklink /J` 落地，而这两处没有带 `CREATE_NO_WINDOW`。`profiles` 联接每次会话都会被删掉重建，所以「多轮黑窗口不停出现、消失」在开会话时同样成立。现在两处都走 `runtime::process::hidden_command`。
- **提交候选制品时的 git。** `extension_projects::resolve_source_commit` 用 `git -C <工作区> rev-parse HEAD` 兜底提交号，重定向了标准输出但没有隐藏窗口——重定向并不阻止 Windows 给控制台子进程开窗口。同样改走 `hidden_command`。
- **工作区构建脚本。** Unreal 的 RunUAT 与 `.ps1`/`.cmd` 构建脚本原先靠调用方在 spawn 处补 `CREATE_NO_WINDOW`，现在由 `native_build_command`/`script_build_command` 自己带上，避免下一个调用方漏掉。

测量手段（不需要管理员权限，只读）：

```powershell
cd <安装目录或仓库>
.\scripts\startup-window-watch.ps1 -Seconds 20
```

脚本用与桌面快捷方式相同的方式启动 Agent（`Start-Process` 走 ShellExecute），记录期间出现的每个顶层窗口（类名、标题、进程、存活时长）和每个新进程（含父进程链）。控制台窗口的标题就是窗口里跑的那个程序，所以标题出现 `C:\WINDOWS\system32\icacls.exe` 就说明是启动期的 icacls 探测。`-NoLaunch` 可在自己双击快捷方式时只做记录。

两处工具本身的坑，都已经修掉，看历史结论时要留意：

- 窗口采集原先用 `EnumWindows` 每 5 ms 采样一次，实测每次循环要 ~20 ms，15 ms 的控制台窗口会被整轮跳过。现在用 `SetWinEventHook` 事件驱动，没有采样间隙；同时用 Toolhelp32 快照记录新进程和父进程链，作为窗口太短看不到时的兜底证据。
- 控制台窗口的类名在这台机器上是 `PseudoConsoleWindow`（由 console 客户端进程持有），默认终端宿主再叠一个 `CASCADIA_HOSTING_WINDOW_CLASS` 窗口。旧版脚本把 `PseudoConsoleWindow` 从控制台计数里排除，**会把闪黑窗口的启动报成 quiet**。现在三个类名都计入。
- 判定标准也变了：旧版打 `RESULT: no console window appeared`，现在打 `RESULT: no console window was shown - this start is quiet.`；按行名判断的地方要跟着改。

自检：把 `-Target` 指向 `cmd.exe` 应看到 2 个控制台窗口（`PseudoConsoleWindow` 与终端宿主窗口），这同时验证了工具不是「永远报 quiet」。

```powershell
.\scripts\startup-window-watch.ps1 -Seconds 8 -Target "$env:SystemRoot\System32\cmd.exe" -Arguments @('/D','/C','timeout 2')
```

本机实测（0.3.70 安装版，干净冷启动，事件驱动记录 28 s）：`console windows that appeared and were shown: 0`，主窗口在双击后约 5.6 s 出现；同一台机器同一个工具在 `cmd.exe` 目标上报告 2 个控制台窗口。启动期仍会派生 `git`、`node`、`powershell`、`cmd`（拓展源对账、沙箱写权限预热 5 个工作区、客户端探测），它们的 `conhost` 子进程都在，但都没有窗口——说明这些位置都已经带上隐藏标志。

`package.ps1` 现在有两道门禁：子系统断言（launcher/Agent 必须是 GUI、MCP companion 必须是控制台）和 `scripts/check-console-spawns.ps1`（字面量控制台程序不得在没带隐藏标志的函数里派生），打包阶段就会拦住回归。
## 用户排查步骤

在 PowerShell 中执行以下命令。路径带空格时保留引号。

```powershell
$root = "$env:LOCALAPPDATA\HiMindAgent"
& "$root\himind-agent-launcher.exe" --state "$root\data\agent-state.json" diagnostics check
```

该命令输出 JSON 并使用退出码：`0` 表示检查通过，`2` 表示存在警告，`1` 表示安装布局或关键依赖失败。`--state` 必须放在 `diagnostics` 子命令之前。检查内容包括：版本目录和 `active-version`、launcher/updater、ICO 格式、WebView2 用户数据目录可写性和锁文件、最近启动是否出现页面 `Started/Finished` 事件、内置 Runtime 状态和桌面快捷方式。

导出支持包：

```powershell
& "$root\himind-agent-launcher.exe" --state "$root\data\agent-state.json" diagnostics export
```

默认输出到 `%LOCALAPPDATA%\HiMindAgent\diagnostics\`。支持包包含 `diagnostics.json`、界面启动记录、Runtime 安装日志、更新日志、SVN 日志和崩溃转储，不包含密码、Agent credential 或 OAuth token。

现场快速判断：

| 现象 | 优先检查 | 处理 |
| --- | --- | --- |
| 白色/通用图标 | `diagnostics check` 的 `icon_format` | 重新安装同一版本；若仍失败，收集诊断包和安装器 SHA-256 |
| 进程和本地端口存在但没有窗口/托盘 | `startup_page_event`、`webview_data_lock` 和 `agent-events.jsonl` | 先运行 `diagnostics export`；退出所有 HiMind Agent 进程后重试，让新版本的隔离目录自愈；不要在 SSH Session 0 中启动 GUI |
| 日志反复出现 `attempt to write a readonly database` | `storage_integrity`、`local-runs.sqlite3-wal` 和 `local-runs.sqlite3-shm` | 退出 Agent 后运行安装目录中的 `repair-storage.ps1`；它只修复 Agent 数据、SQLite sidecar 和 WebView2 目录的完整性级别，不删除数据库 |
| `agent protocol registration repair failed: Access denied` | `protocol_registration` 与启动日志中的 `source=launcher` | 以当前用户运行 `diagnostics check`；确认协议注册指向当前安装，若完整性为 Low，先运行 `repair-storage.ps1` 后重新双击 |
| 第二次双击没有把窗口带到前台 | `agent-events.jsonl` 中的“第二次启动参数已转发” | 如果没有该记录，检查 launcher/单实例事件窗口；如果有记录但页面事件仍为空，按 WebView2 数据目录问题处理 |
| 双击后反复闪出黑窗口 | `scripts/startup-window-watch.ps1` 输出里的控制台窗口标题与「processes started under a HiMind process」父进程链 | 标题/父进程是 `icacls.exe` 或 `node.exe` 说明是 0.3.68 及更早的启动期探测与沙箱预热，升级到 0.3.70+；`cmd.exe` 出现在开 HiMind AI 会话时说明是 DSH 目录联接（0.3.71 起隐藏）；标题是 launcher 自身说明 launcher 退回了控制台子系统，重新安装正式包；注意类名是 `PseudoConsoleWindow` 的窗口也要算，0.3.70 之前的采样脚本会漏报 |
| 双击完全无反应 | `launcher` 检查和安装目录是否存在 | 从 PowerShell 运行 launcher，读取弹窗原因；检查安全软件/AppLocker 拦截记录 |
| 自更新后双击只见到托盘、没有窗口 | 快捷方式参数（右键属性→目标）是否仍带 `--show` | 0.3.69 及更早：双击第二次可把窗口唤到前台，或重新运行安装器重建快捷方式；0.3.70 起默认显示窗口，不再依赖该参数 |
| 安装器中止 | `logs\runtime-install.log` | 先看签名、磁盘空间和 Runtime 包完整性；把诊断包交给支持人员 |
| 窗口打开后白屏 | `agent-events.jsonl` 的 `界面页面加载` 记录 | 有 Started 无 Finished 通常是资源/WebView 加载问题；没有 Started 通常是 WebView 创建失败 |

## 发布门禁

每次发布 Windows 安装器至少执行：

```powershell
.\scripts\build.ps1 -Configuration release
.\scripts\package.ps1 -Version <version> -SkipBuild
.\scripts\build-installer.ps1 -ReleaseDirectory .\artifacts\<version> -RequireAuthenticode
```

启动窗口门禁：`package.ps1` 有两道断言——launcher 与 Agent 是 GUI 子系统、MCP companion 是控制台子系统；以及 `scripts/check-console-spawns.ps1`，字面量控制台程序（cmd、powershell、git、node、icacls、taskkill…）不得在没带 `hidden_command`/`configure_hidden_process`/`creation_flags` 的函数里派生。安装完成后在桌面双击快捷方式，并用

```powershell
.\scripts\startup-window-watch.ps1 -Seconds 20
```

确认输出为 `RESULT: no console window was shown - this start is quiet.`。

在干净 Windows 用户环境验证：安装、桌面快捷方式图标、双击显示主窗口、注销后自动启动只显示托盘、从托盘打开主窗口、卸载后快捷方式和协议注册清理。将 `diagnostics check` 作为安装后冒烟测试；不要把“安装器进度完成”当成桌面可用的唯一信号。

## 后续优化优先级

1. 将 Windows WebView2 Evergreen Runtime 的存在性和版本纳入安装器预检，并在缺失时给出官方安装入口。
2. 在 CI 的干净 Windows runner 上安装构建出的 NSIS 包，调用 `diagnostics check`，并验证 `.lnk` 目标、参数和图标资源。
3. 收集匿名的启动阶段指标：launcher 启动、WebView 创建、页面 Started/Finished、主窗口首次显示；指标只记录版本、阶段和错误码。
4. 为安装器和四个 Windows 二进制启用 Authenticode 签名和时间戳，减少 SmartScreen/安全软件导致的“无反应”误判。
5. 为升级失败保留可运行的 previous 版本，并让 launcher 在 active-version 无效时自动回退后再显示错误。
