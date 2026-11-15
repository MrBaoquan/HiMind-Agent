//! Isolated host-delegated integration records. This never opens or dispatches
//! the installed Mission database and never invokes its formal VERIFY stage.
use std::{error::Error,path::{Path,PathBuf}};
use serde_json::{json,Value};
use crate::{mission::{MissionStore,MissionView},store::local_runs::LocalRunLedger,agent_core_contracts::{LocalRun,LocalRunStatus,LocalRunUsage,RuntimeEvent}};

pub(crate) struct LiveHandoff {store:MissionStore,ledger:LocalRunLedger,mission:MissionView,workspace:PathBuf,candidate:String,nonce:String,runs:Vec<String>,pub envelopes:Vec<Value>,pub checks:Vec<Value>}
const TASKS:[&str;4]=["himind-first","codex-launch","himind-second","codex-resume"];
const INPUTS:[&str;4]=["baseline.json","himind-first.json","codex-launch.json","himind-second.json"];
const OUTPUTS:[&str;4]=["himind-first.json","codex-launch.json","himind-second.json","codex-resume.json"];
// PowerShell's JSON serializer may encode an integral arithmetic result as
// 42.0. Match the separate Python artifact check's numeric contract, without
// rewriting accepted artifact bytes or truncating a fractional/unsafe value.
pub(crate) fn artifact_value(value:&Value)->Result<u64,Box<dyn Error>> {
    if let Some(value)=value.as_u64() {return Ok(value);}
    let number=value.as_f64().filter(|n|n.is_finite() && *n>=0.0
        && *n<=9_007_199_254_740_991.0 && n.fract()==0.0).ok_or("artifact_integer_invalid")?;
    Ok(number as u64)
}
impl LiveHandoff {
    pub(crate) fn new(workspace:&Path,candidate:&str,nonce:&str)->Result<Self,Box<dyn Error>> {
        let data=workspace.with_extension("helper-ledgers");std::fs::create_dir_all(&data)?;
        let store=MissionStore::new(data.join("mission.sqlite3"));let ledger=LocalRunLedger::new(data.join("runs.sqlite3"));
        // This validator is an actual separate owned process, not a model's
        // passed=true. Its logs contain only numeric TAP and no provider data.
        std::fs::write(workspace.join("handoff-check.py"),"import json,sys\nfrom pathlib import Path\np=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\nassert p['passed'] is True and p['value']==int(sys.argv[2]) and p['nonce']==sys.argv[3]\nprint('# tests 1')\nprint('# pass 1')\n")?;
        let mut paths:Vec<String>=INPUTS.iter().chain(OUTPUTS.iter()).copied().chain(["handoff-check.py"]).map(str::to_owned).collect();
        paths.extend(TASKS.iter().map(|id|format!("accepted-handoff-{id}.json")));
        let created=store.create(&serde_json::from_value(json!({"title":"Isolated current-source helper integration; not installed acceptance",
            "workspace":{"root":workspace,"permission_mode":"full_access","read_scope":paths,"write_scope":OUTPUTS}}))?)?;
        let tasks:Vec<Value>=(0..4).map(|i|json!({"id":TASKS[i],"title":TASKS[i],"objective":"Actual runtime dependency transformation, deterministic helper validation only",
            "depends_on":if i==0 {vec![]}else{vec![TASKS[i-1]]},"read_paths":[INPUTS[i],format!("accepted-handoff-{}.json",TASKS[i])],"write_paths":[OUTPUTS[i]],
            "acceptance_criteria":["Fresh nonce and dependency value plus one"],"required_checks":[format!("python handoff-check.py {} {} {}",OUTPUTS[i],41+i,nonce)],
            "execution":{"executor":if i%2==0 {"himind.builtin"}else{"personal.codex"},"service_id":if i%2==0 {"lucen"}else{""},
                "model":if i%2==0 {"himind:lucen:gpt-6.1-sol"}else{""},"role_id":"remediation.executor",
                "expert":{"expert_id":"com.himind.expert.senior-system-architect","expert_version":"1.0.0","expert_digest":"sha256:8cd4326e74539a52f3b4d7582f13a29d9581bc73b8dd8b764f9b4ce7dbe8142c"}}})).collect();
        let planned=store.propose_plan(&created.mission_id,&serde_json::from_value(json!({"summary":"Isolated live helper handoff","tasks":tasks,
            "required_acceptance":[],"budget":{"max_rounds":20,"max_minutes":10,"max_repair_attempts":2}}))?)?;
        let mission=store.confirm_plan(&created.mission_id,&planned.plan_sha256,&TASKS.map(str::to_owned),&paths,&[],&[],"applied_task_contract_isolated_integration")?;
        Ok(Self{store,ledger,mission,workspace:workspace.into(),candidate:candidate.into(),nonce:nonce.into(),runs:vec![],envelopes:vec![],checks:vec![]})
    }
    pub(crate) fn begin(&mut self,index:usize)->Result<String,Box<dyn Error>> {
        let task=self.mission.tasks.iter().find(|t|t.spec.id==TASKS[index]).ok_or("helper_task_missing")?.spec.clone();
        if index>0 {
            self.mission=self.store.get(&self.mission.mission_id)?;
            let envelopes=crate::mission::executor_handoff::build(&self.mission,&task,&self.ledger,&self.store)?;
            if envelopes.len()!=1 || envelopes[0]["context_kind"]!="accepted_producer" || envelopes[0]["artifacts"].as_array().is_none_or(|a|a.len()!=1)
                || envelopes[0]["source_baseline_sha256"].is_null() || envelopes[0]["acceptance"]["candidate"]["source_candidate_id"]!=self.candidate {return Err("helper_accepted_handoff_invalid".into());}
            let path=self.workspace.join(format!("accepted-handoff-{}.json",TASKS[index]));std::fs::write(path,serde_json::to_vec_pretty(&envelopes)?)?;
            self.envelopes.extend(envelopes);
        }
        let run_id=format!("helper-{}-{:016x}",TASKS[index],rand::random::<u64>());
        let run:LocalRun=serde_json::from_value(json!({"schema_version":"local_run.v1","run_id":run_id,"interaction_id":"helper-only",
            "source":"workflow","transport":"local","status":"running","runtime_provider":task.execution.executor,
            "workspace_ref":self.workspace,"created_at":"1","updated_at":"1",
            "steps":[{"step_id":task.id,"title":task.title,"status":"running","runtime_provider":task.execution.executor,
                "model":task.execution.model,"role_id":task.execution.role_id,"attempt":1}]}))?;
        self.ledger.save_run(&run)?;self.store.begin_task_attempt(&self.mission.mission_id,&task.id,&run_id)?;
        let baseline=json!({"files":{(INPUTS[index]):super::executor_live::hash(&self.workspace.join(INPUTS[index]))?},
            "provenance":"host_helper_captured_before_actual_runtime_not_installed_Mission"});
        self.store.append_event(&self.mission.mission_id,"task_baseline_captured",&task.id,"nonempty pre-runtime helper baseline",
            json!({"implementation_run_id":run_id,"protected_baseline":baseline}))?;
        self.runs.push(run_id.clone());Ok(run_id)
    }
    pub(crate) fn complete(&mut self,index:usize,usage:Option<LocalRunUsage>,runtime:&Value)->Result<(),Box<dyn Error>> {
        let task=self.mission.tasks.iter().find(|t|t.spec.id==TASKS[index]).unwrap().spec.clone();let run_id=&self.runs[index];
        if runtime["run_id"]!=*run_id {return Err("helper_runtime_binding_mismatch".into());}
        let mut run=self.ledger.get_run(run_id)?.ok_or("helper_run_missing")?;
        run.status=LocalRunStatus::Succeeded;run.steps[0].status=crate::agent_core_contracts::LocalStepStatus::Succeeded;run.usage=usage;run.updated_at="2".into();self.ledger.save_run(&run)?;
        self.ledger.append_event(&serde_json::from_value::<RuntimeEvent>(json!({"schema_version":"runtime_event.v1","event_id":format!("{run_id}:actual-output"),
            "run_id":run_id,"step_id":TASKS[index],"capability_id":"","sequence":1,"provider":task.execution.executor,"event_type":"tool_completed","occurred_at":"2",
            "payload":{"output":{"passed":true,"evidence":[OUTPUTS[index]],"service_source":if index%2==0 {"custom"}else{"personal_cli_config"},"actual_runtime":runtime}}}))?)?;
        self.store.await_task_verification(&self.mission.mission_id,&task.id,run_id,"deterministic isolated helper validator; not formal VERIFY")?;
        let review_id=format!("{run_id}-host-artifact-check");self.store.begin_verification_attempt(&self.mission.mission_id,&task.id,&review_id)?;
        let records=crate::mission::run_required_checks(&self.workspace,&task.required_checks,std::time::Duration::from_secs(8),&||Ok(false));
        if records.len()!=1 || records[0].exit_code!=0 || records[0].test_summary!="# tests 1\n# pass 1" {return Err("helper_artifact_check_failed".into());}
        let mut review=run.clone();review.run_id=review_id.clone();review.runtime_provider="himind.agent".into();review.steps[0].runtime_provider="himind.agent".into();review.steps[0].model.clear();review.usage=None;self.ledger.save_run(&review)?;
        let identity=crate::mission::scoped_candidate_identity(&self.workspace,[OUTPUTS[index]])?;let mut candidate=identity["candidate"].clone();candidate["files"]=identity["files"].clone();candidate["source_candidate_id"]=json!(self.candidate);
        let checks=serde_json::to_value(&records)?;
        self.store.append_event(&self.mission.mission_id,"host_verification_checked",&task.id,"actual isolated helper artifact validator only",
            json!({"passed":true,"plan_sha256":self.mission.plan_sha256,"implementation_run_id":run_id,"verification_run_id":review_id,
                "candidate_before":candidate,"checks":checks,"classification":"isolated_helper_validator_not_formal_VERIFY_or_independent_QA"}))?;
        self.mission=self.store.record_verification_result(&self.mission.mission_id,&task.id,run_id,&review_id,true,&[format!("helper-check:{review_id}")],"")?;
        self.checks.push(json!({"task_id":task.id,"run_id":run_id,"verification_run_id":review_id,"checks":records,"candidate":candidate,"nonce":self.nonce}));Ok(())
    }
}

