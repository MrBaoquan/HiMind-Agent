# 跨执行器运行与交接契约

本实现属于 `executor-lifecycle` 的源码交付。报告的 `passed` 只表示可送宿主核验，不表示 Mission 已通过、生产已部署或独立 VERIFY 已执行。启动前七文件 full_access 修复和后续受控宿主补丁属于前置基线，未作为本执行者的新成果。

## 问题模型与边界

Workflow 和 Dashboard AgentRun 是两条入口，不能用一条入口的测试代表另一条。需要同时保证：个人 Codex 的 cwd/权限及个人模型来源不变；有限输入与输出不会阻塞取消；成功必须有完整终态；账本保留失败、会话和未知成本；交接只能使用既有路径授权；实现报告不能代替宿主检查和独立核验。

采用共享 Codex 传输实现，而不是复制两套协议规则。stdin 使用最大 16 MiB 的临时文件句柄，避免同步管道写入与 stdout/stderr 相互等待；Copilot 的 argv 输入仍限制 24,000 UTF-16 单元，Codex/builtin stdin 与 ACP 协议不受此 argv 限制。Windows 文件在最后句柄关闭时删除，Unix 文件创建为 0600 后 unlink；大输入不会进入 argv。HiMind 已有文件 stdin 路径保留。

## 终态与恢复

Codex launch/resume 都使用相同的全局 `-C`、`-s`、`-a never`，个人模型沿用个人配置；AgentRun 拒绝注入 HiMind 模型，Workflow 只接受其既有明确 CLI 模型规则。HiMind 密钥环境过滤保留。版本发现有超时、取消、有限输出和进程树清理；显式宿主可执行文件 pin 优先，已验证绝对路径/版本原子保存于 Agent 缓存，不改个人或全局配置。Windows 兼容下限为已验证的 0.167；这不是对所有后续版本的兼容承诺。

两个 Codex 入口共享类型化 JSON 事件、有限内存及临时文件 spool、会话、usage、退出和最终正文校验。每次启动先移除旧最终文件。exit=0 还必须同时满足干净 EOF、非空最终文件、匹配的 agent_message 和 turn.completed；turn.failed、非零退出、缺正文、截断和不匹配均失败。取消优先于同轮观察到的成功，不把任意上游错误文本写入 Codex 诊断事件。

声明结构化结果时，只接受完整 JSON 或完整许可代码围栏，不反向扫描内部单行 JSON。完整 `passed:false` 同样使普通 Workflow 和 AgentRun 失败。Workflow 与 AgentRun 都保留有限的完整最终正文，摘要不替代正文。失败保留可用 usage、session 和数值退出码。

Windows OwnedTree::spawn 先创建并检查 KILL_ON_JOB_CLOSE Job，再以 CREATE_SUSPENDED 创建子进程；检查附加成功后，定位唯一主线程并检查 ResumeThread 的返回值。创建、配置、附加或恢复失败均关闭本次句柄并通过本次 Child 句柄终止和回收，不依赖根退出后的 taskkill 推断归属。Codex、发现探测、HiMind 两条入口、ACP 与 Copilot 共用该路径。ACP/Copilot 在连接或输出 join 前释放 Job，避免已退出根的后代持有管道。非 Windows 仍没有相同 Job 保证，本轮仅验证 Windows。

Workflow usage 按 run/step/attempt 的持久快照恢复：同一尝试重放替换快照，新重试累加；取消后的晚到结果仅记用量，不改取消终态。AgentRun 本地终态记录保留 usage，另外以 UsageRecorded 事件留存会话/版本/来源。个人 Codex 的 turn.completed 原始计数明确标为 provider_session_cumulative，独立于可信 run_increment 保存。Agent 缓存中的 SQLite 收据绑定 provider session 与 run，保存不可变基线；session 文件锁覆盖整次执行，IMMEDIATE 事务提交收据和新基线。同 run 已有收据时拒绝重复启动；相同收据重放不重复记账。外部 session 未知基线、异常子集或计数回退时增量为 null；新的已观测计数可成为后续基线。运行开始先使公开基线未知，工作进程崩溃后另一 run 不会计入未观测历史。cached/reasoning 仅作为 input/output 子集；金额没有可信来源时仍为未知；旧 DTO 中 numeric=0 且 currency 为空是不可比较哨兵，外层 cost=null/cost_status=unknown，不代表免费。Mission 既有可信成本门控、run 去重、暂停恢复和独立核验规则继续使用。

## 双向交接

宿主在下一任务输入生成边界构建 `executor_handoff.v1`，不是向运行中的任务实时注入反馈。逻辑依赖保留 upstream_task_id/logical_dependency_task_id；宿主在一致数据库快照中结合 task_repaired 与 repair_of 链解析 actual_producer_task_id，绑定已通过验收叶的实际 LocalRun、verification run、host event、required checks 与历史 candidate。双跳及直接指向最终叶的原生事件均支持。缺失、循环、歧义、验收绑定不全或旧失败 run 均拒绝作为已验收生产者。repair_of 接收者读取父任务时显式标为 failed_parent_context_not_accepted_output，不宣称该失败运行被验收。原父任务 attempt/review 字段保留在 logical_failed_history，不以原生 completed 投影替代失败运行历史。

产物 hash 是接收输入生成时的当前字节观测，不冒充历史产物签名。只读取上游写范围、接收方读范围、确认范围的交集；拒绝绝对路径、父段和跨范围符号链接。不跟随模型报告中的宿主绝对 evidence_refs；生成资源仍须其单独的任务资源契约。业务授权不等同操作系统沙箱。修复任务的上游失败状态如实保留，不把原失败自动改写为最新修复成功。

## 验证与演进

