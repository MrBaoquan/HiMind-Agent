"""Report pipeline regressions; fixtures are never product/live/QA evidence."""
import copy
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
from executor_delivery_contract import DOMAINS, bind_product_receipt, current_readiness, frozen_assets
from executor_lifecycle_history import EXCLUSIONS, host_observations
from executor_lifecycle_support import digest, write_json

spec = importlib.util.spec_from_file_location('delivery_pipeline', Path(__file__).with_name('executor-lifecycle-user-scope.py'))
pipeline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pipeline)


class OwnRootFixture(unittest.TestCase):
    def setUp(self):
        resource = Path(os.environ['SERVICE_UI_RUNTIME_ROOT']).resolve(strict=True)
        # Retain generated test copies for review; never use default/foreign tmp.
        self.root = Path(tempfile.mkdtemp(prefix='delivery-contract-', dir=resource)).resolve()
        self.out = self.root / 'executor-lifecycle-repair'
        self.out.mkdir()
        self.prepared = {'identity': {'candidate_id': 'fixture-current'}, 'binary_sha256': 'fixture-binary'}
        for name in ['real_codex', 'real_himind']:
            path = self.out / (name + '.json')
            write_json(path, {'candidate_id': 'fixture-current', 'helper_executable_sha256': 'fixture-binary',
                             'classification': 'synthetic_report_pipeline_fixture'})
            self.prepared[name] = {'evidence': str(path), 'sha256': digest(path),
                                  'check': {'exit_code': 0, 'timed_out': False, 'passed': True, 'test_count': 1}}
        self.preparation = {'exit_code': 0, 'timed_out': False}
        self.product = {'exit_code': 0, 'timed_out': False, 'product_evidence_bound': True}
        self.receipt = {'schema_version': 'executor_lifecycle_repair_checks.v2', 'invocation_id': 'fixture-current-invocation',
                        'resource_root': str(self.root), 'candidate_id': 'fixture-current', 'passed': True, 'failures': [],
                        'test_count': 12, 'unique_test_count': 12, 'elapsed_ms': 10,
                        'historical_host_observations': host_observations(),
                        'domains': {name: {'test_count': 1, 'assertions_passed': True} for name in DOMAINS}}
        self.upstream = [{'name': name, 'exit_code': 0, 'timed_out': False}
                         for name in ['upstream-task-center-model', 'upstream-task-view']]

    def readiness(self):
        return current_readiness(self.root, self.prepared, self.preparation, self.product, self.receipt, self.upstream)

    def output(self, receipt=None):
        receipt = receipt or self.receipt
        path = self.out / 'checks-unique.json'
        write_json(path, receipt)
        summary = {key: receipt[key] for key in ['invocation_id', 'passed', 'test_count', 'elapsed_ms', 'failures']}
        summary['evidence'] = str(path)
        return path, json.dumps(summary)


class ReadinessTests(OwnRootFixture):
    def test_current_evidence_can_be_ready_without_future_verdicts_or_historical_stderr(self):
        self.prepared.update(historical909_stderr=None, host_VERIFY_passed=False, independent_qa_passed=False,
                             deployment_passed=False, historical_blockers=['EXECUTOR-HISTORICAL-909-CAUSE'])
        self.assertTrue(self.readiness()['passed'])

    def test_preparation_failure_still_blocks_despite_historical_457(self):
        self.preparation['exit_code'] = 1
        self.receipt['historical_host_observations'] = host_observations()
        self.assertFalse(self.readiness()['passed'])

    def test_preparation_timeout_blocks(self):
        self.preparation['timed_out'] = True
        self.assertFalse(self.readiness()['passed'])

    def test_current_product_failure_and_timeout_each_block(self):
        for field, value in [('exit_code', 1), ('timed_out', True), ('product_evidence_bound', False)]:
            with self.subTest(field=field):
                original = copy.deepcopy(self.product)
                self.product[field] = value
                self.assertFalse(self.readiness()['passed'])
                self.product = original

    def test_zero_or_invalid_test_inventory_blocks(self):
        for field, value in [('test_count', 0), ('unique_test_count', 0), ('test_count', True), ('unique_test_count', 13)]:
            with self.subTest(field=field, value=value):
                original = copy.deepcopy(self.receipt)
                self.receipt[field] = value
                self.assertFalse(self.readiness()['passed'])
                self.receipt = original

    def test_each_missing_or_unexecuted_required_domain_blocks(self):
        for domain in DOMAINS:
            for mode in ['missing', 'zero', 'failed']:
                with self.subTest(domain=domain, mode=mode):
                    original = copy.deepcopy(self.receipt)
                    if mode == 'missing':
                        del self.receipt['domains'][domain]
                    else:
                        self.receipt['domains'][domain]['test_count' if mode == 'zero' else 'assertions_passed'] = 0 if mode == 'zero' else False
                    self.assertFalse(self.readiness()['passed'])
                    self.receipt = original

    def test_mismatched_current_candidate_blocks(self):
        self.receipt['candidate_id'] = 'previous-candidate'
        self.assertFalse(self.readiness()['passed'])

    def test_each_missing_or_modified_live_receipt_blocks(self):
        for domain in ['real_codex', 'real_himind']:
            evidence = self.prepared[domain]
            del self.prepared[domain]
            self.assertFalse(self.readiness()['passed'])
            self.prepared[domain] = evidence
            path = Path(evidence['evidence'])
            prior = path.read_bytes()
            path.write_text('{}', encoding='utf-8')
            self.assertFalse(self.readiness()['passed'])
            path.write_bytes(prior)

    def test_missing_or_timed_out_upstream_checks_block(self):
        saved = copy.deepcopy(self.upstream)
        self.upstream.pop()
        self.assertFalse(self.readiness()['passed'])
        self.upstream = saved
        self.upstream[0]['timed_out'] = True
        self.assertFalse(self.readiness()['passed'])


