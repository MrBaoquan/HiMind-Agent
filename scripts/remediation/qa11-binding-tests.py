"""Provenance regressions only; not business acceptance."""
import copy
import importlib.util
import os
from pathlib import Path
import sys
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('binding',Path(__file__).with_name('qa11-evidence-binding.py'))
b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)
root=Path(os.environ['HIMIND_MISSION_CHECK_ROOT']).resolve(strict=True)
out=root/'independent-qa';out.mkdir(exist_ok=True)
p=out/'provenance-fixture.log';p.write_text('fixture only\n',encoding='utf-8')
import hashlib
base=b.bind_worker(dict(command='python fixture.py',argv=['python','fixture.py'],exit_code=0,evidence=str(p),sha256=hashlib.sha256(p.read_bytes()).hexdigest()))
b.validate_worker(base,root);b.validate_history(b.historical())
cases=[('missing_hash',{'output_sha256':None}),('wrong_hash',{'output_sha256':'0'*64}),('host_spoof',{'source':'host'}),('old_task',{'task_id':'repair-independent-qa-user-scope-10'}),('wrong_command',{'command':'python other.py'}),('wrong_argv',{'argv':['python','other.py']}),('bool_exit',{'exit_code':False}),('history_as_current',{'classification':'historical_host_observation_transcript'})]
for name,change in cases:
    item=copy.deepcopy(base);item.update(change)
    try:b.validate_worker(item,root)
    except (AssertionError,KeyError):print('rejected:',name)
    else:raise AssertionError('accepted '+name)
for name,change in [('host_hash',{'output_sha256':None}),('host_command',{'command':'wrong'}),('host_task',{'task_id':b.TASK}),('host_source',{'source':'worker'})]:
    value=b.historical();value[0].update(change)
    try:b.validate_history(value)
    except AssertionError:print('rejected:',name)
    else:raise AssertionError('accepted '+name)
print('2 positive and 12 negative provenance cases passed; fixture only')
