# 实施交接校正报告

## 结论与边界

仅修订现有两份报告及实施交接；本阶段候选就绪不代表任何生产待办完成、独立验收、正式VERIFY或部署。

上一轮报告曾记录两份报告缺失及FileNotFoundError；该记录仅为历史，非本次现状。本次两份报告均存在且已读取，首次写入前两项原检查均exit_code=0。旧审查只作历史输入，以下预算结论按当前源码核读。

本次只修改已存在的本MD和同名JSON；implemented_backlog_ids为空。源码基线663文件；前置7文件补丁不是本次成果。主动预算增额已存在，不能照抄旧后端最后事件门控结论。

后续既有任务稳定ID未在授权输入提供；以下按原backlog负责人和既有产物分组交接，ID需控制端补充，不虚构或新增任务。

## 上轮输入与源码SHA-256（历史记录，非本次完整范围快照）

- `remediation-input/execution-contract.md` `223f2504846212b0be923ea68557a6066e4add4e561357f656e01f49e5390dd0` (input)

- `remediation-input/backlog.json` `391f81822f5a40715893e580d8525e87924803dcec5b527e7dd0373a36d7f53c` (input)

- `remediation-input/baseline-manifest.json` `b19505f763d23bdc3f0a5a9a23ef3f90a3d5926af36768a8479152cecba4f686` (input)

- `remediation-input/prestart-host-patch.json` `6e3f81575f102378129e43a6be219668ff542c3e65c04f9b4452c51ffa049c0d` (input)

- `src/store/ai_services.rs` `5440f7eb1f704428cdc95e0fa79565eb6743a36494d0572675219660febc3ca0` (663_file_baseline)

- `src/store/atomic_file.rs` `eb46653f385956340cdb97a99cf139613f5853c798e572ac778864cf3c7e293f` (663_file_baseline)

- `src/app/inference_gateway.rs` `12ecb0cf32915380b467d6abbc2ed0b0ba9ce807bf09d16ca8252ce6d056cc82` (663_file_baseline)

- `src/app/chat_responses.rs` `5fe4489f9756cf22e881883f9738ca1b502e433eca04a06789cf4dd055afe91c` (663_file_baseline)

- `src/mission/store.rs` `feb1bddd1e7575a73e7a243c18b949cd35068785912c95c4ab1fe3f6c37b17f6` (prestart_host_patch)

- `src/mission/contract.rs` `2605498f97ac94e6d4bcc813d67ff8f1acdb8a8fbaa1ef87ba5ef9228e220458` (663_file_baseline)

- `src/workflow/runtime.rs` `f7eed24fb61c24e2d3242e782fb8525c61e3b96dd5b07b93e8de8964f7d03d8d` (prestart_host_patch)

- `src/runtime/codex.rs` `1a8a2ac8abc04b2a66891b1542cbf771691c043387fca0a17f97aea373781ffd` (663_file_baseline)

- `src/runtime/deepseek_harness.rs` `108bfad3a1e2ac722751d1c10286293154a9f90454a6cf8d9d4d9345845aa4a0` (663_file_baseline)

- `src/app/mission_dispatch.rs` `3eb07ac62384bb51f8a95aef5724459fbe6a674359a060ddc4237134491d9a43` (prestart_host_patch)

- `frontend/src/pages/AiProvidersPanel.tsx` `6b8fd21325eaa8decff24818d010285cf2b5ecbb7b4d571cdef97d54b2a0f1e9` (663_file_baseline)

- `frontend/src/pages/AiToolsPanel.tsx` `3b34e1fda9b4b6130f477ff8bef1f12ca383520e70ad473d39c7eb235ec9755d` (663_file_baseline)

- `frontend/src/pages/aiToolConnections.ts` `66d166ccb818e537f1fc5a5a03fb506fff9168c37752ee0354a4456b5dafad54` (663_file_baseline)

- `frontend/src/pages/AiConnectionsPage.tsx` `e4c089a4051d0a69a3a70d7784b886e3716b8ebe734dcc2d8a6d59e157755697` (663_file_baseline)

- `frontend/src/pages/AcpProfilesPanel.tsx` `7e3bd8c7e3b0ff1128de3ddd6ed2b6be624d54ae8124fda7ba0d0f134c425c5e` (663_file_baseline)

- `frontend/src/pages/MissionTeamPanel.tsx` `427bcd6bcc210a553d7dbe222fc666d87394ae5a32e6c6cd47446313ed602665` (663_file_baseline)

- `frontend/src/pages/missionView.ts` `0427418ceff74e7a6255b3e3f5493f15851cd0599e77488ff68d48236346c152` (663_file_baseline)

- `frontend/src/components/ModalSurface.tsx` `89952e480723081870aa5e469483c55fe73161c9069b6413751d9d43933ac28a` (663_file_baseline)

- `frontend/styles.css` `c002332bc572c680e60c9a4cec496935238ffa371a66e3b7f5f642bb0e52c64d` (663_file_baseline)

- `frontend/package.json` `46bdde06af14e46bc79ad16039b212727037e56a94634735796e6ddb234fac20` (663_file_baseline)

## 逐项实施交接（各项一次）

### B01 目录锁内原子读改写与上一版本恢复

- 源码依据：src/store/ai_services.rs:528-568; src/store/atomic_file.rs