可复现命令和原始日志哈希以 `reports/executor/result.json` 为准。本轮入口 `scripts/remediation/executor-lifecycle-repair-checks.py` 调用原 `executor-check.py`，把构建、临时文件、测试 Agent home 和日志放在当前 worker 的宿主分配资源根；拒绝零测试、ignored 当通过和缺少必测场景。任务局部 linker 包装只处理 MSVC 的长生成路径，不更改全局配置。历史失败日志保留。

fake CLI/loopback 证据覆盖两个 Codex 入口的大于管道输入、输出先行、不读取输入、取消/超时、子树退出、预发现挂起/继承管道、非零退出、缺终态、完整负面、匹配的截断事件/文件、usage 和下游阻断。宿主账本测试覆盖暂停/恢复、重启、去重、失败返工、必需检查和独立验收边界；这些是隔离契约测试。

真实个人 Codex 证据包含 0.167.0-alpha.4.1 的 launch/resume、同一会话/cwd/full_access 和实际 shell 文件写读。只读 MCP 取得的真实 HiMind 成功与失败运行明确标为历史已部署版本，不能证明本次新源码已上线。未把 probe ready 当成通过。

宿主重部署后仍阻塞生产结论的项目：新 HiMind 代码实跑；远程 AgentRun claim/lease/API 到账及断联重启；双向真实模型交接时产生新 envelope；隔离 Mission 的取消、恢复和晚到完成；宿主独立 VERIFY 与部署健康检查。控制端测试请求与预期在 `reports/executor/host-recheck.json`，本执行者没有调用、取消当前 Mission、重开任务或执行正式 VERIFY。

## 本轮证据边界

本轮固定 47224/128 → 95295/480 仅是累计计数契约 fixture，预期增量 48071/352 不是本轮真实模型计数。实际 Codex launch/resume 的新计数和原始/增量收据单独保存。双向多跳交接、未知基线、竞争恢复与故障注入是隔离产品契约回归，不是宿主正式 VERIFY。HiMind 历史运行刷新读取明确分类；新真实 HiMind 执行沿用已批准的lucen / himind:lucen:gpt-6.1-sol绑定；本轮仍需可在资源范围内调用既有签名运行时的连接入口，不得用历史记录或 probe ready 冒充本轮执行。阶段能否送验以 result.json 的实际检查和限制为准。


## 二次返工：预算与崩溃边界

宿主事件 mission_1794473598404_ffe1e069:717 的首条检查 exit_code=-1、elapsed_ms=62760，原因是超过59秒被终止；后两条 exit_code=-1、elapsed_ms=0，是预算耗尽而未执行。三个 test_summary 为空，数字 TAP 总数未知。它们不是本轮测试结果，也不能把未执行项说成断言失败。

worker 使用原入口的 `--prepare` 进行冷构建、生产构建和真实个人 Codex launch/resume。原无参数检查命令保持不变，校验源码/测试/构建配置/工具链身份、可执行文件及证据哈希，再用资源根内同名最小包在相同cwd解析当前有效Cargo/Rust参数；任何内容、工具链、配置或产物身份失配均失败。实际Cargo fingerprint日志确认build.rs监视worktree中不存在的.git/HEAD导致每次重建，因此无参数入口不触发主程序编译；完整构建仍在worker准备阶段实际执行。回归保留既有产品测试域，用四个相互隔离的测试进程并行运行；每个进程在恢复主线程前加入受控 Job，超时终止该 Job。入口内部预算50秒，逐域缺场景、零测试、ignored、negative或超时均不能通过。生成物只写当前 host_runtime_resources；检查不改写交付源码及报告。

未结算的旧 run 永久不能再次进入提供方。新 run 可以在未知基线上执行；新可观测累计数成为后续基线，不能恢复旧 run 的过期基线。精确回归采用 L=100/10、A崩溃、B=200/20未知、C=230/23增量30/3、旧A试图250/25的序列。

调用方在提供方执行前把 receipt run 与账本路径、LocalRun、step、attempt 持久绑定。提交的 receipt 形成可恢复投递：调用方 SQLite IMMEDIATE 事务同时写入唯一receipt绑定、UsageRecorded事件与run用量投影。投递确认之前再次崩溃只会重放相同事务，不能重复相加。Mission宿主启动及AgentRun进入时恢复已提交待投递项；Workflow恢复旧attempt用量后，缺失业务完成事件仍标记失败/未知，禁止拿receipt冒充业务成功。显式重试不再归零attempt编号。历史失败和缺失usage保留；unknown成本和cached/reasoning子集规则不变。

交接查询在本轮开始时已经使用叶任务ID；新增回归写入非空原生task_baseline_captured事件、父失败baseline和受限artifact，覆盖双向多跳与过期父验证记录，并检查baseline事件、attempt、verification、candidate和checks身份一致。改变当前artifact字节只能改变当前hash观测，不能改变历史baseline hash。


本轮回归还验证了长资源路径引起的CreateProcessW cwd错误：Windows规范化后过长的目录使用既有OS短名，并核对短名canonicalize回同一目录。无短名或身份不符时失败，不创建别名或扩大授权。测试临时目录每次独立且使用短叶名；签名测试在准备阶段生成仅测试用2048位密钥，入口每次实际签名和安装验证，密钥哈希失配即失败。


## 三次返工：取消事务与ACK后的投影修复

以下是第三次返工的历史记录，当时采用727失败基线：原三个命令的退出码均为1，耗时分别43041、4125、3127ms；三个test_summary为空，数字TAP总数未知。首项output_sha256为59fe2929d37a78cefcadd8fd76de6470043e94803b87e9c434fc8e05d9bda87f，后两项均为9199219cd3421e40d8943e174ed06ecaaffebd1826d97a16247cf3bc14c422b2。717是另外一轮诊断超时。第五次返工采用后文748，不覆盖或重建这些历史输出；不能从hash推导具体断言。第三次返工的入口和既有报告确认当时缺少当前候选真实HiMind执行的负面门禁，不能代替本轮执行日志。

