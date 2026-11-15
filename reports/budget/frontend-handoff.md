# B05/B08 后端交接

## DTO（兼容增量）

`MissionView`保留`mission.v1`及旧字段，新增：
- `budget.currency?: string`：成本限额的显式币种。旧计划缺字段可读取，但有成本限额时不可比较，保守停止，不改变旧计划hash。
- `cost_usage: {buckets: Record<string,number>, unknown_runs:number, comparable:boolean}`：币种按trim/ASCII大写分桶，不换汇。只有单币、每run成本完整可信时comparable=true。预算币种还必须匹配。
- `effective_wait: {contract_version:'mission.wait.v1',plan_sha256,cursor,reason,actor,budget_reason,environment_blocked,can_increase_budget,can_resume}`。

列表`list_missions`/MCP mission list、详情get及snapshot统一调用store的load_mission投影，均在SQLite读事务内读预算、任务、关联、用量、事件游标。待办应直接消费列表此字段，不再按最后事件推断。后端不另维护独立inbox数据库。`reason`：budget_exhausted、environment_blocked、manual_pause、system_verification、unknown、not_waiting；`actor`：user/system/unknown/none。预算原因优先但environment_blocked独立保留；task_progress不能遮蔽。缺确认或hash不匹配归unknown，不推测旧事件关联。

`can_increase_budget`表示可提交明确主动增额的前置资格，不承诺参数足额；服务器在IMMEDIATE事务重新核查双hash、Waiting、Running/AwaitingVerification及所有限额。主动暂停用户仍可明确加预算续跑，不自动执行。`can_resume`在环境/预算/执行中阻断时false。系统核验由任务状态判定，不把未知waiting冒称核验。

旧`usage`标量只在单币可信时返回该币金额；混币/未知时currency为空、estimated_cost为兼容占位0，**不得当实测零成本展示**，必须看cost_usage.comparable。旧事件不改写，未带可信完整标记的成本不再安全比较。当前record_run_usage入口依据运行时billing_owner=unreported/币种存在明确记录trusted和complete标记；这不是对上游账单真实性的独立审计。

## 游标

`snapshot`返回`mission`、`events`、`cursor`及新增`requires_snapshot`。游标与mission.effective_wait.cursor取同读事务MAX(sequence)。重复/乱序delta不得重复归约预算；以服务器mission为权威，不从budget_extended再次加钱。requires_snapshot=true（缺口/截断/超前cursor）替换为完整mission，不继续增量归约。所有提交保留同一request_id与原摘要，丢响应原键重试；同键异参拒绝。重试返回当前Mission，不把后来的pause改为active。

## 真实样本及测试

`dto-sample.json`从F01真实store测试输出提取，使用虚构任务和用量；逐例证据`f01.log`…`f10.log`、`b05.log`。F05手动暂停期望按本次明确保留主动增额授权修正；F09旧后端客户端最后事件race拦截不是本后端契约，不伪称运行旧二进制。测试为实际SQLite Store、真实事务锁与重启重开，不是模型请求/MCP网络端到端或浏览器；前端消费实现交给前端专家，宿主另行正式VERIFY。