- 当前覆盖：有加密持久化和独立atomic_file帮助器；目录save_all仍直接fs::write，不是锁内读改写。

- 剩余缺口：将全部目录写入口纳入同一目录锁；临时写/flush/替换/上一有效版本恢复；不能仅给save_all加锁而把load留在锁外。

- 既有实施归属：配置后端既有实施任务 / reports/config；配置后端负责人：实现锁、原子替换及恢复诊断；QA负责中断注入；稳定ID待补充。

- 兼容：保持服务id、加密凭据和旧协议别名、端点族去重；不迁移数据库、不自动恢复损坏版本。

- 可执行回归命令（待执行）：`cargo test ai_services -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：在隔离临时目录以确定性屏障在临时写、flush、rename前后终止子进程并重启解析；并发编辑两个不同id，均保留；损坏备份拒绝恢复。

- 完整输入验收：在临时写、刷新、替换前后逐点终止，重启至少旧/新一份可解析；恢复记录配置版本且不含凭据；并发修改两个不同服务无丢失更新；损坏快照不被自动恢复为有效版本

- 回退：暂停配置写入口，校验并恢复上一版加密目录；保留失败文件供诊断，不退回无保护覆盖写。

### B02 删除幂等清引用及启动一致性诊断

- 源码依据：src/store/ai_services.rs:1179-1205,1918-1958

- 当前覆盖：成功删除会清全部能力路由并保留相对顺序；仅removed分支清引用，已有中断窗口。selection已有atomic_write，不能算双文件事务。

- 剩余缺口：不存在id也清引用；启动一致性诊断及成对恢复边界；目录已提交路由未提交时重试应收敛。

- 既有实施归属：配置后端既有实施任务 / reports/config；配置后端负责人：跨目录/路由恢复；前端负责人：呈现悬空引用诊断；稳定ID待补充。

- 兼容：返回bool仍指目录是否删除；清理不重排其他路由、不擅自选新服务。依赖目录事务基础。

- 可执行回归命令（待执行）：`cargo test ai_services -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：屏障在目录提交后路由提交前终止；重启同id删除两次，比对悬空引用为零、其他主备顺序不变；启动仅报诊断。

- 完整输入验收：目录提交后、路由提交前终止，重启并重试删除相同id，全部悬空引用消失；清理不存在id可重复执行，其他默认/备用相对顺序不变；启动检测不擅自选择新服务

- 回退：停止编辑并成对恢复同版本目录和路由快照；单文件回退禁止；长程事务存储另立决策。

### B03 统一公开错误DTO与脱敏边界

- 源码依据：src/app/inference_gateway.rs:2423-2478,2668-2725,2926-2979,3163-3218,4289-4300,4539-4545

- 当前覆盖：部分日志有摘要脱敏和request_id，互译错误仍把raw_error转detail输出；write_upstream_failure把detail写日志/响应。不是已泄漏事件。

- 剩余缺口：所有直通/互译、请求失败和协议错误统一公开DTO与安全日志，不保留原始上游异常正文。

- 既有实施归属：网关既有实施任务 / reports/gateway；网关负责人：直通/互译统一安全错误；安全复核人：检查响应及日志；稳定ID待补充。

- 兼容：见公开错误契约；保留OpenAI/Anthropic外层协议、HTTP语义和安全code，不能让旧客户端靠原文解析。

- 可执行回归命令（待执行）：`cargo test inference_gateway -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：loopback伪上游401/403/500及异常HTML回显虚构Bearer/x-api-key；直通与四种互译逐路检查响应、SSE、日志无原值，code与request_id关联，限长。

- 完整输入验收：虚构Bearer和x-api-key回显的401/403/500在所有错误分支响应与日志均无原值；客户端仍得到安全code与request_id，限长异常HTML不透传，凭request_id可关联安全诊断

- 回退：保留脱敏边界；适配异常时禁用受影响互译分支并返回安全错误，不恢复原文透传。

### B04 区分完整响应、读取失败与流式中断

- 源码依据：src/app/inference_gateway.rs:4267-4286; src/app/chat_responses.rs; src/store/local_usage.rs

- 当前覆盖：成功body有限长，但read_to_end结果被忽略，Result错误仅表示长度；不能把200和部分body视为完整成功。

- 剩余缺口：结果区分读取I/O、超限、超时；流首字节后失败只标中断；usage完整性随结果上报。

- 既有实施归属：网关既有实施任务 / reports/gateway；网关负责人：读取结果类型及transport_outcome；计量负责人：完整性标记；稳定ID待补充。

- 兼容：见流状态契约；已输出不可重写HTTP状态，不自动重放有副作用的工具调用。

- 可执行回归命令（待执行）：`cargo test inference_gateway -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：伪上游200后截断/timeout，首字节前502结构化错误；已写流记录interrupted且不追加成功终止；超限独立码；比较usage完整性与工具调用次数。

- 完整输入验收：伪上游200后截断/超时：写出前返回结构化502，不记为静默成功，记录request_id与usage不完整；超限与I/O错误不同码；已写出流只标中断，不自动重放工具调用或承诺无缝续流

- 回退：关闭有问题适配路径，保留失败/未知标记；不得以HTTP200重新认定完整成功。

### B05 预算币种与可信用量契约

- 源码依据：src/mission/contract.rs:150-179; src/mission/store.rs:1840-1894,2060-2076; src/agent_core_contracts.rs; src/runtime/deepseek_harness.rs:1271-1274; src/workflow/runtime.rs:94-155

