use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;

pub(crate) const EXTENSION_CANDIDATE_SCHEMA_VERSION: &str = "extension_candidate.v1";
pub(crate) const EXTENSION_LOCK_SCHEMA_VERSION: &str = "extension_lock.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionAssetKind {
    Plugin,
    Skill,
    Workflow,
}

impl ExtensionAssetKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Skill => "skill",
            Self::Workflow => "workflow",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionCandidateState {
    Draft,
    Candidate,
    Tested,
    Confirmed,
    Submitted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionSourceKind {
    Local,
    Github,
    Dashboard,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionSourceRef {
    pub kind: ExtensionSourceKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subdirectory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionDependencyRef {
    pub kind: ExtensionAssetKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_id: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionCandidate {
    pub schema_version: String,
    pub kind: ExtensionAssetKind,
    pub id: String,
    pub version: String,
    pub candidate_sha256: String,
    pub workspace_root: String,
    pub source: ExtensionSourceRef,
    #[serde(default)]
    pub dependencies: Vec<ExtensionDependencyRef>,
    #[serde(default)]
    pub test_report: Value,
    pub state: ExtensionCandidateState,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ExtensionCandidate {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != EXTENSION_CANDIDATE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported extension candidate schema: {}",
                self.schema_version
            )
            .into());
        }
        validate_asset_identity(self.kind, &self.id, &self.version)?;
        validate_sha256("candidate_sha256", &self.candidate_sha256)?;
        if self.workspace_root.trim().is_empty() {
            return Err("extension candidate workspace_root is required".into());
        }
        validate_source_ref(&self.source)?;
        validate_dependencies(&self.dependencies)?;
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err("extension candidate timestamps are required".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionAssetIdentity {
    pub kind: ExtensionAssetKind,
    pub id: String,
    pub version: String,
    pub sha256: String,
}

impl ExtensionAssetIdentity {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        validate_asset_identity(self.kind, &self.id, &self.version)?;
        validate_sha256("asset sha256", &self.sha256)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockDependency {
    pub kind: ExtensionAssetKind,
    pub id: String,
    pub version: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_id: String,
    pub required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockEnvironment {
    #[serde(default)]
    pub capabilities: Vec<ExtensionLockCapability>,
    #[serde(default)]
    pub connectors: Vec<ExtensionLockConnector>,
    #[serde(default)]
    pub runtimes: Vec<ExtensionLockRuntime>,
}

impl ExtensionLockEnvironment {
    pub(crate) fn is_empty(&self) -> bool {
        self.capabilities.is_empty() && self.connectors.is_empty() && self.runtimes.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockCapability {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub availability: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockConnector {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub availability: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential_ownership: String,
    #[serde(default)]
    pub policy_revision: u64,
    #[serde(default)]
    pub credentials: Vec<ExtensionLockConnectorCredential>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockConnectorCredential {
    pub handle: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLockRuntime {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionLock {
    pub schema_version: String,
    pub root: ExtensionAssetIdentity,
    #[serde(default)]
    pub dependencies: Vec<ExtensionLockDependency>,
    #[serde(default, skip_serializing_if = "ExtensionLockEnvironment::is_empty")]
    pub environment: ExtensionLockEnvironment,
    pub generated_at: String,
}

impl ExtensionLock {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != EXTENSION_LOCK_SCHEMA_VERSION {
            return Err(
                format!("unsupported extension lock schema: {}", self.schema_version).into(),
            );
        }
        self.root.validate()?;
        if self.generated_at.trim().is_empty() {
            return Err("extension lock generated_at is required".into());
        }
        for dependency in &self.dependencies {
            validate_asset_identity(dependency.kind, &dependency.id, &dependency.version)?;
            validate_sha256("dependency sha256", &dependency.sha256)?;
            if dependency.kind == self.root.kind
                && dependency.id == self.root.id
                && dependency.version == self.root.version
            {
                return Err("extension lock cannot depend on itself".into());
            }
        }
        let mut identities = std::collections::BTreeSet::new();
        for dependency in &self.dependencies {
            let key = format!(
                "{}:{}:{}",
                dependency.kind.as_str(),
                dependency.id,
                dependency.version
            );
            if !identities.insert(key) {
                return Err("extension lock contains a duplicate dependency".into());
            }
        }
        self.environment.validate()?;
        Ok(())
    }
}

impl ExtensionLockEnvironment {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        let mut capability_ids = std::collections::BTreeSet::new();
        for capability in &self.capabilities {
            validate_asset_id(&capability.id)?;
            validate_optional_label("capability provider", &capability.provider, 500)?;
            validate_optional_label("capability availability", &capability.availability, 50)?;
            if !capability_ids.insert(capability.id.as_str()) {
                return Err("extension lock contains a duplicate environment capability".into());
            }
        }
        let mut connector_ids = std::collections::BTreeSet::new();
        for connector in &self.connectors {
            validate_asset_id(&connector.id)?;
            validate_optional_label("connector availability", &connector.availability, 50)?;
            validate_optional_label(
                "connector credential ownership",
                &connector.credential_ownership,
                50,
            )?;
            if !connector_ids.insert(connector.id.as_str()) {
                return Err("extension lock contains a duplicate environment connector".into());
            }
            let mut handles = std::collections::BTreeSet::new();
            for credential in &connector.credentials {
                validate_asset_id(&credential.handle)?;
                validate_optional_label("connector credential target", &credential.target, 200)?;
                validate_optional_label("connector credential kind", &credential.kind, 50)?;
                if !handles.insert(credential.handle.as_str()) {
                    return Err(format!(
                        "extension lock connector {} contains a duplicate credential",
                        connector.id
                    )
                    .into());
                }
            }
        }
        let mut runtime_ids = std::collections::BTreeSet::new();
        for runtime in &self.runtimes {
            validate_asset_id(&runtime.id)?;
            validate_optional_label("runtime status", &runtime.status, 50)?;
            validate_optional_label("runtime version", &runtime.version, 100)?;
            if !runtime_ids.insert(runtime.id.as_str()) {
                return Err("extension lock contains a duplicate environment runtime".into());
            }
        }
        if self.capabilities.len() > 512 || self.connectors.len() > 256 || self.runtimes.len() > 64
        {
            return Err("extension lock environment contains too many entries".into());
        }
        Ok(())
    }
}

fn validate_optional_label(
    name: &str,
    value: &str,
    max_length: usize,
) -> Result<(), Box<dyn Error>> {
    if value.len() > max_length {
        return Err(format!("{name} is too long").into());
    }
    Ok(())
}

fn validate_source_ref(source: &ExtensionSourceRef) -> Result<(), Box<dyn Error>> {
    if source.id.trim().is_empty() {
        return Err("extension candidate source id is required".into());
    }
    if source.id.len() > 300
        || source.repository.len() > 500
        || source.reference.len() > 300
        || source.commit.len() > 100
        || source.subdirectory.len() > 1000
    {
        return Err("extension candidate source metadata is too long".into());
    }
    Ok(())
}

fn validate_dependencies(dependencies: &[ExtensionDependencyRef]) -> Result<(), Box<dyn Error>> {
    if dependencies.len() > 512 {
        return Err("extension candidate has too many dependencies".into());
    }
    let mut identities = std::collections::BTreeSet::new();
    for dependency in dependencies {
        validate_asset_id(&dependency.id)?;
        if !dependency.version.is_empty() {
            validate_version(&dependency.version)?;
        }
        if !dependency.sha256.is_empty() {
            validate_sha256("dependency sha256", &dependency.sha256)?;
        }
        let key = format!("{}:{}", dependency.kind.as_str(), dependency.id);
        if !identities.insert(key) {
            return Err("extension candidate contains a duplicate dependency".into());
        }
    }
    Ok(())
}

fn validate_asset_identity(
    kind: ExtensionAssetKind,
    id: &str,
    version: &str,
) -> Result<(), Box<dyn Error>> {
    let _ = kind;
    validate_asset_id(id)?;
    validate_version(version)
}

fn validate_asset_id(value: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid extension asset id: {value}").into());
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<(), Box<dyn Error>> {
    let core = value
        .split_once(['-', '+'])
        .map(|(core, _)| core)
        .unwrap_or(value);
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || part.len() > 1 && part.starts_with('0')
        })
    {
        return Err(format!("invalid extension asset version: {value}").into());
    }
    Ok(())
}

fn validate_sha256(name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{name} must be a SHA-256 hex digest").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate(kind: ExtensionAssetKind) -> ExtensionCandidate {
        ExtensionCandidate {
            schema_version: EXTENSION_CANDIDATE_SCHEMA_VERSION.to_string(),
            kind,
            id: "com.example.asset".to_string(),
            version: "1.2.3".to_string(),
            candidate_sha256: "a".repeat(64),
            workspace_root: "C:/workspace".to_string(),
            source: ExtensionSourceRef {
                kind: ExtensionSourceKind::Local,
                id: "local:workspace".to_string(),
                repository: String::new(),
                reference: String::new(),
                commit: String::new(),
                subdirectory: String::new(),
            },
            dependencies: vec![],
            test_report: json!({}),
            state: ExtensionCandidateState::Tested,
            blockers: vec![],
            warnings: vec![],
            created_at: "2026-09-17T00:00:00Z".to_string(),
            updated_at: "2026-09-17T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn candidate_accepts_all_extension_kinds() {
        for kind in [
            ExtensionAssetKind::Plugin,
            ExtensionAssetKind::Skill,
            ExtensionAssetKind::Workflow,
        ] {
            candidate(kind).validate().unwrap();
        }
    }

    #[test]
    fn candidate_rejects_invalid_candidate_digest() {
        let mut value = candidate(ExtensionAssetKind::Workflow);
        value.candidate_sha256 = "not-a-digest".to_string();
        assert!(value.validate().is_err());
    }

    #[test]
    fn lock_rejects_duplicate_and_self_dependencies() {
        let root = ExtensionAssetIdentity {
            kind: ExtensionAssetKind::Workflow,
            id: "com.example.workflow".to_string(),
            version: "1.0.0".to_string(),
            sha256: "a".repeat(64),
        };
        let dependency = ExtensionLockDependency {
            kind: ExtensionAssetKind::Plugin,
            id: "com.example.plugin".to_string(),
            version: "1.0.0".to_string(),
            sha256: "b".repeat(64),
            source_id: "github:example".to_string(),
            required: true,
        };
        let duplicate = ExtensionLock {
            schema_version: EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: root.clone(),
            dependencies: vec![dependency.clone(), dependency],
            environment: ExtensionLockEnvironment {
                capabilities: vec![ExtensionLockCapability {
                    id: "workflow.candidate.freeze".to_string(),
                    provider: "builtin".to_string(),
                    availability: "local".to_string(),
                    required: true,
                }],
                connectors: vec![ExtensionLockConnector {
                    id: "wechat-miniprogram".to_string(),
                    availability: "local".to_string(),
                    credential_ownership: "local".to_string(),
                    policy_revision: 3,
                    credentials: vec![ExtensionLockConnectorCredential {
                        handle: "wechat-upload-private-key".to_string(),
                        target: "private_key_path".to_string(),
                        kind: "file".to_string(),
                        required: true,
                    }],
                    required: true,
                }],
                runtimes: vec![ExtensionLockRuntime {
                    id: "himind.builtin".to_string(),
                    status: "ready".to_string(),
                    version: "0.1.5-rc.2".to_string(),
                    required: true,
                }],
            },
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        assert!(duplicate.validate().is_err());

        let self_dependency = ExtensionLock {
            schema_version: EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: root.clone(),
            dependencies: vec![ExtensionLockDependency {
                kind: root.kind,
                id: root.id,
                version: root.version,
                sha256: root.sha256,
                source_id: String::new(),
                required: true,
            }],
            environment: ExtensionLockEnvironment::default(),
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        assert!(self_dependency.validate().is_err());
    }

    #[test]
    fn schemas_accept_the_rust_contract_shapes() {
        let candidate_schema: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/extension-candidate.schema.json"
        ))
        .unwrap();
        let candidate = serde_json::to_value(candidate(ExtensionAssetKind::Workflow)).unwrap();
        assert!(jsonschema::validator_for(&candidate_schema)
            .unwrap()
            .is_valid(&candidate));

        let lock = ExtensionLock {
            schema_version: EXTENSION_LOCK_SCHEMA_VERSION.to_string(),
            root: ExtensionAssetIdentity {
                kind: ExtensionAssetKind::Workflow,
                id: "com.example.workflow".to_string(),
                version: "1.0.0".to_string(),
                sha256: "a".repeat(64),
            },
            dependencies: vec![ExtensionLockDependency {
                kind: ExtensionAssetKind::Plugin,
                id: "com.example.plugin".to_string(),
                version: "1.0.0".to_string(),
                sha256: "b".repeat(64),
                source_id: "github:example".to_string(),
                required: true,
            }],
            environment: ExtensionLockEnvironment::default(),
            generated_at: "2026-09-17T00:00:00Z".to_string(),
        };
        let lock_schema: Value = serde_json::from_str(include_str!(
            "../contracts/agent-core/v1/extension-lock.schema.json"
        ))
        .unwrap();
        let lock_value = serde_json::to_value(lock).unwrap();
        assert!(jsonschema::validator_for(&lock_schema)
            .unwrap()
            .is_valid(&lock_value));
    }

    #[test]
    fn environment_lock_rejects_duplicate_identities() {
        let environment = ExtensionLockEnvironment {
            capabilities: vec![
                ExtensionLockCapability {
                    id: "workflow.candidate.freeze".to_string(),
                    provider: "builtin".to_string(),
                    availability: "local".to_string(),
                    required: true,
                },
                ExtensionLockCapability {
                    id: "workflow.candidate.freeze".to_string(),
                    provider: "builtin".to_string(),
                    availability: "local".to_string(),
                    required: true,
                },
            ],
            ..Default::default()
        };
        assert!(environment.validate().is_err());
    }
}
