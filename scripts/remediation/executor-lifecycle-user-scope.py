"""Current task report authoring and immutable replay; never host VERIFY.

--prepare records the current-source expensive producer in this caller's root.
--stage runs product checks, authors this Mission's report, then seals it.
--replay only reads the sealed candidate and writes generated caller evidence.
Independent QA uses --prepare and --replay with its own root after its own seal,
or the unchanged underlying product command; it must not author BIZ reports.
"""
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import sys
import uuid
sys.dont_write_bytecode = True
from executor_lifecycle_support import (WORKSPACE, resource, digest, write_json, run, identity, test_environment,
    initialize_test_build_signing, require_identity)
from executor_lifecycle_history import TASK_ID, EXCLUSIONS, PRIOR_FAILURES
from executor_delivery_contract import bind_product_receipt, current_readiness, frozen_assets

SIMPLE = "import json; p=json.load(open('reports/executor/result.json',encoding='utf-8')); assert p.get('passed') is True; assert p.get('checks')"
STRUCTURE = "import json; from pathlib import Path; p=json.loads(Path('reports/executor/result.json').read_text(encoding='utf-8')); assert all(k in p for k in ['passed','implemented_backlog_ids','changed_files','checks','limitations']); cs=p['checks']; assert isinstance(cs,list) and cs; assert all(isinstance(c,dict) and c.get('command') and type(c.get('exit_code')) is int and c.get('evidence') for c in cs); assert any(c['command']=='python scripts/remediation/executor-lifecycle-repair-checks.py' and c['exit_code']==0 for c in cs); fs=p['changed_files']; assert isinstance(fs,list) and fs; roots=['src/runtime','src/mission','scripts','reports/executor','src/workflow']; files=['src/app/mission_dispatch.rs','src/app/mission_supervisor.rs','src/app/mission_commands.rs','src/agent_core_contracts.rs','docs/mixed-executor-runtime.md','src/capability/service.rs']; assert all(isinstance(f,str) and '..' not in f.split('/') and (f in files or any(f.startswith(r+'/') for r in roots)) for f in fs)"
COMMANDS = [['python', '-c', SIMPLE], ['python', 'scripts/remediation/executor-lifecycle-repair-checks.py'], ['python', '-c', STRUCTURE]]
TEXT = ['python -c "' + SIMPLE + '"', 'python scripts/remediation/executor-lifecycle-repair-checks.py', 'python -c "' + STRUCTURE + '"']
DELIVERY = WORKSPACE / 'reports/executor/goal-environment-1'


def read(path):
    return json.loads(Path(path).read_text(encoding='utf-8'))


def check(root, out, index, label):
    invocation_id = uuid.uuid4().hex
    env = dict(test_environment(root, out, label), HIMIND_EXECUTOR_CHECK_INVOCATION=invocation_id)
    record, output = run(root, out, label, COMMANDS[index], 56 if index == 1 else 5, env=env)
    record.update(command=TEXT[index], classification='current_task_original_required_command_not_host_VERIFY')
    if index == 1:
        # A timeout may leave the previous checks.json intact. Only consume the
        # unique receipt emitted by this command with its fresh invocation ID.
        record['product_evidence_bound'] = False
        try:
            receipt, value = bind_product_receipt(root, output, invocation_id)
            record.update(product_receipt=str(receipt), product_receipt_sha256=digest(receipt),
                          product_evidence_bound=True, product_passed=value['passed'],
                          test_count=value['test_count'], unique_test_count=value['unique_test_count'],
                          candidate_id=value.get('candidate_id'), invocation_id=invocation_id)
        except (AssertionError, OSError, ValueError, KeyError) as error:
            record['receipt_failure'] = str(error)
    return record


