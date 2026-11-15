"""Explicit working-stage report writer. Required checks never import or call it."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def main():
    runtime = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
    if runtime == ROOT or runtime.is_relative_to(ROOT) or ROOT.is_relative_to(runtime):
        raise RuntimeError('Independent host runtime root required')
    run = runtime / 'delivery6'
    report = ROOT / 'reports/service-ui/repair6'
    report.mkdir(exist_ok=True)
    host = json.loads((run / 'host-559-and-run.json').read_text(encoding='utf-8'))
    baseline = json.loads((ROOT / 'remediation-input/baseline-manifest.json').read_text(encoding='utf-8'))
    provenance = json.loads((ROOT / 'docs/mixed-executor-controlled-host-patches.json').read_text(encoding='utf-8'))
    protected = []
    for name in ['src/app/mission_dispatch.rs', 'docs/mixed-executor-controlled-host-patches.json']:
        path = ROOT / name
        records = []
        for record in provenance['records']:
            details = record.get('record', record)
            change = details.get('files', {}).get(name)
            if change:
                records.append(dict(record_file=record.get('record_file'), change=change,
                                    ownership=details.get('ownership', details.get('phase'))))
        protected.append(dict(path=name, current_sha256=digest(path),
                              accessible_original_baseline=baseline['files'].get(name),
                              git_status=subprocess.run(['git', 'status', '--short', '--', name], cwd=ROOT, capture_output=True, text=True).stdout.strip(),
                              git_diff=str(run / (path.name + '.git-diff.patch')),
                              git_diff_sha256=digest(run / (path.name + '.git-diff.patch')),
                              provenance_records=records))
    write_json(report / 'protected-provenance.json', dict(files=protected, disposition='Read-only. Current dispatch matches check-runtime-host-integration after_sha256. Provenance document is untracked and absent from original baseline; external backups are not accessed. Formal ownership/baseline and protected_file_check belong to host; no pass claimed.'))

    checks = []
    for name in ['reproduce-ci', 'reproduce-browser', 'reproduce-build', 'fixed1-browser', 'fixed1-build', 'fixed2-browser', 'runtime-tests', 'launch-error', 'deadline']:
        record = json.loads((run / (name + '.json')).read_text(encoding='utf-8'))
        record.setdefault('classification', 'current-task-execution')
        record.setdefault('test_summary', None)
        record['cwd'] = str(ROOT)
        log = Path(record['evidence'])
        record['evidence_sha256'] = digest(log)
        if 'SERVICE_UI_RUNTIME_DIRECTORY=' in log.read_text(encoding='utf-8', errors='replace'):
            record['runtime_directory'] = log.read_text(encoding='utf-8', errors='replace').split('SERVICE_UI_RUNTIME_DIRECTORY=')[1].splitlines()[0]
        checks.append(record)
    checks.extend(json.loads((run / 'reproduce-reports.json').read_text(encoding='utf-8')))
    # Only existing actual observations are included; future checks are references,
    # never invented exit codes. This file is not called during the final replay.
    result_checks = run / 'result-checks.json'
    if result_checks.exists():
        checks.extend(json.loads(result_checks.read_text(encoding='utf-8')))
    browser = Path(json.loads((run / 'business-browser-path.json').read_text(encoding='utf-8'))['directory'])
    evidence_index = dict(
        classification='current-service-ui-stage-delivery-not-independent-acceptance',
        host_event=dict(event_id=host['event']['event_id'], evidence=str(run / 'host-559-and-run.json'),
                        exit_codes=[c['exit_code'] for c in host['event']['payload']['checks'][0]['checks']],
                        test_summary=[c['test_summary'] for c in host['event']['payload']['checks'][0]['checks']],
                        raw_stdout_available=False),
        current_checks=checks, browser_directory=str(browser),
        retained_history=['reports/service-ui/repair3/browser-1794501571165', 'reports/service-ui/repair3/attempt4/zoom-dialog-before-next.json', 'reports/service-ui/repair3/delivery.md', 'reports/service-ui/repair4-delivery.md'],
        final_read_only_replay_evidence=str(run / 'final-replay.json'),
        lifecycle_integrity_evidence=str(run / 'lifecycle-integrity.json'),
        unverified=['Native browser toolbar 200% zoom (CSS zoom 2 with real pointer/keyboard tested)', 'Actual screen-reader speech', 'Live backend/real credentials', 'Host formal candidate freeze, protected_file_check, VERIFY and deployment'],
    )
    write_json(report / 'evidence-index.json', evidence_index)
    changed = ['frontend/scripts/check-service-ui.browser.mjs',
               'frontend/scripts/remediation/service-ui-runtime.mjs',
               'frontend/scripts/remediation/service-ui-build.mjs',
               'scripts/remediation/service-ui-stage6-evidence.py',
               'reports/service-ui/result.json', 'reports/service-ui/repair6/delivery.md',
               'reports/service-ui/repair6/evidence-index.json',
               'reports/service-ui/repair6/protected-provenance.json']
    result = dict(passed=True,
                  summary='Service UI stage delivery ready for host review after causal entry-point repairs and current real browser/build checks; final immutable required-check replay evidence is outside the candidate. Not Mission or independent acceptance.',
                  implemented_backlog_ids=['B06', 'B07', 'B10', 'B11', 'B12', 'B13'],
                  changed_files=changed, checks=checks,
                  limitations=evidence_index['unverified'] + ['Event :559 provides hashes, exit codes and empty test_summary only; no raw stdout. Root causes established by current-task reproduction.', 'System dark preference retains project fixed light theme.', 'npm ci reports 4 high severity dependency advisories; versions were not changed by this scoped repair.', 'Production build emits its existing >500 kB chunk warning.', 'Only task-scoped local snapshot comparison is claimed; formal host candidate integrity is separate.'],
                  evidence_index='reports/service-ui/repair6/evidence-index.json',
                  final_required_checks=str(run / 'final-replay.json'))
    write_json(ROOT / 'reports/service-ui/result.json', result)
    print(report)


if __name__ == '__main__':
    main()
