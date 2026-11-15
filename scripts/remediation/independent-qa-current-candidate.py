"""Fail-closed independent QA; default mode does not rewrite candidate/reports."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
sys.dont_write_bytecode = True
WORKSPACE = Path(__file__).resolve().parents[2]
ROOT = Path(os.environ['HIMIND_MISSION_CHECK_ROOT']).resolve(strict=True)
assert ROOT == Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
OUT = ROOT / 'independent-qa'

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def load(path):
    return json.loads(Path(path).read_text(encoding='utf-8-sig'))

def evidence(record):
    path = Path(record['evidence']).resolve(strict=True)
    assert path.is_relative_to(ROOT), 'foreign evidence prohibited'
    assert sha(path) == record['sha256'], 'evidence changed'
    assert type(record['exit_code']) is int and record['exit_code'] == 0
    assert record.get('command'), 'actual command missing'
    assert not record.get('timed_out') and not record.get('error')
    return path

MANDATORY = {'config','gateway','budget','mission','supervisor','handoff','usage',
             'executor','service-browser','mission-browser','legacy','frontend-build'}

def validate_report_binding(report, frozen):
    """Never consume a previous task's successful report as current evidence."""
    assert report.get('task_id') == frozen['task_id'], 'report task mismatch'
    assert report.get('candidate_id') == frozen['identity']['candidate_id'], 'report candidate mismatch'
    checks = report['checks']
    assert isinstance(checks, list) and checks, 'report checks missing'
    if report.get('evidence_binding_version') == 'qa11.v1':
        import importlib.util
        spec = importlib.util.spec_from_file_location('qa_binding', Path(__file__).with_name('qa11-evidence-binding.py'))
        binding = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(binding)
        binding.validate_history(report['historical_host_checks'])
        for check in checks:
            binding.validate_worker(check, ROOT, frozen['task_id'])
    else:
        assert frozen.get('task_id') != 'repair-independent-qa-user-scope-11', 'current provenance contract missing'
    for check in checks:
        evidence(check)
    for item in report['backlog']:
        assert item.get('evidence'), 'backlog evidence missing'
        for value in item['evidence']:
            path = Path(value).resolve(strict=True)
            assert path.is_relative_to(ROOT), 'foreign backlog evidence prohibited'
            assert any(Path(c['evidence']).resolve(strict=True) == path for c in checks), 'unbound backlog evidence'
    for item in report['fixture_matrix']:
        path = Path(item['evidence']).resolve(strict=True)
        assert path.is_relative_to(ROOT), 'foreign fixture evidence prohibited'
        assert any(Path(c['evidence']).resolve(strict=True) == path for c in checks), 'unbound fixture evidence'


def validate_frozen_owner(frozen, root=ROOT):
    """Bind ownership to host-assigned resource digests, not a previous QA task."""
    assert Path(frozen['resource_root']).resolve(strict=True) == root, 'frozen resource mismatch'
    for field, expected in [('task_id', root.name), ('mission_id', root.parent.name)]:
        value = frozen.get(field)
        assert isinstance(value, str) and value, 'frozen ' + field + ' missing'
        assert hashlib.sha256(value.encode('utf-8')).hexdigest() == expected, 'frozen ' + field + ' mismatch'


def validate():
    frozen = load(OUT / 'frozen.json')
    validate_frozen_owner(frozen)
    report_path = WORKSPACE / 'reports/qa/result.json'
    assert sha(report_path) == frozen['report_sha256'], 'report altered after freeze'
    report = load(report_path)
    validate_report_binding(report, frozen)
    delivery_path = WORKSPACE / 'reports/qa/current-candidate-delivery.json'
    assert sha(delivery_path) == frozen['delivery_sha256'], 'delivery altered after freeze'
    delivery = load(delivery_path)
    assert delivery.get('task_id') == frozen['task_id'], 'delivery task mismatch'
    assert delivery.get('candidate_id') == frozen['identity']['candidate_id'], 'delivery candidate mismatch'
    assert delivery.get('report_sha256') == frozen['report_sha256'], 'delivery report mismatch'
    assert delivery.get('passed') is True, 'delivery not ready'
    assert report['passed'] is True and report['status'] == 'pass'
    assert not report['blocking_findings']
    assert sorted(b['id'] for b in report['backlog']) == [f'B{i:02}' for i in range(1,14)]
    assert all(b['passed'] is True and b['evidence'] for b in report['backlog'])
    assert sorted(f['id'] for f in report['fixture_matrix']) == [f'F{i:02}' for i in range(1,11)]
    assert all(f['passed'] is True and f['tests'] for f in report['fixture_matrix'])
    assert set(report['excluded_by_user']) == {'screen_reader_specific','all_200_percent_zoom'}
    for value in report['excluded_by_user'].values():
        assert value == dict(status='excluded_by_user', passed=None, blocking=False, future_obligation=False)
    checks = frozen['checks']
    assert MANDATORY <= {c['name'] for c in checks}, 'missing mandatory cases'
    for check in checks:
        evidence(check)
        assert check.get('passed') is True
        if check['name'] in {'config','gateway','budget','mission','supervisor','handoff','usage','executor'}:
            assert check['test_count'] > 0 and check['observed_tests'], 'zero tests'
    from executor_lifecycle_support import identity,require_identity,require_artifact
    require_identity(frozen['identity'],identity(),'qa_candidate')
    assert frozen['artifacts'], 'missing binary/configuration artifacts'
    for path, expected in frozen['artifacts'].items():
        require_artifact(ROOT,path,expected,'qa_artifact')
    command = [sys.executable, 'scripts/remediation/executor-lifecycle-repair-checks.py']
    result = subprocess.run(command, cwd=WORKSPACE)
    assert result.returncode == 0, 'current product regressions failed'
    print(json.dumps(dict(passed=True, mandatory_cases=sorted(MANDATORY), classification='independent pre-deploy QA; not formal host VERIFY')))

if __name__ == '__main__':
    try:
        assert len(sys.argv) == 1, 'unexpected arguments'
        validate()
    except Exception as error:
        print(json.dumps(dict(passed=False, error=str(error))))
        sys.exit(1)
