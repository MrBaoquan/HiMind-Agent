"""Run real B01/B02 Rust tests in isolated temporary homes; no production service writes."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[2]
os.chdir(root)
out = Path('reports/config')
out.mkdir(parents=True, exist_ok=True)
changed = ['src/store/ai_services.rs', 'src/store/atomic_file.rs', 'src/app/ai_provider_import.rs', 'scripts/remediation/config-check.py', 'reports/config/result.json']
env = dict(os.environ, RUST_MIN_STACK='16777216')
checks = []
for label, selector, expected in [('ai-services', 'store::ai_services', 46), ('atomic-file', 'store::atomic_file', 3)]:
    command = ['cargo', 'test', '--bin', 'himind-agent', selector, '--', '--nocapture']
    result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    log = out / (label + '-final.log')
    log.write_bytes(result.stdout)
    text = result.stdout.decode('utf-8', errors='replace')
    passed = result.returncode == 0 and f'{expected} passed; 0 failed' in text
    checks.append({'command': subprocess.list2cmdline(command), 'exit_code': result.returncode, 'evidence': str(log), 'sha256': hashlib.sha256(result.stdout).hexdigest(), 'classification': 'real_isolated_rust_tests', 'passed': passed})
inputs = ['remediation-input/execution-contract.md', 'remediation-input/backlog.json', 'reports/implementation-plan.md']
report = {
    'passed': all(c['passed'] for c in checks),
    'summary': 'B01/B02 implemented: directory-scoped locked read/modify/write, synced atomic replacement and validated previous encrypted version recovery, idempotent route cleanup, read-only startup consistency diagnostics.',
    'implemented_backlog_ids': ['B01', 'B02'],
    'changed_files': changed,
    'checks': checks,
    'input_sha256': {p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in inputs},
    'artifact_sha256': {p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in changed if p != 'reports/config/result.json'},
    'coverage': {'real': ['24 concurrent updates of two different service IDs with a barrier', 'actual test child process exits 86 without unwind at before_temporary, temporary_created, temporary_written, temporary_flushed, before_replace, after_replace, directory_committed', 'restart parse and same-ID delete retry twice', 'dangling-reference diagnostic leaves selection bytes unchanged', 'deleting final selected service leaves default unset', 'corrupt primary restores validated encrypted previous snapshot; corrupt backup rejected; recovery version hash contains no credentials', 'existing all-capability route cleanup and legacy compatibility regressions'], 'contract_only': [], 'not_covered': ['physical power failure / hardware cache behavior', 'formal host VERIFY, deployment and frontend diagnostic rendering']},
    'reference_model': 'ADR0023 ordered capability route entries remain the only default/fallback fact source. Deletion preserves survivor order; existing first fallback becomes first by list semantics. No service is selected from the directory, and an empty route stays unset.',
    'limitations': ['Not a cross-file transactional database: directory-to-selection interruption is diagnosed and converges on explicit same-ID deletion retry.', 'Windows process interruption tested, not a physical power cut; Unix parent-directory fsync path is not executed here.', 'An early compilation log was accidentally opened at reports/config-tests-initial.log outside the task write subtree; relocation while open failed, then the completed log was moved to reports/config/initial-build.log. No input baseline or production configuration was changed.', 'Existing compiler warnings retained; no unrelated cleanup.', 'This passed flag is delivery readiness only, not Mission approval or independent verification.'],
    'next_steps': ['Host runs required result JSON check and separately schedules formal VERIFY.', 'Independent verifier replays python scripts/remediation/config-check.py; UI owner may present consistency_diagnostics without changing default selection.']
}
(out / 'result.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(json.dumps({'passed': report['passed'], 'checks': checks}, ensure_ascii=False))
raise SystemExit(0 if report['passed'] else 1)
