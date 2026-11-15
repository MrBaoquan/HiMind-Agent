"""Task ownership guard regression only; not product acceptance."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
sys.dont_write_bytecode = True
path = Path(__file__).with_name('independent-qa-current-candidate.py')
compile(path.read_text(encoding='utf-8'), str(path), 'exec')
spec = importlib.util.spec_from_file_location('qa_entry', path)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
root = Path(os.environ['HIMIND_MISSION_CHECK_ROOT']).resolve(strict=True)
base = dict(resource_root=str(root), task_id='repair-independent-qa-user-scope-9', mission_id='mission_1794473598404_ffe1e069')
qa.validate_frozen_owner(base)
mutations = [('old_task', dict(task_id='repair-independent-qa-user-scope-6')),
             ('wrong_mission', dict(mission_id='other-mission')),
             ('missing_task', dict(task_id=None)),
             ('wrong_resource', dict(resource_root=str(root.parent)))]
for name, change in mutations:
    value = copy.deepcopy(base)
    value.update(change)
    try:
        qa.validate_frozen_owner(value)
    except AssertionError:
        print('rejected:', name)
    else:
        raise AssertionError('accepted ' + name)
print(json.dumps(dict(passed=True, positive_cases=1, rejected_cases=len(mutations), classification='ownership validator regression only')))