- 当前覆盖：usage有currency、billing_owner；成本缺失/空币种保守阻断。MissionBudget只有max_cost无币种，循环把不同非空币种直接累加；Codex workflow分支没有赋usage。

- 剩余缺口：成本按币种分桶、可信/完整显式化；预算同币比较；run幂等去重与重启一致；不能将未上报补零。

- 既有实施归属：预算与Runtime既有实施任务 / reports/budget、reports/executor；Mission负责人：预算门控；Runtime负责人：币种和完整性上报；稳定ID待补充。

- 兼容：见成本契约；旧事件原样保留并标不可比较；扩展原request_id/plan hash幂等不变，不隐式换汇。

- 可执行回归命令（待执行）：`cargo test mission::store -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：分别送USD/CNY/空币种/未报/不完整/零实测成本；断言混币不可比且不派发；重放同run和同request不加倍，旧hash拒绝，重启结果一致。

- 完整输入验收：USD+CNY分桶且不可作为单币限额比较，空币种/未上报成本仍停止成本预算派发；同run用量和同request_id预算扩展重放不重复；旧plan_sha256扩展被拒，重启后结论一致

- 回退：旧事件保留原值并标不可比较，成本预算维持暂停；不补零、不隐式换汇、不删除历史事件。

### B06 统一服务术语与默认来源说明

- 源码依据：frontend/src/pages/AiProvidersPanel.tsx:109-149,566-664,767-800; frontend/src/pages/AiToolsPanel.tsx:45-47,133-135; frontend/src/pages/AiConnectionsPage.tsx:168-196

- 当前覆盖：AI服务文案已部分覆盖，五页签id和默认路由入口存在；远程默认/运行环境/显式任务作用域仍需就近说明。

- 剩余缺口：逐个增删改复制及错误通知统一AI服务；空模型跟随默认、无解析标未解析；上下文深链解释默认来源。

- 既有实施归属：服务UI既有实施任务 / reports/service-ui；产品IA负责人：术语及作用域；前端负责人：反馈与上下文深链；稳定ID待补充。

- 兼容：保留五页签id、深链及默认解析算法，不宣称影响既有任务；不改变模型绑定权限。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:settings-ia`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实页面逐个CRUD含失败通知；从任务显式模型/默认/运行环境三入口交叉导航，校验空模型及未解析说明且旧深链仍到原页。

- 完整输入验收：服务新增/编辑/删除/复制及失败反馈均称AI服务；保留五页签id和深链；远程任务默认、运行环境可用性、任务显式选择分别就近说明；空模型标跟随服务默认，无解析结果标未解析，不宣称覆盖已有任务

- 回退：仅撤回新增解释/链接，保持数据与绑定不变；保留旧深链兼容映射。

### B07 工具可用与需行动同快照摘要

- 源码依据：frontend/src/pages/AiToolsPanel.tsx:130-161; frontend/src/pages/aiToolConnections.ts

- 当前覆盖：connections集合驱动ready/attention筛选，但pendingClientSync另从clients计算；headline仅看attention，可能漏ready且needs_update。

- 剩余缺口：按工具id统一事实集合：runnable与needs_action可重叠；未接入独列；摘要、筛选、行动引用同snapshot时间。

- 既有实施归属：服务UI既有实施任务 / reports/service-ui；工具接入前端负责人：同集合摘要与筛选；产品负责人：行动优先级；稳定ID待补充。

