use crate::capability::types::{InvocationContext, InvocationSource};
use crate::skill::resolver::CapabilityFact;
use crate::skill::{
    catalog_json, client_status_json, client_sync_json, sync_record_to_supported_clients,
    sync_skill_client_json, uninstall_supported_clients_json, unregister_skill_client_json,
    unregister_skill_clients_json,
};
use crate::{Options, VERSION};
use std::error::Error;

pub(crate) fn run(options: &Options, arguments: &[String]) -> Result<(), Box<dyn Error>> {
    // Project deployments are explicit and process-scoped. This avoids
    // changing the machine-wide target just because a CLI was run in a repo.
    // Accept the target flags before or after the subcommand so both common
    // forms work: `skill --workspace X sync` and `skill sync --workspace X`.
    let mut command = Vec::with_capacity(arguments.len());
    let mut workspace = None;
    let mut global = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--workspace" => {
                let path = arguments
                    .get(index + 1)
                    .ok_or("--workspace requires a project directory")?;
                if global {
                    return Err("--workspace 与 --global 不能同时使用".into());
                }
                workspace = Some(crate::skill::target::canonical_workspace_root(
                    std::path::Path::new(path),
                )?);
                index += 2;
            }
            "--global" => {
                if workspace.is_some() {
                    return Err("--workspace 与 --global 不能同时使用".into());
                }
                global = true;
                index += 1;
            }
            value => {
                command.push(value.to_string());
                index += 1;
            }
        }
    }
    if let Some(root) = workspace {
        std::env::remove_var("HIMIND_SKILL_TARGET");
        std::env::set_var("HIMIND_SKILL_WORKSPACE", root);
    } else if global {
        std::env::set_var(
            "HIMIND_SKILL_TARGET",
            crate::skill::target::TARGET_KIND_GLOBAL,
        );
        std::env::remove_var("HIMIND_SKILL_WORKSPACE");
    } else {
        // A persisted workspace is a desktop UI preference, not an implicit
        // CLI mutation target.  CLI commands default to the global target so
        // a previous UI selection cannot redirect an import or uninstall into
        // a repository unexpectedly.  Use --workspace explicitly for a
        // project operation.
        std::env::set_var(
            "HIMIND_SKILL_TARGET",
            crate::skill::target::TARGET_KIND_GLOBAL,
        );
        std::env::remove_var("HIMIND_SKILL_WORKSPACE");
    }
    match command.first().map(String::as_str) {
        Some("catalog") | Some("list") => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(catalog_json(VERSION, "codex", &capability_facts)?)?
        }
        Some("status") => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(client_status_json(VERSION, &capability_facts)?)?
        }
        Some("import-local") | Some("install-local") if command.len() == 2 => {
            let record = crate::app::skill_manager::install_local_package(
                std::path::Path::new(&command[1]),
            )?;
            let capability_facts = capability_facts_for_cli(options)?;
            let clients = sync_record_to_supported_clients(&record, VERSION, &capability_facts)?;
            print_json(serde_json::json!({
                "record": record,
                "clients": clients,
                "deployment": "current-target",
            }))?
        }
        Some("import-github") | Some("install-github")
            if command.len() >= 2 && command.len() <= 4 =>
        {
            let value = crate::app::github_source::import_skill(
                &command[1],
                command.get(2).map(String::as_str).unwrap_or_default(),
                command.get(3).map(String::as_str).unwrap_or_default(),
            )?;
            let record = serde_json::from_value::<crate::skill::types::SkillRecord>(value)?;
            let capability_facts = capability_facts_for_cli(options)?;
            let clients = sync_record_to_supported_clients(&record, VERSION, &capability_facts)?;
            print_json(serde_json::json!({
                "record": record,
                "clients": clients,
                "deployment": "current-target",
            }))?
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
        Some("install") if command.len() == 2 => {
			let state = paired_agent_state(options)?;
			let (catalog_item, record) =
				crate::app::skill_manager::install(options, &state.agent_id, &command[1])?;
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
        Some("author") if command.get(1).map(String::as_str) == Some("list") => {
            print_json(serde_json::json!({ "items": crate::skill::authoring::list()? }))?
        }
        Some("plan") if command.len() == 2 => {
            let state = paired_agent_state(options)?;
            print_json(serde_json::to_value(
                crate::app::skill_manager::plan_install(options, &state.agent_id, &command[1], None)?,
            )?)?
        }
        Some("author")
            if command.get(1).map(String::as_str) == Some("save")
                && command.len() == 3 =>
        {
            let path = command[2].strip_prefix('@').unwrap_or(&command[2]);
            let input = serde_json::from_str::<crate::skill::authoring::SkillDraftInput>(
                &std::fs::read_to_string(path)?,
            )?;
            print_json(serde_json::to_value(crate::skill::authoring::save(input)?)?)?
        }
        Some("author")
            if command.get(1).map(String::as_str) == Some("test")
                && command.len() == 4 =>
        {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(serde_json::to_value(crate::skill::authoring::test(
                &command[2],
                &command[3],
                &capability_facts,
            )?)?)?
        }
        Some("author")
            if command.get(1).map(String::as_str) == Some("confirm")
                && command.len() == 4 =>
        {
            print_json(serde_json::to_value(crate::skill::authoring::confirm(
                &command[2],
                &command[3],
            )?)?)?
        }
        Some("author")
            if command.get(1).map(String::as_str) == Some("submit")
                && command.len() == 4 =>
        {
            let state = paired_agent_state(options)?;
            print_json(serde_json::to_value(crate::skill::authoring::submit(
                options,
                &state.agent_id,
                &command[2],
                &command[3],
            )?)?)?
        }
        Some("uninstall") if command.len() == 2 => {
            print_json(uninstall_supported_clients_json(&command[1])?)?
        }
        Some("register") if command.len() == 3 => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(sync_skill_client_json(
                &command[1],
                &command[2],
                VERSION,
                &capability_facts,
            )?)?
        }
        Some("unregister") if command.len() == 3 => {
            print_json(unregister_skill_client_json(&command[1], &command[2])?)?
        }
        Some("unregister-all") if command.len() == 2 => {
            print_json(unregister_skill_clients_json(&command[1])?)?
        }
        Some("update-workspace") if command.len() == 2 => {
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(crate::skill::update_workspace_skill_json(
                &command[1],
                VERSION,
                &capability_facts,
            )?)?
        }
        Some("set-workspace-enabled") if command.len() == 3 => {
            let enabled = match command[2].trim().to_ascii_lowercase().as_str() {
                "true" | "enabled" | "on" | "1" => true,
                "false" | "disabled" | "off" | "0" => false,
                other => {
                    return Err(format!("启用状态必须是 true 或 false，收到: {other}").into())
                }
            };
            let capability_facts = capability_facts_for_cli(options)?;
            print_json(crate::skill::set_workspace_skill_enabled_json(
                &command[1],
                enabled,
                VERSION,
                &capability_facts,
            )?)?
        }
        _ => {
            return Err(
                "usage: himind-agent skill [--workspace <project-root>|--global] <catalog|import-local path|install-local path|import-github github-url [ref] [subpath]|install-github github-url [ref] [subpath]|market|status|sync|plan <skill-id>|install <skill-id>|register <skill-id> <client-id>|unregister <skill-id> <client-id>|unregister-all <skill-id>|uninstall <skill-id>|update-workspace <skill-id>|set-workspace-enabled <skill-id> <true|false>|author <list|save @json|test id version|confirm id version|submit id version>>".into(),
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
