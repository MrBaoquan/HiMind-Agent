"""Supplied host observations only; never substitute them for caller tests."""
import json
from pathlib import Path

TASK_ID = 'repair-executor-mission-goal-environment-1'
EXCLUSIONS = {
    name: {'status': 'excluded_by_user', 'passed': None,
           'blocking': False, 'future_obligation': False}
    for name in ['screen_reader_speech', 'all_200_percent_zoom']
}


def _earlier_host_observations():
    # The read-only native event response contains commands, hashes and times,
    # never raw stderr. This authorized report input is not a foreign cache.
    path = Path(__file__).resolve().parents[2] / 'reports/executor/user-scope-4/history/host-observations.json'
    if path.is_file():
        value = json.loads(path.read_text(encoding='utf-8'))
        assert value['source'] == 'mission.events.list'
        events = value['events']
        assert [event['sequence'] for event in events] == [909, 916, 917, 921]
        return [{
            'event_id': event['event_id'],
            'classification': 'read_only_native_historical_host_observation_not_this_worker_execution',
            'checks': event['payload']['checks'][0]['checks'],
            'numeric_tap_total': None, 'raw_output_available': False,
            'latest_supplied_host_observation': event['sequence'] == 921,
            'occurred_at': event['occurred_at'],
        } for event in events]
    records = []
    for event, elapsed in [(909, [16315, 17126, 16465]),
                           (917, [10987, 11643, 11541]),
                           (921, [8109, 8122, 7448])]:
        records.append({
            'event_id': f'mission_1794473598404_ffe1e069:{event}',
            'classification': 'supplied_historical_host_observation_not_this_worker_execution',
            'checks': [{'exit_code': code, 'elapsed_ms': duration, 'test_summary': ''}
                       for code, duration in zip([0, 1, 0], elapsed)],
            'numeric_tap_total': None, 'raw_output_available': False,
            'latest_supplied_host_observation': event == 921,
        })
    records.insert(1, {
        'event_id': 'mission_1794473598404_ffe1e069:916',
        'classification': 'historical_record_referenced_by_contract_details_not_supplied',
        'checks': None, 'numeric_tap_total': None, 'raw_output_available': False,
    })
    return records


def host_observations():
    # Supplied task input supersedes the earlier latest observation. Preserve
    # every earlier event without manufacturing its absent stdout/stderr.
    records = _earlier_host_observations()
    for record in records:
        record['latest_supplied_host_observation'] = False
    records.append({
        'event_id': 'mission_1794473598404_ffe1e069:933',
        'classification': 'supplied_historical_host_observation_not_this_worker_execution',
        'checks': [
            {'exit_code': 1, 'elapsed_ms': 25566, 'test_summary': '', 'executed': True},
            {'command': 'python scripts/remediation/executor-lifecycle-repair-checks.py',
             'exit_code': 0, 'elapsed_ms': 47474,
             'test_summary': '# tests 457\n# pass 457\n# fail 0', 'executed': True},
            {'exit_code': -1, 'elapsed_ms': 0, 'test_summary': '', 'executed': False,
             'error': 'Mission check time budget exhausted'},
        ],
        'numeric_tap_total': 457, 'raw_output_available': False,
        'latest_supplied_host_observation': True,
    })
    return records


PRIOR_FAILURES = [
    {'run_id': 'workflow_run_mission_mission_1794473598404_ffe1e069_repair-executor-lifecycle-user-scope-1',
     'code': 'upstream_service_unavailable', 'exit_code': 1, 'event': 'turn.failed',
     'paid_mixed_preparation_completed': False, 'product_root_cause_established': False},
    {'task_id': 'repair-executor-lifecycle-user-scope-2',
     'status': 'failed_or_canceled_as_supplied', 'preparation_completed': False},
    {'events': ['native918', 'native919'], 'os_error': 112,
     'cause': 'disk_full', 'classification': 'supplied_environment_failure_not_this_worker_execution'},
    {'task_id': 'repair-executor-lifecycle-user-scope-3',
     'status': 'unstarted_explicitly_canceled', 'resumed_by_this_worker': False},
    {'events':['native995','native996','native997'],'classification':'supplied_QA6_history_not_current_execution',
     'worker_final_exit_codes':[0,0,0,0],'native997_exit_codes':[0,0,1,-1],
     'check3_diagnostic_reconstruction_sha256':'f04684b5b912e218ddfc0949fa83396942e2d39fc505ae3394c1b4615b580eec',
     'check3_formal_environment_captured':False,
     'reconstruction':'retained deployment files/toolchain equal; only three inherited update-signing fields differed; narrows cause but does not capture formal check environment',
     'check4':'not executed: Mission check time budget exhausted',
     'himind_session_sequences':{'nested_goal_create':29,'turn1_complete':767,'goal_continuation':768,'turn2_start':769,'turn2_abort_dispose':771},
     'headless_exit1':'last turn was aborted/disposed; strict last-turn/final protocol retained'},
    {'events':['native1002','native1003','native1004'],'classification':'supplied_controlled_QA7_cancellation_history',
     'resumed_by_this_worker':False,'product_root_cause_established':False},
]