- 兼容：保留conflict显式查看差异而非覆盖；不叠加多个状态重复计数；不改变已有注册/同步能力。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:settings-ia`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实组件快照注入ready+needs_update/failed/conflict及重复来源同id，K=1时待办至少1，点击摘要筛到同一行；冲突主操作查看差异。

- 完整输入验收：ready+needs_update/failed/conflict分别测试，按工具id去重；K=1时待处理至少1且筛选定位相同行；可运行数量与需更新数量并列、未接入另列；冲突主操作查看差异，不自动覆盖；统计携带同一快照时间

- 回退：隐藏不可信总数，保留逐行状态和更新通知；不回退为全部接好。

### B08 跨层有效预算等待、事务门控与安全恢复

- 源码依据：src/mission/store.rs:486-598; src/mission/contract.rs:150-179; frontend/src/pages/missionView.ts:85-110,215-220; frontend/src/pages/MissionTeamPanel.tsx:482-524; src/app/mission_commands.rs:763; src/capability/service.rs:4267

- 当前覆盖：当前increase_budget已不要求最后事件budget_exhausted：主动增额、事务、双hash、无Running/AwaitingVerification、余额复算、运行环境保留waiting、同键同摘要重放已存在。旧输入对最后事件拒绝的描述已被前置补丁超越。前端仍用最后事件判断预算等待；submit每次生成新键。

- 剩余缺口：统一有效等待后端投影和同快照资格；稳定request_id持久到提交结论、事件游标归约、旧计划隔离和事务竞态测试；不得删除主动增额能力。

- 既有实施归属：预算后端与Mission UI既有实施任务 / reports/budget、reports/mission-ui；Mission后端负责人牵头事务归约/门控；宿主接口负责人同快照资格/兼容；Mission前端负责人共享投影/稳定幂等键；Runtime负责人可信用量/收尾；独立测试负责人执行夹具，均待实名指派；稳定ID待补充。

- 兼容：见wait DTO；保留原请求及Result兼容入口，unknown只诊断；F09为旧后端条件路径，不能在当前后端强加最后事件门控或自动resume循环。

- 可执行回归命令（待执行）：`cargo test mission::store -- --nocapture`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：逐一执行输入fixture_spec全部十例及变体，真实临时store/接口适配器，不支持的操作标未执行；同时保留主动waiting非耗尽增额成功回归；所有拒绝比全budget/status/confirmation和dispatch计数。

- 完整输入验收：IA-003/IA-004/ACC-001：budget_exhausted→task_progress在新契约下经事务复核足额扩展成功；“当前实现新请求拒绝”仅为历史输入预期，不是当前实现事实：当前无最后事件门控，保留主动增额；src/mission/store.rs:530-543校验waiting、双hash及无Running/AwaitingVerification，572-581重新核算全部限额（含返工），不足或成本不可信则拒绝；足额后运行环境阻塞则保持waiting，否则恢复active；前端投影及稳定请求键仍待实施；列表/待办/详情同快照原因、行动者、资格及游标；环境/预算需要处理，主动暂停/系统核验另分组；缺关联显示未知；保留waiting、双计划哈希、无Running、参数及全部限额/可信成本复算；仅明确批准额度，不默认不限额、不扩大权限；同键同摘要只一次扩展，同键异参拒绝；丢响应原键重试返回当前Mission，不强制激活；重放不重复用量/派发；逐例运行fixture_spec，记录目标与当前差异；模拟与源码静态检查不能替代真实接口输出

- 回退：关闭新直接扩展，保留诊断与历史事件；未知保持waiting，不改数据库、不伪造事件、不清成本、不重建任务绕过批准。

### B09 部分完成证据与三类核验结论同位展示

- 源码依据：frontend/src/pages/MissionTeamPanel.tsx:390-452; frontend/src/pages/missionView.ts:113-125; src/app/mission_dispatch.rs:31-78

- 当前覆盖：子任务有状态、required_checks和重核入口，解释实现/独立核验；实际运行行未显示通用证据入口。宿主保留完整证据与哈希但截断提示不是测试正文。

- 剩余缺口：按实际证据存在展示部分完成入口；实现提交/必需检查/独立核验三栏run、时间、结果和引用；正文缺失显式标未提供。

- 既有实施归属：Mission UI既有实施任务 / reports/mission-ui；任务前端负责人：证据入口；宿主证据负责人：实际执行引用；稳定ID待补充。

- 兼容：打开只用现有安全动作，复制保留全路径；重核同一实现与重做实现分开；不从空summary推出0次通过。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:task-view`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实页面造部分完成/核验失败/全部完成三状态；从概览和行进入同证据，复制全值；空test_summary显示未提供；重核不启动实现。

- 完整输入验收：部分完成、核验失败、全部完成均从概览和子任务行看到已有证据；实现提交、必需检查、独立核验分列实际run/id/时间；缺测试正文写未提供，不写0次通过；复制路径保留全值，打开仅用已有安全动作；重核、重做、续跑解释重复范围

- 回退：撤回新打开动作但保留只读引用及复制；不删除历史证据或改写验收结论。

### B10 服务表单复用统一原生模态

- 源码依据：frontend/src/pages/AiProvidersPanel.tsx:268-281,814-824; frontend/src/components/ModalSurface.tsx:19-50

- 当前覆盖：ModalSurface原生dialog/top-layer已有焦点恢复和局部键盘责任；服务表单仍自绘role=dialog且document级Tab循环。实际跳焦未在本次复现。

- 剩余缺口：表单接统一ModalSurface，去document键盘循环；嵌套冲突及保存忙态单一责任。

- 既有实施归属：服务UI基础组件既有实施任务 / reports/service-ui；前端基础组件负责人：ModalSurface责任；无障碍QA：焦点与读屏复核；稳定ID待补充。

- 兼容：保留既有草稿和安全取消；Esc只关闭顶层，关闭回触发器，切步聚焦标题；不引入新全局trap。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:workspace-ui-browser`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实新增→Tab/Shift+Tab→内层冲突→取消→Esc；记录activeElement及AX背景inert，关闭回触发器；NVDA/讲述人不能浏览背景，保存busy只一次提交。

- 完整输入验收：删除表单document级Tab循环，复用ModalSurface；新增→Tab/Shift+Tab→内层冲突→取消→Esc仅最上层可交互；关闭回触发器，切步聚焦标题；讲述人/NVDA不能浏览背景；保存忙态不重复提交

- 回退：禁用冲突嵌套流程，保留单层安全模态及取消出口；不得重装全局焦点循环。

### B11 预设选择语义与跨步字段错误定位

- 源码依据：frontend/src/pages/AiProvidersPanel.tsx:474-491,825-864

- 当前覆盖：预设是button+active class，步骤span；formError为总览字符串，早期字段缺失无稳定字段定位/错误关联。

- 剩余缺口：分类aria-pressed、预设原生radio、步骤aria-current；错误结构映射step/field，摘要链接与aria-invalid/describedby。

- 既有实施归属：服务UI既有实施任务 / reports/service-ui；服务表单前端负责人：radio、步骤及错误映射；无障碍QA：选择/错误播报；稳定ID待补充。

- 兼容：依赖统一模态；跨步保留非秘密输入，不把密钥写报告；不能用视觉选中替代AX语义。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:workspace-ui-browser`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：键盘方向/空格改变预设，AX所选与实际一致；逐个必填缺失，从末步返回正确步骤并定位字段；读屏播报要求，非秘密草稿保留。

