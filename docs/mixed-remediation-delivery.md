# 混合整改部署前交付复核

结论：本阶段交付就绪，建议送宿主正式核验。FREV-E01由新增完整证据核验解除；这不是Mission最终通过，也不代表已部署。两条原required_checks的最终执行回执见 `reports/final/required-checks.json`，以其report_sha256绑定本报告。

本轮仅审查和写报告，执行器personal.codex、空model/service及项目架构师1.0.0绑定未变。未运行QA --prepare/--stage/independent-candidate、冷编译、模型调用、浏览器或部署，未调用宿主VERIFY。当前QA11正式接受由原生1063/1066/1067记录核实；副本不是本轮测试。

证据边界：自身host资源根的READY任务/mission、manifest独立seal及394文件hash全部核实。385个映射记录实际读取；原JSON外部路径仅作为manifest键，不打开原路径。QA19条命令/argv/exit/hash、四条worker最终原命令复跑、浏览器断言测量、四跳helper/session/usage以及原生payload_json均已核对。宿主外层stdout/stderr没有重建。原QA11报告SHA为15b7778a1b5c397189e7967704ebab3581dc24e2c1e71d27afd87083038535f0。

报告/当前prepared候选b6d56e1a…与原宿主候选56e619a6…算法不同；较早prepared收据05d459e1…仍独立保留。prepared的passed:false由准备代码固定初始化，后续产品与宿主验收分开。521个当前授权源码hash与冻结身份匹配；37个超读取范围的冻结条目未打开。663基线有508未变、51变化、104未读取、0个授权可读文件缺失；不把基线并行改动、7文件前置修复或主动增额算本轮成果。

历史：写新报告前逐字节保存全部旧final报告和本文档，旧result SHA30533e30e9c42e9005e827a26eab6adcb15990441f75a1dde0500157e6b91a7d。FREV-E01原阻断、event1083 exit1/131ms/output9199219c…/空test_summary、readiness1 supervisor取消、QA11首轮0/0/1/0及909/933/997/QA7–10历史均保留。event1083没有数字TAP总数，不补造。新报告不是只翻passed；独立新增核对见review-evidence.json与自身root中的hash审查记录。

|待办|核实证据与限制|
|---|---|
|B01 目录锁内原子读改写与上一版本恢复|原日志49项配置/原子文件回归中，七个中断点的子进程实际exit86，重读可解析；并发不同服务与损坏备份拒绝均有命名测试。不是断电/硬件持久性认证。|
|B02 删除幂等清引用及启动一致性诊断|原日志覆盖目录已提交、路由未提交时退出及相同ID删除重试；启动诊断只读，不替换默认服务，剩余路由顺序不变。|
|B03 统一公开错误DTO与脱敏边界|原日志80项网关回归包含7种直通/互译组合×401/403/500，虚构Bearer/x-api-key回显不进入公开响应与日志；多data行SSE负例保留。|
|B04 区分完整响应、读取失败与流式中断|真实loopback伪上游覆盖200后截断、限长、超时、SSE首字节前后；已开始流只记中断、不伪造完成、不重放。|
|B05 预算币种与可信用量契约|实际隔离SQLite按currency分桶；缺currency/trusted/complete及混币保守阻断。run最新投影和request摘要幂等；Codex累计与本次增量分别核实。未知成本不补零。|
|B06 统一服务术语与默认来源说明|真实Chrome生产组件+mock API核对AI服务术语、五页签深链及默认/任务/环境绑定的就近说明；并未改变解析策略。|
|B07 工具可用与需行动同快照摘要|Map按工具id去重，同connections派生ready与attention及筛选；浏览器覆盖ready+needs_update/failed/conflict、K=1、冲突仅差异操作。|
|B08 跨层有效预算等待、事务门控与安全恢复|13项实际Store预算测试覆盖F01–F10；浏览器b-FYDdun经隔离JSON桥调用当前Store，验证丢响应原键重试/暂停重放/环境阻塞。F05主动暂停增额按明确保留的既有授权；F09是新版Store安全语义及组件契约，未运行旧后端二进制。|
|B09 部分完成证据与三类核验结论同位展示|实际React DOM核对暂停时checks_completed先于独立结论、6方向Windows根拼写、外根/歧义/旧候选/空checks负例及10005事件分页；退出码0/1/-1为传输样本，不能当该浏览器真实宿主执行。QA11正式宿主事件另核。|
|B10 服务表单复用统一原生模态|实际原生dialog、嵌套Tab/Esc、触发器返回、背景隔离和保存busy禁重复均由原浏览器断言和当前ModalSurface实现对应。|
|B11 预设选择语义与跨步字段错误定位|原生radio/pressed/current、错误摘要链接及跨步定位保留非密钥字段；晚到服务错误回早步及单次保存有独立扩展断言。|
|B12 运行环境唯一tabpanel所有权|父页唯一tabpanel，五页签aria-controls唯一，方向键/Home/End的选中和焦点由真实浏览器核对。|
|B13 本地令牌、窄容器验证与目录降级六态|原测量320/390/480/620/880及296容器、120字符URL/50%扩写、12/13/14px、六态/快照时间、reduced-motion；抽样文字最低5.4417、焦点8.5142、必要控件边界3.6579、命中区最低28px。非整站AA认证。|

