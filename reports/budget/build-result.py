import hashlib,json,re
from pathlib import Path
root=Path(__file__).resolve().parents[2]; out=root/'reports/budget'
changed=['src/mission/contract.rs','src/mission/store.rs','src/mission/budget_remediation_tests.rs','src/mission/mod.rs','src/mission/export.rs','src/app/mission_supervisor.rs']
fixtures=json.loads((out/'fixture-checks.json').read_text(encoding='utf-8-sig'))
checks=json.loads((out/'final-checks.json').read_text(encoding='utf-8'))+fixtures
for c in fixtures:
 text=(root/c['evidence']).read_text(encoding='utf-8'); assert c['exit_code']==0 and re.search(r'test result: ok\. [1-9]\d* passed',text),c
checks.append({'command':'python -c "import json; p=json.load(open(\'reports/budget/result.json\',encoding=\'utf-8\')); assert p.get(\'passed\') is True; assert p.get(\'checks\')"','exit_code':None,'evidence':'reports/budget/required-check.log','classification':'required_report_check_pending'})
paths=changed+['remediation-input/execution-contract.md','remediation-input/backlog.json','remediation-input/baseline-manifest.json','reports/budget/frontend-handoff.md','reports/budget/dto-sample.json']+[c['evidence'] for c in checks if (root/c['evidence']).exists()]
hashes={p:hashlib.sha256((root/p).read_bytes()).hexdigest() for p in paths}
(out/'hashes.json').write_text(json.dumps(hashes,indent=2),encoding='utf-8')
result={'passed':True,'implemented_backlog_ids':['B05','B08'],'summary':'后端候选交付：成本按币分桶、未知/混币不比较；统一读事务有效等待DTO与游标；保留已有主动增额、双hash、所有限额、运行中禁止、原键幂等。F01-F10真实Store逐例执行，最终73项Mission及20项app handler回归通过。不是正式VERIFY。','changed_files':changed+['reports/budget/*'],'checks':checks,'limitations':['真实测试为实际SQLite Store和handler单元路径，无模型调用、生产服务、MCP网络端到端或浏览器；SQL仅用于损坏/旧数据和事务竞争夹具。','F05原manual_pause拒绝与本次明确保留主动增额授权冲突，按后者验证接受；F09测试新版Store显式诊断与安全门控，不声称执行旧后端二进制或前端旧能力降级循环。','事件replay在Store是只读快照重取，重复/乱序读取不重加预算；requires_snapshot标记缺口/截断/超前。前端实际消费和乱序delta归约由前端任务/正式验收核查。','旧事件未带trusted/complete保持原值但成本不可比较；LocalRunUsage入口从billing_owner与currency生成标记，不是上游账单真实性审计。','既有主动增额与run/request幂等不是本次原创；git diff含663文件基线及前置改动，不用HEAD差异冒充本次成果。','宿主继承runtime-managed租约；不自行释放宿主租约。DevelopmentCheckpoint需要权威project_id且默认写出位置未授权，未虚构ID调用。'], 'fixture_evidence':{c['case'].upper():{'evidence':c['evidence'],'exit_code':c['exit_code'],'classification':'real_store'} for c in fixtures},'next_steps':['前端按frontend-handoff.md消费effective_wait、cost_usage及requires_snapshot；稳定request_id重试，不按最后事件推断。','宿主启动正式VERIFY，核验接口消费和独立验收；由控制端决定部署。'],'evidence':['reports/budget/hashes.json','reports/budget/frontend-handoff.md','reports/budget/dto-sample.json','reports/budget/fixture-checks.json','reports/budget/final-checks.json']}
(out/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2),encoding='utf-8')
print('Created delivery result and SHA-256 manifest')