- 完整输入验收：分类aria-pressed、预设原生radio、步骤aria-current，AX读出实际所选；键盘可改变选择；逐个必填缺失：摘要链接定位字段、aria-invalid/describedby读出要求；末步早期错误回正确步骤并保留非密钥输入

- 回退：保留原生输入和字段错误，只撤复杂跨步增强，改为单页可定位表单；不清空用户非秘密输入。

### B12 运行环境唯一tabpanel所有权

- 源码依据：frontend/src/pages/AiConnectionsPage.tsx:151-196; frontend/src/pages/AcpProfilesPanel.tsx:168

- 当前覆盖：父子都声明ai-panel-acp/tabpanel；父页方向/Home/End键盘切换已有。

- 剩余缺口：子页只普通容器，父页唯一持有tabpanel和id；复核所有五页签controls。

- 既有实施归属：服务UI既有实施任务 / reports/service-ui；AI连接前端负责人：父子语义边界；无障碍QA：AX检查；稳定ID待补充。

- 兼容：不改五页签id及旧深链；保留父页键盘策略和子页业务操作。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:acp-profiles`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实DOM计数ai-panel-acp=1；切换五页签controls唯一有效，方向/Home/End焦点与selection一致；AX仅一个运行环境panel。

- 完整输入验收：ai-panel-acp数量恰为1，父页持有tabpanel，子页普通容器；五页签aria-controls均唯一有效，方向键/Home/End焦点与选择一致，AX仅一个运行环境面板

- 回退：撤回其它页签调整但保留唯一id及父页所有权，不恢复重复面板。

### B13 本地令牌、窄容器验证与目录降级六态

- 源码依据：frontend/styles.css:2991,4383,4776-4778,5584-5614; frontend/src/pages/AiProvidersPanel.tsx:188-191,245

- 当前覆盖：本地12/13/14令牌已存在，事实行仍11px，actions nowrap；健康未知回落凭据已配置并非真实健康结论。窄容器溢出和对比度未实测，保持假设。

- 剩余缺口：局部令牌替换；先测窄容器，复现才加条件回落；健康目录失败隔离列表、旧快照/未知/局部重试及六态。

- 既有实施归属：服务UI既有实施任务 / reports/service-ui；UI负责人：令牌/容器回落；服务前端负责人：健康状态；QA：渲染及故障矩阵；稳定ID待补充。

- 兼容：不扩大为全站字号重构；仅健康目录失败不得清配置；未知不是不可用或成功，不使用模型数量推断健康。

- 可执行回归命令（待执行）：`npm --prefix frontend run check:design-tokens`。仅补充回归，不能替代实测。

- 实测步骤（待验证）：真实内容容器320/390/480/620/880、200%缩放、120字符URL及文案膨胀50%，记录边界；复现时服务<480换行/表单<440单列；六态注入及重试、键盘/AX/status/alert、reduced-motion；合成色文本4.5:1和焦点3:1，命中最低24px及例外。

- 完整输入验收：事实行/备用模型使用12px meta、名称13px、局部标题14px，不留9.5/10/11px裸字号；实测正文4.5:1、非文本/焦点3:1并记录合成色；内容宽320/390/480/620/880、200%、120字符URL及50%文案膨胀测边界遮挡；若复现挤压，服务<480动作换行、表单<440单列；未复现则附记录不宣称缺陷；仅健康目录失败不清配置列表，显示未知及旧快照时间，可局部重试；加载aria-busy、初始空/筛选空、错误、部分可用、成功、无权限均有文字与下一步；键盘/读屏检查status与alert，减少动效不位移缩放；命中区建议32px且核查24px最低及例外

- 回退：撤回布局增强仍保留完整文字/滚动及明确未知；健康查询故障隔离不恢复以模型数推断健康。

## 共享接口交接契约（拟议，尚未实施）

### budget_wait

```json

{
  "status": "proposed_not_implemented",
  "capability": "versioned budget_wait_v1 negotiated before enabling structured action",
  "dto": {
    "mission_id": "string",
    "plan_sha256": "current hash",
    "confirmation_plan_sha256": "confirmed hash",
    "snapshot_cursor": "monotonic event sequence",
    "snapshot_at": "RFC3339",
    "reasons": "array of {kind:budget|environment|manual_pause|verification|unknown,code,actor,plan_sha256,evidence_sequence}",
    "budget_eligibility": "{can_increase,can_resume,rejection_codes,running_count,usage_trusted,remaining_by_limit}",
    "budget": "limits plus currency contract",
    "usage": "totals plus per-currency buckets/completeness"
  },
  "rules": [
    "derive under same transaction/snapshot; no last-event-only budget inference",
    "list/todo/detail use identical cursor; sequence gaps reload authoritative snapshot; stale/old-plan association unknown is diagnostic only",
    "extension same-key same-digest returns current Mission, not forced active; same-key different digest rejects",
    "keep existing proactive waiting budget increase even absent budget_exhausted; explicit amount only; all limits recomputed; Running and AwaitingVerification gate remains",
    "environment unresolved remains waiting after sufficient extension; no permission expansion",
    "legacy Result and request fields retained; deny_unknown_fields means new request fields require capability/version boundary; no automatic resume/increase loop"
  ]
}