F01–F10逐项命名测试全部在实际隔离SQLite日志中核对；F05主动暂停增额保留授权优先，F04是只读快照重取，F09不是旧后端二进制实测。BUDGET-AWAIT-01、QA-LEGACY-01、QA-HARNESS-01、QA-LIVE-01分别由AwaitingVerification门控、实际组件单一恢复CTA/Enter及负例、产品命名空间/来源绑定回归、实际四跳helper支撑。19组中的后端214项和executor468次执行（463个不同ID）分别计数，不合并为本轮TAP。

HiMind实际以lucen/gpt-6.1-sol调用产品Runtime；personal Codex以个人CLI配置launch并同session resume，具体个人模型名未由收据给出，不能猜测。五产物40→41→42→43→44及三份双向envelope/hash/作用域已核对。Codex累计162199/1639扣launch76064/1036，resume增量86135/603；cached/reasoning是子集，不重复相加。HiMind525/35与2274/35为Runtime投影，真实账单/成本未知；default0不等于免费。取消树终止、进程死亡/注入失败、账本与Store重开、返工祖先和恢复资格有不同层级产品回归，不一概称安装Mission或OS重启通过。

普通键盘、焦点陷阱/返回、嵌套Esc、背景隔离、busy/status真实性、窄屏/窄容器及对比度全部保留。读屏专项与所有200%缩放当前均为 `excluded_by_user, passed:null, blocking:false, future_obligation:false`；明确不做，无待办或未来义务。旧冻结报告和日志里的deferred措辞只作历史，不改其字节。Remote Dashboard400/dead_letter保留观察限制，无远程部署、重排或删除。

下一步仅由supervisor实施以下后续阶段；这些尚未发生，不是本次审查的循环前置条件。

部署前：另行正式VERIFY；集成最终源并核对difference-index.json及并行修改，保留前置权限修复/主动增额；固定源码/lock/生产签名公钥/Runtime与最终构建hash，禁止安装测试公钥或helper。验证容量和整套旧安装、配置目录/路由同版本、数据库兼容备份可恢复。既有四项high npm advisory和chunk警告是生产者历史观察，本轮无新网络审计或升级；由控制端部署风险复核。

部署后：核安装exe/Runtime/前端hash、health、网关真实响应与安全request_id；已安装UI五页签/普通键盘/焦点/嵌套模态Esc/背景隔离、busy单提交、字段定位、六态与窄屏/对比度；已安装HiMind↔Codex实际任务、模型来源/权限、usage增量、取消后进程树/claim、恢复与失败返工；预算丢响应同键、异参拒绝、Running/AwaitingVerification/环境/旧计划/未知成本门控、再耗尽及列表待办详情一致。

可恢复回滚：health/安装hash/安全门控/预算异常时由控制端停止新派发、确认旧写进程退出，保留事件/usage/现场；恢复验证过的成套exe+Runtime+前端，目录与路由同版本成对恢复，数据只按已验证兼容快照。数据兼容性未知则保持停止并交接，不破坏性回退。恢复后复核hash/health/配置可解析和原任务确认/限额，不删除失败事件、不清成本、不自动重放工具，保留回滚操作与输出hash。

最终差异索引：`reports/final/difference-index.json`列663基线每个授权条目hash、未读取范围、新增授权代码路径及7个前置文件归属。缺baseline原文处仅提供hash差异，不伪造逐行diff。本reviewer只写result/review-evidence/difference-index/required-checks与本文；原主仓库未打开或写入。
