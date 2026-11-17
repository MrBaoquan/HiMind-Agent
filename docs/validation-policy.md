# 分层验证与证据复用策略

生效日期：2026-11-17。适用于本仓库的人类与 AI 开发、长任务推进、合并、本机更新和正式发布。策略现在生效；文末列出的脚本优化尚未实现。用户对具体任务的验收约定优先，既有授权不因选择验证层级而失效。

## 为什么分层

完整发布验证不是日常编辑循环。2026-11-17 的 `4e9e6e5` candidate 约耗时 21 分 29 秒：Rust 全测及编译约 11 分 11 秒，四个 release 程序约 6 分 45 秒，其余为前端、依赖、VS Code 集成和身份检查。该轮 Agent 2,204 项通过、23 项既有 ignored，launcher/MCP/updater 共 55 项通过，VS Code 10 项通过。浏览器场景来自另行标明的前序验证，安装后另做 14 项冒烟，不将这些混称为同一轮全场景执行。

采用常见的测试分层、按影响选择、内容身份缓存和一次构建多次消费产物。耗时是观测与改进指标，不是超时即跳过检查的理由；冷构建和共享资源争用单独记录。

## 如何选择验证范围

| 层级 | 触发场景 | 必须覆盖 | 默认不做 |
| --- | --- | --- | --- |
| 文档 | 仅说明、注释、AI 指引；不改变合同或生成内容 | 差异、引用、示例与真实入口的一致性 | 应用编译、全测、安装 |
| 开发 | 单次实现或小修复 | 类型/编译或语法检查；改动模块及已识别调用方；缺陷对应回归 | 完整 candidate、release、生产安装 |
| 功能 | 一个功能完成、跨层协议变化 | 模块回归、受影响集成、相关 UI 自动化；检查公开契约与拒绝路径 | 无关业务 E2E、重复安装 |
| 主分支 | 功能批次合并、建立可复用基线 | 累计完整自动测试覆盖；可用基线加可证明的增量覆盖 | 每个提交都从头重跑 |
| 本机更新 | 用户要求更新安装版 | 合法构建收据、准确产物、安装后健康/Runtime/相关能力冒烟、可回退 | 安装相同产物时再次编译与全测 |
| 正式发布 | 发行包、公开版本 | 完整验证覆盖、发布渠道和签名、安装/升级/回退及本次关键用户流程 | 用开发检查代替发布门禁 |

文档中的参数名、权限语义、Schema 或示例若具有执行含义，按对应代码/合同影响验证，不能仅按扩展名当作文档。耗时目标：热缓存下日常检查尽量在几十秒至数分钟内反馈；功能验证控制在数分钟量级。首次编译和现有脚本回退可能超出目标，必须说明原因，不声称已经达到目标。

每项任务开始记录基线 HEAD 与文件差异，包含暂存、未暂存和新文件；跨会话继续使用原任务基线与已有证据。先读源码依赖再确定范围，不只看文件名或与当前 HEAD 的未提交差异。没有可靠基线时检查任务历史与相关模块；无法界定影响范围才扩大验证。

执行前简述：“本次修改 X，运行 A/B，复用 C（输入未受影响），结束条件 D。”这是执行说明，不是额外审批。已有正常运行的全量任务继续完成；不要为应用本策略中断它。

## 按改动选择检查

以下是人工选择起点，不是自动推导完整依赖图的承诺。修改共享接口、重导出、生成器或底层实现时，把其调用方加入。

| 改动 | 定向检查与需要补充的场景 |
| --- | --- |
| UI 文案、样式、局部组件 | TypeScript；相关 copy/layout/页面检查；影响交互或布局时跑对应浏览器场景；UI 文案不默认触发 Rust 全测 |
| 协作创建与指派 | `mission`、`app::mission_commands`、相关 `mcp`/`capability` 测试；前端 `check:mission-assignment-ui`、`check:mission-deep-link` 与相关浏览器场景 |
| 协作调度、并行、复核 | `app::mission_dispatch`、`mission`、相关 Workflow/执行器集成；按行为验证依赖门禁、写入隔离、失败/暂停恢复 |
| 预算及继续执行 | 预算 store、调度门禁、幂等和预算 UI；最终需要对应真实任务追加预算并继续的证据 |
| Windows 引号、序列化、进程执行 | 相应解析单测、`runtime::process`、真实 Windows 含空格/中文路径与取消；补受影响执行器集成，不默认重跑所有业务流程 |
| AI 服务与插件模型选择 | 服务路由/模型校验和相关 capability；`check:ai-service-routing`/`check:ai-service-onboarding`；相关服务选择浏览器场景 |
| 会话历史与活动 | `session_history`、`session_history_sources`、`session_service`、相关 MCP；`check:session-history-browser`；消费者合同变化时补 broker 链路 |
| VS Code 适配器 | 适配器目录内 `npm.cmd test`；涉及 Gateway/共享协议时补宿主与对应跨端集成 |
| 构建、缓存、部署脚本 | 相应 `check-build-*` / `check-deploy-local.ps1`；脚本语法与隔离夹具；影响安装时补实际安装/失败恢复，不因脚本文本变更自动重跑全部业务测试 |
| Rust 依赖、数据库格式、权限基础设施、共享协议 | 分析全体调用方；影响广泛或范围不确定时全量 Rust/前端/集成；存储迁移和权限拒绝路径不得省略 |

