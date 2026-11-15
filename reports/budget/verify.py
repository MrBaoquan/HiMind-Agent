"""Reproduce scoped backend checks without production/model access."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'reports/budget'
env = dict(os.environ, RUST_MIN_STACK='16777216')
checks = []
for name, filt in [('mission-final', 'mission::'), ('handlers-final', 'app::mission')]:
    command = ['cargo', 'test', filt, '--', '--nocapture', '--test-threads=1']
    with (OUT / (name + '.log')).open('w', encoding='utf-8') as log:
        run = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    text = (OUT / (name + '.log')).read_text(encoding='utf-8')
    counts = re.findall(r'test result: ok\. (\d+) passed', text)
    checks.append({'command': ' '.join(command), 'exit_code': run.returncode, 'evidence': 'reports/budget/' + name + '.log', 'passed_tests': sum(map(int, counts)), 'classification': 'real_store_and_handler_unit'})
    if run.returncode:
        (OUT / 'final-checks.json').write_text(json.dumps(checks, indent=2), encoding='utf-8')
        raise SystemExit(run.returncode)
(OUT / 'final-checks.json').write_text(json.dumps(checks, indent=2), encoding='utf-8')
print(json.dumps(checks, indent=2))
