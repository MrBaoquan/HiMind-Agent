"""Worker delivery recording; never invoked by required checks or host VERIFY.

Runs the unchanged three required commands and preserves the report snapshot
seen by each assertion. All raw logs/snapshots are current task resources.
"""
import json
from pathlib import Path
import sys
from executor_lifecycle_support import WORKSPACE, resource, run, digest, write_json


def main():
    assert not sys.argv[1:]
    root, out = resource()
    prior = json.loads((WORKSPACE/'reports/executor/prior-repair6-result.json').read_text(encoding='utf-8'))
    original = prior['checks'][:3]
    assert original[0]['command'] == 'python scripts/remediation/executor-lifecycle-repair-checks.py'
    commands = [['python', 'scripts/remediation/executor-lifecycle-repair-checks.py'], original[1]['argv'], original[2]['argv']]
    cache = json.loads((out/'prepared.json').read_text(encoding='utf-8'))
    record, _ = run(root, out, 'required-product', commands[0], 56)
    record.update(command=original[0]['command'], classification='runtime7_worker_exact_required_command_not_host_VERIFY')
    receipt_path = out/'checks.json'
    receipt = json.loads(receipt_path.read_text(encoding='utf-8'))
    record.update(test_count=receipt['test_count'], unique_test_count=receipt['unique_test_count'],
                  candidate_id=receipt.get('candidate_id'), product_receipt=str(receipt_path), product_receipt_sha256=digest(receipt_path))
    live = {}
    for key in ['real_codex','real_himind']:
        if cache.get(key):
            evidence = Path(cache[key]['evidence']).resolve(strict=True)
            assert evidence.is_relative_to(root), 'preparation evidence outside this task resource'
            live[key] = json.loads(evidence.read_text(encoding='utf-8'))
    ready = record['exit_code'] == 0 and receipt['passed'] and receipt['test_count'] > 0 and len(live) == 2
    history = prior.get('historical_host_observations', [])
    for observation in history:
        if observation['event_id'].endswith(':748'):
            observation['classification'] = 'historical_supplied_host_failure_not_worker_execution'
    history.insert(0, receipt['historical_host_observations'][0])
    report = {
        'schema_version':'executor_repair7_result.v1', 'passed':ready,
        'task_id':'repair-executor-lifecycle-runtime-7', 'mission_id':prior['mission_id'],
        'role':'remediation.executor', 'phase':'deliver',
        'summary':'Current implementation, independent verification budget boundary, candidate compilation and real mixed artifact handoff are prepared; current original checks determine readiness.',
        'next_steps':['Host separately starts formal VERIFY on this delivery candidate.',
                      'Retain B01-B13 independent QA, final review, final build/deployment and installed health gates.'],
        'implemented_backlog_ids':['B05','B08','BUDGET-AWAIT-01'],
        'implemented_backlog_scope':'Current executor lifecycle delivery and narrow Mission budget boundary; no full backlog or independent acceptance claim.',
        'changed_files':['src/app/mission_supervisor.rs','src/mission/store.rs','src/mission/budget_remediation_tests.rs','src/runtime/executor_live.rs',
                         'src/runtime/codex.rs','src/runtime/executor_live_handoff.rs',
                         'src/mission/runtime_resources.rs','src/mission/checks.rs','src/workflow/runtime.rs',
                         'scripts/remediation/executor-lifecycle-repair-checks.py','scripts/remediation/executor-lifecycle-deliver.py',
                         'reports/executor/prior-repair6-result.json','reports/executor/repair-plan.json','reports/executor/result.json',
                         'reports/executor/required-checks.json','reports/executor/repair7-delivery.json',
                         'reports/executor/runtime7-first-required-failure.json','docs/mixed-executor-runtime.md'],
        'checks':[record],
        'limitations':[
            'Event777 is the latest supplied host transcript:305 tests/305 pass/0 fail but entry exit1; second exit1 with empty summary; third budget exhausted exit-1 with empty summary. Its raw output is not available in the current root; no exact historical cause is invented.',
            'Feedback773 is historical supervisor diagnostic text; no inaccessible old raw cache was read. Runtime6 history and its failed readiness assertion are preserved in prior-repair6-result.json.',
            'Real mixed proof uses current-candidate native product API and isolated helper Mission/artifact checks; it is host-delegated evidence, not final installed Mission pipeline acceptance.',
            'Native internally consumes fixed protected configuration. Normal native locks/bootstrap/cache/accounting/log writes can use production operational directories; generated outputs use this task root.',
            'No raw configuration/credential/provider diagnostics were exposed to worker shell or models. Native config recovery/settings/runtime update/workbench sync are prohibited.',
            'Runtime-projected HiMind usage and loopback provenance are not raw upstream billing; unknown upstream cost/owner remain unknown.',
            'Live launch/resume reports actual exits/deadlines/cancel=false. Deterministic process death, helper injection, cancellation and rebuilt-object recovery remain distinct evidence classes.',
            'Business/model scope is policy, not an OS sandbox. No downstream formal VERIFY, QA, final deployment or installed acceptance was executed by this worker.'],
        'execution_binding':prior['execution_binding'], 'historical_host_observations':history,
        'prior_report':'reports/executor/prior-repair6-result.json',
        'earlier_runtime7_required_failure':'reports/executor/runtime7-first-required-failure.json',
        'host_formal_verify_invoked':False, 'independent_qa_invoked':False, 'deployment_invoked':False,
        'resource_contract':{'root':str(root),'external_CONTROL_read':False,'historical_external_cache_read':False,'src_store_written':False},
        'candidate':{'candidate_id':cache['identity']['candidate_id'],'identity_evidence':str(out/'prepared.json'),
                     'identity_evidence_sha256':digest(out/'prepared.json'),'source_file_count':len(cache['identity']['files']),
                     'runtime_profiles':{k:v for k,v in cache['identity']['files'].items() if k.startswith('runtime-profiles/')},
                     'binary':cache['binary'],'binary_sha256':cache['binary_sha256'],
                     'production_binary':cache['production_binary'],'production_binary_sha256':cache['production_binary_sha256'],
                     'native_identity':cache['native_identity'],'expensive_preparation_before_final_freeze':True},
        'preparation':{'classification':'runtime7_worker_preparation_not_host_VERIFY','evidence':str(out/'prepared.json'),
                       'sha256':digest(out/'prepared.json'),'checks':cache['checks'],'real_codex':cache['real_codex'],'real_himind':cache['real_himind']},
        'product_regression':receipt, 'live_mixed_handoff':live,
        'diagnosis':{'current_initial_unprepared_entry':json.loads((out/'initial-unprepared-entry.json').read_text()),
                     'red_before_fix':json.loads((out/'budget-red.json').read_text()),
                     'intermediate_fixture_failure':json.loads((out/'budget-early.json').read_text()),
                     'green_after_fix':json.loads((out/'budget-green.json').read_text()),
                     'intermediate_paid_preparation_rejected':json.loads((out/'preparation-identity-rejected.json').read_text(encoding='utf-8')),
                     'preparation_stopped_before_live_for_projection_fix':json.loads((out/'preparation-stopped-for-resume-projection.json').read_text(encoding='utf-8')),
                     'preparation_attempt_history':[{'evidence':str(p),'sha256':digest(p),
                         'record':json.loads(p.read_text(encoding='utf-8'))} for p in sorted(out.glob('prepare-attempt-*.json'))],
                     'prepaid_product_attempts':[{'evidence':str(p),'sha256':digest(p),
                         'passed':(v:=json.loads(p.read_text(encoding='utf-8')))['passed'],
                         'elapsed_ms':v['elapsed_ms'],
                         'test_count':sum(c['test_count'] for c in v['checks']),
                         'failed_groups':[c['name'] for c in v['checks'] if not c['passed']]}
                         for pattern in ['prepaid-product-*.json','free-product-*.json'] for p in sorted(out.glob(pattern))],
                     'historical_773_fixtures':'Current tests retain terminal immutability, actual foreign parent/FK fixtures, safe direct commands and Windows normalized working directories.',
                     'prior_readiness_blocker':'runtime7 requires current authorized evidence; no pending historical reread or future formal VERIFY blocks current readiness.',
                     'budget_policy':'Admitted implementation and its independent verification finish before budget waiting; new task/repair admission remains blocked. Atomic wait rechecks avoid stale scan overwriting current work or user/environment state.'}
    }
    path = WORKSPACE/'reports/executor/result.json'
    write_json(path, report)
    for index in [1,2]:
        snapshot = out/f'required-assertion-{index+1}-input.json'
        snapshot.write_bytes(path.read_bytes())
        checked, _ = run(root, out, f'required-assertion-{index+1}', commands[index], 5)
        checked.update(command=original[index]['command'], classification='runtime7_worker_exact_original_report_assertion',
                       report_snapshot=str(snapshot), report_snapshot_sha256=digest(snapshot))
        report['checks'].append(checked)
        if checked['exit_code'] != 0 or checked['timed_out']: report['passed'] = False
        write_json(path, report)
    report['summary'] = (f"当前候选已编译，{receipt['test_count']}次产品测试执行（{receipt['unique_test_count']}个唯一测试名），真实HiMind→Codex→HiMind→Codex交接及三个原必需检查通过；仅建议送验。" if report['passed'] else
                         'Current delivery is not ready: actual required checks or preparation are incomplete; see preserved evidence.')
    if not report['passed']:
        report['next_steps'] = ['Repair the current required-check failures and regenerate any changed-candidate preparation.',
                                'Rerun the three original commands; host formal VERIFY follows only after this stage is ready.']
    write_json(path, report)
    delivery = {'classification':'runtime7_worker_delivery_recommendation_not_Mission_acceptance','passed':report['passed'],
                'candidate_id':cache['identity']['candidate_id'],'checks':report['checks'],'test_count':receipt['test_count'],
                'unique_test_count':receipt['unique_test_count'],'domains':receipt['domains'],'summary':report['summary'],
                'next_steps':report['next_steps']}
    write_json(WORKSPACE/'reports/executor/required-checks.json',delivery)
    write_json(WORKSPACE/'reports/executor/repair7-delivery.json',delivery)
    print(json.dumps(delivery,ensure_ascii=False))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
