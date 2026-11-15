"""Task-local preparation and evidence; never rewrites delivery reports during checks."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import runpy
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[2]
INPUTS = ['src', 'scripts', 'styles.css', 'package.json', 'package-lock.json',
          'index.html', 'vite.config.ts', 'tsconfig.json']


def runtime():
    value = os.environ.get('SERVICE_UI_RUNTIME_ROOT', '')
    if not value or not Path(value).is_absolute():
        raise RuntimeError('Missing task host runtime root')
    path = Path(value).resolve(strict=True)
    if not path.is_dir() or path.is_relative_to(ROOT) or ROOT.is_relative_to(path):
        raise RuntimeError('Invalid task host runtime root')
    return path


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest(base):
    result = {}
    for name in INPUTS:
        entry = base / 'frontend' / name
        for path in sorted(entry.rglob('*')) if entry.is_dir() else [entry]:
            if path.is_symlink():
                raise RuntimeError(f'Symlink input: {path}')
            if path.is_file():
                result[path.relative_to(base).as_posix()] = digest(path)
    return result


def toolchain(base):
    entries = {}
    for package in ['typescript', '@typescript/native-preview-win32-x64', 'vite', '@vitejs/plugin-react', 'playwright', 'playwright-core']:
        root = base / 'frontend/node_modules' / package
        if not root.exists():
            entries[package] = None
            continue
        for path in sorted(root.rglob('*')):
            if path.is_file():
                entries[path.relative_to(base / 'frontend/node_modules').as_posix()] = digest(path)
    return entries


def copy_inputs(destination):
    for name in INPUTS:
        source = ROOT / 'frontend' / name
        target = destination / 'frontend' / name
        if source.is_dir():
            shutil.copytree(source, target, dirs_exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
    assert manifest(destination) == manifest(ROOT), 'Execution copy differs from candidate'


def run(command, cwd, directory, label):
    directory.mkdir(parents=True, exist_ok=True)
    temporary = directory / 't'
    temporary.mkdir(exist_ok=True)
    output = directory / 'o'
    output.mkdir(exist_ok=True)
    if command == 'node frontend/scripts/remediation/service-ui-build-modes.mjs':
        # This self-contained original entry already creates/installs its own
        # exact-source copy. Do not nest that copy another time on Windows.
        cwd = ROOT
        output = runtime()
    elif command != 'npm --prefix frontend ci':
        # Browser/build use the repaired original candidate entry itself. Only
        # dependency installation needs a generated source copy to avoid writes.
        cwd = ROOT
    source_record = directory / (label + '-source.json')
    save(source_record, manifest(cwd))
    env = dict(os.environ, TEMP=str(temporary), TMP=str(temporary), TMPDIR=str(temporary),
               SERVICE_UI_RUNTIME_ROOT=str(output),
               npm_config_cache=str(directory / 'npm-cache'), npm_config_update_notifier='false',
               PYTHONDONTWRITEBYTECODE='1')
    started = datetime.now(timezone.utc).isoformat()
    log = directory / (label + '.log')
    process = None
    timed_out = False
    error = None
    code = 1
    with log.open('wb') as stream:
        try:
            process = subprocess.Popen(command, cwd=cwd, shell=True, env=env,
                                       stdout=stream, stderr=subprocess.STDOUT)
            try:
                code = process.wait(timeout=360)
            except subprocess.TimeoutExpired:
                timed_out = True
                subprocess.run(['taskkill', '/pid', str(process.pid), '/t', '/f'],
                               stdout=stream, stderr=subprocess.STDOUT, check=False)
                code = process.wait(timeout=30) or 1
        except Exception as exception:
            error = repr(exception)
            stream.write(error.encode('utf-8'))
        finally:
            if process is not None and process.poll() is None:
                subprocess.run(['taskkill', '/pid', str(process.pid), '/t', '/f'],
                               stdout=stream, stderr=subprocess.STDOUT, check=False)
                process.wait(timeout=30)
    result = dict(command=command, cwd=str(cwd), exit_code=int(code), evidence=str(log),
                  sha256=digest(log), started_at=started,
                  finished_at=datetime.now(timezone.utc).isoformat(), timed_out=timed_out,
                  startup_error=error, test_summary=None, classification='current-task-execution',
                  source_manifest=str(source_record), source_manifest_sha256=digest(source_record))
    save(directory / (label + '.json'), result)
    print(json.dumps(result), flush=True)
    return result


def main():
    base = runtime() / 'q'
    base.mkdir(exist_ok=True)
    copy = base / 's'
    mode = sys.argv[1]
    if mode == 'prepare':
        save(base / 'source-before.json', manifest(ROOT))
        shutil.copyfile(ROOT / 'reports/service-ui/result.json', base / 'inherited-result.json')
        copy_inputs(copy)
        save(base / 'baseline-source.json', manifest(copy))
        result = run('npm --prefix frontend ci', copy, base, 'prepare-ci')
        return result['exit_code']
    if mode == 'sync':
        copy_inputs(copy)
        save(base / 'current-source.json', manifest(copy))
        return 0
    if mode == 'run':
        label, command = sys.argv[2:4]
        assert manifest(copy) == manifest(ROOT), 'Stale execution copy'
        return run(command, copy, base, label)['exit_code']
    if mode == 'freeze':
        copy_inputs(copy)
        report = copy / 'reports/service-ui/result.json'
        report.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / 'reports/service-ui/result.json', report)
        helpers = runpy.run_path(str(ROOT / 'scripts/remediation/service-ui-checks.py'))
        save(base / 'frozen-candidate.json', helpers['snapshot']())
        save(base / 'frozen-source.json', manifest(copy))
        save(base / 'frozen-result.json', dict(sha256=digest(report)))
        candidate_tools = toolchain(ROOT)
        copied_tools = toolchain(copy)
        assert candidate_tools == copied_tools, 'Candidate and installed execution-copy toolchains differ'
        save(base / 'frozen-toolchain.json', candidate_tools)
        return 0
    if mode == 'replay':
        helpers = runpy.run_path(str(ROOT / 'scripts/remediation/service-ui-checks.py'))
        before = json.loads((base / 'frozen-candidate.json').read_text(encoding='utf-8'))
        assert helpers['snapshot']() == before, 'Candidate changed after report/source freeze'
        frozen = json.loads((base / 'frozen-source.json').read_text(encoding='utf-8'))
        result_hash = json.loads((base / 'frozen-result.json').read_text())['sha256']
        frozen_tools = json.loads((base / 'frozen-toolchain.json').read_text())
        checks = []
        drift = []
        try:
            for index, command in enumerate(helpers['COMMANDS'], start=1):
                assert manifest(copy) == manifest(ROOT) == frozen, 'Source identity drift'
                assert digest(copy / 'reports/service-ui/result.json') == result_hash
                assert digest(ROOT / 'reports/service-ui/result.json') == result_hash
                assert toolchain(ROOT) == toolchain(copy) == frozen_tools, 'Toolchain identity drift'
                record = run(command, copy, base, f'final-{index}')
                checks.append(record)
                save(base / 'final-checks.json', checks)
                current = helpers['snapshot']()
                drift = [key for key in sorted(before.keys() | current.keys()) if before.get(key) != current.get(key)]
                save(base / f'integrity-after-{index}.json', dict(changed_entries=drift,
                     copied_source_matches=manifest(copy) == frozen,
                     toolchain_matches=toolchain(ROOT) == toolchain(copy) == frozen_tools,
                     copied_report_matches=digest(copy / 'reports/service-ui/result.json') == result_hash))
                assert not drift, 'Candidate changed during checks; no further checks permitted'
        finally:
            after = helpers['snapshot']()
            save(base / 'snapshot-after.json', after)
            drift = [key for key in sorted(before.keys() | after.keys()) if before.get(key) != after.get(key)]
            save(base / 'local-integrity.json', dict(changed_entries=drift, passed=not drift,
                 classification='local authorized-scope comparison, not host:candidate-integrity',
                 required_check_count=len(checks), execution_copy=str(copy),
                 copied_source_matches=manifest(copy) == frozen,
                 copied_report_matches=digest(copy / 'reports/service-ui/result.json') == result_hash))
        return 0 if len(checks) == 6 and not drift and all(c['exit_code'] == 0 for c in checks) else 1
    raise RuntimeError('Unknown operation')


if __name__ == '__main__':
    sys.exit(main())
