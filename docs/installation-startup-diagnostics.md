# HiMind Agent 安装与启动排查

这份文档用于处理两类现场问题：桌面快捷方式显示为白色/通用图标，以及点击快捷方式后没有窗口。

## 已确认的故障链

### 1. “安装成功”与“窗口可用”之前没有形成同一个验收条件

安装器以前只验证 Runtime 导入，然后在完成页启动 launcher。Tauri 主窗口在 `tauri.conf.json` 中配置为 `visible: false`，窗口是否成功创建依赖 WebView2 和用户数据目录；如果 WebView2 环境创建失败，进程可能仍然有托盘图标，但用户看不到主窗口。

安装器现在使用两种参数：桌面快捷方式和安装完成启动使用 `--show`，登录自启继续只启动托盘驻留。主窗口缺失、显示失败和 `builder.run()` 失败都会写入 `%LOCALAPPDATA%\HiMindAgent\logs\agent-events.jsonl`，并弹出可见错误。

### 2. WebView2 用户数据目录是进程级独占资源

生产 Agent 使用 `%LOCALAPPDATA%\com.himind.agent`，其它 profile 使用自己的 `ebwebview` 目录，并在并行实例时按端口分目录。开发实例、旧进程、异常退出后的锁文件或安全软件造成的占用，都可能表现为“托盘在、窗口没有”。诊断包会记录实际目录。

### 3. 快捷方式图标由 NSIS 资源决定，不能只看 PNG

桌面快捷方式的图标来自安装器生成的 `himind-agent.ico`。Windows Explorer 对损坏的 ICO 可能直接显示白色通用图标，而 NSIS 仍然可以接受并创建快捷方式。构建脚本现在验证 ICO 头、每个目录条目、PNG 载荷边界和 16/24/32/48/64/256 多尺寸集合；缺失任一项会让安装器构建失败。

### 4. 启动错误以前容易丢失

安装版 Agent 是 Windows GUI 子系统，标准错误输出对双击用户不可见。现在 launcher 自身启动失败会弹窗；Agent UI 启动失败会记录错误并弹窗。诊断命令和诊断包不依赖 WebView，可以在白屏或无窗口时使用。主窗口由 setup 阶段显式创建，默认 WebView2 数据目录失败时会保留原目录并用隔离目录重试；若窗口对象创建成功但 15 秒内没有页面事件，启动看门狗会再次隔离重试。

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
| 双击完全无反应 | `launcher` 检查和安装目录是否存在 | 从 PowerShell 运行 launcher，读取弹窗原因；检查安全软件/AppLocker 拦截记录 |
| 安装器中止 | `logs\runtime-install.log` | 先看签名、磁盘空间和 Runtime 包完整性；把诊断包交给支持人员 |
| 窗口打开后白屏 | `agent-events.jsonl` 的 `界面页面加载` 记录 | 有 Started 无 Finished 通常是资源/WebView 加载问题；没有 Started 通常是 WebView 创建失败 |

## 发布门禁

每次发布 Windows 安装器至少执行：

```powershell
.\scripts\build.ps1 -Configuration release
.\scripts\package.ps1 -Version <version> -SkipBuild
.\scripts\build-installer.ps1 -ReleaseDirectory .\artifacts\<version> -RequireAuthenticode
```

在干净 Windows 用户环境验证：安装、桌面快捷方式图标、双击显示主窗口、注销后自动启动只显示托盘、从托盘打开主窗口、卸载后快捷方式和协议注册清理。将 `diagnostics check` 作为安装后冒烟测试；不要把“安装器进度完成”当成桌面可用的唯一信号。

## 后续优化优先级

1. 将 Windows WebView2 Evergreen Runtime 的存在性和版本纳入安装器预检，并在缺失时给出官方安装入口。
2. 在 CI 的干净 Windows runner 上安装构建出的 NSIS 包，调用 `diagnostics check`，并验证 `.lnk` 目标、参数和图标资源。
3. 收集匿名的启动阶段指标：launcher 启动、WebView 创建、页面 Started/Finished、主窗口首次显示；指标只记录版本、阶段和错误码。
4. 为安装器和四个 Windows 二进制启用 Authenticode 签名和时间戳，减少 SmartScreen/安全软件导致的“无反应”误判。
5. 为升级失败保留可运行的 previous 版本，并让 launcher 在 active-version 无效时自动回退后再显示错误。
