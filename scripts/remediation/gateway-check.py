"""Reproduce B03/B04 local gateway tests (no production service required)."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[2]
os.chdir(root)
out = root / 'reports/gateway'
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, RUST_MIN_STACK='16777216', HIMIND_AGENT_HOME=str(out / 'test-home'))
command = ['cargo', 'test', '--locked', '--bin', 'himind-agent', 'app::inference_gateway::tests::', '--', '--nocapture', '--test-threads=1']
if '--record-existing' in sys.argv:
    # Only use after the caller has collected the actual process exit code.
    code = int(sys.argv[sys.argv.index('--record-existing') + 1])
else:
    with (out / 'cargo-test-delivery.log').open('w', encoding='utf-8') as log:
        code = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
log = (out / 'cargo-test-delivery.log').read_text(encoding='utf-8')
checks = [{'command': ' '.join(command), 'exit_code': code, 'classification': 'real_loopback_and_unit', 'evidence': 'reports/gateway/cargo-test-delivery.log'}]
secrets = ['FAKE_BEARER_unpatterned_secret', 'FAKE_XAPI_unpatterned_secret']
events = out / 'test-home/logs/agent-events.jsonl'
leaks = [secret for secret in secrets if secret in events.read_text(encoding='utf-8')]
checks.append({'command': 'scan isolated gateway event log for fake credentials', 'exit_code': int(bool(leaks)), 'classification': 'real_log', 'evidence': str(events.relative_to(root)).replace('\\','/')})
passed = code == 0 and 'test result: ok.' in log and not leaks
files = ['src/app/inference_gateway.rs', 'scripts/remediation/gateway-check.py', 'scripts/remediation/gateway-patch.py', 'scripts/remediation/gateway-finalize.py', 'scripts/remediation/gateway-tests.rs']
hash_paths = ['remediation-input/execution-contract.md','remediation-input/backlog.json','reports/implementation-plan.md',*files,'reports/gateway/cargo-test-delivery.log']
result = {
 'passed': passed,
 'summary': 'B03/B04 local delivery candidate; public provider text withheld, safe request IDs, bounded reads, transport failure classification, incomplete usage and no body replay.',
 'implemented_backlog_ids': ['B03','B04'] if passed else [],
 'changed_files': files + ['reports/gateway/result.json','reports/gateway/summary.md'],
 'checks': checks,
 'coverage': {'real': ['401/403/500 oversized HTML echo: three native and four translation routes','pre-write truncated 200 returns 502','complete JSON and token ledger','timeout after 200 returns 502 with incomplete usage','body size limit returns distinct code','invalid chunk framing returns I/O code','five SSE routes interrupted before/after output; no synthetic completion; one upstream request','SSE provider error secret suppression','complete native SSE usage','isolated response and event-log fake key assertions'], 'contract': ['std::io::Error classifier reset branch'], 'not_covered': ['deterministic downstream TCP reset on every translated route','production providers/deployment and host formal VERIFY','all possible vendor SSE dialects']},
 'limitations': ['No production or host VERIFY executed; passed means ready for independent verification only.','Existing duplicate test attribute counts one test twice; 68 executions are not 68 unique tests.','Earlier failed compile/tests retained in cargo-test.log and cargo-test-2.log; not counted as passes.','Scratch patch scripts document implementation steps only; use gateway-check.py to reproduce validation, do not reapply patches.','Native/translated non-stream client-write failures are classified, but deterministic downstream reset integration coverage remains absent.'],
 'next_steps': ['Host performs independent formal VERIFY for B03/B04, then deployment only through controller.'],
 'sha256': {p: hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in hash_paths}
}
(out / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n',encoding='utf-8')
print(json.dumps({'passed':passed,'cargo_exit_code':code,'event_log_leaks':len(leaks)}))
sys.exit(0 if passed else 1)
