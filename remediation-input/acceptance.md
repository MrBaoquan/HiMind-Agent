# 独立验收复核 / audit.acceptance

## 结论 / summary

**verdict=pass，仅指审查方案交付质量符合契约，送交宿主正式 VERIFY，不代表 Mission 最终通过。** 本轮完整读取六份契约、原验收、全部专项及综合交付，复核18项发现、18项处置、13项待办和离线原型。ACC-001 的原阻断是“只修前端展示无法满足后端恢复门控”，不是要求本报告任务实施生产整改。修正后的 IA、综合方案、B08 和原型已真实改变该设计：后端事务规则先行，接口资格协商，前端共享投影及稳定幂等键随后，配套成功、拒绝、重放和恢复夹具。独立静态复核后关闭此**审查方案阻断**，保留生产风险及所有未测范围；不是只因上游自报或为了退出码改 verdict。

仅修改 reports/acceptance.json 和 reports/acceptance.md；source、contracts、evidence/ui 和上游报告保持不变。没有执行正式 VERIFY。

## plan：高风险与最小充分矩阵

优先保护：配置完整性、秘密边界、预算/权限门控、失败恢复；其次是计量可信、状态一致、证据可发现性及无障碍。标准来自 BRIEF、report-contract 和18个 audit-topics。

|矩阵|输入与边界|独立核查结果|
|---|---|---|
|来源/保护|六契约、manifest、含未提交快照及安装版|HEAD不能代替工作树；原检查校验源码hash|
|代码事实|全部18项ID、原文、条件、影响、优先级|专项逐字引用由原检查匹配，核心分支定点复读；不等于全仓调用链审计|
|真实接入路径|服务→路由→工具接入→执行器/任务|七图六AX及报告核对；保存/请求/自检未执行|
|预算恢复|新请求、环境并存、旧计划、Running、未知成本|跨层规则和F01–F10已形成可复现规格；真实接口未执行|
|失败恢复/回归|响应丢失、同键异参、重启、重复事件、再耗尽|有不变性断言、停止与移交路径；不把模拟当生产回归|
|追溯/冲突|18 disposition、13 owner/依赖/验收/回退|全量追溯完整，依赖无环由原检查校验|
|原型|离线、虚构数据、层级、忙/错/空、预算模拟|静态读全部HTML/JS；无浏览器、键盘、窄屏实测|

## execute：全部发现与追溯

以下路径相对 source；完整引用在专项JSON，checked_files仅列本轮实际读取文件。confirmed限静态实现或安装观察，不是全部故障已复现。

|ID / 优先级|独立事实核对与边界|待办/结论|
|---|---|---|
|AI-001 P1|store/ai_services.rs:528–568 整文件解析及fs::write；中断风险未注入|B01锁内原子写与恢复合理|
|AI-002 P1|同文件1179–1205 removed为真才清引用；重删无法修复悬空|B02→B01，换序不是事务|
|AI-003 P1|app/inference_gateway.rs:2420–2488 原始detail进入客户端；401/403/500等条件通道|B03公共DTO/虚构秘密夹具；非真实泄漏事件|
|AI-004 P2|同文件4279–4287吞读取错误；成功写出/记账引用经原检查匹配|B04→B03；不承诺流后无缝重放|
|AI-005 P2|mission/store.rs:1766–1805 非空币种直接相加，空币种保守阻断|B05分桶/拒绝比较；非已有账单损失|
|IA-001 P2|main.tsx:2260模型来源反馈、SettingsPage:792–795执行工具，与服务/任务作用域不同|B06保留稳定页签与默认策略|
|IA-002 P2|AiToolsPanel:135–161两套统计，安装图与AX同屏就绪/待处理0/待更新1|B07与UI-007合并，不推断调用失败|
|IA-003 P1|TaskCenterPage:298–305未传事件，taskCenterModel:6–13、68–71遗漏独立预算/环境待办|B08共享投影须以接口与后端资格为前提；已同步修正|
|IA-004 P1|MissionTeamPanel:332、355–356及missionView:101–107依赖任意最后事件|B08含后端同类门控修正，不再只补按钮|
|IA-005 P2|MissionTeamPanel:330、368–370总体completed才显示已有证据，424–435无同位入口|B09→B08；不是证据丢失|
|UI-001 P1|AiProvidersPanel:250–283及812–820自管模态；ModalSurface:19–50原生dialog并有局部Tab处理|B10统一责任；实际跳焦/穿透未复现|
|UI-002 P2|AiProvidersPanel:839–853普通button仅active；service-create AX支持|B11→B10；不是键盘不能点击|
|UI-003 P2|同文件823–824全局alert、861–864无字段错误关联|B11；实际错误态未复现|
|UI-004 P2|AiConnectionsPage:185–187与AcpProfilesPanel:167–169重复ID；runtime AX:46–47一致|B12；非实听重复播报|
|UI-005 P2|styles.css:5584–5585为11、5680–5683为10/9.5及透明度，对照专项12/13/14令牌|B13；不直接判对比度失败|
|UI-006 P2 hypothesis|styles.css:5594–5606锁动作第三列，双列表单引用由原检查匹配|B13先量测再条件修复；无窄屏证据|
|UI-007 P2|同IA-002的源码、安装像素及AX支持竞争摘要|B07不重复计开发量|
|UI-008 P2|AiProvidersPanel:242–288目录失败置null并凭模型数兜底|B13健康未知显式呈现；当前文案不是请求成功|