边界场景选择规则：命令启动改动要验取消/恢复；执行器交接改动要验真实跨执行器；预算改动要验追加与继续；状态投影/UI 改动要验真实状态及相关界面。未受影响的长流程可复用历史证据，写明边界。用户明确要求本轮实际执行的场景仍执行，不能用基线替代。

优先 Playwright 等 E2E；WebView2 HTML 层使用浏览器自动化。真实模型调用只覆盖需要模型/执行器/权限现场证明的关键路径，不能用夹具冒充真实任务。当前用户已确认不做读屏及 200% 缩放，不自动重新纳入。

## 现有命令与使用限制

以下命令现已存在。先配置 `docs/fast-build.md` 中的本机工具与受管构建根；不要为了复用或加速更改全局工具、业务 ACL 或生产状态。依赖存在且身份匹配时复用，锁文件变化或依赖缺失/损坏时才重新安装。

```powershell
# 在 frontend 目录中：类型与具体页面检查，不触发 Rust 全测。
./node_modules/.bin/tsc.cmd --noEmit
npm.cmd run check:mission-assignment-ui
# 需要生产前端产物时才运行，包含 TypeScript 与 Vite。
npm.cmd run build
# 需要某个浏览器场景时，从 package.json 选择对应 check:*browser。
```

Rust 定向检查在主仓库根目录运行，使用现有环境函数隔离缓存、临时目录和测试数据；示例筛选器需按当前任务替换：

```powershell
$ErrorActionPreference = 'Stop'
. ./scripts/build-support.ps1
$previousToolchain = @{}
$previousBuild = @{}
$buildLock = $null
try {
    $managedBuildRoot = Get-BuildRoot $env:HIMIND_BUILD_ROOT
    $buildLock = Enter-BuildLock $managedBuildRoot
    $previousToolchain = Initialize-BuildToolchain
    $previousBuild = Enter-BuildEnvironment $managedBuildRoot 'dev'
    cargo check --locked --bins
    if ($LASTEXITCODE -ne 0) { throw 'cargo check failed' }
    cargo test --locked --bin himind-agent 'app::mission_dispatch::tests::' -- --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw 'selected tests failed' }
} finally {
    Exit-BuildEnvironment $previousBuild
    Exit-BuildEnvironment $previousToolchain
    if ($buildLock) { $buildLock.Dispose() }
}
```

核实测试实际匹配数量，零匹配不能计作通过；必要时先以同一筛选器加 `-- --list` 盘点。其他 bin 的相关测试使用各自 `--bin`。不要把 `cargo check` 写成测试通过，也不要把普通 `--locked` 写成离线验证；只有显式 `--offline` 且依赖齐备时才是离线运行。

```powershell
# 在主仓库根目录：先解释现有 quick 会选择的范围。
pwsh -File scripts/verify.ps1 -Mode quick -Explain
# 综合开发检查：可能因未知改动、缺基线或工具变化退回全量。
pwsh -File scripts/verify.ps1 -Mode quick
# 完整 candidate 验证，不生成新的安装产物。
pwsh -File scripts/verify.ps1 -Mode candidate
# 功能收敛且需要新安装产物时：完整 candidate + release 构建。
pwsh -File scripts/build.ps1
# 之后部署同一 verified manifest；不再次调用 Cargo/npm。
pwsh -File scripts/deploy-local.ps1 -ArtifactManifest '<实际 verified manifest 路径>' -BuildRoot $env:HIMIND_BUILD_ROOT
```

当前 `quick` 按“上次对应槽的源码快照”计算影响，不等于任务完整差异，也不是几秒内结束的保证。没有基线、工具环境变化或 `src/app/*` 等未被映射识别的路径可触发全量。纯文档和已界定的小改动直接用适当的定向命令，不为使用 quick 而接受无关全量执行。