#[test]
fn executor_live_helper_accepted_scope_baseline_and_checks_are_bound() {
    for value in [json!(42),json!(42.0)] {assert_eq!(artifact_value(&value).unwrap(),42);}
    for value in [json!(42.5),json!(-1),json!("42"),json!(true),Value::Null,json!(9_007_199_254_740_992.0)] {
        assert!(artifact_value(&value).is_err());
    }
    let root=std::env::temp_dir().join(format!("live-handoff-{:016x}",rand::random::<u64>()));std::fs::create_dir_all(&root).unwrap();
    let nonce="1".repeat(32);std::fs::write(root.join("baseline.json"),json!({"value":40,"nonce":nonce}).to_string()).unwrap();
    let mut helper=LiveHandoff::new(&root,&"a".repeat(64),&nonce).unwrap();
    let run=helper.begin(0).unwrap();std::fs::write(root.join("himind-first.json"),json!({"passed":true,"value":41,"nonce":nonce}).to_string()).unwrap();
    helper.complete(0,None,&json!({"run_id":run,"classification":"isolated_nonpaid_unit_fixture"})).unwrap();
    helper.begin(1).unwrap();assert_eq!(helper.envelopes.len(),1);
    assert_eq!(helper.envelopes[0]["upstream_run_id"],run);assert_eq!(helper.envelopes[0]["artifacts"][0]["path"],"himind-first.json");
    assert_eq!(helper.checks[0]["checks"][0]["exit_code"],0);assert!(helper.envelopes[0]["source_baseline_sha256"].as_str().unwrap().len()==64);
    // Scope isolation is production behavior even with other readable outputs.
    assert_eq!(helper.envelopes[0]["artifacts"].as_array().unwrap().len(),1);
}
