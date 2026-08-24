use crate::capability::types::{InvocationContext, InvocationSource};
use crate::skill::resolver::CapabilityFact;
use crate::skill::{
    catalog_json, client_status_json, client_sync_json, sync_record_to_supported_clients,
    uninstall_supported_clients_json,
};
use crate::{Options, VERSION};
use std::error::Error;

pub(crate) fn run(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    match arguments.first().map(String::as_str) {
        Some("catalog") | Some("list") => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(catalog_json(VERSION, "codex", &capability_facts)?)?
        }
        Some("status") => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(client_status_json(VERSION, &capability_facts)?)?
        }
        Some("import-local") if arguments.len() == 2 => {
            let record = crate::app::skill_manager::install_local_package(
                std::path::Path::new(&arguments[1]),
            )?;
            print_json(serde_json::to_value(record)?)?
        }
        Some("import-github") if arguments.len() == 3 || arguments.len() == 4 => {
            let record = crate::app::github_source::import_skill(
                &arguments[1],
                &arguments[2],
                arguments.get(3).map(String::as_str).unwrap_or_default(),
            )?;
            print_json(record)?
        }
        Some("sync") => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(client_sync_json(VERSION, &capability_facts)?)?
        }
		Some("market") => {
			let state = paired_agent_state(options)?;
			print_json(serde_json::json!({
				"items": crate::app::skill_manager::catalog(options, &state.agent_id)?,
			}))?
		}
        Some("install") if arguments.len() == 2 => {
			let state = paired_agent_state(options)?;
			let (catalog_item, record) =
				crate::app::skill_manager::install(options, &state.agent_id, &arguments[1])?;
			let capability_facts = capability_facts_for_cli(options)?;
			let clients =
				sync_record_to_supported_clients(&record, VERSION, &capability_facts)?;
			print_json(serde_json::json!({
				"catalog_item": catalog_item,
				"record": record,
				"codex": clients.get("codex"),
				"github_copilot": clients.get("github-copilot"),
				"workbuddy": clients.get("workbuddy"),
				"clients": clients,
			}))?
		}
        Some("author") if arguments.get(1).map(String::as_str) == Some("list") => {
            print_json(serde_json::json!({ "items": crate::skill::authoring::list()? }))?
        }
        Some("plan") if arguments.len() == 2 => {
            let state = paired_agent_state(options)?;
            print_json(serde_json::to_value(
                crate::app::skill_manager::plan_install(options, &state.agent_id, &arguments[1], None)?,
            )?)?
        }
        Some("author")
            if arguments.get(1).map(String::as_str) == Some("save")
                && arguments.len() == 3 =>
        {
            let path = arguments[2].strip_prefix('@').unwrap_or(&arguments[2]);
            let input = serde_json::from_str::<crate::skill::authoring::SkillDraftInput>(
                &std::fs::read_to_string(path)?,
            )?;
            print_json(serde_json::to_value(crate::skill::authoring::save(input)?)?)?
        }
        Some("author")
            if arguments.get(1).map(String::as_str) == Some("test")
                && arguments.len() == 4 =>
        {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(serde_json::to_value(crate::skill::authoring::test(
                &arguments[2],
                &arguments[3],
                &capability_facts,
            )?)?)?
        }
        Some("author")
            if arguments.get(1).map(String::as_str) == Some("confirm")
                && arguments.len() == 4 =>
        {
            print_json(serde_json::to_value(crate::skill::authoring::confirm(
                &arguments[2],
                &arguments[3],
            )?)?)?
        }
        Some("author")
            if arguments.get(1).map(String::as_str) == Some("submit")
                && arguments.len() == 4 =>
        {
            let state = paired_agent_state(options)?;
            print_json(serde_json::to_value(crate::skill::authoring::submit(
                options,
                &state.agent_id,
                &arguments[2],
                &arguments[3],
            )?)?)?
        }
        Some("uninstall") if arguments.len() == 2 => {
            print_json(uninstall_supported_clients_json(&arguments[1])?)?
        }
        _ => {
            return Err(
                "usage: himind-agent skill <catalog|import-local path|import-github owner/repo ref [subpath]|market|status|sync|plan <skill-id>|install <skill-id>|uninstall <skill-id>|author <list|save @json|test id version|confirm id version|submit id version>>".into(),
            )
        }
    }
    Ok(())
}

fn paired_agent_state(options: &Options) -> Result<crate::api::types::AgentState, Box<dyn Error>> {
    let state = crate::api::client::load_agent_state(&options.state_path)?;
    options.set_agent_credential(&state.credential);
    Ok(state)
}

fn capability_facts_for_cli(options: &Options) -> Result<Vec<CapabilityFact>, Box<dyn Error>> {
    crate::skill::capability_facts_from_gateway(
        options,
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::store::types::LocalWorkerStatus::default(),
        )),
        &InvocationContext::new(InvocationSource::Cli, "skill-cli"),
    )
}

fn print_json(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