优先级合理：P1针对全目录、条件秘密泄漏、核心恢复及模态风险；P2针对计量和局部语义/可用性。UI-001需实测再判断实际焦点影响；UI-006不升级confirmed。不以文件大小或未测对比度凑问题。

## 领域、方案冲突和分期

AI六主题（目录、路由、凭据、用量预算、执行器/客户端、配置所有权）、IA六主题（发现、术语、默认覆盖、下一步、进度证据、恢复）、视觉六主题（层级、表单、模态、反馈动效、响应式、对比度/无障碍）均有coverage及证据边界。AI有组件/数据流图；IA有导航和四条核心路径；视觉有本地令牌、状态/键盘/窄屏建议。

18项disposition各一次、均accept，全部落入B01–B13。UI-006采纳验证及条件修复，不声称已复现；IA-002/UI-007合并B07。owner为职责，实名待指派。依赖B02→B01、B04→B03、B09→B08、B11→B10；M1八项、M2五项。回退不恢复秘密透传、不混币、不删除证据。综合明确本地12/13/14优先于IA通用14–16；保留页签id；双文件事务存储是后续选项而非强制大迁移。默认请求优先级及权限产品定义仍需M0确认，不伪造既有接口。

## ACC-001：独立关闭依据（仅审查方案）

### 当前源码仍有的生产风险

可复核静态输入：waiting、当前/确认计划H、真实预算耗尽、无Running，新request_id，事件budget_exhausted→task_progress。store.rs:464–470查询任意最后事件并拒绝；前端:332同样丢预算入口。仅显示按钮不能恢复。

独立读 store.rs:350–534确认：:418–430参数约束；:432–454事务和完整输入摘要，成功同键同参提前返回当前Mission、异参拒绝；:456–462 waiting及双计划哈希；:472–505无Running、真实耗尽和全部限额复算；:507–528写一条budget_extended，环境仍阻塞则waiting，否则active。:367–386普通resume可提交诊断事件后返回Err，不是恢复成功。:1766–1805未知成本继续停止。mission_commands.rs:679–688当前是Result<MissionView,String>，没有方案中的新结构化拒绝码。MissionTeamPanel:505–526每次提交新UUID，稳定键确为待实施。

### 上游实际修正与一致性

|审查点|本轮实际文件依据|独立结论|
|---|---|---|
|目标后端门控|ia.json:300–325/ia.md:156–200；optimization.md:106–115；B08:214–242|选择后端事务内当前计划/确认边界+真实用量归约，task_progress不清原因；无法可靠关联unknown拒绝。不是前端自行授权|
|责任/次序|IA preferred_owner、综合:108–122、B08 owner/implementation_order|Mission后端牵头→接口同快照及能力协商→前端共享投影/稳定键→独立真实回归；Runtime负责可信计量|
|安全|IA guards、综合:113–115、B08 acceptance/rollback|保留waiting/双哈希/无Running/所有限额；不补零、不换币、不自动unlimited、不扩OS权限；B05后置不推迟已有fail-closed保护|
|成功与拒绝|IA interface_semantics、综合:113–124、B08 F01/F02/F05–F09|active不等于派发；环境仍waiting；拒绝比较完整预算/状态/确认/dispatch；普通resume诊断Err单列例外|
|幂等/重放/恢复|B08 fixture_spec:243–551，综合:126–141|基线Token100/100追加50，明确初始值、操作、150目标与唯一事件；F03丢响应同键/异参/重启/暂停，F04重复乱序/缺游标，F10再耗尽；不要求模拟冒充真实API|
|离线表达|prototype.html:10、17–29|新契约成功/环境/计划/Running/成本拒绝及旧版一次重检均有对应分支；标注未实施、无接口、内存键；未假称生产修复|

F01–F10规格具备隔离临时数据、固定时钟、事件映射、事务屏障、前后快照、脱敏请求响应、扩展事件数、dispatch计数及重启重放证据要求。适配器尚未实现，不能直接当可运行测试程序；其要求已经足以作为后续实现及验收输入，能力不足应报未执行。旧版本只读诊断→一次明确resume重检→Err+waiting刷新→明确额度扩展是静态推导路径；遇并发事件停止移交，不循环、不改数据库、不伪造事件。必须真实验证后才能推广。

因此关闭的是旧方案缺少后端决策、职责、安全与成功/拒绝/恢复规格的缺陷。生产代码仍待B08实施，F01–F10均未由本轮运行。原型重放按钮主要演示状态不变和日志，不是通用事件归约引擎；仅离线设计切片，这一限制不被误计为后端验证。

## 真实界面与离线原型

