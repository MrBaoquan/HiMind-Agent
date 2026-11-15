"""QA-only negative boundary tests; not product/browser acceptance."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
sys.dont_write_bytecode = True
entry = Path(__file__).with_name('independent-qa-current-candidate.py')
compile(entry.read_text(encoding='utf-8'), str(entry), 'exec')
spec = importlib.util.spec_from_file_location('qa_entry', entry)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
root = Path(os.environ['HIMIND_MISSION_CHECK_ROOT']).resolve(strict=True)
path = root / 'independent-qa' / 'binding-fixture.log'
path.write_text('QA validator fixture only; not product evidence\n', encoding='utf-8')
check = dict(command='validator fixture', exit_code=0, evidence=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest())
frozen = dict(task_id='repair-independent-qa-user-scope-6', identity=dict(candidate_id='test-only'))
base = dict(task_id=frozen['task_id'], candidate_id='test-only', checks=[check], backlog=[dict(evidence=[str(path)])], fixture_matrix=[dict(evidence=str(path))])
qa.validate_report_binding(base, frozen)
mutations = [
    ('old_task', lambda p: p.update(task_id='repair-independent-qa-user-scope-4')),
    ('old_candidate', lambda p: p.update(candidate_id='old')),
    ('empty_checks', lambda p: p.update(checks=[])),
    ('hash_changed', lambda p: p['checks'][0].update(sha256='0'*64)),
    ('nonzero', lambda p: p['checks'][0].update(exit_code=1)),
    ('boolean_exit', lambda p: p['checks'][0].update(exit_code=False)),
    ('timed_out', lambda p: p['checks'][0].update(timed_out=True)),
    ('missing_backlog', lambda p: p['backlog'][0].update(evidence=[])),
]
for name, mutate in mutations:
    value = copy.deepcopy(base)
    mutate(value)
    try:
        qa.validate_report_binding(value, frozen)
    except (AssertionError, KeyError, FileNotFoundError):
        print('rejected:', name)
    else:
        raise AssertionError('accepted ' + name)
print(json.dumps(dict(passed=True, classification='QA validator unit boundaries only', rejected_cases=len(mutations))))
