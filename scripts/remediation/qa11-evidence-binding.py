"""QA-only provenance boundary. Historical host transcripts never count as worker checks."""
import hashlib
import json
import re
from pathlib import Path

TASK = 'repair-independent-qa-user-scope-11'
MISSION = 'mission_1794473598404_ffe1e069'
SIMPLE = "import json; p=json.load(open('reports/qa/result.json',encoding='utf-8')); assert p.get('passed') is True; assert len(p.get('backlog',[]))==13"
STRUCTURE = "import json; p=json.load(open('reports/qa/result.json',encoding='utf-8')); assert all(k in p for k in ['passed','status','backlog','implemented_backlog_ids','changed_files','checks','limitations','blocking_findings','producer_task_ids']); assert p['status']=='pass' and p['passed'] is True; assert not p['blocking_findings']; cs=p['checks']; assert isinstance(cs,list) and cs; assert all(isinstance(c,dict) and c.get('command') and type(c.get('exit_code')) is int and c.get('evidence') for c in cs); expected=['npm --prefix frontend run build','python scripts/remediation/independent-qa-current-candidate.py']; assert all(any(c['command']==cmd and c['exit_code']==0 for c in cs) for cmd in expected); fs=p['changed_files']; roots=['reports/qa','frontend/scripts/remediation','scripts/remediation']; assert isinstance(fs,list); assert all(isinstance(f,str) and '..' not in f.split('/') and any(f.startswith(r+'/') for r in roots) for f in fs)"
COMMANDS = ['npm --prefix frontend run build', 'python -c "'+SIMPLE+'"', 'python scripts/remediation/independent-qa-current-candidate.py', 'python -c "'+STRUCTURE+'"']
HASHES = ['7a7d7671fe5db173708917619fbfe76c7eb98160abf26dcb1981e93b54f98085','01ba4719c80b6fe911b091a7c05124b64eeece964e09c058ef8f9805daca546b','f54386b0d6ffa2a9ec0ffe865bc4d3dce6c0b1a2768a9def464ffb0f84ca9ca4','01ba4719c80b6fe911b091a7c05124b64eeece964e09c058ef8f9805daca546b']

def historical():
    return [dict(source='host', classification='historical_host_observation_transcript', event_id=MISSION+':1053', task_id='repair-independent-qa-user-scope-10', command=c, exit_code=0, output_sha256=h, raw_output_available=False, current_worker_execution=False, **({'tap':dict(tests=468,passed=468,failed=0)} if i==2 else {})) for i,(c,h) in enumerate(zip(COMMANDS,HASHES))]

def validate_history(records):
    assert records == historical(), 'historical host transcript binding mismatch'

def validate_worker(record, root, task_id=TASK):
    assert record.get('source') == 'worker', 'worker source mislabeled'
    assert record.get('task_id') == task_id, 'worker task mismatch'
    assert record.get('classification') == 'current_worker_execution', 'worker classification mismatch'
    assert isinstance(record.get('argv'),list) and record['argv'], 'argv missing'
    import subprocess
    executed = subprocess.list2cmdline(record['argv'])
    assert record.get('executed_command') == executed, 'argv command mismatch'
    expected = 'npm --prefix frontend run build' if record['argv'] == ['npm.cmd','--prefix','frontend','run','build'] else executed
    assert record.get('command') == expected, 'command mismatch'
    assert type(record.get('exit_code')) is int, 'exit code must be integer'
    path=Path(record['evidence']).resolve(strict=True)
    assert path.is_relative_to(root.resolve(strict=True)), 'foreign evidence'
    h=record.get('output_sha256')
    assert isinstance(h,str) and re.fullmatch('[a-f0-9]{64}',h), 'output hash missing'
    assert hashlib.sha256(path.read_bytes()).hexdigest()==h==record.get('sha256'), 'output hash mismatch'
    return path

def bind_worker(record, task_id=TASK):
    value = dict(record, source='worker', task_id=task_id, classification='current_worker_execution', executed_command=record['command'], output_sha256=record['sha256'])
    if record.get('argv') == ['npm.cmd','--prefix','frontend','run','build']:
        value['command'] = 'npm --prefix frontend run build'
    return value
