//! Bounded, prompt-free ACP discovery for the shared Mission assignment directory.
use super::*;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub(crate) struct SessionCatalog {
    pub current_model: String,
    pub models: Vec<String>,
}

type Cached = (Instant, Result<SessionCatalog, String>);
static CACHE: OnceLock<Mutex<BTreeMap<String, Cached>>> = OnceLock::new();

pub(crate) fn session_catalog(
    provider: &str,
    workspace: Option<&str>,
) -> Result<SessionCatalog, String> {
    let profile = profile_for(provider).map_err(|error| error.to_string())?;
    let workspace = match workspace.filter(|value| !value.trim().is_empty()) {
        Some(workspace) => {
            process::canonical_workspace(workspace).map_err(|error| error.to_string())?
        }
        None => {
            // A disposable discovery workspace must not become backed-up state.
            let root = crate::store::paths::agent_home().join("acp/catalog-workspace.cache");
            fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            root
        }
    };
    // Include configuration in the key so editing credentials/commands invalidates
    // discovery immediately; do not put credentials into traces or error messages.
    let fingerprint = format!(
        "{provider}:{}:{:x}",
        workspace.display(),
        Sha256::digest(serde_json::to_vec(&profile).map_err(|error| error.to_string())?)
    );
    let cache = CACHE.get_or_init(Default::default);
    if let Some((_, result)) = cache
        .lock()
        .map_err(|_| "ACP catalog cache unavailable")?
        .get(&fingerprint)
        .filter(|(time, _)| time.elapsed() < Duration::from_secs(60))
    {
        return result.clone();
    }
    let result = discover(&profile, provider, workspace);
    let mut entries = cache.lock().map_err(|_| "ACP catalog cache unavailable")?;
    entries.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(60));
    entries.insert(fingerprint, (Instant::now(), result.clone()));
    result
}

fn discover(
    profile: &AcpProfile,
    provider: &str,
    workspace: PathBuf,
) -> Result<SessionCatalog, String> {
    let plan = AcpSessionPlan {
        workspace,
        prompt: String::new(),
        policy: PermissionPolicy::Deny,
        attribution: AcpAttribution {
            provider: provider.into(),
            run_id: "catalog".into(),
        },
        mcp: None,
        requested_model: None,
        probe_only: true,
    };
    let mut process = AcpProcess::spawn(
        profile,
        plan,
        AcpTrace {
            mode: AcpTraceMode::Off,
            path: PathBuf::new(),
        },
    )
    .map_err(|error| error.to_string())?;
    let outcome = match process.drive(Duration::from_secs(20), &mut || Ok(false)) {
        Ok(outcome) => outcome,
        Err(error) => {
            let failure = session_failure(process, &error.to_string());
            if failure.contains("ENOTCACHED") {
                return Err("ACP 适配器尚未安装或未缓存，请先在执行器设置中完成接入；目录探测不会自动下载软件。".into());
            }
            return Err(failure);
        }
    };
    process.finish().map_err(|error| error.to_string())?;
    Ok(SessionCatalog {
        current_model: outcome.model,
        models: outcome.model_choices,
    })
}
