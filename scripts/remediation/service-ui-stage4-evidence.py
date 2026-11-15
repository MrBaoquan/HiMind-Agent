import hashlib
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parents[2]
REPORT = ROOT / 'reports/service-ui'
STAGE = REPORT / 'repair4'


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def relative(path):
    return path.relative_to(ROOT).as_posix()


def write_json(path, content):
    path.write_text(json.dumps(content, ensure_ascii=False, indent=2), encoding='utf-8')


def capture(command):
    started = datetime.now(timezone.utc).isoformat()
    completed = subprocess.run(command, cwd=ROOT, shell=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=dict(os.environ, PYTHONDONTWRITEBYTECODE='1'))
    return dict(command=command, exit_code=completed.returncode, started_at=started, finished_at=datetime.now(timezone.utc).isoformat(), classification='current-worker-execution', test_summary=None), completed.stdout


def main():
    if sys.argv[1:] != ['--work-stage']:
        raise RuntimeError('One-time work-stage evidence authoring requires --work-stage; never invoke from required checks.')
    if STAGE.exists():
        raise RuntimeError('Existing stage evidence is immutable; use a new explicitly authorized stage, not overwrite.')
    if os.environ.get('SERVICE_UI_RUNTIME_ROOT'):
        raise RuntimeError('This blocked-environment receipt is only for the currently missing host output authorization.')
    runner = runpy.run_path(str(ROOT / 'scripts/remediation/service-ui-checks.py'))
    commands = runner['COMMANDS']
    snapshot = runner['snapshot']
    STAGE.mkdir()
    old_result = (REPORT / 'result.json').read_bytes()
    (STAGE / 'result-before.json').write_bytes(old_result)
    baseline = json.loads((ROOT / 'remediation-input/baseline-manifest.json').read_text(encoding='utf-8'))
    provenance_path = ROOT / 'docs/mixed-executor-controlled-host-patches.json'
    provenance = json.loads(provenance_path.read_text(encoding='utf-8'))
    protected = ['docs/mixed-executor-controlled-host-patches.json', 'src/app/mission_dispatch.rs']
    protected_before = {name: digest(ROOT / name) for name in protected}
    head = subprocess.run(['git', '--no-optional-locks', 'rev-parse', 'HEAD'], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    audit = dict(classification='read-only-provenance-not-protected-file-check', git_head=head, baseline_manifest_sha256=digest(ROOT / 'remediation-input/baseline-manifest.json'), files={})
    for name in protected:
        git_base = subprocess.run(['git', '--no-optional-locks', 'show', f'HEAD:{name}'], cwd=ROOT, capture_output=True)
        git_diff = subprocess.run(['git', '--no-optional-locks', 'diff', '--', name], cwd=ROOT, capture_output=True)
        git_status = subprocess.run(['git', '--no-optional-locks', 'status', '--short', '--', name], cwd=ROOT, capture_output=True)
        stem = Path(name).name
        (STAGE / f'{stem}.git-diff.patch').write_bytes(git_diff.stdout)
        (STAGE / f'{stem}.git-diagnostics.log').write_bytes(git_status.stdout + git_base.stderr + git_diff.stderr)
        records = []
        for entry in provenance.get('records', []):
            record = entry.get('record', {})
            change = record.get('files', {}).get(name)
            if change:
                after = change.get('after_sha256', change.get('after'))
                records.append(dict(record_file=entry.get('record_file'), record_sha256=entry.get('record_sha256'), change=change, current_matches_after=after == protected_before[name], phase=record.get('phase', record.get('status_at_integration')), lease_id=record.get('lease_id')))
        audit['files'][name] = dict(current_sha256=protected_before[name], original_manifest_sha256=baseline['files'].get(name), git_head_sha256=hashlib.sha256(git_base.stdout).hexdigest() if git_base.returncode == 0 else None, git_head_available=git_base.returncode == 0, git_status=git_status.stdout.decode('utf-8', errors='replace').strip(), provenance_records=records, conclusion='Current source matches recorded host after hash; backups not independently accessed.' if any(record['current_matches_after'] for record in records) else 'Formal baseline/ownership cannot be established here; refer to host or owner.')
    audit['host_request'] = 'Host/owners must determine current protected baselines and unresolved provenance; no protected_file_check pass is claimed.'
    history = ROOT / 'reports/service-ui/repair3/browser-1794501571165'
    history_before = {relative(path): digest(path) for path in sorted(history.rglob('*')) if path.is_file()}
    extra_commands = [
        'node --test frontend/scripts/remediation/service-ui-runtime.test.mjs',
        'node --check frontend/scripts/check-service-ui.browser.mjs',
        'node --check frontend/scripts/remediation/service-ui-scenarios.mjs',
        'python scripts/remediation/service-ui-checks.py',
        'python scripts/remediation/service-ui-delivery3.py',
    ]
    attempts = []
    buffered = []
    for index, command in enumerate(extra_commands):
        check, output = capture(command)
        check['evidence'] = relative(STAGE / f'supplemental-{index + 1}.log')
        attempts.append(check)
        buffered.append((ROOT / check['evidence'], output))
    changed = [
        'frontend/package.json',
        'frontend/src/pages/ServiceUi.css',
        'frontend/scripts/check-service-ui.browser.mjs',
        'frontend/scripts/remediation/service-ui-browser.mjs',
        'frontend/scripts/remediation/service-ui-build.mjs',
        'frontend/scripts/remediation/service-ui-runtime.mjs',
        'frontend/scripts/remediation/service-ui-runtime.test.mjs',
        'frontend/scripts/remediation/service-ui-scenarios.mjs',
        'scripts/remediation/service-ui-delivery3.py',
        'scripts/remediation/service-ui-checks.py',
        'scripts/remediation/service-ui-stage4-evidence.py',
        'reports/service-ui/repair4-delivery.md',
        'reports/service-ui/result.json',
    ]
    limitations = [
        'No host-confirmed external runtime directory: approved_absolute_write_paths is empty and SERVICE_UI_RUNTIME_ROOT is absent. TEMP alone is not authorization.',
        'npm ci and npm run build were not executed: npm cache/log/temp authorization cannot safely be assumed. No worker success or TAP totals are borrowed from host history.',
        'Browser original command executes only the fail-closed gate; current production rendering, all business scenarios, success/startup-failure/timeout cleanup remain unverified.',
        'Five-check post-freeze candidate identity regression is blocked; safe-subset snapshots are local only, not formal host:candidate-integrity.',
        '320px summary fix and stronger real-browser assertions are implemented but have no current rendered regression.',
        'Protected source matches accessible host provenance, but original protected baselines and external backup records need host/owner adjudication. No protected_file_check pass.',
        'Native screen-reader speech, real backend, credentials, deployment and native toolbar zoom are not verified here.',
        'Build now outputs an external dist; deployment/build owner must consume that artifact without reverting check output into the candidate.',
    ]
    result = dict(passed=False, summary='Stage not ready: output isolation and narrow-summary fix implemented; missing authorized host output blocks complete required checks and browser regression. This is not Mission/independent acceptance.', implemented_backlog_ids=['B06','B07','B10','B11','B12','B13'], changed_files=changed, checks=[attempts[0]], limitations=limitations, historical_host_record=dict(source='mission_1794473598404_ffe1e069:543', provenance='user-provided-task-contract-not-independently-fetched', worker_execution=False, checks=[dict(command=command, exit_code=0, test_summary=None) for command in commands]), not_executed_required_checks=[dict(command=commands[index], status='blocked-not-executed', exit_code=None, reason='No host-confirmed external npm cache/log/temp output authorization.') for index in [0,2]], design_report='reports/service-ui/repair4-delivery.md', evidence_index='reports/service-ui/repair4/evidence-index.json')
    write_json(REPORT / 'result.json', result)
    before = snapshot()
    required = []
    for index in [1,3,4]:
        check, output = capture(commands[index])
        check['evidence'] = relative(STAGE / f'required-{index + 1}.log')
        check['classification'] = 'current-worker-required-check'
        required.append(check)
        buffered.append((ROOT / check['evidence'], output))
    after = snapshot()
    drift = [label for label in sorted(before.keys() | after.keys()) if before.get(label) != after.get(label)]
    audit['unchanged_during_current_checks'] = {name: protected_before[name] == digest(ROOT / name) for name in protected}
    history_after = {relative(path): digest(path) for path in sorted(history.rglob('*')) if path.is_file()}
    for path, output in buffered:
        path.write_bytes(output)
    result['checks'] = required + attempts
    result['check_attempts'] = [dict(check, expected_gate_failure=check['command'] in extra_commands[3:] or check['command'] == commands[1]) for check in result['checks']]
    write_json(STAGE / 'snapshot-before-safe-subset.json', before)
    write_json(STAGE / 'snapshot-after-safe-subset.json', after)
    write_json(STAGE / 'local-safe-subset-integrity.json', dict(classification='work-stage-safe-subset-only-not-host-candidate-integrity', unchanged=not drift, changed_entries=drift, scope=runner['SCOPE'], executed_commands=[check['command'] for check in required], full_five_check_post_freeze_regression=False, note='Captured in memory while commands ran; evidence files materialized afterward during explicit work-stage authoring, not by the checks.'))
    write_json(STAGE / 'protected-provenance.json', audit)
    write_json(STAGE / 'preserved-history.json', dict(directory=relative(history), before=history_before, after=history_after, unchanged=history_before == history_after, cleanup='No deletion or cleanup; host must retain referenced evidence.'))
    write_json(STAGE / 'checks.json', dict(executed=required + attempts, not_executed=result['not_executed_required_checks'], historical_host=result['historical_host_record']))
    write_json(STAGE / 'output-inventory.json', dict(authorized_absolute_write_paths=[], runtime_root_available=False, root_environment='SERVICE_UI_RUNTIME_ROOT', mappings=[
        dict(entry='service-ui-browser.mjs -> check-service-ui.browser.mjs', before='SERVICE_UI_EVIDENCE or reports/service-ui/repair3/browser-TIMESTAMP (direct fallback repair2-browser)', after='unique service-ui-browser-* directory under authorized root', artifacts=['screenshots','AX JSON','assertions','failure text','cleanup JSON']),
        dict(entry='Vite createServer', before='frontend/node_modules/.vite cache', after='runtime/vite-cache; configFile:false; virtual TSX stays in memory'),
        dict(entry='Playwright chromium.launch', before='ambient OS temp', after='runtime/tmp through TEMP/TMP/TMPDIR before dynamic import; existing browser channel executable unchanged'),
        dict(entry='npm run build', before='frontend/dist and potential bundled Vite config temp', after='runtime/dist; cache runtime/vite-cache; direct Node import of existing config with configFile:false'),
        dict(entry='npm ci / npm command logging', before='ambient npm cache/log and temp', after='host environment prepared by service-ui-checks.py: external npm_config_cache and TEMP/TMP/TMPDIR; frontend/node_modules remains normal dependency install; direct host commands need same authorized environment'),
        dict(entry='service-ui-delivery3.py', before='timestamped repair3 run logs, reports, result.json auto-update', after='read-only delivery files; delegates to external-output checks runner; all logs/checks/snapshots in host root'),
        dict(entry='service-ui-stage4-evidence.py --work-stage / existing service-ui-report.py', before='explicit work-stage report authoring', after='not invoked by any original check; new stage authoring is one-time and refuses overwrite'),
    ], retention='Runtime evidence is not automatically pruned; referenced directories are retained under host policy. Only own browser/Vite resources are closed.'))
    write_json(STAGE / 'business-review.json', dict(classification='source-review-plus-preserved-history-not-current-browser-verification', backlog_ids=result['implemented_backlog_ids'], current_browser_verified=False, source_evidence={
        'B06':['frontend/src/pages/AiProvidersPanel.tsx','frontend/src/pages/AcpProfilesPanel.tsx','frontend/src/pages/AiServiceRoutesPanel.tsx'],
        'B07':['frontend/src/pages/aiToolConnections.ts','frontend/src/pages/AiToolsPanel.tsx'],
        'B10':['frontend/src/components/ModalSurface.tsx','frontend/src/pages/AiProvidersPanel.tsx'],
        'B11':['frontend/src/pages/AiProvidersPanel.tsx'],
        'B12':['frontend/src/pages/AiConnectionsPage.tsx','frontend/src/pages/AcpProfilesPanel.tsx'],
        'B13':['frontend/src/pages/ServiceUi.css','frontend/scripts/remediation/service-ui-scenarios.mjs'],
    }, historical_ui006_before='reports/service-ui/repair3/attempt4/zoom-dialog-before-next.json', historical_ui006_after='reports/service-ui/repair3/required-20261112T153942934757Z/browser/zoom-dialog-before-next.json', current_fix='Local service-providers container below 480px stacks summary/action; new five-width and nested-container real geometry assertions; not rendered this stage.', retained_scenarios=['320','390','480','620','880','200% CSS zoom','long URL','50% copy expansion','five tabs and deep links','unique acp tabpanel','nested modal and presets','font tokens','contrast','degraded states','light/dark preference','reduced motion']))
    write_json(STAGE / 'host-event-links.json', dict(failed_run='local-run:workflow_run_mission_mission_1794473598404_ffe1e069_repair-service-ui-delivery-3', verification_run='local-run:workflow_run_mission_mission_1794473598404_ffe1e069_repair-service-ui-delivery-3_verify', events=[dict(id='mission_1794473598404_ffe1e069:537', raw_event_accessible=False, details='Not reconstructed or guessed.'),result['historical_host_record']], accessible_provenance='docs/mixed-executor-controlled-host-patches.json', failure='Accessible supervisor note: five original exits zero but 53 scoped report-file mutations rejected candidate-integrity; historical evidence retained.'))
    result['changed_files'] = sorted(set(changed + [relative(path) for path in STAGE.rglob('*') if path.is_file()] + ['reports/service-ui/repair4/evidence-index.json']))
    write_json(REPORT / 'result.json', result)
    files = [ROOT / name for name in result['changed_files'] if name != result['evidence_index']]
    write_json(STAGE / 'evidence-index.json', dict(stage_passed=False, stage='repair-service-ui-delivery-4', runtime_directory=None, reason='No host-authorized external runtime directory; no fallback used.', checks=required + attempts, blocked_checks=result['not_executed_required_checks'], files={relative(path): digest(path) for path in files}, retention='Do not auto-clean referenced historical or host-owned evidence; formal freeze/protected baseline/VERIFY belong to host.'))
    print(json.dumps(dict(passed=False, summary='Output isolation implemented; authorized runtime output and full checks/render regression remain blocked.', evidence=[relative(STAGE / 'evidence-index.json'),relative(REPORT / 'result.json'),relative(REPORT / 'repair4-delivery.md')]), ensure_ascii=True))


if __name__ == '__main__':
    main()
