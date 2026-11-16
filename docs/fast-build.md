# 本地快速构建与空间管理

需要 PowerShell 7、Cargo/MSVC、Node/npm。延续现有工具，不需要 sccache。
推荐将专用 Git worktree 和构建根放在空间较大的卷；不同任务不共享可变源码、frontend/dist 或 node_modules。

```powershell
$env:HIMIND_BUILD_ROOT = 'G:/HiMindAgentBuilds/shared-v1'
# 只解释影响范围，不安装依赖、不编译。
pwsh -File scripts/verify.ps1 -Mode quick -Explain
# 日常检查，Cargo 原生增量；每次检查 dist/依赖内容，调用 Cargo 检查缺失输出。
pwsh -File scripts/verify.ps1 -Mode quick
# 完整功能验证并冻结四个可安装 EXE。先配置公钥环境变量，见下文。
pwsh -File scripts/build.ps1 -RuntimeReleaseDirectory '<已签名 Runtime 目录>'
# 使用同一 verified manifest 打包；打包不会再次编译 Agent。
pwsh -File scripts/package.ps1 -ArtifactManifest '<build-manifest.verified-*.json>' -BuildRoot $env:HIMIND_BUILD_ROOT
# 直接安装返回的 verified manifest；不调用 Cargo/rustc/npm。
pwsh -File scripts/deploy-local.ps1 -ArtifactManifest '<build-manifest.verified-*.json>' -RuntimeReleaseDirectory '<已签名 Runtime 目录>'
# 显示可回收项；Apply 才删除。预算按实际项目大小调整。
pwsh -File scripts/build-cache.ps1 -TargetBudgetBytes 40GB
pwsh -File scripts/build-cache.ps1 -TargetBudgetBytes 40GB -Apply
```

构建根也可用各入口的 `-BuildRoot` 指定；不指定时使用 LOCALAPPDATA/HiMindAgentBuilds/shared-v1。
脚本只接管带自己 ownership 标记的目录，拒绝已有非空陌生目录。
根下只有 dev/verify 两个固定 target 槽；共享写锁覆盖依赖、编译、验证和制品冻结，冲突时直接说明并退出。
quick 使用开发增量，candidate 关闭增量并固定较小调试符号；release 保留 Cargo 默认优化配置。
构建环境仅在当前进程作用域内设置并恢复，Cargo 下载和 npm 缓存、临时文件归该根所有。
固定 Cargo home 不复制用户全局凭据；私有依赖需在该环境中另行配置。
`-UseSccache` 可选，缺失时正常降级，缓存上限 10G。

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
candidate 覆盖 Rust 完整测试、前端 check:ui 和 VSCode 扩展测试；quick 按模块选择测试，未知改动扩大检查。
格式检查限工作区变更的 Rust 文件（无变更时检查 build.rs），不声称清除了历史全仓格式差异。
安装、健康、取消恢复等现场检查必须重新执行，不从历史收据推断当前可用。
旧 -SkipTests/-SkipIntegration/-SkipDependencyInstall 可用于开发构建，但不会生成允许部署的完整验证 manifest。
GitHub 发布脚本可用 -ArtifactManifest/-BuildRoot 消费 release-channel verified manifest，并把同一 Runtime 传给安装包；它会校验 release channel、版本、制品收据与 Runtime 哈希。公开发布仍需配置发布签名；本机 unsigned package 只作打包验证，不是可发布签名产物。

回收默认保留最近两个完整制品、所有 current/previous pins、最近使用的 target 槽及全部证据。
超预算缓存可重建，Apply 会清空对应缓存目录；固定 target 总量仍超预算时会明确报告保留部分超标。
预算是回收策略而非文件系统硬配额。应定期运行回收；证据、隔离实例和未完成 staging 不自动删除，以便故障调查。
Plan/Apply 使用同一写锁，并检查遗留原生进程、路径归属和目录链接；不涉及源码、生产目录或别的项目。
回滚代码：还原本分支构建脚本和 build.rs；旧候选/备份不受缓存清理影响。无需变更全局工具配置。

可重复检查：check-build-support.ps1、check-build-pipeline.ps1、check-build-cache.ps1、check-deploy-local.ps1。
前三者接受 FixtureRoot，建议也放 G 盘。pipeline 检查使用真实小型 Rust/npm 工程，不能代替实际 Agent 安装验证。
本机实测结果与来源见本任务 .himind/long-tasks/20261116-fast-build-storage-optimization；不把小工程耗时当成 Agent 性能。

本机隔离实测（Windows x64、Rust 1.97.1、Node 22.18.0；观测到其他任务并行编译）：完整 Agent 候选 1,098s，包含 Rust 全测 474s 和 release 构建 528s；无改动候选 36.7s，8 个阶段均从哈希核对过的成功收据复用；Rust 单文件变更 360s，UI 变更 477s（前端重建 1.8s）。这轮完整验证覆盖 2,162 个 Rust 测试通过、VS Code 10 项测试通过、前端检查通过。另一次 513s 冷 release 编译只作为编译耗时记录；该操作最终因并发源码漂移失败，且没有安装，不能计作冷候选通过。

首次隔离安装 39.5s，重复安装 30.1s；使用同一 verified manifest 的本机打包通过，四个 EXE 和 Runtime SHA-256 与冻结制品一致。仅 package/check 脚本末次修复使用定向验证，没有为此重跑 Rust 全测；完整候选来自 `agent-candidate-result.json`，安装/回滚/拒绝证据分别在 `agent-install-*-result.json`，打包在 `agent-package-validation.json`。verify target 实测约 4.55 GB；全部实际路径、耗时、并行采样说明见本机 evidence/agent-measurements.json。Windows 卷采样包含并行任务，不代表本任务进程峰值。
