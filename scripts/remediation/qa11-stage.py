"""QA11 authoring before final freeze; default independent entry stays read-only."""
import importlib.util
import json
import os
from pathlib import Path
import sys
sys.dont_write_bytecode=True
from executor_lifecycle_support import resource, identity, require_identity, digest, write_json, run
from executor_delivery_contract import frozen_assets

def module(name):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(name+'.py'))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
b=module('qa11-evidence-binding')
root,out=resource();qa=root/'independent-qa';qa.mkdir(exist_ok=True)
def load(p):return json.loads(Path(p).read_text(encoding='utf-8-sig'))
def binding(r):
    if r.get('source')=='worker':
        # Re-serialize an already captured record without changing command/output.
        raw=dict(r,command=r['executed_command'])
        r=b.bind_worker(raw)
    else:r=b.bind_worker(r)
    b.validate_worker(r,root)
    return r

def stage():
    prepared=load(out/'prepared.json');require_identity(prepared['identity'],identity(),'qa11_stage')
    assert prepared['real_codex'] and prepared['real_himind'], 'own live preparation missing'
    backend=load(qa/'backend-checks.json');browser=load(qa/'browser-checks.json')
    checks=[binding(r) for r in backend+browser+[load(qa/'budget-browser.json')]+load(qa/'supplementary-checks.json')]
    assert all(r.get('passed') is True and r['exit_code']==0 and not r.get('timed_out') for r in checks), 'current suite failed'
    product=load(qa/'product-check.json');receipt=load(product['product_receipt'])
    assert product['exit_code']==0 and receipt['passed'] is True and receipt['test_count']>0
    product.update(name='executor',passed=True,test_count=receipt['test_count'],observed_tests=receipt['observed_tests'])
    checks.append(binding(product))
    by={c['name']:c for c in checks}
    items=load('remediation-input/backlog.json')['items']
    mapping={**{f'B{i:02}':'config' for i in [1,2]},**{f'B{i:02}':'gateway' for i in [3,4]},'B05':'budget','B06':'service-browser','B07':'service-browser','B08':'budget-browser','B09':'mission-browser',**{f'B{i:02}':'service-browser' for i in [10,11,12,13]}}
    backlog=[dict(id=x['id'],title=x['title'],acceptance=x['acceptance'],passed=True,evidence=[by[mapping[x['id']]]['evidence']],classification='current worker; production components/Store with isolated transport, not installed acceptance') for x in items]
    fixtures=[]
    for i in range(1,11):
        names=[t for t in by['budget']['observed_tests'] if f'f{i:02}_' in t]
        assert names, 'missing fixture '+str(i)
        fixtures.append(dict(id=f'F{i:02}',passed=True,tests=names,evidence=by['budget']['evidence'],classification='actual isolated SQLite Store tests; not installed API',current_vs_target='F05 proactive manual-pause extension retained by execution contract' if i==5 else 'original retained Store cases'))
    exclusions={k:dict(status='excluded_by_user',passed=None,blocking=False,future_obligation=False) for k in ['screen_reader_specific','all_200_percent_zoom']}
    history=load(qa/'history/qa10-result.json')['historical_facts']
    history.update({'classification':'supplied historical observations, not this worker','1051':dict(event_id=b.MISSION+':1051',task_id='repair-independent-qa-user-scope-10',status='failed',protected_differences=dict(count=0)),'1053':dict(event_id=b.MISSION+':1053',classification='historical_host_observation_transcript',raw_output_available=False,not_current_worker=True,does_not_rewrite_1051=True)})
    report=dict(passed=True,status='pass',task_id=b.TASK,mission_id=b.MISSION,candidate_id=prepared['identity']['candidate_id'],summary='Current own-root preparation and independent product/browser coverage completed. Final original-command replay is a separate generated receipt; not host formal VERIFY.',backlog=backlog,fixture_matrix=fixtures,implemented_backlog_ids=[x['id'] for x in backlog],changed_files=['scripts/remediation/independent-qa-current-candidate.py','scripts/remediation/qa11-evidence-binding.py','scripts/remediation/qa11-binding-tests.py','scripts/remediation/qa11-stage.py','reports/qa/result.json','reports/qa/current-candidate-delivery.json'],checks=checks,blocking_findings=[],producer_task_ids=['repair-executor-mission-goal-environment-1','atomic-service-config','gateway-error-stream','budget-state-contract','service-ui','mission-ui'],excluded_by_user=exclusions,evidence_binding_version='qa11.v1',historical_host_checks=b.historical(),historical_facts=history,limitations=['Only current task pre-deploy readiness; not formal VERIFY, final review, deployment or installed Mission acceptance.','Native fixed-route helper and browser production components/fixture transport are distinct coverage layers; not installed scheduler acceptance.','QA8 capacity failure, QA9 controlled stop and prior failure facts preserved. Event1053 is a supplied transcript, not retained raw output or current worker execution.','104 baseline entries outside read scope not read; no foreign caches or CONTROL read.','Original task10 full 17-item text is not present in supplied task record; current explicit inherited constraints and retained QA matrix applied without reducing coverage.'],evidence_gap=dict(location='QA report serialization and validation omit source/output_sha256 and exact command validation; host raw collection boundary not independently inspected',repair='QA-only binding validator and positive/negative regressions; no host code or receipt fabrication'),next_steps=['Inspect final-required-checks.json in this own resource; any failure overrides readiness.','Host separately initiates formal VERIFY; later review/deployment/installed health remain separate.'])
    # An earlier staged read-only invocation is attached only after it really ran.
    if (qa/'staged-independent.json').exists():
        prior=load(qa/'staged-independent.json');assert prior['exit_code']==0
        report['checks'].append(binding(prior))
    write_json(Path('reports/qa/result.json'),report)
    delivery=dict(passed=True,status='pass',task_id=b.TASK,candidate_id=report['candidate_id'],report_sha256=digest('reports/qa/result.json'),summary=report['summary'],next_steps=report['next_steps'])
    write_json(Path('reports/qa/current-candidate-delivery.json'),delivery)
    assets={str(root / name): value for name,value in frozen_assets(root,out,prepared).items()}
    write_json(qa/'frozen.json',dict(task_id=b.TASK,mission_id=b.MISSION,resource_root=str(root),identity=prepared['identity'],report_sha256=digest('reports/qa/result.json'),delivery_sha256=digest('reports/qa/current-candidate-delivery.json'),checks=checks,artifacts=assets))
    print('QA candidate staged and frozen; not formal VERIFY')

def required():
    initial=digest('reports/qa/result.json');current=identity();records=[]
    commands=[['npm.cmd','--prefix','frontend','run','build'],['python','-c',b.SIMPLE],['python','scripts/remediation/independent-qa-current-candidate.py'],['python','-c',b.STRUCTURE]]
    for i,cmd in enumerate(commands):
        r,_=run(root,out,'qa11-final-required-'+str(i+1),cmd,180 if i==0 else 65)
        r=binding(r);records.append(r)
        assert digest('reports/qa/result.json')==initial, 'report changed during required checks'
        require_identity(current,identity(),'qa11_final')
    passed=all(r['exit_code']==0 and not r['timed_out'] for r in records)
    write_json(qa/'final-required-checks.json',dict(passed=passed,classification='current_worker_original_required_checks_not_host_VERIFY',checks=records,report_sha256=initial,candidate_id=current['candidate_id']))
    print(json.dumps(dict(passed=passed,exit_codes=[r['exit_code'] for r in records])))
    return 0 if passed else 1
if __name__=='__main__':
    if sys.argv[1:]==['--stage']:stage()
    elif sys.argv[1:]==['--required']:raise SystemExit(required())
    else:raise ValueError('expected --stage or --required')
