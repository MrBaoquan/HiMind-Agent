//! Opt-in trusted native integration. Only this module consumes production
//! configuration internally; no config or provider diagnostics cross the boundary.
use std::{error::Error,fs,io::Read,path::{Path,PathBuf},sync::{Arc,RwLock,atomic::{AtomicU8,AtomicU64}}};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
thread_local! {static ACTIVE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
pub(crate) fn native_active()->bool {ACTIVE.with(|a|a.get())}
pub(crate) fn hash(path:&Path)->Result<String,Box<dyn Error>> {Ok(format!("{:x}",Sha256::digest(fs::read(path)?)))}

// Only allowlisted codes can leave the native boundary. Never format an
// underlying configuration, provider or runtime diagnostic in test output.
pub(crate) fn redacted_code(error:&dyn Error)->&'static str {
    match error.to_string().as_str() {
        "native_configuration_unavailable"=>"native_configuration_unavailable",
        "native_resources_unavailable"=>"native_resources_unavailable",
        "native_runtime_unavailable"=>"native_runtime_unavailable",
        "native_runtime_path_limit"=>"native_runtime_path_limit",
        "native_runtime_access_denied"=>"native_runtime_access_denied",
        "native_runtime_nonzero_exit"=>"native_runtime_nonzero_exit",
        "native_execution_failed"=>"native_execution_failed",
        "fixed_provenance_invalid"=>"fixed_provenance_invalid",
        "native_payload_invalid"=>"native_payload_invalid",
        "native_dependency_failed"=>"native_dependency_failed",
        _=>"native_setup_failed",
    }
}
fn runtime_error(error:Box<dyn Error>)->Box<dyn Error> {
    let message=error.to_string();
    if message.contains("ENAMETOOLONG") || message.contains("SQLITE_CANTOPEN") {return "native_runtime_path_limit".into();}
    if message.contains("EACCES") || message.contains("EPERM") {return "native_runtime_access_denied".into();}
    if error.downcast_ref::<super::deepseek_harness::WorkflowRuntimeFailure>().and_then(|e|e.provenance.as_ref())
        .is_some_and(|p|p["exit_code"].is_i64()) {return "native_runtime_nonzero_exit".into();}
    "native_execution_failed".into()
}