```

### public_error

```json

{
  "status": "proposed_not_implemented",
  "dto": "{error:{code:string,message:safe_localized_bounded_string,request_id:string,retryable:boolean}}",
  "rules": [
    "protocol adapter preserves Anthropic type/error.type or OpenAI envelope and HTTP status while exposing safe stable code/request_id",
    "allowlist safe diagnostic fields; no raw upstream body/HTML/URL credentials or secret-bearing exception detail in response or log",
    "bounded sanitized diagnostic linked by request_id; test Bearer and x-api-key dummy sentinel across pass-through and translations",
    "do not claim upstream request_id from prior 502 is fixed by report"
  ]
}

```

### transport

```json

{
  "status": "proposed_not_implemented",
  "states": [
    "complete",
    "read_failed",
    "limit_exceeded",
    "timeout",
    "interrupted",
    "canceled"
  ],
  "dto": "{request_id,transport_outcome,headers_sent,bytes_written,usage_completeness:complete|partial|unknown}",
  "rules": [
    "before first client byte return safe structured 502 for read failure/timeout, distinct oversize code",
    "after headers/bytes cannot rewrite HTTP status; terminate stream, record interrupted, no success marker",
    "usage can be partial/unknown even HTTP200; no auto replay of tool calls or seamless-resume promise"
  ]
}

```

### cost

```json

{
  "status": "proposed_not_implemented",
  "current": "LocalRunUsage estimated_cost/currency/billing_owner; MissionBudget max_cost has no currency, old events not migrated",
  "dto": "usage:{input_tokens,output_tokens,cost_by_currency:{USD:amount,CNY:amount},cost_status:trusted|partial|unreported,run_id}; budget:{max_cost,currency} behind version negotiation",
  "rules": [
    "retain raw legacy events and label incomparable; never silently sum USD+CNY as one budget amount",
    "unknown currency, missing/untrusted/incomplete cost stops cost-limited dispatch; no zero fill/implicit FX",
    "measured zero only trusted with explicit currency and complete evidence",
    "same run reconciliation deduplicates; request_id plus digest and expected_plan_sha256 protects extension",
    "limits, user-selected permissions and proactive increase are preserved; unknown legacy budget currency requires explicit compatible handling, not default USD"
  ]
}