FOLLOW-UP诊断总预算为60秒，正式VERIFY总预算为min(Mission剩余时间,300秒)，单命令预算为min(剩余总预算,300秒)。这些宿主预算不变。昂贵构建和真实模型执行在worker准备阶段完成；无参数原入口核验当前源码、测试、工具链和有效配置身份，再实际运行产品回归，并为结构检查留出时间。新资源根可自行创建其executor生成目录，禁止借用旧worker缓存。

724的四个源码hash经重新读取仍匹配审阅值，因此确认U3-01源码交错存在；没有把它称作线上计费事故。WorkflowRunner.cancel现在把取消意图交给调用方事务，在同一SQLite IMMEDIATE锁内读取最新LocalRun并保存取消，不能再把早先Running/usage=None的快照写回。提供方投递在此事务前完成时取消保留其usage；在其后完成时投递保留Canceled。src/store没有修改。

恢复不再把delivered=1当作可变投影完整性证明，也不再遇到已有marker便直接返回。启动恢复扫描已完成绑定，投递事务从持久runtime_usage事件及不可变receipt按step/attempt重建投影；已有marker只阻止第二条receipt事件。修复保持Canceled、失败历史及未知业务结果，既不执行提供方也不补业务成功。重复/并发恢复是赋值重建，不是重复相加；代价是启动时会核对已ACK绑定。重建保留其他提供方可比较的同币种已知金额；任一未知或混合币种保持金额未知，不能将其替换成可比较的零。

AC-05回归通过真实WorkflowRunner.cancel入口安排旧Running快照、真实隔离CLI结算40/4、delivery与ACK、取消提交，并在晚取消补偿前由父进程终止实际OS子进程。另一分支在同一边界显式注入旧版全量快照损坏，随后也终止进程；独立重启进程并发及重复恢复，检查Canceled、40/4、单条receipt事件、单次提供方执行且无ToolCompleted。这里的旧版覆盖是故障注入，当前事务分支应直接保住usage；capability/service的参数查询路径是源码审阅，没有冒称本轮实际MCP取消请求。

U2-01新增真实Codex transport入口序列：L100/10，子进程中的A未settle即被终止，B200/20增量未知，C230/23增量30/3；旧A尝试250/25必须在spawn前拒绝且不改变当前pending。既有计数回退、cached/reasoning子集、未知成本和收据重启测试继续保留。U2-02除既有helper错误与独立重启覆盖外，另有完整execute_managed/claim/execute_claimed/caller测试，以隔离loopback API和CLI为夹具验证跨重试并发恢复、旧失败历史和无虚构succeeded；它的AFTER_COMMIT是helper错误注入，不是真实进程死亡或生产Dashboard验收。实际通过状态和数量只以本轮结果JSON及原始日志为准。

原入口本轮实际暴露了已退出根的继承管道场景：Job原先直到排空返回才释放，保留后代消耗2秒排空预算，在并发回归中触发既有3秒断言。Codex transport现在在根退出/终止后尽早关闭Job；正常退出先在既有排空预算内给予读取线程最多500ms处理EOF，再查询Job活动进程数。若仍有活动成员且输出未到EOF，则记录失败，不能把清理造成的EOF当成正常业务完成。查询失败同样失败；只有活动成员而输出已经干净关闭不构成输出协议失败，关闭Job仍会回收它们。回归使用确定性边界，先由独立句柄观察根已退出、后代仍活，再允许产品回收，并独立观察后代退出；原失败预期及小于3秒约束保留。libtest子串过滤原先让runtime::tests::组重跑workflow::runtime::tests::，已去掉这部分重复执行；全部独立场景仍由Workflow运行时组实际执行。所有中间失败日志保留，不与727混同。


## 第五次返工：attempt发布事务与真实no-op

最新宿主实际失败基线是事件748：三个原检查exit_code均为1，耗时128/123/128ms，三个test_summary均为空，数字TAP总数未知。741、727、717只作历史。LocalRun创建前timeout_seconds超过86400的失败不证明worker执行或业务代码结果。当前源码将IMPLEMENT和VERIFY的单step预算限制为Mission剩余秒数clamp(1,86400)，保留多日Mission总预算。

普通执行器在业务fail_when及artifact校验之前，通过调用方SQLite IMMEDIATE事务提交step/attempt的不可变usage事件和重建投影。成功及携带usage的错误都经过此入口。快照UPDATE到事件INSERT之间不再有可见的独立提交；取消竞争同一写锁，终态保持，已付费attempt不会消失。相同attempt重放校验原usage，新的attempt继续累加；已知同币种金额和owner语义、未知/混合币种以及Codex未知价格保持原约束。

最新状态取消事务返回changed/no-op。数据库已Succeeded、Failed或Canceled时，跳过usage rebuild、UPDATE、事件与projection/outbox重排，包括调用方仍持有旧非终态对象的情况。已ACK收据的受损投影仍通过独立显式恢复入口修复，不能用终态取消no-op替代恢复。

新增实际runner发布屏障在事务内观察USD3投影、在另一连接观察USD1及未发布的B事件，再发起cancel并在提交后等待其完成；成功和错误路径均重复恢复并启动独立恢复进程。终态no-op回归对比完整payload、事件及outbox状态/payload，并用数据库触发器拒绝任何UPDATE。历史U2/U3真实进程死亡与AgentRun helper注入继续分别分类。通过与数量以本轮原始输出为准。

