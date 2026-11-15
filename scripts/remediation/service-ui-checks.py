import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parents[2]
COMMANDS = [
    'npm --prefix frontend ci',
    'node frontend/scripts/remediation/service-ui-browser.mjs',
    'npm --prefix frontend run build',
    '''python -c "import json; p=json.load(open('reports/service-ui/result.json',encoding='utf-8')); assert p.get('passed') is True; assert p.get('checks')"''',
    '''python -c "import json; p=json.load(open('reports/service-ui/result.json',encoding='utf-8')); assert all(k in p for k in ['passed','implemented_backlog_ids','changed_files','checks','limitations']); assert set(['B06','B07','B10','B11','B12','B13']).issubset(set(p['implemented_backlog_ids'])); cs=p['checks']; assert isinstance(cs,list) and cs; assert all(isinstance(c,dict) and c.get('command') and type(c.get('exit_code')) is int and c.get('evidence') for c in cs); expected=['npm --prefix frontend ci','node frontend/scripts/remediation/service-ui-browser.mjs','npm --prefix frontend run build']; assert all(any(c['command']==cmd and c['exit_code']==0 for c in cs) for cmd in expected); assert isinstance(p['changed_files'],list); roots=['frontend/src','frontend/scripts','reports/service-ui','scripts/remediation']; files=['frontend/styles.css','frontend/package.json','frontend/package-lock.json']; assert all(isinstance(f,str) and (f in files or any(f.startswith(r+'/') for r in roots)) and '..' not in f.split('/') for f in p['changed_files'])"''',
    'node frontend/scripts/remediation/service-ui-build-modes.mjs',
]
SCOPE = [
    'src', 'scripts', 'docs', 'remediation-input', 'reports', 'resources', 'icons',
    'Cargo.toml', 'Cargo.lock', 'build.rs', 'tauri.conf.json', 'frontend/src',
    'frontend/scripts', 'frontend/styles.css', 'frontend/package.json',
    'frontend/package-lock.json', 'frontend/index.html', 'frontend/vite.config.ts',
    'frontend/tsconfig.json',
]


def require_runtime_root():
    requested = os.environ.get('SERVICE_UI_RUNTIME_ROOT', '')
    if not requested or not Path(requested).is_absolute():
        raise RuntimeError('SERVICE_UI_RUNTIME_ROOT requires an existing host-provided, explicitly authorized absolute directory. No report-directory fallback.')
    runtime = Path(requested).resolve(strict=True)
    workspace = ROOT.resolve(strict=True)
    if runtime == workspace or runtime.is_relative_to(workspace) or workspace.is_relative_to(runtime):
        raise RuntimeError('Runtime root must be independent of the workspace.')
    if not runtime.is_dir():
        raise RuntimeError('Runtime root is not a directory.')
    return runtime


def snapshot():
    entries = {}
    for scope in SCOPE:
        base = ROOT / scope
        paths = [base]
        if base.is_dir():
            paths.extend(base.rglob('*'))
        for path in paths:
            label = path.relative_to(ROOT).as_posix()
            if path.is_symlink():
                raise RuntimeError(f'Symlink in local snapshot: {label}')
            if path.is_file():
                with path.open('rb') as source:
                    digest = hashlib.file_digest(source, 'sha256').hexdigest()
                entries[label] = dict(kind='file', sha256=digest)
            else:
                entries[label] = dict(kind='directory' if path.is_dir() else 'missing')
    return entries


def write_json(path, content):
    path.write_text(json.dumps(content, ensure_ascii=False, indent=2), encoding='utf-8')


def run_command(command, log, environment):
    started = datetime.now(timezone.utc).isoformat()
    process = None
    timed_out = False
    try:
        with log.open('wb') as output:
            process = subprocess.Popen(command, cwd=ROOT, shell=True, env=environment, stdout=output, stderr=subprocess.STDOUT)
            try:
                exit_code = process.wait(timeout=360)
            except subprocess.TimeoutExpired:
                timed_out = True
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/pid', str(process.pid), '/t', '/f'], stdout=output, stderr=subprocess.STDOUT, check=False)
                else:
                    process.kill()
                exit_code = process.wait(timeout=30)
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=30)
    return dict(command=command, exit_code=exit_code if exit_code != 0 or not timed_out else 1,
                evidence=str(log), output_sha256=hashlib.sha256(log.read_bytes()).hexdigest(),
                started_at=started, finished_at=datetime.now(timezone.utc).isoformat(),
                classification='current-worker-required-check', test_summary=None, timed_out=timed_out)


def main():
    host_root = require_runtime_root()
    run = Path(tempfile.mkdtemp(prefix='service-ui-checks-', dir=host_root))
    temporary = run / 'tmp'
    temporary.mkdir()
    environment = dict(os.environ, SERVICE_UI_RUNTIME_ROOT=str(host_root), TEMP=str(temporary), TMP=str(temporary), TMPDIR=str(temporary), npm_config_cache=str(run / 'npm-cache'), npm_config_update_notifier='false', PYTHONDONTWRITEBYTECODE='1')
    print(f'SERVICE_UI_CHECK_DIRECTORY={run}', flush=True)
    before = snapshot()
    write_json(run / 'snapshot-before.json', before)
    checks = []
    try:
        for index, command in enumerate(COMMANDS, start=1):
            check = run_command(command, run / f'check-{index}.log', environment)
            checks.append(check)
            write_json(run / 'checks.json', checks)
            print(json.dumps(check, ensure_ascii=False), flush=True)
    finally:
        after = snapshot()
        write_json(run / 'snapshot-after.json', after)
        drift = [label for label in sorted(before.keys() | after.keys()) if before.get(label) != after.get(label)]
        write_json(run / 'local-integrity.json', dict(passed=not drift, changed_entries=drift, scope=SCOPE, classification='local-task-scope-regression-not-host-candidate-integrity'))
    return 0 if not drift and all(check['exit_code'] == 0 for check in checks) else 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print(f'Service UI checks blocked/failed: {error}', file=sys.stderr)
        sys.exit(1)