class ReceiptBindingTests(OwnRootFixture):
    def test_unique_current_receipt_is_bound(self):
        path, output = self.output()
        bound, receipt = bind_product_receipt(self.root, output, self.receipt['invocation_id'])
        self.assertEqual(bound, path)
        self.assertEqual(receipt, self.receipt)

    def test_stale_shared_receipt_cannot_replace_missing_current_output(self):
        write_json(self.out / 'checks.json', self.receipt)
        with self.assertRaisesRegex(AssertionError, 'missing or ambiguous'):
            bind_product_receipt(self.root, '', 'another-invocation')

    def test_summary_from_another_invocation_is_rejected(self):
        _, output = self.output()
        with self.assertRaisesRegex(AssertionError, 'another invocation'):
            bind_product_receipt(self.root, output, 'another-invocation')

    def test_forged_current_summary_cannot_bind_stale_receipt(self):
        _, output = self.output()
        summary = json.loads(output)
        summary['invocation_id'] = 'new-invocation'
        with self.assertRaisesRegex(AssertionError, 'stale product receipt'):
            bind_product_receipt(self.root, json.dumps(summary), 'new-invocation')

    def test_duplicate_or_inconsistent_summaries_are_rejected(self):
        _, output = self.output()
        with self.assertRaisesRegex(AssertionError, 'ambiguous'):
            bind_product_receipt(self.root, output + '\n' + output, self.receipt['invocation_id'])
        summary = json.loads(output)
        summary['test_count'] += 1
        with self.assertRaisesRegex(AssertionError, 'summary/receipt mismatch'):
            bind_product_receipt(self.root, json.dumps(summary), self.receipt['invocation_id'])

    def test_foreign_receipt_reference_is_rejected_before_read(self):
        # Another fixture is still generated under our task resource; it models
        # an out-of-caller-root reference without reading any real foreign cache.
        foreign = Path(tempfile.mkdtemp(prefix='foreign-model-', dir=self.root.parent)) / 'receipt.json'
        write_json(foreign, self.receipt)
        _, output = self.output()
        summary = json.loads(output)
        summary['evidence'] = str(foreign)
        with self.assertRaisesRegex(AssertionError, 'outside own resource root'):
            bind_product_receipt(self.root, json.dumps(summary), self.receipt['invocation_id'])

    def test_killed_wrapper_keeps_actual_exit_and_does_not_attach_stale_receipt(self):
        write_json(self.out / 'checks.json', self.receipt)
        failed = {'exit_code': -1, 'timed_out': True, 'test_count': 0}
        with patch.object(pipeline, 'test_environment', return_value={}), patch.object(pipeline, 'run', return_value=(failed, '')):
            result = pipeline.check(self.root, self.out, 1, 'killed-invocation')
        self.assertEqual(result['exit_code'], -1)
        self.assertFalse(result['product_evidence_bound'])
        self.assertNotIn('product_receipt', result)