FOLLOW-UP仍为总计60秒，正式检查为min(Mission剩余,300秒)，单命令为min(剩余总预算,300秒)。准备阶段构建并绑定当前源码、测试、构建配置、工具链和二进制；原无参数入口不改交付候选，实际执行保留的产品域，50秒内部预算为结构检查留时。宿主只额外保留允许列表化的失败码及executor-lifecycle-repair内相对证据引用，不捕获凭据或任意提供方原文。

第五次返工中间回归保留了超时失败及进程退出时限断言失败。入口随后将原Workflow套件按runner、模型与Codex协议分组，优先调度耗时套件，保持四个独立进程上限；准备阶段读取当前二进制的完整测试清单，执行时逐组核对名称和数量。调度调整前后仍为427次执行、422个唯一测试名，没有移除原场景。清单只是准备证据，实际完成数量仍以最终日志为准。

repair5 历史限制记录：当时固定HiMind native producer未形成可执行的生产配置消费入口，故保持失败。该轮记录不替代 repair6 明确任务契约；repair6 的原生生产配置内部消费与正常运行写入边界见下文。helper推理本身不代表最终安装Mission流水线或独立验收通过。

## repair6 当前任务边界与交付模型

本节记录 repair-executor-lifecycle-runtime-6 的设计；执行结论以 result.json 的本轮记录为准。748 仍是最新给定宿主失败基线：三个 exit_code=1，三个 test_summary 为空，不能推导 TAP 数量。创建前 timeout_seconds 超过86400的失败独立保留，不等同于业务 worker 或回归执行失败。759的81项属于宿主观察，不替代本轮产品回归。

R5-F4 的状态模型是：A已发布 USD1/local；B仍在执行；实际取消和取消投影完成；B再返回 USD2/local 或携带相同usage的 RuntimeFailure。取消终态保持 Canceled，但 usage 是可追加的付费事实。caller 的 IMMEDIATE 事务同时提交终态usage和当前投影，投影重放比较 run 内容，内容相同不改变 payload、投递状态或重试计数。取消事务也在相同事务内投影，避免稍后的旧取消对象覆盖新增usage。终态无变化取消仍直接返回，不更新、不修复、不重排队。回归使用通道握手确认取消和原投影已完成，再释放B；没有用sleep推断交错。

原生入口由本任务契约授权，不能把观察反馈当成授权源。只有显式 HIMIND_EXECUTOR_LIVE_HIMIND=1 的可信原生 helper 会内部读取既有生产 lucen 配置；worker shell 和模型不读取、复制或输出原始生产配置。原生 API 使用生产 Agent 的正常 lock/bootstrap/cache/accounting/log 目录；不能声称所有原生写入都在任务资源。会话、交接产物、收据与生成日志放在本任务 host_runtime_resources 下。此区别不会扩大模型文件访问范围。

原生 helper 在任何 recovery-capable 服务加载前对配置持有禁止写入/删除的 Windows 句柄，并拒绝 reparse、缺失、过大、无效JSON/字段/受保护凭据；配置错误只产生有界脱敏失败。显式构造 Independent Options，不调用 Options::from_env，不同步工作台；元数据使用已知模型表。只接受现有安装的可信 Runtime，不安装/更新，不改服务/default/model/credential，不回写恢复配置。固定alias、90秒期限和禁模型工具的overlay沿用产品入口；config/runtime哈希前后校验，formal检查只做无推理身份复核。候选身份包括编译内嵌 runtime-profiles 和其他相关输入。

真实 helper 交接链使用新nonce、非空历史baseline哈希和各跳产物SHA256：HiMind first → personal Codex launch → HiMind second → 同session的Codex resume。HiMind仅接收宿主限定的依赖内容并产生JSON；Codex使用既有个人配置，service/model保持空，执行生成目录内的文件读写。计数/usage及退出、deadline和取消结果分别记录。HiMind token证据来自runtime session projection，不能称为原始上游账单；未知上游金额/归属为null，endpoint仅证明本机环回网关。

这条链是 host_delegated/current_candidate_test_binary_calling_product_runtime_api 的证据。产品的accepted lineage、非空baseline、范围交集及多跳修复历史另有实际产品回归；任何helper结果都不自动等于最终安装Mission流水线验收。正式VERIFY、独立QA、最终复核、最终构建/部署和安装后B01–B13验收仍由后续阶段承担，本执行者不调用或冒充这些阶段。

## runtime7 预算边界与检查入口

最新给定宿主失败基线为事件777：第一命令 exit1，TAP 305 tests /305 pass /0 fail，57898ms，输出 SHA256 为5022057d26af58a52d97fcef2a5b34148600b8f3f86005a0943b1e4880e61a56；第二命令 exit1、9197ms、空 test_summary；第三命令 exit-1、0ms、空 test_summary，错误为 Mission check time budget exhausted。748等旧记录仅是历史，原失败和空摘要保留。没有获准的777原始输出副本，不能把305项通过解释为完整入口通过，也不能声称已读旧缓存。runtime7直接入口首次实测因本任务根缺少prepared.json而失败，属于当前准备缺失，不能据此断言777也有同一原因。

BUDGET-AWAIT-01 采用已获准任务的收尾边界：实现提交实际usage后，Running及AwaitingVerification不由预算扫描转成Waiting；独立验证仍需独立run、当前绑定和宿主证据，预算在下一任务或返工派发前生效。预算等待写入在IMMEDIATE事务内复核当前状态及真实限额，旧扫描不能覆盖正在运行、验证、手动暂停、环境等待或终态。显式恢复待验证任务仍需已确认计划和当前workspace claim；环境阻断优先，预算不会因此增加，也不授予下一任务。此策略不自动重开已完成任务，不放宽pending-verification预算扩展限制。

