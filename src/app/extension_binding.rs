//! Workspace-scoped extension targets. The host never interprets target keys.
use crate::capability::plugin::{PluginCredentialRequest, PluginRegistryItem};
use crate::capability::types::InvocationContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BindingDeclaration {
    pub root_field: String,
    pub bind_capability: String,
    pub bound_capabilities: Vec<String>,
    pub target_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub(crate) struct Binding {
    pub extension_id: String,
    pub credential_id: String,
    pub credential_ref: String,
    pub workspace: String,
    pub revision: String,
    pub target: BTreeMap<String, String>,
    pub declaration_fingerprint: String,
}

pub(crate) struct PreparedBinding {
    path: PathBuf,
    plugin: PluginRegistryItem,
    credential: PluginCredentialRequest,
    pub snapshot: Option<Binding>,
    pub workspace: String,
    pub is_bind: bool,
    expected_revision: String,
}

fn canonical(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = Path::new(value);
    if !path.is_absolute() || !path.is_dir() {
        return Err("existing absolute workspace required".into());
    }
    Ok(path.canonicalize()?)
}

fn path_for(workspace: &str, plugin: &str) -> PathBuf {
    crate::store::paths::agent_home()
        .join("extension-bindings")
        .join(format!(
            "{:x}.json",
            Sha256::digest(format!("{plugin}\n{workspace}"))
        ))
}

fn workspace(
    context: &InvocationContext,
    input: &Value,
    field: &str,
) -> Result<String, Box<dyn Error>> {
    // Only session/runtime-owned context is trusted. Never fall back to input.
    let trusted = canonical(&context.trusted_workspace_ref)
        .map_err(|_| "trusted session workspace missing; reopen this project through its host")?;
    let requested = input
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(&context.trusted_workspace_ref);
    let scope = canonical(requested)?;
    if !scope.starts_with(&trusted) {
        return Err("extension target workspace is outside the trusted session".into());
    }
    let value = scope.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let value = value.strip_prefix("//?/").unwrap_or(&value).to_string();
    #[cfg(windows)]
    let value = value.to_lowercase();
    Ok(value)
}

pub(crate) fn prepare(
    context: &InvocationContext,
    capability: &str,
    input: &mut Value,
) -> Result<Option<PreparedBinding>, Box<dyn Error>> {
    let plugin = crate::capability::plugin::scan_plugins()?
        .into_iter()
        .find(|p| p.enabled && p.capabilities.iter().any(|c| c.id == capability));
    let Some(plugin) = plugin else {
        return Ok(None);
    };
    let credential = plugin
        .credentials
        .iter()
        .find(|c| {
            c.workspace_binding.as_ref().is_some_and(|b| {
                b.bind_capability == capability
                    || b.bound_capabilities.iter().any(|v| v == capability)
            })
        })
        .cloned();
    let Some(credential) = credential else {
        return Ok(None);
    };
    let policy = credential
        .workspace_binding
        .as_ref()
        .ok_or("missing binding declaration")?;
    let workspace = workspace(context, input, &policy.root_field)?;
    input[&policy.root_field] = json!(workspace);
    let path = path_for(&workspace, &plugin.id);
    let is_bind = capability == policy.bind_capability;
    let previous: Option<Binding> = if path.exists() {
        Some(serde_json::from_slice(&std::fs::read(&path)?)?)
    } else {
        None
    };
    let expected_revision = previous
        .as_ref()
        .map(|b| b.revision.clone())
        .unwrap_or_else(|| "unbound".into());
    if is_bind {
        if input
            .get("binding_revision")
            .and_then(Value::as_str)
            .is_some_and(|r| r != expected_revision)
        {
            return Err("binding changed before rebinding approval".into());
        }
        input["binding_revision"] = json!(expected_revision);
    }
    let snapshot = if is_bind {
        None
    } else {
        let binding: Binding = serde_json::from_slice(
            &std::fs::read(&path)
                .map_err(|_| "extension target is not bound for this page root")?,
        )?;
        if binding.extension_id != plugin.id
            || binding.credential_id != credential.id
            || binding.workspace != workspace
            || binding.declaration_fingerprint
                != crate::capability::plugin::credential_declaration_fingerprint(
                    &plugin.credentials,
                )
        {
            return Err("extension binding declaration changed; verify and bind again".into());
        }
        let mut expected_fields = binding.target.clone();
        expected_fields.insert("credential_ref".into(), binding.credential_ref.clone());
        for (field, expected) in &expected_fields {
            if input.get(field).is_some_and(|v| {
                !v.is_null() && v.as_str().is_none_or(|v| !v.is_empty() && v != expected)
            }) {
                return Err("requested target differs from the workspace binding".into());
            }
            input[field] = json!(expected);
        }
        if input
            .get("binding_revision")
            .and_then(Value::as_str)
            .is_some_and(|v| v != binding.revision)
        {
            return Err("extension target revision changed".into());
        }
        input["binding_revision"] = json!(binding.revision);
        Some(binding)
    };
    Ok(Some(PreparedBinding {
        path,
        plugin,
        credential,
        snapshot,
        workspace,
        is_bind,
        expected_revision,
    }))
}

impl PreparedBinding {
    pub(crate) fn execution_lock(
        &self,
    ) -> Result<crate::store::atomic_file::AtomicFileLock, Box<dyn Error>> {
        let lock = crate::store::atomic_file::lock(&self.path)?;
        let current_revision = if self.path.exists() {
            serde_json::from_slice::<Binding>(&std::fs::read(&self.path)?)?.revision
        } else {
            "unbound".into()
        };
        if current_revision != self.expected_revision {
            return Err("extension binding changed after approval".into());
        }
        let current_plugin = crate::capability::plugin::find_plugin(&self.plugin.id)?
            .ok_or("extension removed before execution")?;
        if !current_plugin.enabled
            || current_plugin.version != self.plugin.version
            || current_plugin.path != self.plugin.path
            || crate::capability::plugin::credential_declaration_fingerprint(
                &current_plugin.credentials,
            ) != crate::capability::plugin::credential_declaration_fingerprint(
                &self.plugin.credentials,
            )
        {
            return Err("extension identity or declaration changed after approval".into());
        }
        if let Some(snapshot) = &self.snapshot {
            let current: Binding = serde_json::from_slice(&std::fs::read(&self.path)?)?;
            if &current != snapshot {
                return Err(
                    "extension target changed after approval; request a new approval".into(),
                );
            }
            let fingerprint = crate::capability::plugin::credential_declaration_fingerprint(
                &self.plugin.credentials,
            );
            crate::store::plugin_credentials::broker_instance_fields(
                &self.plugin.id,
                &self.credential.id,
                &snapshot.credential_ref,
                &fingerprint,
            )?;
        }
        Ok(lock)
    }

    pub(crate) fn complete(&self, input: &Value, result: &mut Value) -> Result<(), Box<dyn Error>> {
        if !self.is_bind {
            return Ok(());
        }
        let target = result
            .get("binding_target")
            .and_then(Value::as_object)
            .ok_or("extension did not return a verified binding target")?;
        let policy = self
            .credential
            .workspace_binding
            .as_ref()
            .ok_or("missing binding declaration")?;
        if target.len() != policy.target_fields.len()
            || target.keys().any(|k| !policy.target_fields.contains(k))
        {
            return Err("extension binding target has undeclared fields".into());
        }
        let mut fields = BTreeMap::new();
        for (k, v) in target {
            let s = v
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 2048)
                .ok_or("invalid target field")?;
            if input
                .get(k)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty() && v != s)
            {
                return Err("verified target differs from requested target".into());
            }
            fields.insert(k.clone(), s.to_string());
        }
        let reference = input
            .get("credential_ref")
            .and_then(Value::as_str)
            .ok_or("credential reference required")?;
        let fingerprint =
            crate::capability::plugin::credential_declaration_fingerprint(&self.plugin.credentials);
        let secrets = crate::store::plugin_credentials::broker_instance_fields(
            &self.plugin.id,
            &self.credential.id,
            reference,
            &fingerprint,
        )?;
        let text = serde_json::to_string(&fields)?;
        if self
            .credential
            .fields
            .iter()
            .filter(|f| f.secret)
            .filter_map(|f| secrets.get(&f.key))
            .any(|v| text.contains(v))
        {
            return Err("secret in extension binding target".into());
        }
        let binding = Binding {
            extension_id: self.plugin.id.clone(),
            credential_id: self.credential.id.clone(),
            credential_ref: reference.to_string(),
            workspace: self.workspace.clone(),
            revision: format!("{:032x}", rand::random::<u128>()),
            target: fields,
            declaration_fingerprint: fingerprint,
        };
        crate::store::atomic_file::atomic_write(&self.path, &serde_json::to_vec_pretty(&binding)?)?;
        result["binding"] = serde_json::to_value(binding)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::types::InvocationSource;

    #[test]
    fn bindings_isolate_pages_extensions_and_credentials_and_reject_forged_workspace() {
        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!("generic-binding-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", root.join("private"));
        let home = root.join("private");
        let (plugin, c) =
            crate::app::secure_import::tests::fixture(&home, "com.fixture.alpha", "fixture_alpha_");
        for page in ["a", "b"] {
            std::fs::create_dir_all(root.join("repo").join(page)).unwrap();
        }
        std::fs::create_dir_all(root.join("outside")).unwrap();
        let context = InvocationContext::new(InvocationSource::Mcp, "fixture")
            .with_trusted_workspace(root.join("repo").to_string_lossy().to_string());
        let fingerprint =
            crate::capability::plugin::credential_declaration_fingerprint(&plugin.credentials);
        for page in ["a", "b"] {
            crate::store::plugin_credentials::save_instance(
                &plugin.id,
                &c.id,
                page,
                &BTreeMap::from([("endpoint".into(), "https://example.com".into())]),
                &BTreeMap::from([("token".into(), format!("fixture_alpha_{page}"))]),
                &fingerprint,
            )
            .unwrap();
            let mut input = json!({"page_root":root.join("repo").join(page),"credential_ref":page,"resource":page});
            let prepared = prepare(&context, "fixture.target.bind", &mut input)
                .unwrap()
                .unwrap();
            let _guard = prepared.execution_lock().unwrap();
            let mut output = json!({"binding_target":{"resource":page}});
            prepared.complete(&input, &mut output).unwrap();
        }
        let mut input = json!({"page_root":root.join("repo/a")});
        let prepared = prepare(&context, "fixture.target.publish", &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(input["resource"], "a");
        assert_eq!(input["credential_ref"], "a");
        assert!(input["binding_revision"].is_string());
        assert!(prepare(
            &context,
            "fixture.target.publish",
            &mut json!({"page_root":root.join("outside")})
        )
        .is_err());
        assert!(prepare(
            &context,
            "fixture.target.publish",
            &mut json!({"page_root":root.join("repo/a"),"resource":"b"})
        )
        .is_err());
        assert!(prepare(
            &InvocationContext::new(InvocationSource::Mcp, "untrusted"),
            "fixture.target.publish",
            &mut json!({"page_root":root.join("repo/a")})
        )
        .is_err());
        assert!(prepare(
            &InvocationContext::new(InvocationSource::Mcp, "descriptive-only")
                .with_workspace_ref(root.join("repo").to_string_lossy().to_string()),
            "fixture.target.publish",
            &mut json!({"page_root":root.join("repo/a")})
        )
        .is_err());
        // Two approvals may start at the same revision. Only the first rebind
        // may commit; the other must request a new approval for the new state.
        let mut rebind_input =
            json!({"page_root":root.join("repo/a"),"credential_ref":"b","resource":"b"});
        let pending_rebind = prepare(&context, "fixture.target.bind", &mut rebind_input)
            .unwrap()
            .unwrap();
        let mut concurrent_input =
            json!({"page_root":root.join("repo/a"),"credential_ref":"b","resource":"b"});
        let concurrent = prepare(&context, "fixture.target.bind", &mut concurrent_input)
            .unwrap()
            .unwrap();
        {
            let _guard = concurrent.execution_lock().unwrap();
            concurrent
                .complete(
                    &concurrent_input,
                    &mut json!({"binding_target":{"resource":"b"}}),
                )
                .unwrap();
        }
        assert!(pending_rebind.execution_lock().is_err());
        assert!(prepare(&context, "fixture.target.bind", &mut rebind_input).is_err());
        let mut current: Binding =
            serde_json::from_slice(&std::fs::read(&prepared.path).unwrap()).unwrap();
        current.revision = "changed".into();
        std::fs::write(&prepared.path, serde_json::to_vec(&current).unwrap()).unwrap();
        assert!(prepared.execution_lock().is_err());
        assert_ne!(
            path_for(&prepared.workspace, "com.fixture.other"),
            prepared.path
        );
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }
}