struct NativeEnvironment(Vec<(&'static str,Option<std::ffi::OsString>)>);
impl NativeEnvironment {
    fn enter(root:&Path)->Self {
        let keys=["HIMIND_AGENT_HOME","HIMIND_AGENT_PROFILE","HIMIND_DSH_HOME_ROOT","DSH_HOME","HIMIND_DSH_EXECUTABLE","HIMIND_DSH_TIMEOUT_SECONDS"];
        let saved=keys.into_iter().map(|k|(k,std::env::var_os(k))).collect();
        std::env::remove_var("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_PROFILE","production");
        std::env::set_var("HIMIND_DSH_HOME_ROOT",root.join("native-sessions"));
        std::env::remove_var("DSH_HOME");
        std::env::set_var("HIMIND_DSH_TIMEOUT_SECONDS","90");
        ACTIVE.with(|a|a.set(true));Self(saved)
    }
}
impl Drop for NativeEnvironment {fn drop(&mut self) {for (key,value) in &self.0 {match value {Some(v)=>std::env::set_var(key,v),None=>std::env::remove_var(key)}} ACTIVE.with(|a|a.set(false));}}

struct ConfigGuard { _handles:Vec<fs::File>, hashes:Value }
impl ConfigGuard {
    fn acquire()->Result<Self,Box<dyn Error>> {
        // No recovery-capable loader is called before every protected snapshot
        // is parsed, validated and locked against write/delete/reparse changes.
        let home=crate::store::paths::agent_home();
        let mut handles=vec![];
        for dir in home.ancestors() {
            use std::os::windows::fs::{OpenOptionsExt,MetadataExt};
            if fs::symlink_metadata(dir)?.file_attributes() & 0x400 != 0 {return Err("config_redirect_rejected".into());}
            let handle=fs::OpenOptions::new().read(true).share_mode(1).custom_flags(0x02200000).open(dir)?;
            if handle.metadata()?.file_attributes() & 0x400 != 0 {return Err("config_redirect_rejected".into());}
            handles.push(handle);
        }
        let mut hashes=json!({});
        for name in ["ai-services.json","ai-service-selection.json"] {
            use std::os::windows::fs::{OpenOptionsExt,MetadataExt};
            let path=home.join(name);
            let mut handle=fs::OpenOptions::new().read(true).share_mode(1).custom_flags(0x00200000).open(&path)?;
            if handle.metadata()?.file_attributes() & 0x400 != 0 {return Err("config_redirect_rejected".into());}
            let mut bytes=vec![];handle.by_ref().take(2*1024*1024+1).read_to_end(&mut bytes)?;
            if bytes.len()>2*1024*1024 {return Err("config_size_rejected".into());}
            let mut value:Value=serde_json::from_slice(&bytes).map_err(|_|"config_invalid")?;
            if name=="ai-services.json" {
                let rows=value.as_object_mut().ok_or("config_invalid")?;
                for (id,row) in rows.iter_mut() {
                    // Match PersistedCustomAIService defaults before parsing
                    // the public native value. Unknown fields remain untouched.
                    for field in ["image_model","image_adapter"] {if row.get(field).is_none() {row[field]=json!("");}}
                    let service:crate::store::ai_services::CustomAIService=serde_json::from_value(row.clone()).map_err(|_|"config_invalid")?;
                    if service.id!=*id || service.display_name.trim().is_empty() || id.is_empty() ||
                        !id.bytes().all(|b|b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b)) {return Err("config_invalid".into());}
                    let url=url::Url::parse(service.base_url.trim()).map_err(|_|"config_invalid")?;
                    if !matches!(url.scheme(),"http"|"https") || !url.username().is_empty() || url.password().is_some() {return Err("config_invalid".into());}
                    for field in ["encrypted_api_key","encrypted_balance_access_token"] {
                        if let Some(secret)=row[field].as_str() {crate::store::credentials::unprotect_secret_for_current_user(secret).map_err(|_|"config_invalid")?;}
                        else if field=="encrypted_api_key" {return Err("config_invalid".into());}
                    }
                }
                let fixed=rows.get("lucen").ok_or("fixed_service_missing")?;
                if fixed["model"]!="gpt-6.1-sol" && !fixed["models"].as_array().is_some_and(|a|a.iter().any(|m|m=="gpt-6.1-sol")) {return Err("fixed_model_missing".into());}
            } else {
                #[derive(Default,serde::Deserialize)]
                #[serde(default,deny_unknown_fields)]
                struct Selection {schema_version:u32,capability_routes:std::collections::BTreeMap<String,Vec<crate::store::ai_services::CapabilityRouteEntry>>}
                #[derive(Default,serde::Deserialize)]
                #[serde(default)]
                struct Legacy {active_service_id:String,image_service_id:String,media_services:std::collections::BTreeMap<String,String>,capability_fallbacks:std::collections::BTreeMap<String,Vec<String>>}
                if !value.is_object() {return Err("selection_invalid".into());}
                if value.get("capability_routes").is_some() {let _:Selection=serde_json::from_value(value).map_err(|_|"selection_invalid")?;}
                else {let _:Legacy=serde_json::from_value(value).map_err(|_|"selection_invalid")?;}
            }
            hashes[name]=json!(format!("{:x}",Sha256::digest(bytes)));handles.push(handle);
        }
        Ok(Self{_handles:handles,hashes})
    }
    fn verify(&self)->Result<(),Box<dyn Error>> {
        for name in ["ai-services.json","ai-service-selection.json"] {
            if self.hashes[name]!=hash(&crate::store::paths::agent_home().join(name))? {return Err("config_changed".into());}
        } Ok(())
    }
}
fn options(root:&Path)->crate::Options {
    crate::Options {api_base:Arc::new(RwLock::new(String::new())),state_path:root.join("executor/native-state.json"),
        workbench_mode:Arc::new(AtomicU8::new(crate::app::runtime_mode::AgentMode::Independent.as_code())),
        once:true,interval_seconds:1,local_app:false,hidden:true,local_port:0,reenroll:false,enrollment_token:String::new(),
        agent_credential:Arc::new(RwLock::new(String::new())),identity_generation:Arc::new(AtomicU64::new(0)),
        platform_access:Arc::new(RwLock::new(None)),task_execution:Arc::new(RwLock::new(None)),provider_availability_override:Arc::new(AtomicU8::new(1))}
}
fn root()->Result<PathBuf,Box<dyn Error>> {
    if std::env::var("HIMIND_EXECUTOR_LIVE_HIMIND").as_deref()!=Ok("1") {return Err("native_opt_in_required".into());}
    Ok(PathBuf::from(std::env::var_os("SERVICE_UI_RUNTIME_ROOT").ok_or("resource_missing")?).canonicalize()?)
}
fn short_resource(root:&Path)->Result<PathBuf,Box<dyn Error>> {
    let short=PathBuf::from(std::env::var_os("EXECUTOR_SHORT_ROOT").ok_or("resource_short_path_missing")?);
    if short.canonicalize()?!=root.join("executor").canonicalize()? {return Err("resource_short_path_mismatch".into());}
    Ok(short)
}
fn runtime_identity()->Result<Value,Box<dyn Error>> {
    let exe=super::deepseek_harness::executor_installed_runtime_only().map_err(|_|"installed_runtime_unavailable")?;
    let package=exe.parent().and_then(Path::parent).ok_or("runtime_invalid")?;
    let mut files=json!({});
    for relative in ["node.exe","runtime.json","node_modules/@deepseek-ai/dsh/lib/bin.js"] {files[relative]=json!(hash(&package.join(relative))?);}
    files["entry"]=json!(hash(&exe)?);Ok(files)
}

pub(crate) fn stage(workspace:&Path,input:&str,output:&str,task:&str,run:&str)->Result<Value,Box<dyn Error>> {
    let root=root()?;let candidate=std::env::var("HIMIND_EXECUTOR_CANDIDATE_ID")?;
    if candidate.len()!=64 || !candidate.bytes().all(|b|b.is_ascii_hexdigit()) {return Err("candidate_invalid".into());}
    let scope=workspace.canonicalize()?;
    if !scope.starts_with(&root) || !["baseline.json","codex-launch.json"].contains(&input) || !["himind-first.json","himind-second.json"].contains(&output) {return Err("artifact_scope_rejected".into());}
    let input_path=scope.join(input);
    if fs::symlink_metadata(&input_path)?.file_type().is_symlink() {return Err("artifact_redirect_rejected".into());}
    let raw=fs::read(&input_path)?;if raw.is_empty() || raw.len()>4096 {return Err("artifact_size_rejected".into());}
    let payload:Value=serde_json::from_slice(&raw)?;
    let expected=super::executor_live_handoff::artifact_value(&payload["value"])?
        .checked_add(1).ok_or("artifact_integer_overflow")?;
    let nonce=payload["nonce"].as_str().ok_or("artifact_invalid")?;
    if nonce.len()!=32 || !nonce.bytes().all(|b|b.is_ascii_hexdigit()) {return Err("artifact_invalid".into());}
    let env=NativeEnvironment::enter(&short_resource(&root)?);let config=ConfigGuard::acquire().map_err(|_|"native_configuration_unavailable")?;
    let runtime=runtime_identity().map_err(|_|"native_runtime_unavailable")?;
    // Pin the trusted installed executable; resolve_executable must not choose
    // an alternate command, installer, updater or workbench.
    let exe=super::deepseek_harness::executor_installed_runtime_only()?;std::env::set_var("HIMIND_DSH_EXECUTABLE",&exe);
    if !run.starts_with(&format!("helper-{task}-")) {return Err("caller_identity_invalid".into());}
    let resources=crate::mission::CheckRuntimeResources::prepare_at(&scope,"executor-native",&run,&root.join("executor/native-resources"))
        .map_err(|_|"native_resources_unavailable")?;
    let accepted=if task=="himind-second" {let bytes=fs::read(scope.join("accepted-handoff-himind-second.json"))?;
        if bytes.len()>64*1024 {return Err("handoff_size_rejected".into());}
        let envelope:Value=serde_json::from_slice(&bytes)?;
        if envelope.as_array().is_none_or(|a|a.len()!=1) || envelope[0]["context_kind"]!="accepted_producer"
            || envelope[0]["artifacts"][0]["path"]!=input || envelope[0]["artifacts"][0]["sha256"]!=format!("{:x}",Sha256::digest(&raw))
            || envelope[0]["acceptance"]["candidate"]["source_candidate_id"]!=candidate {return Err("handoff_binding_rejected".into());}
        String::from_utf8(bytes)?}else{"[]".into()};
    let prompt=format!("Accepted host-helper dependency envelope: {accepted}. Fixed read-only dependency transformation task. No tools. Input artifact {input}, SHA256 {:x}, content {}. Compute value plus 1. Preserve nonce. Return exactly one JSON object with passed:true, nonce and value. Do not claim Mission verification.",Sha256::digest(&raw),String::from_utf8(raw.clone())?);
    let started=std::time::Instant::now();
    let result=super::deepseek_harness::execute_workflow_in_run_home_with_resources(&options(&root),scope.to_str().ok_or("workspace_invalid")?,
        &prompt,90,true,&run,"REAL_HIMIND",Some("himind:lucen:gpt-6.1-sol"),super::deepseek_harness::WorkflowAccess::Read,&||Ok(false),Some(&resources))
        .map_err(runtime_error)?;
    config.verify()?;if runtime_identity()?!=runtime {return Err("runtime_changed".into());}
    if result.model!="himind:lucen:gpt-6.1-sol" || result.service_source!="custom" || !["http://127.0.0.1","http://localhost"].contains(&result.endpoint.as_str()) {return Err("fixed_provenance_invalid".into());}
    let value=crate::workflow::parse_runtime_payload(&result.text).ok_or("native_payload_invalid")?;
    if value["passed"]!=true || super::executor_live_handoff::artifact_value(&value["value"])?!=expected || value["nonce"]!=nonce {return Err("native_dependency_failed".into());}
    fs::write(scope.join(output),serde_json::to_vec_pretty(&value)?)?;
    let evidence=json!({"classification":"host_delegated_worker_invocation/current_candidate_test_binary_calling_product_runtime_api",
        "candidate_id":candidate,"task_id":task,"run_id":run,"depends_on_artifact":input,"input_sha256":hash(&input_path)?,"output_artifact":output,"output_sha256":hash(&scope.join(output))?,
        "read_scope":[input],"write_scope":[output],"artifact_delivery":"native_host_embeds_only_scoped_dependency_in_no_tools_prompt",
        "service_id":"lucen","model":"himind:lucen:gpt-6.1-sol","provider":"himind.builtin","tool_policy":"none",
        "endpoint_provenance":"ephemeral_loopback_gateway_only_not_upstream_endpoint","endpoint":result.endpoint,
        "usage_status":if result.usage.is_some() {"reported_runtime_projection"}else{"unknown"},"usage":result.usage,"usage_source":"runtime_session_projection_not_raw_upstream_billing","upstream_cost":null,"upstream_billing_owner":null,
        "configuration_sha256":config.hashes,"runtime_sha256":runtime,"exit_code":0,"deadline_seconds":90,"canceled":false,"elapsed_ms":started.elapsed().as_millis(),
        "runtime_operational_writes":"production Agent locks/bootstrap/cache/accounting/logs allowed; generated receipts/sessions in task resource",
        "passed":true,"mission_pipeline_acceptance":false});
    drop(config);drop(env);Ok(evidence)
}

#[test]
#[ignore="explicit native opt-in; configuration hash revalidation only; no paid inference"]
fn executor_native_identity_probe() {
    let result=(||->Result<Value,Box<dyn Error>> {let root=root()?;let _env=NativeEnvironment::enter(&short_resource(&root)?);let config=ConfigGuard::acquire()?;
        let runtime=runtime_identity()?;config.verify()?;Ok(json!({"configuration_sha256":config.hashes,"runtime_sha256":runtime}))})();
    let value=result.unwrap_or_else(|_|panic!("native_identity_unavailable_or_invalid; diagnostics redacted"));
    let path=root().unwrap().join("executor-lifecycle-repair/native-identity.json");
    fs::write(path,serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[test]
fn executor_native_guard_rejects_corruption_and_prevents_config_writes() {
    let _lock=crate::store::paths::test_env_lock();
    let fixture=crate::runtime::process::codex_test_support::Environment::new();
    let home=crate::store::paths::agent_home();fs::create_dir_all(&home).unwrap();
    let main=home.join("ai-services.json");let selection=home.join("ai-service-selection.json");
    assert!(ConfigGuard::acquire().is_err());
    assert!(!main.exists() && !selection.exists(), "missing configuration must not be restored");
    fs::write(&main,b"broken").unwrap();fs::write(&selection,b"{}").unwrap();
    fs::write(home.join("ai-services.json.bak"),b"{}").unwrap();
    assert!(ConfigGuard::acquire().is_err());
    assert_eq!(fs::read(&main).unwrap(),b"broken");
    assert!(!home.join("ai-services.json.corrupt").exists());assert!(!home.join("ai-services.recovery.json").exists());
    fs::write(&main,b"{}").unwrap();
    crate::store::ai_services::upsert(crate::store::ai_services::CustomAIServiceInput {
        id:"lucen".into(),display_name:"isolated guard fixture".into(),base_url:"https://invalid.example/v1".into(),
        protocol:crate::store::ai_services::AIServiceProtocol::OpenaiResponses,model:"gpt-6.1-sol".into(),models:vec!["gpt-6.1-sol".into()],
        image_model:String::new(),image_adapter:String::new(),media:Default::default(),api_key:"isolated-unit-test-only".into(),endpoints:vec![]}).unwrap();
    let guard=ConfigGuard::acquire().unwrap();
    assert!(fs::write(&main,b"cannot replace").is_err());assert!(fs::remove_file(&main).is_err());
    assert!(fs::write(&selection,b"cannot replace").is_err());
    // The real native loader can read a locked valid snapshot without recovery.
    assert!(crate::store::ai_services::list().is_ok());guard.verify().unwrap();
    drop(guard);drop(fixture);
}