回归通过真实MissionStore记录110 tokens/100限额及成功实现待验证状态，再调用supervisor扫描；测试内独立验证夹具不是本任务正式VERIFY。成功和失败分支都检查claim、独立身份、缺失宿主证据拒绝、重放和重启、随后预算等待及同键扩展。原全部产品域保留，并纳入supervisor与原预算安全回归。预算测试的生成数据库迁到继承TEMP，避免检查改写reports/budget或冻结候选。测试清单、实际逐组执行、断言、退出码及耗时均保留在本任务资源根。

昂贵编译和可信native opt-in准备完成后才冻结候选；无参数必需检查不编译主程序、不重复推理、不改源码或交付报告。缺缓存、身份失配、零测试、忽略场景和超时均失败关闭。runtime7只按当前授权证据判断本阶段就绪，旧日志补读及尚未开始的下游正式VERIFY/QA/部署不作为本阶段等待条件。原报告保存在prior-repair6-result.json，最终本轮结论和限制见result.json。

runtime7首个完整候选的原命令退出1/1/1，首项51.25秒，282项完整计数中281通过、1失败；另有未完成套件，不能把开始执行的322个测试名计作完成。失败原报告保存在runtime7-first-required-failure.json。实测显示后置workflow、预算及supervisor套件因内部总预算未完成；Codex可恢复通知成功夹具在terminal之后写finished文件，造成200ms真实退出期限下的夹具负载问题。该文件写入现放在terminal之前，产品期限和负面断言不变。

调度改为六个隔离且预归属Job的测试进程，耗时Workflow和预算套件优先，名称/数量/忽略状态仍与同一二进制清单逐组严格核对。--prepare在付费调用前用同一调度器执行完整清单并要求47秒内成功；最终原必需检查继续重跑全清单，内部50秒上限不变。产品资源应用合并重复调用，仍对调用方workspace、整个资源树、reparse、设备别名及数量上限作完整校验；既有五项资源边界回归纳入入口。此处修改的是当前候选源码，没有修改已安装宿主或其预算。

完整预校验进一步保留了协议组40秒超时及拆分后的取消夹具失败。协议组按生命周期/通知拆分为互斥清单，准备阶段强制验证其并集等于原完整协议清单且无重复。取消夹具在发出通知后等待显式release_work边界，由实际取消入口终止进程，避免以900ms睡眠与进程树清理速度竞争；成功工作、真实执行超时和关机期限的负面断言均保留。两次失败的原始日志、整数退出码及计数仍在本任务资源根，最终结果不覆盖这些历史。

中间准备的付费流程已在调度源码调整前启动，最终候选身份变化被拒绝；该轮不能作为当前交付。原始live日志另记录0通过/1失败、exit101：PowerShell输出42.0，独立Python数值检查接受，而Rust helper严格JSON表示比较错误拒绝。现统一为精确非负整数数值契约：整数或安全范围内无小数的有限浮点数可作为输入；字符串、布尔、负数、分数及不安全浮点数仍拒绝，递增检查溢出，原artifact字节及SHA256不改写。此修改仅在隔离native交接helper及测试中，未放宽生产required checks。失败付费阶段与身份拒绝记录保存在本任务资源根preparation-identity-rejected.json。后续准备运行期间冻结编辑，只有完整当前候选准备和三个原检查通过才建议送验。

当前预算边界的派生effective_wait投影同时复用当前确认计划及workspace claim的窄准入检查。暂停已获准的待验证任务时，can_resume为true而can_increase_budget仍为false；保留预算原因，等待原因显示manual_pause。环境阻断优先于该恢复动作，失配claim/计划不提供恢复按钮。此项在新付费调用前主动停止准备后修复；停止的Windows原始退出整数、校验结果及最后live日志时间关系另存preparation-stopped-for-resume-projection.json，没有将停止当成成功。

最终编排采用八个隔离Job进程，将四个长套件按已冻结二进制的精确测试名交错分片，并将生命周期协议终态测试独立调度。每个原组的分片并集必须严格等于原清单，全部原重叠执行继续保留，不能以分片忽略失败或超时。前一二进制的免费调度对比仍为454次完整执行，耗时18.81秒；此对比不冒充最终源码候选，最终准备和原必需检查仍须实际重跑。未改宿主预算或已安装宿主；源中合并资源树重复校验保留完整检查，但当前宿主就绪依赖入口自身缩短耗时。

最终调度另将四项200ms关机通知协议测试放在并行产品套件结束后单独执行；仍消耗同一47/50秒总预算，原200ms关机期限、真实执行超时、退出失败及未完成断言均不变，不自动重试失败。当前测试二进制的完整免费校验454次全部通过、25.875秒；保留先前并发压力下该正例失败的原日志。这样为已安装宿主对每条命令进行资源树验证留出余量，不依赖部署源中资源扫描优化。

## 当前 user-scope-1 恢复责任与独立重做协议

本轮直接用户范围优先：读屏语音和全部 200% 缩放为 `user_deferred / not_in_current_scope`，`passed: null`，不阻断当前送验。保留旧失败、未知与既有 CSS 补充记录，不启动 NVDA/Narrator 或新缩放验证。普通键盘、焦点、模态背景隔离、status/alert、窄屏与所有非延期产品门禁不减弱。原 QA 报告不修改；runtime7 报告和原失败逐字归档在 `reports/executor/user-scope-1/history/`。

问题模型：Waiting 不会由 supervisor 自动派发，已有实现与精确当前 claim 只授予显式恢复已提交核验的窄资格。由该既有准入派生 `verification_resume_required / actor=user / can_resume=true`；Active 核验为 `system_verification / actor=system / can_resume=false`。手动暂停保留 `manual_pause` 的可选性；环境阻塞、失配确认、外来或失配 claim、缺实现 run、已有实现 worker 均不获得恢复资格。保存的核验 ID 本身不证明进程存活。前端另以当前 Mission/task/run 绑定的 running activity 判断真实运行者，不从历史 start 事件猜测。已有严格 legacy claim 恢复与预算收尾策略不变。

