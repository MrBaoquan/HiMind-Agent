# 本地快速构建与空间管理

先按 [分层验证策略](validation-policy.md) 选择日常开发、功能、主分支、本机更新或正式发布范围；AI 先读仓库根目录 [AGENTS.md](../AGENTS.md)。本文说明现有脚本与机器配置，不代表每次修改都应运行完整候选构建。纯文档不运行应用构建；小改动可直接选择策略中的定向检查。

支持 Windows x64 开发机。需要 PowerShell 7、Git、Rust MSVC、Visual Studio C++ x64/x86 工具、Windows 10/11 SDK、Node.js 22.6+ 和 npm 9+；不需要 sccache。工具版本差异会使本机验证收据失效并重新执行相关步骤。
每台机器把专用 Git worktree 和构建根放在本地空间充足的固定磁盘；不要在网络盘、云同步目录或多机共享目录中使用可变构建根。不同 worktree 不共享可变源码、`frontend/dist` 或 `node_modules`。

```powershell
# 按这台机器的盘符和可用空间设置；也可以完全使用默认的 LOCALAPPDATA 路径。
$env:HIMIND_BUILD_ROOT = 'D:/HiMindAgentBuilds/shared-v1'
# 每台机器分别配置已签名 Runtime 与官方公钥路径，不复制私钥。
$env:HIMIND_RUNTIME_RELEASE_DIRECTORY = 'D:/HiMindRuntime/release-matching-repository-manifest'
$env:HIMIND_SIGNING_PUBLIC_KEY_PATH = "$env:LOCALAPPDATA/HiMindAgent/trusted-keys/himind-production-2026.pem"
$env:HIMIND_SIGNING_KEY_ID = 'himind-production-2026'
$env:HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH = "$env:LOCALAPPDATA/HiMindAgent/trusted-keys/himind-extension-production-2026.pem"
$env:HIMIND_EXTENSION_SIGNING_KEY_ID = 'himind-extension-production-2026'
# 在开始较长的构建前确认这台机器的依赖与目标架构。
pwsh -File scripts/check-build-machine.ps1 -BuildRoot $env:HIMIND_BUILD_ROOT
# 只解释影响范围，不安装依赖、不编译。
pwsh -File scripts/verify.ps1 -Mode quick -Explain
# 综合开发检查，Cargo 原生增量；未知影响/缺基线/工具变化可能退回全量。
# 已界定的小改动优先按验证策略直接运行相关检查。
pwsh -File scripts/verify.ps1 -Mode quick
# 完整功能验证并冻结四个可安装 EXE。先配置公钥环境变量，见下文。
pwsh -File scripts/build.ps1
# 使用同一 verified manifest 打包；打包不会再次编译 Agent。
pwsh -File scripts/package.ps1 -ArtifactManifest '<build-manifest.verified-*.json>' -BuildRoot $env:HIMIND_BUILD_ROOT
# 直接安装返回的 verified manifest；不调用 Cargo/rustc/npm。
pwsh -File scripts/deploy-local.ps1 -ArtifactManifest '<build-manifest.verified-*.json>'
# 显示可回收项；Apply 才删除。预算按实际项目大小调整。
pwsh -File scripts/build-cache.ps1 -TargetBudgetBytes 40GB
pwsh -File scripts/build-cache.ps1 -TargetBudgetBytes 40GB -Apply
```

构建根也可用各入口的 `-BuildRoot` 指定；不指定时使用 `%LOCALAPPDATA%/HiMindAgentBuilds/shared-v1`。`-RuntimeReleaseDirectory` 可覆盖 Runtime 环境变量。相对路径按当前 repository 解析，跨机器建议用绝对路径。
脚本只接管带自己 ownership 标记的目录，拒绝已有非空陌生目录。
根下只有 dev/verify 两个固定 target 槽；共享写锁覆盖依赖、编译、验证和制品冻结，冲突时直接说明并退出。
quick 使用开发增量，candidate 关闭增量并固定较小调试符号；release 保留 Cargo 默认优化配置。
构建环境仅在当前进程作用域内设置并恢复，Cargo 下载和 npm 缓存、临时文件归该根所有。
固定 Cargo home 不复制用户全局凭据；私有依赖需在该环境中另行配置。
`-UseSccache` 可选，缺失时正常降级，缓存上限 10G。
机器预检会在安装依赖或编译之前检查 Windows x64、PowerShell、Git、Rust host target/format、Node/npm、Visual Studio C++ 工具、Windows SDK 和构建卷空间；完整候选还会先核对精确签名 Runtime，缺项时打印安装/配置提示并停止。

构建根、Cargo/npm 缓存、验证收据、证据和 verified manifest 是本机数据。身份包含 repository、工具链和 Runtime 位置；不要在开发机间复制或合并 `HIMIND_BUILD_ROOT`。每台机器独立运行 candidate 验证和构建；要传给另一台机器的是最终 package/installer，而不是带有绝对证据路径的 verified manifest。Git 分支和源码可通过团队正常 Git 流程同步。公钥可单独分发，签名私钥只留在受控签名环境。

