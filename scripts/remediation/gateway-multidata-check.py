"""Run this repair's real checks, preserving raw output and actual return codes.
No historical check is promoted to a current execution. Run from the workspace.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[2]
os.chdir(root)
out = Path('reports/gateway')
env = dict(os.environ, RUST_MIN_STACK='16777216', HIMIND_AGENT_HOME=str(root / out / 'multidata-test-home'), RUST_TEST_THREADS='1')
commands = [
    ('cargo test remediation_stream_boundaries_real_loopback -- --nocapture', 'multidata-boundaries.log'),
    ('cargo test remediation_stream_multidata_error_no_leak -- --nocapture', 'multidata-errors.log'),
    ('cargo test --locked --bin himind-agent app::inference_gateway::tests:: -- --nocapture --test-threads=1', 'multidata-suite.log'),
]
checks = []
for command, logfile in commands:
    with (out / logfile).open('w', encoding='utf-8') as log:
        code = subprocess.run(command.split(), env=env, stdout=log, stderr=subprocess.STDOUT).returncode
        log.write(f'\nEXIT_CODE={code}\n')
    checks.append(dict(command=command, exit_code=code, evidence=str(out / logfile).replace('\\', '/'), classification='real_loopback_and_unit'))
    if code:
        break

passed = len(checks) == len(commands) and all(c['exit_code'] == 0 for c in checks)
# Cargo exit 0 must include the named tests, not merely zero-test executables.
for _, logfile in commands[:2]:
    text = (out / logfile).read_text(encoding='utf-8') if (out / logfile).exists() else ''
    passed = passed and '1 passed; 0 failed' in text

files = ['src/app/inference_gateway.rs', 'scripts/remediation/gateway-multidata-check.py', 'reports/gateway/result.json', 'reports/gateway/summary.md']
result = dict(
    passed=passed,
    summary='B03/B04 targeted repair: bounded complete SSE events are classified before forwarding; multiline JSON data is joined and normalized for native and translated paths; incomplete events fail closed with incomplete usage and no replay.',
    implemented_backlog_ids=['B03', 'B04'],
    changed_files=files,
    checks=checks,
    limitations=[
        'This is local worker delivery readiness, not Mission acceptance, host formal VERIFY or deployment.',
        'Host event mission_1794473598404_ffe1e069:455 historically recorded exit 0 but security verification failed; no TAP count was supplied. Those observations are not current worker test evidence.',
        'Real loopback upstreams use fabricated credentials, not production providers. Deterministic downstream TCP resets on every translation route remain untested.',
        'Only JSON SSE data and the protocol terminal [DONE] are accepted by provider relays; unclassifiable data fails closed. LF/CRLF supported; bare CR-only streams are outside this repair.',
        'The inherited workspace lease is runtime-managed; no host VERIFY or production service was invoked.',
    ],
    next_steps=['Submit this candidate and its raw logs to the controller for independent formal VERIFY; deploy only through the controller.'],
    sha256={p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in [
        'remediation-input/execution-contract.md', 'remediation-input/backlog.json', 'reports/implementation-plan.md',
        'src/app/inference_gateway.rs', 'scripts/remediation/gateway-multidata-check.py',
        *[c['evidence'] for c in checks],
    ]},
)
(out / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
validations = [
    "import json; p=json.load(open('reports/gateway/result.json',encoding='utf-8')); assert p.get('passed') is True; assert p.get('checks')",
    "import json; p=json.load(open('reports/gateway/result.json',encoding='utf-8')); assert all(k in p for k in ['passed','implemented_backlog_ids','changed_files','checks','limitations']); cs=p['checks']; assert isinstance(cs,list) and cs; assert all(isinstance(c,dict) and c.get('command') and type(c.get('exit_code')) is int and c.get('evidence') for c in cs); expected=['cargo test remediation_stream_boundaries_real_loopback -- --nocapture','cargo test remediation_stream_multidata_error_no_leak -- --nocapture']; assert all(any(c['command']==cmd and c['exit_code']==0 for c in cs) for cmd in expected)",
]
for index, expression in enumerate(validations, 1):
    logfile = out / f'multidata-result-check-{index}.log'
    with logfile.open('w', encoding='utf-8') as log:
        code = subprocess.run(['python', '-c', expression], env=env, stdout=log, stderr=subprocess.STDOUT).returncode
        log.write(f'EXIT_CODE={code}\n')
    checks.append(dict(command=f'python -c "{expression}"', exit_code=code, evidence=str(logfile).replace('\\', '/')))
    if code:
        result['passed'] = False
result['checks'] = checks
(out / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(json.dumps({'passed': result['passed'], 'checks': checks}, ensure_ascii=False))
sys.exit(0 if result['passed'] else 1)
