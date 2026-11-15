"""Native Windows workspace readiness replay. Freeze/prepare first; checks never author candidate files.

All generated artifacts stay in the host-assigned runtime. This is not host VERIFY.
"""
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
assert RUNTIME.is_dir() and not RUNTIME.is_relative_to(ROOT) and not ROOT.is_relative_to(RUNTIME)
OUT = RUNTIME / 'native-frozen-replay'
TEMP = RUNTIME / 'native-runner-temp'
TEMP.mkdir(exist_ok=True)
os.environ.update(TEMP=str(TEMP), TMP=str(TEMP), TMPDIR=str(TEMP), npm_config_cache=str(RUNTIME / 'npm-cache'), npm_config_update_notifier='false')
SOURCE = ['frontend/src', 'frontend/scripts', 'frontend/styles.css', 'frontend/package.json',
          'frontend/package-lock.json', 'frontend/index.html', 'frontend/vite.config.ts', 'frontend/tsconfig.json',
          'scripts/remediation', 'src', 'Cargo.toml', 'Cargo.lock', 'build.rs', 'tauri.conf.json']
PROTECTED = ['reports/mission-ui', 'reports/qa/result.json']
REPORT_CHECK_1 = "import json; p=json.load(open('reports/mission-ui/result.json',encoding='utf-8')); assert p.get('passed') is True"
REPORT_CHECK_2 = "import json; p=json.load(open('reports/mission-ui/result.json',encoding='utf-8')); assert all(k in p for k in ['passed','implemented_backlog_ids','changed_files','checks','limitations']); cs=p['checks']; assert isinstance(cs,list) and cs; assert all(isinstance(c,dict) and c.get('command') and type(c.get('exit_code')) is int and c.get('evidence') for c in cs); expected=['npm --prefix frontend run check:task-view','npm --prefix frontend run check:task-center-model','npm --prefix frontend run build','node frontend/scripts/remediation/mission-ui-clock-evidence-browser.mjs']; assert all(any(c['command']==cmd and c['exit_code']==0 for c in cs) for cmd in expected); assert set(['B08','B09']).issubset(set(p['implemented_backlog_ids'])); fs=p['changed_files']; assert isinstance(fs,list) and fs; roots=['frontend/src','frontend/scripts','reports/mission-ui','scripts/remediation']; files=['frontend/styles.css','frontend/package.json','frontend/package-lock.json']; assert all(isinstance(f,str) and '..' not in f.split('/') and (f in files or any(f.startswith(r+'/') for r in roots)) for f in fs)"
COMMANDS = [
    'npm --prefix frontend run check:task-view',
    'npm --prefix frontend run check:task-center-model',
    'npm --prefix frontend run build',
    'python -c "' + REPORT_CHECK_1 + '"',
    'node frontend/scripts/remediation/mission-ui-clock-evidence-browser.mjs',
    'python -c "' + REPORT_CHECK_2 + '"',
    'node frontend/scripts/remediation/mission-ui-retained-browser.mjs budget',
    'node frontend/scripts/remediation/service-ui-build-modes.mjs',
    'node frontend/scripts/remediation/mission-ui-retained-browser.mjs service',
]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def manifest(names):
    result = {}
    for name in names:
        entry = ROOT / name
        for path in sorted(entry.rglob('*')) if entry.is_dir() else [entry]:
            if path.is_file():
                result[path.relative_to(ROOT).as_posix()] = digest(path)
    return result


def toolchain():
    # Identify executable packages actually used by the unchanged commands.
    js = """const fs=require('fs'),crypto=require('crypto'),path=require('path');const result={node:process.version,executable:process.execPath,executable_sha256:crypto.createHash('sha256').update(fs.readFileSync(process.execPath)).digest('hex')};
for(const name of ['typescript','vite','@vitejs/plugin-react','playwright','playwright-core','react','react-dom']){const p=path.resolve('node_modules',name,'package.json');const data=JSON.parse(fs.readFileSync(p));const entry=name==='typescript'?path.resolve(path.dirname(p),data.bin.tsc):require.resolve(name); result[name]={version:data.version,metadata_sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex'),entry_sha256:crypto.createHash('sha256').update(fs.readFileSync(entry)).digest('hex')};}console.log(JSON.stringify(result));"""
    result = json.loads(subprocess.check_output(['node', '-e', js], cwd=ROOT / 'frontend'))
    result['runtime_policy'] = 'host-owned generated outputs only; not host VERIFY'
    result['rustc'] = subprocess.check_output(['rustc', '-Vv'], text=True)
    result['cargo'] = subprocess.check_output(['cargo', '-V'], text=True)
    result['python'] = sys.version
    result['python_executable_sha256'] = digest(Path(sys.executable))
    result['execution_environment'] = {key: os.environ.get(key) for key in ['SERVICE_UI_RUNTIME_ROOT', 'SERVICE_UI_BROWSER_CHANNEL', 'SERVICE_UI_TIMEOUT_MS', 'CARGO_NET_OFFLINE', 'NODE_OPTIONS']}
    result['npm_version'] = subprocess.check_output(['cmd', '/d', '/s', '/c', 'npm --version'], text=True).strip()
    modules = ROOT / 'frontend/node_modules'
    result['tool_package_files'] = {p.relative_to(modules).as_posix(): digest(p)
        for name in ['typescript', '@typescript', 'vite', '@vitejs', 'playwright', 'playwright-core', 'react', 'react-dom', 'rolldown', '@rolldown']
        for p in sorted((modules / name).rglob('*')) if p.is_file()}
    result['runtime_root'] = str(RUNTIME)
    result['probe_binary_sha256'] = digest(RUNTIME / 'probe/t/debug/mission-ui-store-probe.exe')
    result['probe_lock_sha256'] = digest(RUNTIME / 'probe/Cargo.lock')
    result['probe_inputs'] = json.loads((RUNTIME / 'probe/inputs.json').read_text())
    for name, value in result['probe_inputs'].items():
        assert digest(ROOT / name) == value, 'Prepared probe source identity drift: ' + name
    return result