方案取舍：只改派生投影，避免改动预算、工作区占用或确认模型；单纯前端归一不能修复后端责任语义，因而不作为本轮生产方案。回归通过真实 SQLite Store、control、snapshot/list 投影及 supervisor 扫描；SQL 仅种入明确标识的旧版/损坏状态，独立 reviewer 是确定性夹具，不能冒称本 Mission 正式 VERIFY。缺失 claim 的迁移前投影必须拒绝；正常 reopen 可恢复同一仍有效 attempt，既有防陈旧 claim 复活回归继续保留。

独立 QA 必须由宿主分配**自己的新资源根**并使用当前源码，不能传递本轮缓存、密钥或原始配置。以下 PowerShell 协议可在已设置 MSVC/Rust/Python 的同一源码工作目录运行；宿主预置两个资源环境变量，二者必须指向 QA 自己的同一根。仅在该原生使用已获授权时启用固定配置消费：

```powershell
if (-not $env:SERVICE_UI_RUNTIME_ROOT -or $env:SERVICE_UI_RUNTIME_ROOT -ne $env:HIMIND_MISSION_CHECK_ROOT) { throw 'QA resource root missing/mismatched' }
$env:PYTHONDONTWRITEBYTECODE='1'
$env:HIMIND_EXECUTOR_LIVE_HIMIND='1'
python scripts/remediation/executor-lifecycle-repair-checks.py --prepare
if ($LASTEXITCODE -ne 0) { throw 'Current-source native preparation failed; retain logs' }
python scripts/remediation/executor-lifecycle-repair-checks.py
if ($LASTEXITCODE -ne 0) { throw 'Current-source product/identity regression failed' }
```

脚本自动把 Cargo 下载缓存、构建、测试临时 home、日志与 helper 产物放在调用者资源根；新根从网络获取固定 lockfile 依赖，不借其他任务缓存。准备阶段可付费执行 HiMind first → Codex launch → HiMind second → 同会话 Codex resume；正式无参数入口只复核哈希并运行产品回归，不重复推理。配置由可信 native 在 loader 之前锁定并内部使用；模型工具禁用、缺失/损坏脱敏失败、固定 lucen 路由及 personal Codex 配置不变。QA 不得 shell 扫描、复制或输出生产配置/凭据/provider 诊断。

`executor-lifecycle-repair/prepared.json` 绑定源码（含 runtime-profiles）、工具链、有效 Cargo 配置、测试及生产二进制、native 配置/运行时哈希；`checks-*.json` 与逐组原始日志给出实际清单、非零数量、耗时、退出码和失败。`real-himind.json` 及其引用的 Codex 收据记录各跳 run、session、来源、范围、baseline/candidate/check/artifact 哈希及 usage；未知成本保持未知。该证据分类为 current-source native helper / host-delegated product runtime API，**不是完整安装 Mission 流水线验收**。

报告作者先保存旧报告，使用本轮执行记录撰写报告，然后封存当前源码和报告。三条任务原 required_checks 必须逐字执行；无参数入口缺缓存、身份变化、零测试、忽略、超时均失败。封存后仅向本任务资源根写 replay 记录，不改候选或报告，不使用旧 runtime7 的报告作者脚本改写本轮结果。`result.json` 的当前 staged delivery 结论须结合其指向的 frozen replay 真实结果；未来宿主 VERIFY、B01–B13 独立 QA、最终复核、最终构建/部署与安装健康由后续职责完成，缺最终安装证据仍阻断该后续安装验收，但不伪装为本轮 helper 已通过。

可执行编排入口为 `python scripts/remediation/executor-lifecycle-user-scope.py --prepare`（调用上述原准备命令并保留唯一日志与退出码）。Mission 唯一业务写入者才使用 `--stage` 撰写本轮报告并封存；`--replay` 严格读取已封存候选，按原顺序执行全部三条原命令并将不变性与结果写到调用者资源根的 `executor-lifecycle-repair/frozen-replay/result.json`。独立 QA 不使用 `--stage` 修改业务报告，按上方原产品协议在自己的资源根重复即可；此分工不需要移交个人配置或另一任务缓存。

本轮首次免费完整校准实际退出失败，原 `runner_preserves_cancellation_that_arrives_during_a_step` 在 usage=None 处失败。旧夹具的 50ms 取消与 300ms 返回睡眠不能证明步骤已进入；并发负载下取消可能早于执行，真实未付费路径合理地没有 usage。仅把该夹具改为进入/取消提交的双向通道握手，生产取消逻辑、全部原终态和 usage 断言不变。原失败校准、日志与测试二进制保留在本任务资源根，哈希索引为 `reports/executor/user-scope-1/preparation-history.json`；这次准备未付费，不冒充最终候选通过。

第二次免费完整校准保留了通知后失败关闭的 3 秒断言失败。产品在 terminal/cancel/deadline 路径同步启动无自身期限的 `taskkill`，即使树已通过 Job 归属也等待外部命令。现在直接使用该已归属 Job 句柄终止，错误仍失败关闭，root 由保留句柄回收；所有原数值退出、协议、200ms 关闭期限、3 秒断言及独立后代存亡观察继续执行。非 Windows 路径保持原实现，不作额外平台声明。旧失败没有内部时序分解，不能声称测得具体 taskkill 耗时；本轮新日志明确输出实际关闭测试耗时。

## user-scope-4 当前授权与隔离重做协议

