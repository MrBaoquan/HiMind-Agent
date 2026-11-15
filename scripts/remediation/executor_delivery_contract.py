"""Current worker delivery gates, separate from historical/downstream verdicts.

These checks consume actual own-root preparation and product receipts. Historical
TAP totals, absent old stderr, and later host VERIFY/QA cannot satisfy a gate or
create an extra prerequisite. This module never signs those later verdicts.
"""
import json
from pathlib import Path

from executor_lifecycle_support import digest


DOMAINS = {
    'lineage', 'ownership_windows', 'usage', 'compatibility_and_input',
    'receipt_delivery', 'prestart_and_diagnostics', 'native_guard',
    'budget_boundary', 'recovery_projection', 'resource_boundary',
    'real_codex', 'real_himind', 'mission_methods', 'build_environment', 'build_signing_product',
}


def own_file(root, name):
    path = Path(name).resolve(strict=True)
    assert path.is_relative_to(root.resolve(strict=True)), 'delivery evidence outside own resource root'
    assert path.is_file(), 'delivery evidence is not a file'
    return path


def bind_product_receipt(root, output, invocation_id):
    summaries = []
    for line in output.splitlines():
        try:
            value = json.loads(line)
        except (ValueError, TypeError):
            continue
        if isinstance(value, dict) and 'invocation_id' in value and 'failures' in value:
            summaries.append(value)
    assert len(summaries) == 1, 'current invocation product receipt missing or ambiguous'
    summary = summaries[0]
    assert summary['invocation_id'] == invocation_id, 'product summary belongs to another invocation'
    path = own_file(root, summary['evidence'])
    receipt = json.loads(path.read_text(encoding='utf-8'))
    assert receipt['schema_version'] == 'executor_lifecycle_repair_checks.v2'
    assert receipt['invocation_id'] == invocation_id, 'stale product receipt'
    assert Path(receipt['resource_root']).resolve(strict=True) == root.resolve(strict=True)
    for key in ['passed', 'test_count', 'elapsed_ms', 'failures']:
        assert receipt[key] == summary[key], 'product summary/receipt mismatch: ' + key
    return path, receipt


def current_readiness(root, prepared, preparation, product, receipt, upstream):
    """Fail closed for current work; historical and later verdicts are not inputs."""
    failures = []
    if preparation.get('exit_code') != 0 or preparation.get('timed_out'):
        failures.append('current preparation did not complete')
    candidate_id = prepared.get('identity', {}).get('candidate_id')
    if not candidate_id or receipt.get('candidate_id') != candidate_id:
        failures.append('current candidate identity mismatch')
    if (product.get('exit_code') != 0 or product.get('timed_out') or
            product.get('product_evidence_bound') is not True or receipt.get('passed') is not True or
            receipt.get('failures') != []):
        failures.append('current product invocation did not pass with bound evidence')
    if (type(receipt.get('test_count')) is not int or receipt['test_count'] <= 0 or
            type(receipt.get('unique_test_count')) is not int or
            not 0 < receipt['unique_test_count'] <= receipt['test_count']):
        failures.append('current product test inventory is empty or invalid')
    domains = receipt.get('domains', {})
    if not DOMAINS.issubset(domains):
        failures.append('required current product domain missing')
    for name in sorted(DOMAINS):
        domain = domains.get(name, {})
        if (type(domain.get('test_count')) is not int or domain['test_count'] <= 0 or
                domain.get('assertions_passed') is not True):
            failures.append(name + ': nonzero actual assertions required')
    for name in ['real_codex', 'real_himind']:
        evidence = prepared.get(name)
        if not evidence:
            failures.append(name + ': own current live evidence missing')
            continue
        try:
            path = own_file(root, evidence['evidence'])
            assert digest(path) == evidence['sha256'], 'live evidence changed'
            value = json.loads(path.read_text(encoding='utf-8'))
            assert value['candidate_id'] == candidate_id, 'live candidate mismatch'
            assert value['helper_executable_sha256'] == prepared['binary_sha256'], 'live binary mismatch'
            assert evidence['check']['exit_code'] == 0 and evidence['check']['passed'] is True
            assert evidence['check']['test_count'] > 0 and not evidence['check']['timed_out']
        except (AssertionError, OSError, ValueError, KeyError) as error:
            failures.append(name + ': ' + str(error))
    expected_upstream = {'upstream-task-center-model', 'upstream-task-view'}
    if {r.get('name') for r in upstream} != expected_upstream or any(
            r.get('exit_code') != 0 or r.get('timed_out') for r in upstream):
        failures.append('retained upstream model/view checks incomplete')
    return {'passed': not failures, 'failures': failures,
            'classification': 'current_worker_delivery_only_not_host_VERIFY_or_independent_QA'}


def frozen_assets(root, out, prepared):
    names = [out / 'prepared.json', prepared['binary'], prepared['production_binary'],
             root / 'executor/linkwrap.exe', out / 'test-signing-key.pem']
    if 'build_signing' in prepared.get('identity',{}):
        names += [out/'build-signing/test-build-signing.json',out/'build-signing/test-public.pem']
    for name in ['real_codex', 'real_himind']:
        names.append(prepared[name]['evidence'])
    native = json.loads(own_file(root, prepared['real_himind']['evidence']).read_text(encoding='utf-8'))
    artifact_root = Path(native['artifact_root']).resolve(strict=True)
    assert artifact_root.is_relative_to(root.resolve(strict=True))
    names += [artifact_root / name for name in native['artifacts']]
    return {str(path.relative_to(root)): digest(path)
            for path in sorted({own_file(root, name) for name in names})}