def dist_manifest(path):
    return {p.relative_to(path).as_posix(): digest(p) for p in sorted(path.rglob('*')) if p.is_file()}


def execute(command):
    # Preserve the exact Python -c payload without cmd.exe's additional quoting
    # layer. COMMANDS still records the original required command verbatim.
    if command == COMMANDS[3]:
        argv = ['python', '-c', REPORT_CHECK_1]
    elif command == COMMANDS[5]:
        argv = ['python', '-c', REPORT_CHECK_2]
    else:
        argv = ['cmd', '/d', '/s', '/c', command]
    return subprocess.run(argv, cwd=ROOT, capture_output=True)


def prepare():
    out = RUNTIME / ('native-prepared-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S'))
    out.mkdir(exist_ok=False)
    before = manifest(SOURCE); tools = toolchain()
    save(out / 'source.json', before); save(out / 'toolchain.json', tools)
    records = []
    for index in [0, 1, 2, 4, 6, 7, 8]:
        command = COMMANDS[index]
        started = datetime.datetime.now(datetime.timezone.utc).isoformat()
        result = execute(command)
        log = out / f'{index}.log'; log.write_bytes(result.stdout + result.stderr)
        records.append(dict(command=command, exit_code=result.returncode, evidence=str(log), sha256=digest(log),
                            started_at=started, finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                            classification='actual current-task preparation execution; not host VERIFY'))
        save(out / 'commands.json', records); print(json.dumps(records[-1]), flush=True)
    save(out / 'invariants.json', dict(source_unchanged=manifest(SOURCE) == before, toolchain_unchanged=toolchain() == tools))
    assert manifest(SOURCE) == before and toolchain() == tools
    assert all(record['exit_code'] == 0 for record in records)


def freeze():
    OUT.mkdir(exist_ok=False)
    save(OUT / 'source.json', manifest(SOURCE))
    save(OUT / 'reports.json', manifest(PROTECTED))
    save(OUT / 'toolchain.json', toolchain())
    save(OUT / 'evidence.json', {str(p.relative_to(RUNTIME)): digest(p) for p in RUNTIME.rglob('*')
                               if p.is_file() and p.suffix in ['.json', '.jsonl', '.log', '.png', '.zip']
                               and not p.is_relative_to(OUT) and not p.is_relative_to(RUNTIME / 'cargo-home')
                               and not p.is_relative_to(RUNTIME / 'probe/t')})
    prepared = json.loads((ROOT / 'reports/mission-ui/result.json').read_text(encoding='utf-8'))
    save(OUT / 'prepared-dist.json', dist_manifest(Path(prepared['prepared_build_dist'])))
    save(OUT / 'freeze.json', dict(frozen_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                                  classification='task-local source/report/evidence freeze; not formal host VERIFY'))
    print('FROZEN=' + str(OUT), flush=True)


def checks():
    frozen_source = json.loads((OUT / 'source.json').read_text(encoding='utf-8'))
    frozen_reports = json.loads((OUT / 'reports.json').read_text(encoding='utf-8'))
    frozen_tools = json.loads((OUT / 'toolchain.json').read_text(encoding='utf-8'))
    frozen_evidence = json.loads((OUT / 'evidence.json').read_text(encoding='utf-8'))
    records = []
    for index, command in enumerate(COMMANDS):
        assert manifest(SOURCE) == frozen_source and manifest(PROTECTED) == frozen_reports, 'Candidate drift'
        assert toolchain() == frozen_tools, 'Toolchain/prepared binary drift'
        started = datetime.datetime.now(datetime.timezone.utc).isoformat()
        result = execute(command)
        log = OUT / f'{index}.log'; log.write_bytes(result.stdout + result.stderr)
        record = dict(command=command, exit_code=result.returncode, evidence=str(log), sha256=digest(log),
                      started_at=started, finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
        records.append(record); save(OUT / 'commands.json', records)
        print(json.dumps(record), flush=True)
        if command == 'npm --prefix frontend run build' and result.returncode == 0:
            output = (result.stdout + result.stderr).decode('utf-8', errors='replace')
            directory = next(line.split('=', 1)[1].strip() for line in output.splitlines() if line.startswith('SERVICE_UI_RUNTIME_DIRECTORY='))
            assert dist_manifest(Path(directory) / 'dist') == json.loads((OUT / 'prepared-dist.json').read_text()), 'Frozen production build differs from prepared output'
    drift = [name for name, value in frozen_evidence.items() if digest(RUNTIME / name) != value]
    invariant = dict(source_unchanged=manifest(SOURCE) == frozen_source,
                     reports_unchanged=manifest(PROTECTED) == frozen_reports,
                     toolchain_binary_inputs_unchanged=toolchain() == frozen_tools,
                     preserved_evidence_unchanged=not drift, changed_evidence=drift,
                     required_check_count=6, all_commands_passed=all(r['exit_code'] == 0 for r in records),
                     classification='current task readiness regression; not host formal VERIFY')
    invariant['passed'] = all(invariant[k] for k in ['source_unchanged', 'reports_unchanged', 'toolchain_binary_inputs_unchanged', 'preserved_evidence_unchanged', 'all_commands_passed'])
    save(OUT / 'invariants.json', invariant)
    print(json.dumps(invariant), flush=True)
    raise SystemExit(0 if invariant['passed'] else 1)


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    {'prepare': prepare, 'freeze': freeze, 'checks': checks}[sys.argv[1]]()