```

## 预算夹具覆盖

保留输入fixture_spec完整定义与全部变体，按真实临时store/接口适配器运行；不支持操作即未执行，不以静态或模拟通过替代。每次reset，固定时钟，竞态用事务屏障而不是sleep。

- F01：increase(K) → reload；预期 tokens_limit=150; budget_extended_count=1; status=active; budget_reason=false; plan_and_confirmation_unchanged; dispatch_count=0；当前差异：F01/F02 last-event rejection statement is historical and no longer matches current prestart-patched store; run to establish actual result；待验证。

- F02：increase(K) → reload → resolve_environment_via_existing_authorized_path → recheck_and_resume → reload；预期 after_increase:waiting,environment_reason=true,budget_reason=false; after_resolve:active_only_if_all_gates_pass; no_permission_expansion；当前差异：F01/F02 last-event rejection statement is historical and no longer matches current prestart-patched store; run to establish actual result；待验证。

- F03：increase(K)_drop_response → concurrent_retry_same_K_same_input → restart → retry_same_K → pause → retry_same_K → increase(K,additional_tokens=60)；预期 total_increase=50; budget_extended_count=1; replay_returns_current_waiting; different_input_rejected; no_duplicate_dispatch；当前差异：当前已有成功请求同摘要重放；前端稳定键待改；待验证。

- F04：increase(K) → restart → replay_full_log → replay_duplicate_and_out_of_order_sequences_3_4 → reload；预期 budget_limit=150; budget_reason=false; cursor_monotonic; no_duplicate_budget_or_dispatch; sequence_gap_requires_snapshot；当前差异：目标投影/事件去重夹具，未执行；待验证。

- F05：reset_each_variant → increase_new_K → reload；预期 reject; budget_status_confirmation_unchanged; no_dispatch; old_plan_not_projected; unknown_diagnostic_only；当前差异：当前部分保护已有；关联归约待实现；待验证。

- F06：reset_each_variant → increase(K) → reload → finish_running → refresh_and_explicit_retry；预期 reject_while_running; no_mutation_on_reject; success_only_after_all_gates_rechecked；当前差异：需测试事务锁/屏障，不用sleep；待验证。

- F07：reset_each_variant → increase_insufficient_or_untrusted → reload；预期 reject; budget_status_confirmation_unchanged; no_dispatch; no_zero_fill_fx_or_auto_unlimited；当前差异：现有fail-closed须保留；目标含可信计量；待验证。

- F08：reset_each_variant → increase_variant → reload；预期 reject; budget_status_confirmation_unchanged; no_dispatch; no_budget_permission_expansion；当前差异：保留现有参数门控；待验证。

- F09：diagnose_and_reload → user_explicit_once_resume_if_safe → reload → user_explicit_increase_if_new_exhausted_event_confirmed → reload；预期 safe:resume_Err_and_waiting_then_explicit_extension; progress_race:reject_stop_and_handoff; unsafe:no_auto_resume_or_extension; no_loop_or_event_forgery；当前差异：仅旧后端/旧能力条件兼容路径，非当前store必经路径；当前主动增额直接复核限额及运行环境，不引入resume循环，真实接口未验证；待验证。

- F10：increase(K) → consume_tokens_to_150 → append_new_budget_exhausted → reload → replay_old_plan_events；预期 new_exhaustion_enters_budget_todo; old_plan_not_reactivated; success_not_completion_or_verification；当前差异：目标恢复后再耗尽覆盖；待验证。

## 跨执行器双向交接和生命周期矩阵

HiMind uses pinned himind service alias and per-run DSH home; Codex workflow uses cli:codex namespace and personal CLI context. Do not transfer credentials, silently substitute model, or assume Codex usage reported; workflow Codex branch currently leaves usage unset. Different AgentRun entry rejects HiMind model entirely.

交接包必填：upstream_run_id and attempt/repair_of；source baseline manifest SHA256 plus per-file input/output hashes；read/write scope and current plan/confirmation hashes；capability differences and unsupported operations as unknown；requested/actual executor/service/model namespace and effective user-selected permission mode；usage input/output tokens, cost currency/billing owner/completeness, same-run dedup key；commands/exit codes/raw output hashes and implementation vs host check vs independent verification references

源码依据：src/workflow/runtime.rs:94-155 (Codex model prefix and access; HiMind run-home usage)；src/runtime/codex.rs:54-55 (separate AgentRun path rejects HiMind model)；src/runtime/deepseek_harness.rs:1423-1544 (requested model, per-run usage)；src/app/mission_dispatch.rs:31-78 (full host evidence retained; prompt projected)

|方向|场景|操作及验收|本次状态|

|---|---|---|---|

|HiMind AI -> Codex|handoff|source submits redacted manifest; receiver verifies upstream run, every file hash, scope, capability/model/permission differences and usage before reading/continuing|待验证，未执行|

|HiMind AI -> Codex|failure|inject bounded upstream error or CLI failure; retain failed run + safe request_id, no completed state; partial usage retained|待验证，未执行|

|HiMind AI -> Codex|cancel|cancel via existing authorized host path; stop process tree; no further dispatch or writes; release claims only after stop|待验证，未执行|

|HiMind AI -> Codex|retry|retry same handoff/request key; deduplicate budget/usage and side effects; new run references prior failed run|待验证，未执行|

|HiMind AI -> Codex|resume|restart host and reload authoritative plan/cursor; validate hashes and stopped claims; permission/limit recheck; no stale stream replay|待验证，未执行|

|HiMind AI -> Codex|rework|link repair_of, source baseline and changed-file hashes; preserve prior failed evidence; obey existing max_repair_attempts|待验证，未执行|

|HiMind AI -> Codex|independent_verification|separate verifier receives immutable implementation/check evidence; must not count self-report as independent acceptance|待验证，未执行|

|Codex -> HiMind AI|handoff|source submits redacted manifest; receiver verifies upstream run, every file hash, scope, capability/model/permission differences and usage before reading/continuing|待验证，未执行|

|Codex -> HiMind AI|failure|inject bounded upstream error or CLI failure; retain failed run + safe request_id, no completed state; partial usage retained|待验证，未执行|

|Codex -> HiMind AI|cancel|cancel via existing authorized host path; stop process tree; no further dispatch or writes; release claims only after stop|待验证，未执行|

|Codex -> HiMind AI|retry|retry same handoff/request key; deduplicate budget/usage and side effects; new run references prior failed run|待验证，未执行|

|Codex -> HiMind AI|resume|restart host and reload authoritative plan/cursor; validate hashes and stopped claims; permission/limit recheck; no stale stream replay|待验证，未执行|

|Codex -> HiMind AI|rework|link repair_of, source baseline and changed-file hashes; preserve prior failed evidence; obey existing max_repair_attempts|待验证，未执行|

|Codex -> HiMind AI|independent_verification|separate verifier receives immutable implementation/check evidence; must not count self-report as independent acceptance|待验证，未执行|

## 检查证据：历史、本次、待执行严格分开

### 宿主历史观察

上游 local-run:workflow_run_mission_mission_1794473598404_ffe1e069_repair-implementation-plan-1 因502/himind_gateway_upstream_failed中断，request_id=req_himind_1a1cf0f1e09_69；未复现，未修改网关。

事件 mission_1794473598404_ffe1e069:408 两项exit_code均1，elapsed分别122/114ms；输出hash依次 f889572e10e5d2ee1bd614d9ed070d499056232f0afd291f4184de4566bd049a、1d2745380b0fd1bd1b9016346d205457e8cd2d55a5c4ce683fb5351b6016c222。两项test_summary为空，无数字TAP汇总或失败断言正文，不据此推断原因。

### 上一轮报告记录的首次执行（历史，非本次执行）

```powershell

python -c "import json; p=json.load(open('reports/implementation-plan.json',encoding='utf-8')); assert p.get('passed') is True; assert len(p.get('backlog',[]))==13"

```

exit_code=1；stderr=FileNotFoundError: reports/implementation-plan.json；stdout为空。JSON缺失，未到任何断言。MD read亦返回not found。

```powershell

