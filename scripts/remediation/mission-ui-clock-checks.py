"""Execute selected checks without rewriting the candidate; retain raw evidence."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

sys.stdout.reconfigure(encoding='utf-8', errors='replace')
workspace = Path(__file__).resolve().parents[2]
root = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
assert root.is_dir() and not root.is_relative_to(workspace) and not workspace.is_relative_to(root)
commands = [
    'npm --prefix frontend run check:task-view',
    'npm --prefix frontend run check:task-center-model',
    'npm --prefix frontend run build',
    'node frontend/scripts/remediation/mission-ui-clock-evidence-browser.mjs',
    'node frontend/scripts/remediation/service-ui-build-modes.mjs',
]
out = Path(tempfile.mkdtemp(prefix='checks-', dir=root))
records = []

def manifest():
    files = []
    for name in ['frontend/src', 'frontend/scripts', 'frontend/styles.css', 'frontend/package.json', 'frontend/package-lock.json', 'scripts/remediation']:
        path = workspace / name
        files.extend(path.rglob('*') if path.is_dir() else [path])
    return {p.relative_to(workspace).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(files) if p.is_file()}

before = manifest()
(out / 'source-before.json').write_text(json.dumps(before, indent=2), encoding='utf-8')
for index in [int(arg) for arg in sys.argv[1:]] or range(len(commands)):
    command = commands[index]
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    result = subprocess.run(['cmd', '/d', '/s', '/c', command], cwd=workspace, capture_output=True)
    output = result.stdout + result.stderr
    log = out / f'{index}.log'
    log.write_bytes(output)
    record = dict(command=command, exit_code=result.returncode, evidence=str(log),
                  output_sha256=hashlib.sha256(output).hexdigest(), started_at=started,
                  finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  classification='current worker execution; not host VERIFY')
    records.append(record)
    (out / 'commands.json').write_text(json.dumps(records, indent=2), encoding='utf-8')
    print(json.dumps(record), flush=True)
    print(output.decode('utf-8', errors='replace'), flush=True)
after = manifest()
(out / 'source-after.json').write_text(json.dumps(after, indent=2), encoding='utf-8')
changed = [name for name in set(before) | set(after) if before.get(name) != after.get(name)]
(out / 'candidate-integrity.json').write_text(json.dumps(dict(unchanged=not changed, changed_files=changed), indent=2), encoding='utf-8')
print('MISSION_UI_CHECKS=' + str(out), flush=True)
sys.exit(0 if not changed and all(c['exit_code'] == 0 for c in records) else 1)