本任务直接承载用户「读屏和200缩放不是延期，是确认不做」。读屏专用支持/语音验收和所有 200% 缩放支持/验收明确不做，无未来义务。新报告对这两项统一使用 `status: excluded_by_user, passed: null, blocking: false, future_obligation: false`，不计通过，也不列为未解决限制、后续测试或待办。上文 user-scope-1 与冻结报告的旧措辞仅为历史；不改写旧证据。普通键盘、焦点锁定/返回、模态与背景隔离、状态语义、普通窄屏以及其他产品、预算、跨执行器和独立检查要求全部保留。

当前候选已有恢复责任、Store/supervisor、确定性预算交错、终态事务、usage/receipt/outbox、跨执行器谱系与 Windows 预执行 Job 归属修复；本任务先检验现状，只修复实际重现的范围内缺陷。历史 909 的三个检查为 0/1/0，16315/17126/16465ms；916 为 0/1/0，11653/11238/11691ms；917 为 0/1/0，10987/11643/11541ms；最新历史观察 921 为 0/1/0，8109/8122/7448ms。四者 test_summary 均为空，不能推导测试数量。只读 `mission.events.list` 补齐了命令、输出哈希与时间，记录保存在 `reports/executor/user-scope-4/history/host-observations.json`；接口未返回原始 stderr。777 的 305/305 不替代这些观察，也不替代本任务执行。事件 909 原始 stderr 未提供，不从服务中断猜测产品根因。parent1 的 upstream_service_unavailable、exit1、turn.failed，parent2 的取消，以及 native918/919 的磁盘满错误112 均保留。未启动且取消的 repair3 不恢复。

独立 QA 使用宿主为自己分配的新资源根、当前源码和既有 MSVC/Rust/Python/Node 工具链。两个资源环境变量必须指向同一个自己的根；不得复制本任务或父任务缓存、CONTROL、密钥或原始配置。准备前查看卷可用空间；可对**精确的本任务目录句柄**设置可逆 NTFS 压缩，不递归调用 compact.exe 处理超长路径，不删除旧证据、不移动宿主根、不改变文件字节。准备入口在冷构建前要求至少 16 GiB，仅当本任务根已有完整成功日志、哈希匹配及仍存在的测试产物时，对后续构建使用 4 GiB；生产构建前亦要求 4 GiB。后续构建仍调用 Cargo、完整校准和全部身份检查；下限不是对外部并发磁盘占用的保证，磁盘不足须停止并保留原因。

```powershell
if (-not $env:SERVICE_UI_RUNTIME_ROOT -or $env:SERVICE_UI_RUNTIME_ROOT -ne $env:HIMIND_MISSION_CHECK_ROOT) { throw 'Own resource root missing/mismatched' }
$env:PYTHONDONTWRITEBYTECODE='1'
$env:HIMIND_EXECUTOR_LIVE_HIMIND='1'
python scripts/remediation/executor-lifecycle-user-scope.py --prepare
if ($LASTEXITCODE -ne 0) { throw 'Retain failed preparation; do not blindly repeat paid inference' }
# Mission report author alone uses --stage; independent QA must not use it.
python scripts/remediation/executor-lifecycle-user-scope.py --seal-read-only
if ($LASTEXITCODE -ne 0) { throw 'Current candidate cannot be sealed' }
python scripts/remediation/executor-lifecycle-user-scope.py --replay
if ($LASTEXITCODE -ne 0) { throw 'Original required commands failed; retain result' }
```

`--prepare` 实际以 `HIMIND_EXECUTOR_LIVE_HIMIND=1` 调用原 `python scripts/remediation/executor-lifecycle-repair-checks.py --prepare`。固定 native HiMind 路由与已安装 personal Codex 分别运行 first→launch→second→resume；不安装/更新 runtime、不切换模型/服务。native 在 loader 前持有配置防写守卫、禁用模型工具、仅输出配置/运行时哈希和脱敏结果。允许的正常 native 锁、bootstrap、会计及日志副作用由既有 native 操作承担；生成的测试、构建、缓存、会话和证据留在调用者资源根。不得 shell/模型读取、扫描、复制或打印原始配置、秘密及 provider 诊断。

所有昂贵准备在冻结前完成。报告作者 `--stage` 先以原产品命令执行完整回归，再写报告、执行两条原报告断言、封存；封存后的 `--replay` 按合同原顺序真实执行全部三个原命令并校验候选/报告不变性，仅写自身生成证据，不修改源码或报告、不重复付费推理。独立 QA 的 `--seal-read-only` 只在其自己的准备已完成后生成自己的封存，不修改业务报告。逐域清单、计数、断言、耗时、退出码、原始日志和身份摘要保存在 `executor-lifecycle-repair`；缺准备、身份失配、零测试、跳过必需场景、超时或真实断言失败均失败关闭。

本轮报告 `passed` 仅表示当前阶段送验就绪，须结合其 frozen replay 真实结果；不自签宿主正式 VERIFY，不替代独立 QA、最终部署和安装后验收。后续职责尚未执行不单独阻断本阶段。历史 helper、宿主观察、当前产品回归、当前真实 native helper 和未来安装流水线分别披露，不宣称未经执行的跨平台结果。

## user-scope-5 报告、调用身份与冻结闭环

最新给定宿主观察是事件933：报告断言 exit1、25566ms、空 test_summary；完整产品入口 exit0、47474ms、TAP457/pass457/fail0；结构检查 exit-1、0ms、`Mission check time budget exhausted`，未实际执行。这是宿主历史执行，不是本轮 worker 执行，也不表示三个检查通过。909/916/917/921及其空摘要、777的305项、历史456项生产者校准、服务故障、磁盘满与取消记录分别保留。