`build.ps1`、`verify.ps1 -Mode candidate` 都不是日常小修复默认入口。`verify.ps1 -Mode quick -Build` 当前不支持。安装/打包仍严格要求完整 candidate 收据；尚无 `affected`/`local` 验证 profile、定向收据导入器或绕过全量的受支持部署参数。

如果用户要求新安装版，当前在本次开发改动收敛后集中运行一次现有 candidate 构建并部署；已经匹配的步骤由现有内容校验缓存复用。如果产物身份不再匹配，解释失效原因并完成必要构建。不要手改 manifest、伪造 `full_tests=true`、弱化 `Read-BuildManifest` 或用 Skip 参数制造“已验证安装版”。文档/AI 说明提交本身不要求重建已安装的业务程序；报告当前源码 HEAD 与安装版提交不同及其原因。

## 复用、失败与收尾

可信复用需要原始成功记录，以及相关源码（包括测试和夹具）、依赖锁文件、工具/构建选项、合同和输出内容身份。仅时间新、版本号相同或旧报告一句“通过”不够。跨提交复用须证明相关输入未变；带嵌入 Git 身份的可执行文件不能仅因生产逻辑相同就宣称为新提交产物。

可以组合基线与增量结果，不要求所有用例来自同一轮。相关生产代码或依赖变化使对应旧结果失效；只修改一个测试时，该测试及其依赖范围重新验证，其余需核对身份后复用。保留旧失败日志，并记录定向复跑原因；新故障不能被旧成功覆盖。对待运行或失败的检查不得标记成功，ignored 不计为通过。

安装后的程序哈希、监听进程/路径/端口、HTTP/MCP 构建身份、Runtime 实际状态、相关能力可调用性必须现场核验。窗口“看起来正确”、旧截图或 stdio companion 的 `local_service_online=false` 都不能独立判断安装版健康；按各自 `*_expected` 与实际进程边界解释。

Rust 进程内用例目前共享环境变量、路径和端口，保留串行参数。允许无共享写入的前后端检查并行；固定构建根、同一前端 dist/node_modules 和部署遵守锁。先隔离测试，再评估 `cargo-nextest` 或分组并行，不直接把线程数拉高。

每项检查至少保留：命令/工作目录、检查范围与匹配数量、输入身份或快照、结果和耗时、原始日志位置；复用另记来源、哈希与未受影响理由。推荐在既有 `.himind/reviews/<任务>/` 中维护简短表格或 JSON，不另建庞大验收系统。现有脚本收据仍按其格式保存，不用自制表格替代安装门禁。

交付记录可用下表，状态明确区分“本轮执行”“历史复用”“忽略”“未执行”“失败”。未执行项说明影响边界和原因，不能算作覆盖。

| 检查 | 状态 | 输入/影响范围 | 来源与结果 |
| --- | --- | --- | --- |
| 示例：进程引号回归 | 本轮执行 | 修复后的进程解析与 Windows 启动 | 当前日志、匹配数量、退出码、耗时 |
| 示例：无关服务 UI | 历史复用 | 相关源码/依赖未变 | 原始成功日志及 SHA-256、身份比较 |
| 示例：正式发布签名 | 未执行 | 本次仅开发修改 | 不宣称已发布 |

结束条件：本次影响范围覆盖完整、相关失败解决或明确报告、产物/部署要求达到、未执行边界清楚。满足后停止验证；新改动、新失败或未解决风险才扩大/重跑。后续优化进入待办。不要为了在新提交号上得到“同一轮全绿”重跑所有步骤。

## 后续脚本优化，尚未实现

按以下顺序推进，另行实施时各自做定向验证，不能在后续会话中误认为这些能力已经可用：

1. 分离开发检查、测试、构建、部署入口；输出选择理由与实际范围。补充 `src/app`、会话与公开合同的人工依赖映射，保留无法判定时的扩大策略。
2. 纯测试缓存按相关内容和工具身份复用，Git commit 保留作来源；实际构建继续识别嵌入提交号、前端产物、公钥和 Runtime。减少无改动依赖目录重复扫描，保留缺失/损坏输出检查。
3. 在完整基线和定向证据可核验后，设计本机更新的正式收据与准入规则；不直接放宽正式发布门禁。验收“无关变更可复用、相关变化失效、篡改/缺失拒绝、来源可追溯”。
4. 隔离共享环境、目录与端口后评估测试并行；证明无互扰再启用。分别记录编译、运行、身份扫描和部署耗时。

本次策略文档落地不修改流水线行为，也不为了说明策略而重跑全测或重新部署 Agent。