class FreezeTests(OwnRootFixture):
    def setUp(self):
        super().setUp()
        (self.root / 'executor').mkdir()
        for name in ['binary', 'production_binary']:
            path = self.out / (name + '.exe')
            path.write_bytes(b'synthetic fixture executable')
            self.prepared[name] = str(path)
        (self.root / 'executor/linkwrap.exe').write_bytes(b'fixture linker')
        (self.out / 'test-signing-key.pem').write_bytes(b'fixture only')
        self.artifact = self.out / 'artifact.json'
        self.artifact.write_bytes(b'fixture artifact')
        native = json.loads(Path(self.prepared['real_himind']['evidence']).read_text(encoding='utf-8'))
        native.update(artifact_root=str(self.out), artifacts={'artifact.json': digest(self.artifact)})
        write_json(Path(self.prepared['real_himind']['evidence']), native)
        write_json(self.out / 'prepared.json', self.prepared)
        self.source = self.out / 'candidate.rs'
        self.source.write_text('// fixture candidate', encoding='utf-8')
        self.manifest = lambda: {'fixture': digest(self.source)}
        write_json(self.out / 'frozen-candidate.json', {'files': self.manifest(),
                   'assets': frozen_assets(self.root, self.out, self.prepared)})

    def replay(self, checker):
        with patch.object(pipeline, 'manifest', side_effect=self.manifest), patch.object(pipeline, 'check', side_effect=checker):
            with contextlib.redirect_stdout(io.StringIO()):
                return pipeline.replay(self.root, self.out)

    def good_check(self, *args):
        return {'exit_code': 0, 'timed_out': False, 'test_count': 12,
                'product_evidence_bound': True, 'product_passed': True}

    def test_three_commands_complete_without_mutating_frozen_input(self):
        before = self.source.read_bytes()
        self.assertEqual(self.replay(self.good_check), 0)
        self.assertEqual(self.source.read_bytes(), before)
        result = json.loads((self.out / 'frozen-replay/result.json').read_text(encoding='utf-8'))
        self.assertEqual(len(result['checks']), 3)
        self.assertTrue(result['candidate_unchanged'])

    def test_live_artifact_mutation_before_replay_blocks_execution(self):
        self.artifact.write_bytes(b'changed')
        with patch.object(pipeline, 'check') as checker:
            with patch.object(pipeline, 'manifest', side_effect=self.manifest):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(pipeline.replay(self.root, self.out), 1)
            checker.assert_not_called()

    def test_source_mutation_during_replay_blocks_remaining_commands(self):
        calls = []
        def mutate(*args):
            calls.append(args)
            self.source.write_bytes(b'changed during check')
            return self.good_check()
        self.assertEqual(self.replay(mutate), 1)
        self.assertEqual(len(calls), 1)

    def test_zero_tests_or_unbound_or_failed_product_never_pass_replay(self):
        for field, value in [('test_count', 0), ('product_evidence_bound', False), ('product_passed', False), ('exit_code', -1)]:
            with self.subTest(field=field):
                def failed(*args):
                    result = self.good_check()
                    result[field] = value
                    return result
                self.assertEqual(self.replay(failed), 1)


class HistoryTests(unittest.TestCase):
    def test_latest_933_is_separate_from_earlier_empty_summaries(self):
        records = host_observations()
        self.assertEqual([r['event_id'].rsplit(':', 1)[1] for r in records], ['909', '916', '917', '921', '933'])
        self.assertTrue(all(c['test_summary'] == '' for r in records[:-1] for c in (r['checks'] or [])))
        self.assertEqual([r['latest_supplied_host_observation'] for r in records], [False] * 4 + [True])
        self.assertEqual([c['exit_code'] for c in records[-1]['checks']], [1, 0, -1])
        self.assertEqual([c['elapsed_ms'] for c in records[-1]['checks']], [25566, 47474, 0])
        self.assertFalse(records[-1]['checks'][2]['executed'])
        self.assertEqual(records[-1]['numeric_tap_total'], 457)

    def test_user_exclusions_are_not_deferred_or_passed(self):
        for item in EXCLUSIONS.values():
            self.assertEqual(item, {'status': 'excluded_by_user', 'passed': None,
                                    'blocking': False, 'future_obligation': False})


