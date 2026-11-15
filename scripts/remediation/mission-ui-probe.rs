//! Generated-test adapter. Original contract/store/candidate are compiled verbatim.
//! Workflow/expert/model integrations fail closed and are not exercised. No host
//! scheduler, model, production home or formal verification is started.
#![allow(dead_code, unused_imports)]
use serde_json::{json, Value};
use std::error::Error;
use std::io::{BufRead, Write};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
mod agent_core_contracts {
    #[derive(Debug,Clone,PartialEq,serde::Serialize,serde::Deserialize)]
    pub struct LocalRunUsage {pub input_tokens:u64,pub output_tokens:u64,pub estimated_cost:f64,pub currency:String,pub billing_owner:String}
}
mod expert {pub type ExpertSummary = serde_json::Value;}
mod runtime {pub mod acp {pub fn is_provider(_: &str)->bool {false}}}
mod store {pub mod paths {pub fn agent_home()->std::path::PathBuf {panic!("production home forbidden in UI probe")}}}
mod workflow {
    use super::*;
    pub struct WorkflowStore;
    pub struct WorkflowPackage {pub roles:std::collections::HashMap<String,Value>,pub steps:Vec<Step>}
    pub struct Step {pub role_id:String}
    impl WorkflowPackage {pub fn accepts_provider(&self,_:&str)->bool {false}}
    impl WorkflowStore {
        pub fn open_default()->Result<Self>{Err("workflow integrations disabled in UI probe".into())}
        pub fn load_pinned_version(&self,_:&str,_:&str,_:&str)->Result<(WorkflowPackage,())>{Err("workflow integrations disabled".into())}
    }
    pub fn validate_model_alias(_: &str)->std::result::Result<(),String>{Err("model integrations disabled".into())}
    pub fn model_alias_matches_provider(_: &str,_:&str)->std::result::Result<(),String>{Err("model integrations disabled".into())}
    pub fn model_alias_parts(_: &str)->Option<(&str,&str,&str)>{None}
    pub fn supports_full_access(_: &str)->bool{false}
}
mod app {pub mod mission_commands {
    #[derive(Debug,Clone,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
    pub struct MissionExecutionOption {pub executor:String,pub label:String,pub available:bool,pub reason:String,pub models:Vec<serde_json::Value>}
    pub fn mission_assignment_options_sync(_: &str)->std::result::Result<crate::mission::MissionAssignmentOptions,String>{Err("catalog integration disabled".into())}
}}
mod mission {
    #[path = "@WORKSPACE@/src/mission/contract.rs"] pub mod contract;
    #[path = "@WORKSPACE@/src/mission/store.rs"] pub mod store;
    #[path = "@WORKSPACE@/src/mission/candidate.rs"] mod candidate;
    pub(crate) use candidate::protected_files;
    pub(crate) use contract::*;
    pub(crate) use store::MissionStore;
    pub const FEEDBACK_CONTEXT_POLICY:&str="test adapter does not process feedback";
    #[path = "@CHECKS@"] pub mod checks;
    pub fn validate_catalog_assignment(_: &MissionAssignmentInput,_:&MissionAssignmentOptions)->crate::Result<()> {Err("expert integrations disabled".into())}
    pub fn validate_expert_pin(_: &MissionExpertPin)->crate::Result<()> {Err("expert integrations disabled".into())}
}
use mission::*;
fn confirmed(store:&MissionStore, label:&str, root:&std::path::Path)->Result<MissionView> {
    let workspace=root.join(label).to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let mission=store.create(&serde_json::from_value(json!({"title":label,"objective":"隔离 Store 状态验证；不调用专家或正式验收","workspace":{"root":workspace,"read_scope":["frontend"],"write_scope":["frontend"]},"acceptance_criteria":["浏览器状态与接口一致"]}))?)?;
    let specs=vec!["first","next"].into_iter().map(|id|json!({"id":id,"title":if id=="first"{"界面交付与长文本证据"}else{"后续交付"},"objective":"状态验证","depends_on":if id=="next"{vec!["first"]}else{vec![]},"read_paths":["frontend"],"write_paths":["frontend/src"],"acceptance_criteria":["保留证据"],"required_checks":["node --version"],"execution":{"executor":"himind.builtin","model":"","role_id":"ui.implementer","service_id":""},"repair_of":""})).collect::<Vec<_>>();
    let plan=store.propose_plan(&mission.mission_id,&serde_json::from_value(json!({"summary":"隔离状态验证","tasks":specs,"required_acceptance":[],"budget":{"max_rounds":20,"max_minutes":1440,"max_repair_attempts":2,"max_total_tokens":100,"max_cost":null,"currency":"USD"}}))?)?;
    store.confirm_plan(&mission.mission_id,&plan.plan_sha256,&["first".into(),"next".into()],&["frontend/src".into()],&[],&[],"ui-probe")
}
fn seed(store:&MissionStore, root:&std::path::Path)->Result<Value> {
    let mut ids=serde_json::Map::new();
    for label in ["budget","environment","pause","verification","partial","running","coordination","failed","rejected","completed"] {
        let m=confirmed(store,label,root)?; let id=&m.mission_id; ids.insert(label.into(),json!(id));
        if label=="pause" {store.control(id,"pause")?;continue;}
        store.begin_task_attempt(id,"first",&format!("{label}-implementation"))?;
        if label=="running" {continue;}
        if ["budget","environment","coordination","failed"].contains(&label) {
            if ["budget","environment"].contains(&label) {store.record_run_usage(id,"first",&format!("{label}-implementation"),&agent_core_contracts::LocalRunUsage{input_tokens:100,output_tokens:0,estimated_cost:0.25,currency:"USD".into(),billing_owner:"local".into()})?;}
            store.complete_task_attempt(id,"first",&format!("{label}-implementation"),false,&[format!("local-run:{label}-implementation")],"隔离测试：保留已有实现引用")?;
            if ["budget","environment"].contains(&label) {store.next_ready_task(id)?;}
            if label=="environment" {store.append_event(id,"runtime_workspace_blocked","first","命令环境需要处理",json!({"plan_sha256":m.plan_sha256}))?;}
            if label=="failed" {store.append_event(id,"mission_failed","first","测试失败记录",json!({}))?;}
            store.append_event(id,"task_progress","first","预算等待后的进度事件",json!({}))?;
            if label=="coordination" {store.append_event(id,"mission_feedback_submitted","","收到意见供后续输入参考",json!({}))?;store.append_event(id,"mission_follow_up_format_retry","","系统重试整理跟进方案",json!({}))?;}
            continue;
        }
        store.await_task_verification(id,"first",&format!("{label}-implementation"),"隔离样本实现摘要（未附正文）")?;
        if label=="verification" {store.begin_verification_attempt(id,"first","verification-review")?; continue;}
        // Contract state seed, explicitly NOT an actual independent expert run.
        let implementation=format!("{label}-implementation");
        let review=format!("{label}-review-contract-seed");
        let passed=label!="rejected";
        store.begin_verification_attempt(id,"first",&review)?;
        store.append_event(id,"host_verification_checked","first","隔离测试种子：检查与独立验收显示格式",json!({"plan_sha256":m.plan_sha256,"implementation_run_id":implementation,"verification_run_id":review,"passed":passed,"checks":[{"command":"contract-example","exit_code":if passed{0}else{1},"test_summary":"","output_sha256":"test-seed-not-a-real-check"}],"reviewer_output":{"passed":passed,"summary":"隔离状态样本；非实际专家验收"}}))?;
        store.record_verification_result(id,"first",&implementation,&review,passed,&["reports/example/long-path-这是隔离样本证据引用-未读取文件.md".into()],if passed{""}else{"隔离样本：核验未通过"})?;
        if label=="completed" {
            store.begin_task_attempt(id,"next","completed-next-implementation")?;
            store.await_task_verification(id,"next","completed-next-implementation","隔离样本第二项摘要")?;
            store.begin_verification_attempt(id,"next","completed-next-review-contract-seed")?;
            store.append_event(id,"host_verification_checked","next","隔离完成状态样本",json!({"plan_sha256":m.plan_sha256,"implementation_run_id":"completed-next-implementation","verification_run_id":"completed-next-review-contract-seed","passed":true,"checks":[],"reviewer_output":null}))?;
            store.record_verification_result(id,"next","completed-next-implementation","completed-next-review-contract-seed",true,&["local-run:completed-next-review-contract-seed".into()],"")?;
        }
    }
    Ok(Value::Object(ids))
}
fn call(store:&MissionStore, root:&std::path::Path, value:Value)->Result<Value> {
    let id=value["missionId"].as_str().unwrap_or("");
    match value["command"].as_str().unwrap_or("") {
        "seed"=>seed(store,root),
        "list_missions"=>Ok(serde_json::to_value(store.list(500)?)?),
        "get_mission_snapshot"=>Ok(serde_json::to_value(store.snapshot(id,value["afterSequence"].as_u64().unwrap_or(0))?)?),
        "increase_mission_budget"=>Ok(serde_json::to_value(store.increase_budget(id,&serde_json::from_value(value["input"].clone())?,"browser-test-user")?)?),
        "control_mission"=>Ok(serde_json::to_value(store.control(id,value["action"].as_str().unwrap_or(""))?)?),
        "environment_only"=>{let m=store.get(id)?;store.control(id,"pause")?;store.append_event(id,"runtime_workspace_blocked","first","环境待处理",json!({"plan_sha256":m.plan_sha256}))?;Ok(serde_json::to_value(store.get(id)?)?)},
        "append_progress"=>{store.append_event(id,"task_progress","first","后续进度",json!({}))?;Ok(serde_json::to_value(store.get(id)?)?)},
        _=>Err("unsupported probe command".into())
    }
}
fn main()->Result<()> {
    let root=std::path::PathBuf::from(std::env::args().nth(1).ok_or("output root required")?).canonicalize()?;
    let store=MissionStore::new(root.join("ui.sqlite"));
    for line in std::io::stdin().lock().lines() {
        let response=serde_json::from_str(&line?).map_err(|e|Box::new(e) as Box<dyn Error>).and_then(|v|call(&store,&root,v));
        println!("{}",match response {Ok(v)=>json!({"ok":true,"value":v}),Err(e)=>json!({"ok":false,"error":e.to_string()})});
        std::io::stdout().flush()?;
    }
    Ok(())
}