933报告断言的可追溯原因是原 `reports/executor/result.json` 的 `passed:false`；原报告明确将 `EXECUTOR-HISTORICAL-909-CAUSE`（历史909原始 stderr 未取得）列为唯一阻断。该报告逐字归档于 `reports/executor/user-scope-5/history/result.json`。现有材料不证明933发生产品断言或候选身份失败；其第三项是宿主总预算未执行，不能称产品成功或失败。909的原始原因仍未知，不伪造原始输出；未知历史元数据不再代替本任务的实际交付门槛。

报告编排现在使用当前task5归档和明确的 `current_readiness`：本任务准备退出0、当前候选一致、非零完整产品清单、逐域非零断言、当前真实HiMind/Codex证据及上游model/view检查均必须成立。正式VERIFY、独立QA和部署的后续结论不参与此谓词；也不能用933或历史结果满足当前门槛。`--prepare` 先保留自身缺准备诊断，再自动运行报告闭环回归和普通上游model/view检查，随后调用不变的原产品 `--prepare`，无需手工伪造中间JSON。

每次产品检查使用新的 `HIMIND_EXECUTOR_CHECK_INVOCATION`，原入口在唯一收据和stdout摘要中回传相同身份。包装器只读取该次摘要明确引用且位于调用者根内的收据；中断、超时、缺摘要、重复摘要、身份不匹配不能读取先前共享 `checks.json` 充数。原命令、产品断言、50秒内部预算、47秒准备校准及冷构建16GiB/后续构建4GiB门槛均保留。

报告编制完成后才冻结；除源码、报告、工具链、有效Cargo配置、runtime-profiles及native配置身份外，封存还绑定准备收据、测试与生产二进制、链接器、测试签名夹具、脱敏真实链路收据及每份交接artifact。`--replay` 在原三个命令前、各命令后均验证不变性，所有新增输出仅留在当前资源根。零产品测试、失败或无本次收据均不能通过。已有封存拒绝再次prepare/stage/seal覆盖。

脱敏独立重做协议沿用上一节的环境设置与 `--prepare → --seal-read-only → --replay`，仅用独立QA自己的宿主资源根和已有工具/runtime；Mission报告作者使用 `--prepare → --stage → --replay`。`python scripts/remediation/test_executor_delivery_contract.py` 的生成报告、冻结和故障夹具只验证编排机制，不能替代实际产品、模型链路、Windows进程、独立QA或安装验收。其输出保存在调用者资源根，不要求配置秘密、provider诊断、旧任务缓存或CONTROL。读屏和全部200%缩放继续为 `excluded_by_user / passed:null / blocking:false / future_obligation:false`；普通键盘、焦点、模态隔离、状态语义、窄屏及其他门槛不变。

## Mission Goal 与 TEST 构建签名环境

Mission 是单步调度的唯一所有者。`write_mission_methods_overlay` 在保留指派模型、文件、Shell 和 MCP 的同时追加禁用 `tool-skill`、`tool-goal`；不改 base patch 或其他会话，交互/native Goal 仍可使用。当前编译二进制的正负回归验证最终有效行、原配置字节、其他会话隔离及重复 overlay。严格进程非零退出与最终结果规则不变。QA6 给定会话序列为 Goal create29、turn1 complete767、continuation768、turn2 start769、abort/dispose771；headless 汇总最后被中止的 turn 而退出1，不能将此改为成功。

准备与只读检查都经 `test_environment` 使用调用者资源根 `executor-lifecycle-repair/build-signing` 中明确的 TEST RSA 公钥和 JSON 配置；更新/扩展 key ID、公钥路径与 build channel 来自该配置，父进程签名字段先移除，不读取生产私钥或公钥路径。固定公钥在本任务资源中通过 Windows CNG 生成，私钥从未导出，无生产信任；准备只创建缺失的初始文件，既有文件变化不会被悄悄覆盖。身份同时绑定配置内容、公钥内容及有效环境 hash。编译产品回归逐项比较 build.rs 实际嵌入的两份公钥/ID与有效 TEST 输入的 hash，并执行产品 RSA 公钥解析。真实编译参数、工具链、依赖、源文件、有效 Cargo 配置、测试/生产二进制、签名夹具、链接器、live evidence 和 artifact 所有权/哈希门禁继续保留。失配仅输出组件名与前后 hash，不输出环境值。

`test_executor_build_environment.py` 在本任务资源根生成隔离夹具，以有界子进程执行真实身份、Cargo 配置探针和产物验证。正例改变无关父签名/进程上下文；负例改变编译参数、Cargo 配置、源码、依赖、TEST key ID、同名公钥内容、工具链选择、产物字节和归属。准备阶段与每次原只读产品检查都实际执行该测试，不能用未执行的源码断言代替。Windows 探针使用相同任务目录的短路径，产物仍留原资源根。

独立 QA 仍使用上节自己的根与 `--prepare → --seal-read-only → --replay`；入口自动建立并使用确定 TEST 配置，无需传递任何生产签名环境、密钥或另一任务缓存。Mission 作者先归档旧报告至 `reports/executor/goal-environment-1/history`，完成全部源码修改与昂贵准备后 stage，再冻结并逐字运行原三条 required_checks。冻结后的输出仅在资源根，不重复付费推理。QA6/native995–997 与受控 QA7 cancellation1002–1004 作为给定历史保留；997 的0/0/1/-1与 worker0/0/0/0分别记录。check3 的输出 hash 仅匹配诊断重建，重建中源码/工具链相同且三项继承签名字段不同，这没有捕获正式检查环境；check4预算耗尽未执行。该限制不伪装为已确定的正式根因或本轮测试。933报告退出1是历史 passed:false 与职责错误，933产品457/457退出0、最后检查预算耗尽的事实独立保留。