class StageTests(OwnRootFixture):
    def setUp(self):
        super().setUp()
        self.workspace = self.root / 'fixture-workspace'
        self.delivery = self.workspace / 'reports/executor/user-scope-5'
        (self.delivery / 'history').mkdir(parents=True)
        prior = {'mission_id': 'fixture-mission', 'execution_binding': {'executor': 'personal.codex'},
                 'implemented_backlog_ids': ['B05'], 'passed': False,
                 'blockers': [{'id': 'EXECUTOR-HISTORICAL-909-CAUSE'}]}
        write_json(self.delivery / 'history/result.json', prior)
        self.report_path = self.workspace / 'reports/executor/result.json'
        write_json(self.report_path, prior)
        for name in ['reports/qa/result.json', 'reports/mission-ui/result.json', 'reports/mission-ui/user-scope-1/dto-contract.json']:
            path = self.workspace / name
            path.parent.mkdir(parents=True, exist_ok=True)
            write_json(path, {'task_id': 'fixture-upstream', 'passed': False})
        self.prepared.update(production_binary_sha256='fixture-production', native_identity={}, configuration={},
                             checks=[], identity={'candidate_id': 'fixture-current', 'files': {}, 'toolchain': {}})
        for name, value in [('prepared', self.prepared), ('task-prepare', self.preparation),
                            ('initial-unprepared-entry', {'exit_code': 1}), ('disk-preflight', {'fixture': True}),
                            ('delivery-contract-tests', {'exit_code': 0, 'timed_out': False, 'test_count': 1, 'assertions_passed': True})]:
            if 'exit_code' in value:
                value.update(command='fixture-' + name, evidence=str(self.out / (name + '.log')))
            write_json(self.out / (name + '.json'), value)
        for item in self.upstream:
            item.update(command='fixture-' + item['name'], evidence=str(self.out / (item['name'] + '.log')))
            write_json(self.out / (item['name'] + '.json'), item)

    def checker(self, root, out, index, label):
        if index == 1:
            path, _ = self.output()
            return dict(self.product, command=pipeline.TEXT[index], test_count=self.receipt['test_count'],
                        product_receipt=str(path), evidence=str(path), elapsed_ms=1)
        # Run the exact two real Python report assertions against generated
        # fixture reports. This never executes a provider or signs a real pass.
        log = self.out / (label + '.log')
        with log.open('w', encoding='utf-8') as stream:
            process = subprocess.run(pipeline.COMMANDS[index], cwd=self.workspace,
                                     stdout=stream, stderr=subprocess.STDOUT, timeout=5)
        return {'command': pipeline.TEXT[index], 'exit_code': process.returncode,
                'timed_out': False, 'evidence': str(log), 'test_count': 0, 'elapsed_ms': 1}

    def stage(self):
        with patch.object(pipeline, 'WORKSPACE', self.workspace), patch.object(pipeline, 'DELIVERY', self.delivery), \
                patch.object(pipeline, 'identity', return_value=self.prepared['identity']), \
                patch.object(pipeline, 'check', side_effect=self.checker), \
                patch.object(pipeline, 'manifest', side_effect=lambda: {'report': digest(self.report_path)}), \
                patch.object(pipeline, 'frozen_assets', return_value={'fixture': 'assets'}), \
                contextlib.redirect_stdout(io.StringIO()):
            return pipeline.stage(self.root, self.out)

    def test_report_generation_then_seal_preserves_later_stage_responsibility(self):
        self.assertEqual(self.stage(), 0)
        report = json.loads(self.report_path.read_text(encoding='utf-8'))
        self.assertTrue(report['passed'])
        self.assertNotIn('blockers', report)
        self.assertFalse(report['host_formal_verify_invoked'])
        self.assertFalse(report['independent_qa_invoked'])
        self.assertFalse(report['deployment_invoked'])
        sealed = json.loads((self.out / 'frozen-candidate.json').read_text(encoding='utf-8'))
        self.assertEqual(sealed['report_sha256'], digest(self.report_path))
        self.assertEqual([r['exit_code'] for r in report['checks'][-2:]], [0, 0])
        self.assertIn('EXECUTOR-HISTORICAL-909-CAUSE', (self.delivery / 'history/result.json').read_text(encoding='utf-8'))

    def test_missing_current_domain_does_not_overwrite_report_or_create_seal(self):
        before = self.report_path.read_bytes()
        del self.receipt['domains']['budget_boundary']
        with self.assertRaisesRegex(AssertionError, 'current stage incomplete'):
            self.stage()
        self.assertEqual(self.report_path.read_bytes(), before)
        self.assertFalse((self.out / 'frozen-candidate.json').exists())

    def test_sealed_report_cannot_be_staged_again(self):
        write_json(self.out / 'frozen-candidate.json', {'fixture': 'existing seal'})
        before = self.report_path.read_bytes()
        with self.assertRaisesRegex(AssertionError, 'already sealed'):
            self.stage()
        self.assertEqual(self.report_path.read_bytes(), before)


if __name__ == '__main__':
    unittest.main(verbosity=2)