python -c "import json; from pathlib import Path; p=json.loads(Path('reports/implementation-plan.json').read_text(encoding='utf-8')); assert all(k in p for k in ['passed','implemented_backlog_ids','changed_files','checks','limitations']); assert isinstance(p['checks'],list) and p['checks']; assert all(isinstance(c,dict) and c.get('command') and isinstance(c.get('exit_code'),int) and c.get('evidence') for c in p['checks']); assert isinstance(p['implemented_backlog_ids'],list); assert isinstance(p['changed_files'],list) and p['changed_files']; assert set(p['changed_files']) <= {'reports/implementation-plan.md','reports/implementation-plan.json'}; assert Path('reports/implementation-plan.md').read_text(encoding='utf-8').strip()"

```

exit_code=1；stderr=FileNotFoundError: reports/implementation-plan.json；stdout为空。JSON缺失，未到任何断言。MD read亦返回not found。

上一轮记载输入解析及hash命令（历史记录，exit_code=0）：Python json.loads逐个读取backlog/baseline-manifest/prestart-host-patch，hashlib.sha256读取上述文件；输出valid和663文件。源hash及基线归属详上表。

### 上一轮报告记录的生成后复跑（历史，非本次执行）

上一轮报告记载两项原命令在报告生成后已逐条直接执行成功；随后用相同python -c payload经subprocess argv再次执行并捕获stdout/stderr，exit_code均0，stdout/stderr均为空。JSON checks保存命令、退出码、原始空输出及SHA-256；空输出不是数字TAP汇总。合并空输出SHA-256均为e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855。仅报告结构检查，不是生产/独立验收。

## 限制与下一步

- No production or real interface/browser/keyboard/narrow-screen lifecycle tests executed here; all such plans pending.

- 上一轮报告缺失记录仅为历史；本次两份现有报告已读取并修订。

- No numeric TAP summary for host observations; do not invent counts.

- Stable IDs for existing downstream implementation tasks are absent from allowed inputs; mapped by existing backlog ownership and provided artifact groups only. Control owner must supply exact IDs; no new tasks/approvals created.

- No formal VERIFY or independent acceptance/deployment invoked.

- Proposed DTOs are handoff targets, not currently available API contracts.

- Control owner supplies stable IDs for existing implementation tasks using owner/group mapping; no new scheduling is performed here.

- Existing config/gateway/budget/service-UI/Mission-UI/Runtime tasks implement remaining gaps, preserving prestart changes and proactive budget extension.

- Existing QA task executes per-item real scenarios and both-direction lifecycle matrix; capture raw outputs/snapshots/hashes with actual run IDs.

- Host separately starts formal VERIFY after this stage; deployment remains control owner responsibility.

## 本阶段交付结论

报告内容交接保留；本次写入前两项原检查实际exit_code均0；最终修订版的检查结果以本次运行记录末尾真实复跑为准，不以历史成功替代。passed仅为当前阶段送验建议；后续任务稳定ID需控制端从既有计划补充，不代表新增审批或阻断本报告交付。下一步由既有实施负责人依逐项缺口/兼容约束/实测计划执行；真实场景仍待验证，正式VERIFY由宿主另行启动。


## 本轮校正依据与范围观测

结论：`budget_exhausted→task_progress`本身不导致当前后端拒绝。`src/mission/store.rs:492-528`处理参数及同键重放；`530-543`检查waiting、计划/确认hash和运行/待核验任务，不检查最后事件，且无耗尽原因时使用主动增额原因；`544-574`计算增额并复核全部限额，`1817-1898`含返工、轮次、时间、token及可信成本；不足则拒绝。`579-581`调用`2088-2098`检查当前计划运行环境：阻塞保持waiting，否则active。主动预算扩展是既有能力，不记本次实现。前端`missionView.ts:101-108`仍依最后事件标预算等待，共享投影、稳定幂等键及真实竞态验证仍由原预算/Mission UI任务承接。

事件`mission_1794473598404_ffe1e069:418`的`protected_file_check.matches=false`及`reports`路径差异仍是未追溯消解的历史事实；不推断具体文件、操作或责任，不以本轮观测证明历史已修复。

最新宿主事件`mission_1794473598404_ffe1e069:422`：第一项原检查exit_code=0、elapsed_ms=262；第二项exit_code=0、elapsed_ms=111；两项output_sha256均`01ba4719c80b6fe911b091a7c05124b64eeece964e09c058ef8f9805daca546b`，test_summary均为空，无数字TAP汇总。这是宿主历史成功观察，不是修改后成功证据。

本次写入前已在运行记录输出全部19个task read roots的目录成员及文件SHA256清单，汇总`a67da72c32213684dfb65cc5dabb95a68cfef8740a37b9de2a0665086c2ebbae`；另输出排除两份授权报告后的逐root摘要（无读取错误或符号链接）。结束以同算法同范围对照，结果仅保留运行记录，不另写快照文件。该观测仅限本轮时间区间，不冒充宿主执行前基线；若出现非授权差异，记录可见路径且不恢复、不删除、不声称范围无误。

本轮两项原命令原样保留在上述命令块和JSON required_checks中；写入前真实运行均成功、无输出、无TAP数字。修改后仍须实际复跑，最终运行记录是依据。下一步：既有负责人按逐项交接执行待验证测试；本阶段交付后返回宿主另行正式核验，不执行或等待正式VERIFY。
