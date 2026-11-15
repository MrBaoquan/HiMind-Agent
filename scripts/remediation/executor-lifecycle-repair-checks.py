"""Bounded current-product regressions. Worker preparation: --prepare.

The three required host commands stay unchanged. Preparation performs expensive
builds/live execution; checks validate their identity and execute product tests
again. Missing live evidence, negative results, timeout and platform skips fail.
No delivered source/report is changed by this entry.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import uuid
sys.dont_write_bytecode=True
from executor_lifecycle_support import (WORKSPACE,resource,identity,configuration,digest,write_json,run,test_environment,
    initialize_test_build_signing,require_identity,require_artifact)
from executor_lifecycle_history import host_observations

REQUIRED = {
    "lineage": [
        "mission::store::tests::executor_handoff_native_multihop_both_directions_preserves_failed_parents",
        "mission::executor_handoff::tests::handoff_paths_require_both_scopes_and_never_follow_absolute_evidence",
        "app::mission_dispatch::tests::executor_bidirectional_handoff_binds_run_hash_scope_permission_and_model_source",
    ],
    "ownership_windows": [
        "runtime::owned_tree::tests::ownership_setup_failures_reap_suspended_child_before_code_runs",
        "runtime::owned_tree::tests::ownership_immediate_descendant_root_exit_and_close_are_owned",
        "runtime::owned_tree::tests::ownership_transport_immediate_root_exit_cancel_timeout_and_shutdown",
    ],
    "usage": ["runtime::codex_usage::tests::" + name for name in [
        "usage_launch_resume_fixed_cumulative_fixture_and_replay",
        "usage_unknown_baseline_reset_and_subset_failures_never_become_zero",
        "usage_concurrent_resume_reservation_is_atomic_and_session_bound",
        "usage_abandoned_turn_invalidates_baseline_after_restart",
        "usage_process_restart_preserves_baseline_and_immutable_receipt",
        "usage_exact_abc_crash_reentry_cannot_reuse_baseline_or_replace_pending",
    ]],
    "compatibility_and_input": [
        "runtime::codex::tests::executor_codex_selection_persists_revalidates_and_honors_pin",
        "runtime::codex::tests::executor_codex_selection_restart_stale_schema_and_incompatible_fallback",
        "runtime::codex::tests::executor_agent_run_launch_resume_liveness_final_and_usage",
        "workflow::runtime::tests::executor_prompt_transport_and_strict_payload",
        "workflow::runtime::tests::executor_workflow_large_input_no_reader_cancel_timeout_and_output_first",
        "workflow::runtime::tests::codex_cli_final_protocol_and_shutdown_are_fail_closed",
    ],
    "receipt_delivery": [
        "workflow::runner::tests::executor_fail_when_paid_usage_survives_both_branches_and_manual_cancel",
        "workflow::runner::tests::executor_ordinary_publish_snapshot_event_cancel_barrier_is_atomic",
        "workflow::runner::tests::executor_late_ordinary_usage_after_completed_cancel_projects_once",
        "workflow::runner::tests::executor_terminal_cancel_is_transactional_noop_for_stale_and_current_callers",
        "workflow::runner::tests::executor_paid_attempt_delivery_replay_is_additive_and_preserves_owner",
        "workflow::runner::tests::executor_ordinary_retry_entry_preserves_paid_failure_and_does_not_reexecute_completed_attempt",
        "workflow::runner::tests::executor_workflow_receipt_commit_crash_recovers_once_after_restart",
        "workflow::runner::tests::executor_workflow_cancel_ack_projection_survives_process_death_and_reconciles_legacy_overwrite",
        "workflow::runner::tests::executor_cancel_rebuild_preserves_comparable_cost_and_unknown_currency",
        "runtime::codex::tests::executor_agent_run_receipt_crash_restart_and_concurrent_recovery_are_once",
        "runtime::codex::tests::executor_agent_run_managed_claim_caller_receipt_recovery_preserves_failure",
        "runtime::codex::tests::executor_product_transport_exact_labca_rejects_stale_identity_before_provider",
    ],
    "prestart_and_diagnostics": [
        "app::mission_dispatch::tests::executor_multiday_budget_creates_both_workflow_packages_with_legal_step_timeout",
        "app::mission_dispatch::tests::executor_prestart_claim_reconcile_is_narrow_and_preserves_failed_history",
        "mission::store::tests::executor_legacy_claim_restore_rechecks_current_task_atomically",
        "mission::checks::tests::remediation_diagnostics_are_bounded_allowlisted_and_relative",
    ],
    "native_guard": ["runtime::executor_live::executor_native_guard_rejects_corruption_and_prevents_config_writes","runtime::executor_live_handoff::executor_live_helper_accepted_scope_baseline_and_checks_are_bound"],
    "budget_boundary": ["app::mission_supervisor::tests::executor_budget_awaiting_verification_preserves_admitted_boundary",
        "app::mission_supervisor::tests::executor_budget_pending_resume_requires_current_confirmation_and_claim"],
    "recovery_projection": ["app::mission_supervisor::tests::executor_recovery_projection_store_supervisor_and_negative_boundaries",
        "mission::store::tests::executor_recovery_projection_missing_claim_fails_closed_before_legacy_restore"],
    "resource_boundary": ["mission::runtime_resources::tests::apply_overrides_child_root_hints_without_changing_parent_environment",
        "mission::runtime_resources::tests::redirected_cache_ancestors_and_existing_outputs_are_rejected",
        "mission::runtime_resources::tests::windows_paths_use_ordinary_disk_syntax_and_reject_device_aliases"],
    "mission_methods": ["runtime::deepseek_harness::tests::mission_methods_overlay_keeps_assigned_model_shell_files_and_mcp",
        "runtime::deepseek_harness::tests::mission_methods_overlay_does_not_change_native_goal_or_other_sessions"],
    "build_signing_product": ["runtime::deepseek_harness::tests::executor_test_build_signing_matches_effective_test_inputs",
        "app::system::tests::embedded_extension_key_is_trusted_when_the_build_carries_it"],
    "real_codex": ["runtime::codex::tests::executor_real_codex_launch_resume_shell_preflight"],
}
SUMMARY = re.compile(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out")
GROUPS = [
    ("lineage", ["executor_handoff"]),
    ("ownership_windows", ["runtime::owned_tree::tests::"]),
    ("usage", ["runtime::codex_usage::tests::", "--skip", "usage_restart_worker"]),
    ("codex_contract", ["runtime::codex::tests::", "--skip", "executor_real_codex_launch_resume_shell_preflight", "--skip", "executor_selection_restart_worker", "--skip", "executor_agent_receipt_restart_worker", "--skip", "executor_abc_interrupted_worker"]),
    ("workflow_contract", ["workflow::", "--skip", "workflow::runtime::", "--skip", "workflow::runner::"]),
    ("workflow_runner", ["workflow::runner::", "--skip", "receipt_recovery_restart_worker", "--skip", "cancel_projection_process_worker", "--skip", "ordinary_usage_restart_worker", "--skip", "executor_workflow_cancel_ack_projection_survives_process_death_and_reconciles_legacy_overwrite"]),
    ("workflow_runtime", ["workflow::runtime::", "--skip", "codex_cli_"]),
    ("workflow_codex_protocol_lifecycle", ["workflow::runtime::tests::codex_cli_cancel_", "workflow::runtime::tests::codex_cli_exited_", "workflow::runtime::tests::codex_cli_final_"]),
    ("workflow_codex_protocol_notifications", ["workflow::runtime::tests::codex_cli_recoverable_", "workflow::runtime::tests::codex_cli_turn_failed_"]),
    ("cancel_projection", ["workflow::runner::tests::executor_workflow_cancel_ack_projection_survives_process_death_and_reconciles_legacy_overwrite", "--exact"]),
    ("native_guard", ["runtime::executor_live::executor_native_guard_rejects_corruption_and_prevents_config_writes", "runtime::executor_live_handoff::executor_live_helper_accepted_scope_baseline_and_checks_are_bound"]),
    ("process_contract", ["runtime::process::tests::"]),
    ("himind_stdin", ["headless_stdin_"]),
    # libtest filters are substrings; without this exclusion this group also
    # reruns every workflow::runtime:: test concurrently with workflow_runtime.
    ("runtime_contract", ["runtime::tests::", "--skip", "workflow::"]),
    ("himind_input_contract", ["runtime::deepseek_harness::headless_input_tests::"]),
    ("himind_runtime_contract", ["runtime::deepseek_harness::tests::", "--skip", "installed_runtime_revision_identity_and_status_use_package_version",
        "--skip", "executor_test_build_signing_matches_effective_test_inputs"]),
    ("build_signing_product", ["app::system::tests::embedded_extension_key_is_trusted_when_the_build_carries_it", "--exact"]),
    ("effective_test_build_signing", ["runtime::deepseek_harness::tests::executor_test_build_signing_matches_effective_test_inputs", "--exact", "--ignored"]),
    ("dispatch_contract", ["app::mission_dispatch::tests::"]),
    ("mission_contract", ["mission::store::tests::"]),
    ("supervisor_contract", ["app::mission_supervisor::tests::"]),
    ("budget_contract", ["mission::store::budget_remediation_tests::"]),
    ("check_diagnostics", ["mission::checks::tests::"]),
    ("resource_contract", ["mission::runtime_resources::tests::"]),
    ("acp_contract", ["runtime::acp::tests::", "--skip", "real_acp_catalog_probe", "--skip", "real_acp_executor_smoke"]),
]
# Start the retained longest suites first, with independent process environments.
# Splitting groups changes scheduling only; the prepared inventory must match
# every executed group's names and counts, including overlapping legacy groups.
GROUPS.sort(key=lambda item:{'workflow_contract':0,'dispatch_contract':1,'codex_contract':2,'workflow_codex_protocol_lifecycle':3,
    'workflow_runner':4,'workflow_codex_protocol_notifications':5,'workflow_runtime':6,'ownership_windows':7,
    'mission_contract':8,'himind_stdin':9,'budget_contract':10,'cancel_projection':11,'process_contract':12,
    'supervisor_contract':13,'himind_input_contract':14,'check_diagnostics':15}.get(item[0],16))
CHECK_WORKERS = 8

def environment_regressions(root,out,label):
    record,output=run(root,out,label,['python','-B','scripts/remediation/test_executor_build_environment.py'],12)
    counts=re.findall(r'^Ran (\d+) tests? in ',output,re.M)
    record.update(test_count=int(counts[0]) if len(counts)==1 else 0,
        assertions_passed=len(counts)==1 and int(counts[0])>0 and bool(re.search(r'^OK\s*$',output,re.M)))
    record['passed']=record['exit_code']==0 and not record['timed_out'] and record['assertions_passed']
    outcomes=re.findall(r'^(test_\w+) \([^\r\n]+\) \.\.\. (ok|FAIL|ERROR)$',output,re.M)
    record['observed_tests']=['build_environment::'+name for name,_ in outcomes]
    record['passed_test_count']=sum(status=='ok' for _,status in outcomes)
    record['failed_test_count']=sum(status!='ok' for _,status in outcomes)
    record['passed'] &= len(outcomes)==record['test_count'] and record['failed_test_count']==0
    return record

def planned_tests(names,args):
    includes=[];excludes=[];exact=False;index=0
    while index<len(args):
        arg=args[index]
        if arg=='--skip':
            index+=1;excludes.append(args[index])
        elif arg=='--exact':exact=True
        elif arg=='--ignored':pass
        else:includes.append(arg)
        index+=1
    return sorted(name for name in names if (not includes or any(name==part if exact else part in name for part in includes))
        and not any(part in name for part in excludes))

def parse_tests(record,text):
    summaries=SUMMARY.findall(text)
    record['test_count']=sum(int(s[1])+int(s[2]) for s in summaries)
    record['passed_test_count']=sum(int(s[1]) for s in summaries)
    record['failed_test_count']=sum(int(s[2]) for s in summaries)
    record['observed_tests']=re.findall(r"^test ([\w:]+) \.\.\.",text,re.M)
    record['assertions_passed']=bool(summaries) and all(status=='ok' and int(p)>0 and int(f)==int(i)==int(m)==0 for status,p,f,i,m,_ in summaries)
    record['passed']=record['exit_code']==0 and not record['timed_out'] and record['assertions_passed']
    return record


def product_groups(root,out,binary,inventory,deadline,prefix=''):
    # Split only process scheduling, using the frozen binary's exact names.
    # Large serial suites otherwise leave a long tail in the host's 60s total
    # budget. Each original inventory entry is retained, including overlaps.
    weights={'workflow_contract':40,'dispatch_contract':39,'codex_contract':36,'workflow_runner':39,
        'workflow_codex_protocol_lifecycle':38,'workflow_codex_protocol_notifications':17,
        'workflow_runtime':18,'mission_contract':17,'ownership_windows':10,'himind_stdin':6,
        'budget_contract':6,'cancel_projection':7,'process_contract':5,'supervisor_contract':5,
        'himind_input_contract':5,'check_diagnostics':4}
    scheduled=[]
    for label,args in GROUPS:
        names=inventory[label]
        if label in {'workflow_contract','workflow_runner','codex_contract','dispatch_contract'}:
            parts=[names[::2],names[1::2]]
        elif label=='workflow_codex_protocol_lifecycle':
            parts=[[n for n in names if 'final_protocol_and_shutdown' in n],
                   [n for n in names if 'final_protocol_and_shutdown' not in n]]
        else:parts=[names]
        assert all(parts) and sorted(n for part in parts for n in part)==names, 'shards must preserve the complete original inventory'
        for index,part in enumerate(parts):
            weight=weights.get(label,2)/len(parts)
            if label=='workflow_codex_protocol_lifecycle':weight=30 if index==0 else 8
            scheduled.append((label if len(parts)==1 else f'{label}_part_{index+1}',
                args if len(parts)==1 else [*part,'--exact'],part,label,weight))
    scheduled.sort(key=lambda item:-item[4])
    def group(item):
        label,args,expected,parent,_=item
        record,text=run(root,out,prefix+label,[str(binary),*args,'--nocapture','--test-threads=1'],min(40,max(.1,deadline-time.monotonic())))
        parse_tests(record,text)
        record['group']=label
        record['source_group']=parent
        record['execution_phase']='isolated_short_shutdown_protocol' if parent=='workflow_codex_protocol_notifications' else 'parallel_product_regression'
        record['expected_tests']=expected
        record['inventory_matched']=sorted(record['observed_tests'])==expected and record['test_count']==len(expected)
        record['passed']=record['passed'] and record['inventory_matched']
        return record
    with concurrent.futures.ThreadPoolExecutor(max_workers=CHECK_WORKERS) as pool:
        futures=[pool.submit(group,item) for item in scheduled if item[3]!='workflow_codex_protocol_notifications']
        for future in concurrent.futures.as_completed(futures):
            yield future.result()
    # These real process tests intentionally retain a 200ms shutdown limit.
    # Run them after our own high-load work, rather than relaxing that limit
    # or retrying a failed assertion. They still consume the same total budget.
    for item in scheduled:
        if item[3]=='workflow_codex_protocol_notifications':yield group(item)

def prepare(root,out):
    initialize_test_build_signing(out)
    environment=environment_regressions(root,out,'prepare-build-environment')
    assert environment['passed'],'build environment regression failed: '+environment['evidence']
    # Check volume capacity before either expensive build. Do not delete old
    # evidence or borrow another task's target directory to recover capacity.
    def disk_preflight(phase, minimum):
        free=shutil.disk_usage(root).free
        path=out/(phase+'-disk-'+str(time.time_ns())+'.json')
        write_json(path,{'phase':phase,'free_bytes':free,'minimum_free_bytes':minimum,
            'passed':free>=minimum,'resource_root':str(root)})
        assert free>=minimum,'insufficient disk before '+phase+': '+str(path)
    # A fresh dependency build and a relink of this caller's already completed
    # build have different space needs. Neither path skips Cargo, calibration,
    # source/binary identity, or live-evidence validation. Only accept a
    # successful build record whose artifact still exists in this own root.
    own_completed_build=False
    for record_path in (root/'executor').glob('repair2-prebuild-*.json'):
        prior=json.loads(record_path.read_text(encoding='utf-8'))
        if prior.get('exit_code')!=0 or prior.get('argv')!=['cargo','test','--locked','--bin','himind-agent','--no-run','--message-format=json']:continue
        log=Path(prior['evidence']).resolve(strict=True)
        assert log.is_relative_to(root),'build record outside this worker resource'
        if digest(log)!=prior['sha256']:continue
        artifacts=[json.loads(line) for line in log.read_text(encoding='utf-8',errors='replace').splitlines() if line.startswith('{')]
        for artifact in artifacts:
            if artifact.get('reason')!='compiler-artifact' or not artifact.get('executable') or artifact['target']['name']!='himind-agent' or not artifact['profile'].get('test'):continue
            path=Path(artifact['executable']).resolve(strict=True)
            assert path.is_relative_to(root),'build artifact outside this worker resource'
            own_completed_build=any(a.get('reason')=='build-finished' and a.get('success') is True for a in artifacts)
    disk_preflight('test-build-owned-relink' if own_completed_build else 'test-build-cold',(4 if own_completed_build else 16)*1024**3)
    before=identity()
    records=[environment]
    # Existing build wrapper handles MSVC long paths and only uses this root.
    command=[sys.executable,str(WORKSPACE/'scripts/remediation/executor-check.py'),'repair2-prebuild',*before['build_command']]
    wrapper,output=run(root,out,'prepare-build-wrapper',command,1800)
    record=next((json.loads(line) for line in output.splitlines() if line.startswith('{"command":')),None)
    assert record and wrapper['exit_code']==record['exit_code']==0 and not wrapper['timed_out'],'prebuild failed: '+wrapper['evidence']
    records.append(record)
    log=Path(record['evidence']).resolve(strict=True);assert log.is_relative_to(root)
    artifacts=[json.loads(line) for line in log.read_text(encoding='utf-8',errors='replace').splitlines() if line.startswith('{')]
    binaries=[a['executable'] for a in artifacts if a.get('reason')=='compiler-artifact' and a.get('executable') and a['target']['name']=='himind-agent' and a['profile'].get('test')]
    assert len(binaries)==1,'exactly one test artifact required'
    source=Path(binaries[0]).resolve(strict=True);assert source.is_relative_to(root)
    binary=out/'himind-lifecycle-tests.exe';shutil.copy2(source,binary)
    signing,text=run(root,out,'prepare-signature-fixture',[str(binary),'workflow::store::tests::valid_signature_allows_required_install','--exact','--nocapture','--test-threads=1'],240)
    parse_tests(signing,text);records.append(signing);assert signing['passed'],'test signing fixture preparation failed'
    config_check,config=configuration(root,out);records.append(config_check)
    listing,text=run(root,out,'prepare-test-inventory',[str(binary),'--list','--format','terse'],10)
    assert listing['exit_code']==0 and not listing['timed_out'],'product test inventory unavailable'
    inventory=[line[:-6] for line in text.splitlines() if line.endswith(': test')]
    assert inventory,'invalid product test inventory'
    groups={label:planned_tests(inventory,args) for label,args in GROUPS}
    assert all(names and len(names)==len(set(names)) for names in groups.values()),'empty or ambiguous product test group'
    protocol=[name for label,names in groups.items() if label.startswith('workflow_codex_protocol_') for name in names]
    assert sorted(protocol)==planned_tests(inventory,['workflow::runtime::tests::codex_cli_']) and len(protocol)==len(set(protocol)), 'split protocol inventory must retain every scenario exactly once'
    records.append(listing)
    guard_args=next(args for label,args in GROUPS if label=='native_guard')
    guard,text=run(root,out,'prepare-native-guard',[str(binary),*guard_args,'--nocapture','--test-threads=1'],15)
    parse_tests(guard,text);records.append(guard)
    assert guard['passed'] and sorted(guard['observed_tests'])==groups['native_guard'],'native guard/helper product regression failed'
    boundary,text=run(root,out,'budget-green',[str(binary),'app::mission_supervisor::tests::executor_budget_','--nocapture','--test-threads=1'],15)
    parse_tests(boundary,text);records.append(boundary)
    boundary['classification']='runtime7_current_source_store_supervisor_boundary_regression'
    write_json(out/'budget-green.json',boundary)
    assert boundary['passed'] and sorted(boundary['observed_tests'])==sorted(REQUIRED['budget_boundary']),'budget boundary regression failed before paid preparation'
    # Exercise the exact retained inventory/scheduler before any paid call.
    # A successful calibration is evidence, not a replacement for required
    # checks, which execute every group again after final identity freeze.
    calibration_started=time.monotonic()
    calibration=list(product_groups(root,out,binary,groups,calibration_started+47,'prepare-product-'))
    calibration_passed=all(r['passed'] for r in calibration) and time.monotonic()-calibration_started<47
    calibration_path=out/('prepaid-product-'+str(time.time_ns())+'.json')
    write_json(calibration_path,{'passed':calibration_passed,'checks':calibration,'elapsed_ms':round((time.monotonic()-calibration_started)*1000)})
    assert calibration_passed,'full product calibration failed before paid preparation: '+str(calibration_path)
    records.extend(calibration)
    disk_preflight('production-build',4*1024**3)
    build,text=run(root,out,'prepare-production-build',['cargo','build','--locked','--bin','himind-agent'],1800)
    records.append(build);assert build['exit_code']==0 and not build['timed_out'],'production build failed'
    production_binary=(Path(test_environment(root,out,'production-identity')['CARGO_TARGET_DIR'])/'debug/himind-agent.exe').resolve(strict=True)
    assert production_binary.is_relative_to(root),'production binary outside resource'
    live_env=test_environment(root,out,'live-mixed')
    live_env['HIMIND_EXECUTOR_CANDIDATE_ID']=before['candidate_id']
    live_env['HIMIND_CODEX_TIMEOUT_SECONDS']='180'
    if os.environ.get('HIMIND_EXECUTOR_LIVE_HIMIND')=='1':
        probe,text=run(root,out,'prepare-native-preflight',[str(binary),'runtime::executor_live::executor_native_identity_probe','--exact','--ignored','--nocapture'],8,env=live_env)
        parse_tests(probe,text);records.append(probe);assert probe['passed'],'native configuration/runtime preflight unavailable'
    require_identity(before,identity(),'prepaid_candidate')
    # Explicit opt-in controls only the trusted native consumer. Codex keeps
    # its existing personal configuration and blank model/service binding.
    live,text=run(root,out,'prepare-real-codex',[str(binary),REQUIRED['real_codex'][0],'--exact','--ignored','--nocapture','--test-threads=1'],420,env=live_env)
    parse_tests(live,text);records.append(live)
    live_path=re.search(r'real Codex launch/resume shell result: (.+)',text)
    live_evidence=None
    if live.get('passed') and live_path:
        path=Path(live_path[1].strip()).resolve(strict=True);assert path.is_relative_to(root)
        value=json.loads(path.read_text(encoding='utf-8'))
        assert value['classification']=='real_personal_codex_model_launch_resume_shell'
        assert value['launch']['session_id']==value['resume']['session_id']
        assert value['launch']['run_id']!=value['resume']['run_id']
        for token in ['input_tokens','output_tokens']:
            a=value['launch']['usage_receipt']['provider_raw'][token];b=value['resume']['usage_receipt']['provider_raw'][token]
            assert b>a and value['resume']['usage'][token]==b-a
        value.update(candidate_id=before['candidate_id'],helper_executable_sha256=digest(binary))
        write_json(path,value)
        live_evidence={'evidence':str(path),'sha256':digest(path),'check':live}
    real_himind=None
    native_blocker='native_fixed_route_opt_in_or_execution_missing'
    if live.get('passed') and os.environ.get('HIMIND_EXECUTOR_LIVE_HIMIND')=='1':
        path=out/'real-himind.json';value=json.loads(path.read_text(encoding='utf-8'))
        assert value['candidate_id']==before['candidate_id'] and value['passed'] is True
        assert value['model']=='himind:lucen:gpt-6.1-sol' and value['service_id']=='lucen'
        assert value['bidirectional_handoff_executed'] and len(value['stages'])==2
        value['helper_executable_sha256']=digest(binary);write_json(path,value)
        real_himind={'evidence':str(path),'sha256':digest(path),'check':live}
        native_blocker=None
    native_identity=None
    if real_himind:
        probe,text=run(root,out,'prepare-native-identity',[str(binary),'runtime::executor_live::executor_native_identity_probe','--exact','--ignored','--nocapture'],8,env=live_env)
        parse_tests(probe,text);records.append(probe);assert probe['passed'],'native identity unavailable'
        native_identity=json.loads((out/'native-identity.json').read_text(encoding='utf-8'))
        assert all({k:stage[k] for k in native_identity}==native_identity for stage in value['stages']),'native identity changed'
    after=identity();require_identity(before,after,'prepared_candidate')
    cache={'schema_version':'executor_lifecycle_prepared.v3','identity':after,'configuration':config,'binary':str(binary),'binary_sha256':digest(binary),'production_binary':str(production_binary),'production_binary_sha256':digest(production_binary),
        'build_records':[record,build],'real_codex':live_evidence,'real_himind':real_himind,'native_blocker':native_blocker,'native_identity':native_identity,'checks':records,
        'product_test_inventory':groups,
        'prepaid_product_calibration':{'evidence':str(calibration_path),'sha256':digest(calibration_path)},
        'test_signing_key_sha256':digest(out/'test-signing-key.pem'),
        'linker_sha256':digest(root/'executor/linkwrap.exe'),
        'classification':'this_worker_preparation_not_host_VERIFY','passed':False}
    write_json(out/'prepared.json',cache)
    print(json.dumps({'prepared_binary':str(binary),'candidate_id':after['candidate_id'],'real_codex':bool(live_evidence),
        'real_himind':bool(real_himind),'native_blocker':native_blocker,'evidence':str(out/'prepared.json')},ensure_ascii=False),flush=True)
    return 0 if live_evidence and real_himind else 1

def validate_evidence(root,value):
    path=Path(value['evidence']).resolve(strict=True)
    assert path.is_relative_to(root),'cached evidence outside this worker resource'
    assert digest(path)==value['sha256'],'cached evidence hash mismatch'
    return json.loads(path.read_text(encoding='utf-8'))

def main():
    root,out=resource()
    assert sys.platform=='win32','Windows required; skipped ownership is not coverage'
    if sys.argv[1:]==['--prepare']:return prepare(root,out)
    assert not sys.argv[1:],'unexpected check arguments'
    started=time.monotonic();deadline=started+50
    records=[];observed=set();successful=set()
    receipt={'schema_version':'executor_lifecycle_repair_checks.v2','passed':False,'classification':'this_round_current_candidate_execution',
        'invocation_id':os.environ.get('HIMIND_EXECUTOR_CHECK_INVOCATION') or uuid.uuid4().hex,
        'cwd':str(WORKSPACE),'resource_root':str(root),'checks':records,'required':REQUIRED,'test_count':0,'domains':{},
        'historical_host_observations':host_observations()+[{'event_id':'mission_1794473598404_ffe1e069:777','classification':'older_supplied_host_failure_not_worker_execution',
            'checks':[{'exit_code':1,'elapsed_ms':57898,'output_sha256':'5022057d26af58a52d97fcef2a5b34148600b8f3f86005a0943b1e4880e61a56','test_summary':'# tests 305\n# pass 305\n# fail 0'},
                {'exit_code':1,'elapsed_ms':9197,'test_summary':''},
                {'exit_code':-1,'elapsed_ms':0,'test_summary':'','error':'Mission check time budget exhausted'}],
            'numeric_tap_total':305,'raw_output_available':False},
            {'event_id':'mission_1794473598404_ffe1e069:748','classification':'historical_supplied_host_failure_not_local_execution',
            'checks':[{'exit_code':1,'elapsed_ms':128,'output_sha256':'a08dc3e86afd6d9a5ff09983d0526b98947e5e94cdcb6af74ca0558b153f4baf','test_summary':''},
                {'exit_code':1,'elapsed_ms':123,'output_sha256':'9199219cd3421e40d8943e174ed06ecaaffebd1826d97a16247cf3bc14c422b2','test_summary':''},
                {'exit_code':1,'elapsed_ms':128,'output_sha256':'9199219cd3421e40d8943e174ed06ecaaffebd1826d97a16247cf3bc14c422b2','test_summary':''}],
            'numeric_tap_total':None,'raw_output_available':False},
            {'event_id':'mission_1794473598404_ffe1e069:741','classification':'historical_observation_only_details_not_supplied','numeric_tap_total':None},
            {'event_id':'mission_1794473598404_ffe1e069:727','classification':'historical_supplied_failure_not_local_execution',
            'checks':[{'exit_code':1,'elapsed_ms':43041,'output_sha256':'59fe2929d37a78cefcadd8fd76de6470043e94803b87e9c434fc8e05d9bda87f','test_summary':''},
                {'exit_code':1,'elapsed_ms':4125,'output_sha256':'9199219cd3421e40d8943e174ed06ecaaffebd1826d97a16247cf3bc14c422b2','test_summary':''},
                {'exit_code':1,'elapsed_ms':3127,'output_sha256':'9199219cd3421e40d8943e174ed06ecaaffebd1826d97a16247cf3bc14c422b2','test_summary':''}],
            'numeric_tap_total':None,'raw_output_available':False},
            {'event_id':'mission_1794473598404_ffe1e069:717','classification':'separate_supplied_historical_diagnostic_timeout',
            'regression':{'exit_code':-1,'elapsed_ms':62760,'error':'terminated after exceeding 59 seconds','test_summary':''},
            'remaining_checks':[{'exit_code':-1,'elapsed_ms':0,'outcome':'not_executed_check_budget_exhausted','test_summary':''}]*2,
            'numeric_tap_total':None}], 'budget_policy':{'follow_up_total_seconds':60,'formal_verify_total':'min(mission_remaining,300s)',
                'per_command':'min(remaining_total,300s)','entry_internal_seconds':50},'failures':[]}
    try:
        cache=json.loads((out/'prepared.json').read_text(encoding='utf-8'))
        current=identity();require_identity(cache['identity'],current,'prepared_candidate')
        receipt['candidate_id']=current['candidate_id']
        binary=require_artifact(root,cache['binary'],cache['binary_sha256'],'test_binary')
        require_artifact(root,cache['production_binary'],cache['production_binary_sha256'],'production_binary')
        require_artifact(root,root/'executor/linkwrap.exe',cache['linker_sha256'],'linker')
        require_artifact(root,out/'test-signing-key.pem',cache['test_signing_key_sha256'],'signature_fixture')
        config_check,config=configuration(root,out);records.append(config_check)
        require_identity(cache['configuration'],config,'effective_configuration')
        environment=environment_regressions(root,out,'check-build-environment');records.append(environment)
        receipt['test_count']+=environment['test_count']
        observed.update(environment['observed_tests'])
        if environment['passed']:successful.update(environment['observed_tests'])
        receipt['domains']['build_environment']={'test_count':environment['test_count'],
            'assertions_passed':environment['passed'],'execution_records':[environment]}
        assert environment['passed'],'build environment regression failed'
        native_env=test_environment(root,out,'native-identity')
        native_env['HIMIND_EXECUTOR_LIVE_HIMIND']='1'
        native_check,native_text=run(root,out,'native-identity',[str(binary),'runtime::executor_live::executor_native_identity_probe','--exact','--ignored','--nocapture'],5,env=native_env)
        parse_tests(native_check,native_text);records.append(native_check)
        receipt['test_count']+=native_check['test_count'];observed.update(native_check['observed_tests'])
        if native_check['passed']:successful.update(native_check['observed_tests'])
        assert native_check['passed'],'native config/runtime identity probe failed'
        require_identity(cache['native_identity'],json.loads((out/'native-identity.json').read_text(encoding='utf-8')),'native_identity')
        inventory=cache['product_test_inventory']
        assert set(inventory)=={label for label,_ in GROUPS},'prepared test groups differ'
        for record in product_groups(root,out,binary,inventory,deadline):
                records.append(record);observed.update(record['observed_tests'])
                if record['passed']:successful.update(record['observed_tests'])
                receipt['test_count']+=record['test_count']
                # Full names/inventory/assertions remain in the hashed receipt
                # and raw per-suite log. Keep host stdout bounded: repeating
                # hundreds of expected and observed names costs check budget.
                print(json.dumps({key:record[key] for key in ['name','exit_code','elapsed_ms','timed_out',
                    'test_count','passed_test_count','failed_test_count','passed','inventory_matched','evidence','sha256']},
                    ensure_ascii=False),flush=True)
                if not record['passed']:receipt['failures'].append(record['name']+': failed/timeout/zero/ignored tests')
        for domain,names in REQUIRED.items():
            if domain=='real_codex':continue
            found=[name for name in names if name in observed]
            contributing=[record for record in records if set(record.get('observed_tests',[])) & set(names)]
            receipt['domains'][domain]={'required_scenarios':len(names),'executed_required_scenarios':len(found),
                'test_count':len(found),'assertions_passed':all(name in successful for name in names),
                'execution_records':[{'name':r['name'],'elapsed_ms':r['elapsed_ms'],'exit_code':r['exit_code'],
                    'suite_test_count':r['test_count'],'evidence':r['evidence']} for r in contributing]}
            if found!=names:receipt['failures'].append(domain+': missing '+str(sorted(set(names)-observed)))
        for domain in ['real_codex','real_himind']:
            evidence=cache.get(domain)
            if not evidence:
                receipt['failures'].append(domain+': current-candidate worker live execution missing')
                receipt['domains'][domain]={'classification':'not_executed','test_count':0,'assertions_passed':False}
                continue
            value=validate_evidence(root,evidence)
            receipt['domains'][domain]={'classification':'current_candidate_worker_live_execution_revalidated','test_count':evidence['check']['test_count'],
                'evidence':evidence['evidence'],'sha256':evidence['sha256'],'assertions_passed':evidence['check']['passed']}
            assert evidence['check']['passed'],domain+' worker execution failed'
            if domain=='real_codex':
                assert value['classification']=='real_personal_codex_model_launch_resume_shell'
                assert value['launch']['usage_receipt']['run_increment'] and value['resume']['usage_receipt']['run_increment']
                assert value['candidate_id']==current['candidate_id'] and value['helper_executable_sha256']==cache['binary_sha256']
            else:
                assert value['classification']=='real_himind_current_candidate_execution'
                assert value['candidate_id']==current['candidate_id'] and value['passed'] is True
                assert value['helper_executable_sha256']==cache['binary_sha256']
                assert value['model']=='himind:lucen:gpt-6.1-sol' and value['service_id']=='lucen'
                assert value.get('bidirectional_handoff_executed') is True
                assert len(value['stages'])==2 and len(value['artifacts'])==5
                assert len(value['accepted_envelopes'])==3 and len(value['artifact_checks'])==4
                for envelope in value['accepted_envelopes']:
                    assert envelope['context_kind']=='accepted_producer'
                    assert envelope['source_baseline_sha256'] and len(envelope['artifacts'])==1
                    assert envelope['acceptance']['candidate']['source_candidate_id']==current['candidate_id']
                    assert all(check['exit_code']==0 and check['test_summary']=='# tests 1\n# pass 1' for check in envelope['acceptance']['checks'])
                    artifact=envelope['artifacts'][0];assert value['artifacts'][artifact['path']]==artifact['sha256']
                for check in value['artifact_checks']:
                    assert check['candidate']['source_candidate_id']==current['candidate_id']
                    assert check['run_id']!=check['verification_run_id']
                    assert len(check['checks'])==1 and check['checks'][0]['exit_code']==0
                artifacts=Path(value['artifact_root']).resolve(strict=True);assert artifacts.is_relative_to(root)
                for name,sha in value['artifacts'].items():
                    path=(artifacts/name).resolve(strict=True);assert path.is_relative_to(artifacts)
                    assert digest(path)==sha,'live handoff artifact changed'
                assert value['stages'][0]['output_sha256']==value['artifacts']['himind-first.json']
                assert value['stages'][1]['input_sha256']==value['artifacts']['codex-launch.json']
                assert value['historical_baseline_sha256']==digest(WORKSPACE/'remediation-input/baseline-manifest.json')
                assert value['codex_launch_run']!=value['codex_resume_run']
                assert value['mission_pipeline_acceptance'] is False
        assert time.monotonic()<deadline,'regression exceeded 50 second internal budget'
        assert receipt['test_count']>0,'no product tests ran'
        receipt['passed']=not receipt['failures']
    except Exception as error:
        receipt['failures'].append(str(error))
    finally:
        receipt['elapsed_ms']=round((time.monotonic()-started)*1000)
        receipt['observed_tests']=sorted(observed);receipt['unique_test_count']=len(observed)
        path=out/('checks-'+str(time.time_ns())+'.json');write_json(path,receipt);write_json(out/'checks.json',receipt)
        print(json.dumps({'passed':receipt['passed'],'test_count':receipt['test_count'],'elapsed_ms':receipt['elapsed_ms'],
            'failures':receipt['failures'],'evidence':str(path),'invocation_id':receipt['invocation_id']},ensure_ascii=False),flush=True)
        if receipt['test_count']:
            print('# tests',receipt['test_count'])
            print('# pass',sum(r.get('passed_test_count',0) for r in records))
            print('# fail',sum(r.get('failed_test_count',0) for r in records))
        if not receipt['passed']:
            failures=' '.join(receipt['failures'])
            code=('missing_preparation' if not (out/'prepared.json').is_file() else
                'candidate_mismatch' if 'identity mismatch' in failures or 'changed' in failures else
                'live_evidence_missing' if 'live execution missing' in failures else
                'check_deadline' if 'budget' in failures else 'product_regression_failed')
            print('# remediation',code,path.relative_to(root).as_posix(),flush=True)
    return 0 if receipt['passed'] else 1

if __name__=='__main__':
    raise SystemExit(main())