本轮重新只读观察七张原图像素：tasks有完成/失败、workspace unavailable及重试，无预算详情；ai-connections有就绪/待处理0/待更新1；services有工作台未配置和本机服务/操作；gateway有入口及两条观测；routes有默认/备用/自检及无可用服务；runtime有引擎/CLI/接入分组；service-create有三步骤、空连接表单与取消/下一步。六AX定点复读支持这些语义及重复面板。扩展名与字节不符时read_image拒绝，按授权media.image.read读取原像素，未转换、改名或覆盖证据。预览尺寸不是CSS视口或对比度测量。版本是v0.3.92(e630a08)，不等于当前未提交源码构建。

完整静态阅读prototype.html（长CSS行工具显示有截断，不据此声称逐字符审完该行）：内联资源、虚构example.invalid、无真实网络/保存，服务→模型、执行器独立、五类等待与部分结果分开；六内容态、筛选空、dialog/radio/名称错误关联及焦点归还有代码。B08显式提交中需手动返回模拟结果；不是并发/真实授权测试。UI-004未模拟，分类完整方案/全向导/业务校验未实现，旧visual原型行号已漂移且综合已说明。本轮按当前id和行段核对，不把旧行号当现状。键盘/窄屏只是实现意图。

## 实际审阅文件

六契约：contracts/BRIEF.md、report-contract.md、audit-topics.json、check.py、source-status.txt、source-manifest.json（分段全文）。
全部报告：reports/ai.json、ai.md、ia.json、ia.md、visual.json、visual.md、optimization.md、backlog.json、prototype.html、acceptance.json、acceptance.md。
界面：evidence/ui/README.md；tasks.png、ai-connections.png、services.png、gateway.png、routes.png、runtime.png、service-create.png；ai-connections.ax.txt、services.ax.txt、gateway.ax.txt、routes.ax.txt、runtime.ax.txt、service-create.ax.txt（AX为定点段）。
定点源码：source/src/store/ai_services.rs；source/src/app/inference_gateway.rs、mission_commands.rs；source/src/mission/store.rs；source/frontend/src/main.tsx、components/ModalSurface.tsx；source/frontend/src/pages/AiToolsPanel.tsx、MissionTeamPanel.tsx、AiProvidersPanel.tsx、missionView.ts、taskCenterModel.ts、TaskCenterPage.tsx、AiConnectionsPage.tsx、AcpProfilesPanel.tsx、SettingsPage.tsx；source/frontend/styles.css。

这与JSON checked_files一致，不继承旧验收中本轮未重读的credentials/builtin_ai_router/assignment/agentApi为本轮阅读。辅助coverage调用链仅据专项和检查器引文匹配，未逐行审全仓。

## verify：真实检查与历史来源

宿主历史事件 mission_1794443924791_2d612a63:348：命令python contracts/check.py acceptance，exit_code=1、elapsed_ms=212、output_sha256=a10c2344db92c071e979a39fd796ec31035f28bce1ed5cd034d4180b5d3b065c。test_summary为空，无检查正文或数字TAP汇总；不能反推其具体失败原因。

本轮修改前真实重跑同一命令：exit_code=1，stderr为 `AssertionError: Independent reviewer requests changes; do not conceal failure`，定位check.py:94（入口:99）。检查已到最终verdict断言，非沙箱不能运行或缺验收文件。原验收对上游的旧阻断描述已与修正交付不符，本轮依据上述独立证据更新关闭记录及结论，保留blocking_issues字段和生产风险，不改检查器、不缩减输入。

修改后实际执行 `python contracts/check.py acceptance`，退出码 **0**，stdout：`{"passed": true, "stage": "acceptance", "source_files_unchanged": true, "meaning": "Structural and evidence checks only; independent semantic review still required."}`。JSON checks同步记录；补记后再次执行同一命令绑定最终交付，以工具记录为准。未输出数字TAP计数，不编造测试通过数量。检查仅验证源码基线、结构及引用，不替代独立语义复核或真实接口实测。

## deliver：限制与下一步 / next_steps

未启动/构建产品或浏览器；未运行原型交互、真实模型请求、保存、自检、预算接口、Rust测试、事件/故障注入、重启恢复、焦点/读屏/键盘、窄屏、200%放大、长文本、主题、动画、对比度和实际命中区测量。tasks无AX；预算/环境/部分完成详情缺安装证据。上游node/mock-DOM检查只作其报告中的执行记录，本轮未复跑，不能算真实接口回归。源码保护由指定检查验证；未对全部报告和截图建立本轮前后哈希证明，不宣称全工作区字节级无变化。

1. 完成本阶段必需检查后将两份验收及关闭依据交宿主另行正式VERIFY，不自行调用或宣布其通过。
2. 指派B01–B13实名owner；B08按后端→接口协商→前端→独立夹具次序推进，真实API未证实前不开放承诺可恢复的入口。
3. 后续取得F01–F10真实脱敏请求响应/快照/事件/dispatch/重启重放证据；人工离线检查原型键盘/窄屏。未知或失败保留，不以结构绿色代替生产整改。