def manifest():
    paths = set(identity()['files'])
    paths.add('docs/mixed-executor-runtime.md')
    paths.add('reports/qa/result.json')
    paths.update(p.relative_to(WORKSPACE).as_posix() for p in (WORKSPACE / 'reports/executor').rglob('*') if p.is_file())
    paths.update(p.relative_to(WORKSPACE).as_posix() for p in (WORKSPACE / 'reports/mission-ui/user-scope-1').rglob('*') if p.is_file())
    paths.update(p.relative_to(WORKSPACE).as_posix() for p in (WORKSPACE / 'reports/mission-ui/native-workspace-4').rglob('*') if p.is_file())
    paths.update(['reports/mission-ui/result.json', 'reports/mission-ui/paused-verification-3/dto-contract.json']
                 if (WORKSPACE / 'reports/mission-ui/paused-verification-3/dto-contract.json').is_file() else ['reports/mission-ui/result.json'])
    return {name: digest(WORKSPACE / name) for name in sorted(paths)}


def stage(root, out):
    assert not (out / 'frozen-candidate.json').exists(), 'candidate already sealed; stage cannot rewrite it'
    prior = read(DELIVERY / 'history/result.json')
    prepared = read(out / 'prepared.json')
    require_identity(prepared['identity'],identity(),'stage_candidate')
    assert prior['execution_binding']['executor'] == 'personal.codex'
    assert prepared.get('real_codex') and prepared.get('real_himind'), 'native chain missing'
    product = check(root, out, 1, 'stage-required-product')
    assert product.get('product_evidence_bound') is True, 'current product receipt not bound; report not authored'
    receipt = read(product['product_receipt'])
    preparation = read(out / 'task-prepare.json')
    upstream = [read(out / (name + '.json')) for name in ['upstream-task-center-model', 'upstream-task-view']]
    readiness = current_readiness(root, prepared, preparation, product, receipt, upstream)
    contract = read(out / 'delivery-contract-tests.json')
    if (contract.get('exit_code') != 0 or contract.get('timed_out') or
            contract.get('assertions_passed') is not True or contract.get('test_count', 0) <= 0):
        readiness['passed'] = False
        readiness['failures'].append('current delivery pipeline regression incomplete')
    write_json(out / 'stage-readiness.json', readiness)
    assert readiness['passed'], 'current stage incomplete; report not authored: ' + str(readiness['failures'])
    files = ['src/runtime/deepseek_harness.rs','scripts/remediation/executor_lifecycle_support.py',
             'scripts/remediation/executor-check.py','scripts/remediation/test_executor_build_environment.py',
             'scripts/remediation/independent-qa-current-candidate.py',
             'scripts/remediation/executor-lifecycle-repair-checks.py',
             'scripts/remediation/executor_lifecycle_history.py', 'scripts/remediation/executor-lifecycle-user-scope.py',
             'scripts/remediation/executor_delivery_contract.py',
             'docs/mixed-executor-runtime.md', 'reports/executor/result.json']
    files += [p.relative_to(WORKSPACE).as_posix() for p in DELIVERY.rglob('*') if p.is_file()]
    live = {key: read(prepared[key]['evidence']) for key in ['real_codex', 'real_himind']}
    report = {
        'schema_version': 'executor_user_scope_delivery.v1', 'passed': True,
        'task_id': TASK_ID, 'mission_id': prior['mission_id'],
        'execution_binding': {**prior['execution_binding'], 'role_id': 'remediation.executor'}, 'implemented_backlog_ids': prior['implemented_backlog_ids'],
        'summary': 'Mission 单步禁用嵌套 Goal/skill；准备和检查使用任务内确定 TEST 构建签名配置并绑定公钥内容。当前源码隔离构建、完整非零产品及跨进程环境回归和固定 HiMind→Codex→HiMind→同会话 Codex helper 已完成。仅当前阶段送验建议。',
        'changed_files': sorted(set(files)), 'checks': [product, preparation, contract, *upstream],
        'stage_readiness': readiness,
        'current_scope': {**EXCLUSIONS,
                          'ordinary_keyboard_focus_modal_background_status_alert_narrow_screens': 'retained; upstream evidence preserved; no new browser/speech/zoom claims'},
        'limitations': [
            'passed means staged current-task delivery readiness only. Final sealed replay is recorded separately in the same task root; any replay failure overrides this recommendation without mutating the sealed report.',
            'Latest supplied historical host observation933: report exit1/25566ms/empty summary; product exit0/47474ms/457 pass457 fail0; structure exit-1/0ms/not executed/Mission check time budget exhausted. These are host observations, never current worker checks. 909/916/917/921 retain empty summaries and unknown original stderr.',
            'This native current-source helper calls product runtime APIs with isolated helper Mission/artifact checks. It is host-delegated evidence, not complete installed Mission acceptance or independent QA.',
            'B01-B13 independent QA, final review, final installed build/deploy and installed health acceptance remain later host stages; final installation acceptance remains blocked until those stages run. No deployment or formal VERIFY was invoked.',
            'HiMind tokens are native runtime session projections, not upstream invoices. Unknown costs/billing owners remain unknown; Codex session totals and run increments are distinct.',
            'OS process death, injected helper failure, reconstructed objects and AgentRun full entry are separately classified by retained product tests; not all recovery is an OS restart.',
            'Native internally consumes fixed existing protected configuration with guards before loaders; normal native operational writes may use native operational directories. Generated evidence/builds/Cargo caches remain in this task root. No raw config/secrets/provider diagnostics exposed.',
            'Saved verification IDs alone are not process liveness. Frontend live activity still requires current mission/task/distinct run ownership; no frontend permissions or dispatch synthesized.',
            'Earlier task4 compact.exe scope incident is preserved in the archived report; it is not an operation of this worker.',
            'QA6/native995–997 and controlled QA7 cancellation1002–1004 are supplied historical observations. The deployment-environment reconstruction narrows signing drift, but is not a capture of the formal check environment. No historical result is rewritten or treated as this run.',
        ],
        'historical_host_observations': receipt['historical_host_observations'],
        'prior_incomplete_and_failed_runs': PRIOR_FAILURES,
        'history': {'archive': 'reports/executor/goal-environment-1/history/result.json', 'old_reports_unchanged': True,
                    'qa_original_sha256': digest(WORKSPACE / 'reports/qa/result.json')},
        'upstream_contract': {'path': 'reports/mission-ui/user-scope-1/dto-contract.json',
                              'sha256': digest(WORKSPACE / 'reports/mission-ui/user-scope-1/dto-contract.json'),
                              'latest_ui_report': 'reports/mission-ui/result.json',
                              'latest_ui_task_id': read(WORKSPACE / 'reports/mission-ui/result.json')['task_id'],
                              'latest_ui_report_sha256': digest(WORKSPACE / 'reports/mission-ui/result.json'),
                              'QA-LEGACY-01': 'producer correction tested; independent QA recheck remains host-owned',
                              'QA-LIVE-01': 'new current-task native helper executed; independent own-root repetition and installed acceptance not claimed'},
        'candidate': {'candidate_id': prepared['identity']['candidate_id'], 'identity_evidence': str(out / 'prepared.json'),
                      'identity_sha256': digest(out / 'prepared.json'), 'test_binary_sha256': prepared['binary_sha256'],
                      'production_binary_sha256': prepared['production_binary_sha256'], 'native_identity': prepared['native_identity'],
                      'runtime_profiles': {k: v for k, v in prepared['identity']['files'].items() if k.startswith('runtime-profiles/')},
                      'configuration': prepared['configuration'], 'toolchain': prepared['identity']['toolchain']},
        'preparation': {'record': preparation, 'checks': prepared['checks'], 'real_codex': prepared['real_codex'], 'real_himind': prepared['real_himind']},
        'product_regression': receipt, 'live_mixed_handoff': live,
        'diagnosis': {'initial_current_entry': read(out / 'initial-unprepared-entry.json'),
                      'historical909': 'Only supplied exit/time/empty summary known; exact stderr and candidate/preparation identity not supplied. Do not infer a product assertion failure from upstream service unavailability.',
                      'event933_report_exit1': 'Historical task4 report (preserved under user-scope-5/history/result.json) has passed=false and EXECUTOR-HISTORICAL-909-CAUSE blocker. The first command asserts passed is True. Missing old stderr was incorrectly treated as a current-delivery prerequisite; historical cause remains unknown and preserved. No933 product assertion failure is inferred.',
                      'QA6': PRIOR_FAILURES[-2],
                      'QA7': PRIOR_FAILURES[-1],
                      'build_signing':'Both preparation and read-only checker normalize inherited signing fields to an own-root deterministic TEST public key/config; effective config and public bytes are hashed. Other compiler/configuration/source/dependency/binary/evidence guards remain active. No production keys/configs are shell-read.',
                      'retained_repairs': 'Prior source includes channel-based cancellation fixture, Job-handle shutdown, strict plan/claim/budget admission; current product execution revalidates these rather than claiming them as new edits.',
                      'disk_preflight': read(out / 'disk-preflight.json')},
        'host_formal_verify_invoked': False, 'independent_qa_invoked': False, 'deployment_invoked': False,
        'resource_contract': {'root': str(root), 'other_task_cache_content_read': False, 'CONTROL_read': False,
                              'src_store_written': False, 'unexpected_directory_enumeration': False,
                              'incident_evidence': str(out / 'disk-preflight.json')},
        'frozen_readiness_replay': {'evidence': str(out / 'frozen-replay/result.json'),
                                    'classification': 'separate read-only replay after report authoring; consult actual outcome'},
        'next_steps': ['Run/inspect the sealed original-command replay in this task root.',
                       'Host starts formal VERIFY after readiness; independent QA repeats native protocol in its own fresh root.',
                       'Later final review/build/deployment/installed health acceptance remains host responsibility.'],
    }
    path = WORKSPACE / 'reports/executor/result.json'
    write_json(path, report)
    for index in [0, 2]:
        snapshot = out / f'stage-assertion-{index}-input.json'
        snapshot.write_bytes(path.read_bytes())
        record = check(root, out, index, f'stage-required-{index}')
        record.update(report_snapshot=str(snapshot), report_snapshot_sha256=digest(snapshot))
        report['checks'].append(record)
        report['passed'] &= record['exit_code'] == 0 and not record['timed_out']
        write_json(path, report)
    assert report['passed'], 'report structure checks failed; no seal created'
    write_json(out / 'frozen-candidate.json', {'files': manifest(), 'assets': frozen_assets(root, out, prepared), 'candidate_id': prepared['identity']['candidate_id'],
               'sealed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'report_sha256': digest(path)})
    print(json.dumps({'staged': True, 'candidate_id': prepared['identity']['candidate_id'], 'test_count': receipt['test_count'],
                      'unique_test_count': receipt['unique_test_count'], 'seal': str(out / 'frozen-candidate.json')}, ensure_ascii=False))
    return 0


def replay(root, out):
    sealed = read(out / 'frozen-candidate.json')
    target = out / 'frozen-replay'
    target.mkdir(exist_ok=True)
    records = []
    result = {'classification': 'current_task_read_only_original_required_commands_not_host_VERIFY', 'passed': False, 'checks': records}
    try:
        assert manifest() == sealed['files'], 'candidate changed before replay'
        prepared = read(out / 'prepared.json')
        assert frozen_assets(root, out, prepared) == sealed['assets'], 'prepared/live assets changed before replay'
        for index in range(3):
            records.append(check(root, out, index, f'frozen-required-{index}'))
            assert manifest() == sealed['files'], 'candidate changed during replay'
            assert frozen_assets(root, out, prepared) == sealed['assets'], 'prepared/live assets changed during replay'
        result['candidate_unchanged'] = manifest() == sealed['files']
        result['report_sha256'] = digest(WORKSPACE / 'reports/executor/result.json')
        result['passed'] = result['candidate_unchanged'] and len(records) == 3 and all(
            r['exit_code'] == 0 and not r['timed_out'] for r in records) and records[1].get('product_evidence_bound') is True and records[1].get('product_passed') is True and records[1]['test_count'] > 0
    except Exception as error:
        result['failure'] = str(error)
    write_json(target / 'result.json', result)
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result['passed'] else 1


def main():
    root, out = resource()
    if sys.argv[1:] == ['--prepare']:
        assert not (out / 'frozen-candidate.json').exists(), 'candidate already sealed; preparation cannot rewrite it'
        initialize_test_build_signing(out)
        # Every input needed by --stage is produced by this own-root protocol,
        # rather than requiring hand-authored files or another task's cache.
        write_json(out / 'disk-preflight.json', {'free_bytes': shutil.disk_usage(root).free,
                   'resource_root': str(root), 'compression_performed': False,
                   'policy': 'existing product cold/relink disk gates remain enforced'})
        if not (out / 'prepared.json').exists() and not (out / 'initial-unprepared-entry.json').exists():
            initial = check(root, out, 1, 'initial-unprepared-entry')
            initial['classification'] = 'own_initial_unprepared_diagnostic_not_required_frozen_check'
            write_json(out / 'initial-unprepared-entry.json', initial)
        contract, output = run(root, out, 'delivery-contract-tests',
                               ['python', 'scripts/remediation/test_executor_delivery_contract.py'], 30)
        counts = re.findall(r'^Ran (\d+) tests? in ', output, re.M)
        contract.update(test_count=int(counts[0]) if len(counts) == 1 else 0,
                        assertions_passed=len(counts) == 1 and int(counts[0]) > 0 and bool(re.search(r'^OK\s*$', output, re.M)),
                        classification='current_worker_report_pipeline_unit_regression_not_product_or_live_QA')
        write_json(out / 'delivery-contract-tests.json', contract)
        if contract['exit_code'] != 0 or contract['timed_out'] or not contract['assertions_passed']:
            print(json.dumps(contract, ensure_ascii=False))
            return 1
        for name, script in [('upstream-task-center-model', 'check-task-center-model.mts'),
                             ('upstream-task-view', 'check-task-view.mts')]:
            record, _ = run(root, out, name, ['node', '--experimental-strip-types', 'frontend/scripts/' + script], 30)
            record['classification'] = 'current_worker_retained_upstream_model_view_not_browser_or_QA'
            write_json(out / (name + '.json'), record)
            if record['exit_code'] != 0 or record['timed_out']:
                print(json.dumps(record, ensure_ascii=False))
                return 1
        env = dict(os.environ, HIMIND_EXECUTOR_LIVE_HIMIND='1')
        record, _ = run(root, out, 'task-prepare', ['python', 'scripts/remediation/executor-lifecycle-repair-checks.py', '--prepare'], 2400, env=env)
        record['classification'] = 'current_source_expensive_preparation_not_required_check_not_host_VERIFY'
        write_json(Path(record['evidence']).with_suffix('.json'), record)
        write_json(out / 'task-prepare.json', record)
        print(json.dumps(record, ensure_ascii=False))
        return 0 if record['exit_code'] == 0 and not record['timed_out'] else 1
    if sys.argv[1:] == ['--stage']:
        return stage(root, out)
    if sys.argv[1:] == ['--replay']:
        return replay(root, out)
    if sys.argv[1:] == ['--seal-read-only']:
        assert not (out / 'frozen-candidate.json').exists(), 'candidate already sealed; existing seal is immutable'
        prepared = read(out / 'prepared.json')
        require_identity(prepared['identity'],identity(),'seal_candidate')
        assert prepared['real_codex'] and prepared['real_himind']
        write_json(out / 'frozen-candidate.json', {'files': manifest(), 'assets': frozen_assets(root, out, prepared), 'candidate_id': prepared['identity']['candidate_id'],
                   'sealed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   'report_sha256': digest(WORKSPACE / 'reports/executor/result.json')})
        print('Caller candidate sealed without modifying source or reports; not host VERIFY.')
        return 0
    raise ValueError('expected --prepare, --stage (Mission writer only), --seal-read-only, or --replay')


if __name__ == '__main__':
    raise SystemExit(main())