构建签名环境变量：HIMIND_SIGNING_PUBLIC_KEY_PATH、HIMIND_SIGNING_KEY_ID、HIMIND_EXTENSION_SIGNING_PUBLIC_KEY_PATH、HIMIND_EXTENSION_SIGNING_KEY_ID。
本地安装默认读取 AgentHome/trusted-keys 下的官方公钥；公钥内容必须与产物记录匹配。
可使用 `-AgentHome <独立目录> -LocalPort <空闲端口> -StatePath <独立状态>` 隔离安装，显式传入两个公钥路径。
独立 AgentHome 默认使用 build-acceptance profile，避免共享生产 WebView/单实例资源。
安装前核验四个 EXE、公钥、Runtime、完整验证及原始复用证据；逐卷计算复制、备份、解压和回滚空间，再停旧进程。
Runtime 仅在已记录的包/签名、安装内容摘要和实际版本探测全部匹配时复用；仅版本号相同不够。
部署保留 EXE/Runtime 包备份；失败自动恢复文件、Runtime 状态指针、active-version 和原有可用性。
成功部署会固定 current/previous manifest 引用；重复安装同一产物保留原 previous。

每步明确 EXECUTED / REUSED / FAILED。evidence/<运行>/result.json 含本轮状态，REUSED 的 source 指向原始成功执行及 SHA-256。
源码含脏文件按内容哈希；锁文件、工具/构建参数、公钥内容、前端 dist、Runtime 和 Git 提交改变会使相关结果失效。
依赖和冻结产物亦校验内容。失败不进入缓存；中途源码变更使整轮失败。
candidate 覆盖 Rust 完整测试、前端 check:ui 和 VSCode 扩展测试；quick 按模块选择测试，未知改动扩大检查。quick 当前仍进入前端构建/检查、身份扫描和 Cargo 检查流程，并非所有小改动都能快速结束；先 Explain，避免把入口名称当作耗时承诺。
格式检查限工作区变更的 Rust 文件（无变更时检查 build.rs），不声称清除了历史全仓格式差异。
安装、健康、取消恢复等现场检查必须重新执行，不从历史收据推断当前可用。
旧 -SkipTests/-SkipIntegration/-SkipDependencyInstall 可用于开发构建，但不会生成允许部署的完整验证 manifest。
当前安装门禁仍要求完整 candidate 收据，不接受策略中的普通定向验证记录；本机更新的定向收据准入是后续待办，不能据此跳过现有校验。无需新产物的检查结束后停止，不为维持安装版与纯文档提交号一致而再次发布。
GitHub 发布脚本可用 -ArtifactManifest/-BuildRoot 消费 release-channel verified manifest，并把同一 Runtime 传给安装包；它会校验 release channel、版本、制品收据与 Runtime 哈希。公开发布仍需配置发布签名；本机 unsigned package 只作打包验证，不是可发布签名产物。

回收默认保留最近两个完整制品、所有 current/previous pins、最近使用的 target 槽及全部证据。
超预算缓存可重建，Apply 会清空对应缓存目录；固定 target 总量仍超预算时会明确报告保留部分超标。
预算是回收策略而非文件系统硬配额。应定期运行回收；证据、隔离实例和未完成 staging 不自动删除，以便故障调查。
Plan/Apply 使用同一写锁，并检查遗留原生进程、路径归属和目录链接；不涉及源码、生产目录或别的项目。
回滚代码：还原本分支构建脚本和 build.rs；旧候选/备份不受缓存清理影响。无需变更全局工具配置。

可重复检查：check-build-machine.ps1、check-build-support.ps1、check-build-pipeline.ps1、check-build-cache.ps1、check-deploy-local.ps1。
前三者接受 FixtureRoot，建议也放 G 盘。pipeline 检查使用真实小型 Rust/npm 工程，不能代替实际 Agent 安装验证。
本机实测结果与来源见本任务 .himind/long-tasks/20261116-fast-build-storage-optimization；不把小工程耗时当成 Agent 性能。

本机隔离实测（Windows x64、Rust 1.97.1、Node 22.18.0；观测到其他任务并行编译）：完整 Agent 候选 1,098s，包含 Rust 全测 474s 和 release 构建 528s；无改动候选 36.7s，8 个阶段均从哈希核对过的成功收据复用；Rust 单文件变更 360s，UI 变更 477s（前端重建 1.8s）。这轮完整验证覆盖 2,162 个 Rust 测试通过、VS Code 10 项测试通过、前端检查通过。另一次 513s 冷 release 编译只作为编译耗时记录；该操作最终因并发源码漂移失败，且没有安装，不能计作冷候选通过。

首次隔离安装 39.5s，重复安装 30.1s；使用同一 verified manifest 的本机打包通过，四个 EXE 和 Runtime SHA-256 与冻结制品一致。仅 package/check 脚本末次修复使用定向验证，没有为此重跑 Rust 全测；完整候选来自 `agent-candidate-result.json`，安装/回滚/拒绝证据分别在 `agent-install-*-result.json`，打包在 `agent-package-validation.json`。verify target 实测约 4.55 GB；全部实际路径、耗时、并行采样说明见本机 evidence/agent-measurements.json。Windows 卷采样包含并行任务，不代表本任务进程峰值。
