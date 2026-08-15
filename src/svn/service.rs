use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;
use walkdir::{DirEntry, WalkDir};

use crate::store::credentials::{
    list_local_svn_connections, load_local_svn_connection_secret, remove_local_svn_connection,
    save_local_svn_connection, update_local_svn_connection_status,
};
use crate::svn::types::{
    ApplyProjectAclRequest, CloneExhibitRepositoryRequest, CreateExhibitRepositoryPathRequest,
    CreateRepositoryRequest, EnsureProjectExhibitsAccessRequest, ImportLocalExhibitRequest,
    InitializeExhibitRepositoryRequest, MigrationIgnorePolicy, MigrationSourceScanRequest,
    PreviewProjectAclRequest, ProjectAclEntry, ReconcileProjectAclRequest,
    SaveSvnConnectionRequest, SvnCheckoutRequest, SvnConnectionSummary, SvnWorkspaceRequest,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const SVN_CONNECTION_ID: &str = "company-svn";
const SVN_ADMIN_CONNECTION_ID: &str = "company-svn-admin";
const SVN_ADMIN_URL: &str = "http://svn.andcrane.com";
const SVN_SERVICE_URL: &str = "http://svn.andcrane.com/repo";
const DEFAULT_SVN_USER_PASSWORD: &str = "123456";
const UNITY_TEMPLATE_URL: &str = "http://svn.andcrane.com/repo/UNIArtTemplate";
const UNREAL_TEMPLATE_ROOT_URL: &str = "http://svn.andcrane.com/repo/repo_UETemplates";
const TEMPLATE_MARKER_FILE: &str = ".himind-template.json";
const SVN_ADMIN_READ_ATTEMPTS: usize = 3;
const SVN_ADMIN_CREATE_PATH_ATTEMPTS: usize = 2;
const PROJECT_REPOSITORY_BROAD_ACL: [(&str, &str); 3] =
    [("/", "r"), ("/trunk", "r"), ("/trunk/exhibits", "no")];
const MIGRATION_PROPERTY_NAMES: [&str; 7] = [
    "svn:ignore",
    "svn:externals",
    "svn:mime-type",
    "svn:eol-style",
    "svn:keywords",
    "svn:executable",
    "svn:needs-lock",
];

#[derive(Clone, Default)]
struct SvnDiagnosticContext {
    task_id: String,
    execution_id: String,
}

thread_local! {
    static SVN_DIAGNOSTIC_CONTEXT: RefCell<SvnDiagnosticContext> = RefCell::new(SvnDiagnosticContext::default());
}

static SVN_DIAGNOSTIC_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static SVN_ADMIN_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

pub(crate) struct SvnDiagnosticContextGuard {
    previous: SvnDiagnosticContext,
}

impl SvnDiagnosticContextGuard {
    pub(crate) fn enter(task_id: &str, execution_id: &str) -> Self {
        let next = SvnDiagnosticContext {
            task_id: task_id.to_string(),
            execution_id: execution_id.to_string(),
        };
        let previous = SVN_DIAGNOSTIC_CONTEXT.with(|context| context.replace(next));
        Self { previous }
    }
}

impl Drop for SvnDiagnosticContextGuard {
    fn drop(&mut self) {
        SVN_DIAGNOSTIC_CONTEXT.with(|context| {
            context.replace(self.previous.clone());
        });
    }
}

#[derive(Debug)]
struct SvnAdminRequestError {
    action: String,
    category: &'static str,
    attempt: usize,
    max_attempts: usize,
    http_status: Option<u16>,
    elapsed_ms: u128,
    retryable: bool,
    message: String,
}

impl fmt::Display for SvnAdminRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SvnAdmin request failed (action={}, category={}, attempt={}/{}, http_status={}, elapsed_ms={}): {}",
            self.action,
            self.category,
            self.attempt,
            self.max_attempts,
            self.http_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.elapsed_ms,
            self.message
        )
    }
}

impl Error for SvnAdminRequestError {}

#[derive(Debug)]
struct ExhibitImportPartialFailure {
    message: String,
    result: Value,
}

impl fmt::Display for ExhibitImportPartialFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ExhibitImportPartialFailure {}

pub(crate) fn task_failure_result(error: &(dyn Error + 'static)) -> Option<Value> {
    error
        .downcast_ref::<ExhibitImportPartialFailure>()
        .map(|failure| failure.result.clone())
}

pub(crate) fn bootstrap_svn_credentials() -> Result<bool, Box<dyn Error>> {
    let username = std::env::var("SVN_USERNAME").unwrap_or_default();
    let password = std::env::var("SVN_PASSWORD").unwrap_or_default();
    if username.trim().is_empty() && password.is_empty() {
        return Ok(false);
    }
    if username.trim().is_empty() || password.is_empty() {
        return Err("SVN_USERNAME and SVN_PASSWORD must be configured together".into());
    }
    save_local_svn_connection(
        SVN_CONNECTION_ID,
        "公司 SVN",
        SVN_SERVICE_URL,
        username.trim(),
        &password,
        "svn",
    )?;
    unsafe {
        std::env::remove_var("SVN_USERNAME");
        std::env::remove_var("SVN_PASSWORD");
    }
    Ok(true)
}

pub(crate) fn bootstrap_svn_admin_credentials() -> Result<bool, Box<dyn Error>> {
    let username = std::env::var("SVN_ADMIN_USERNAME").unwrap_or_default();
    let password = std::env::var("SVN_ADMIN_PASSWORD").unwrap_or_default();
    if username.trim().is_empty() && password.is_empty() {
        return Ok(false);
    }
    if username.trim().is_empty() || password.is_empty() {
        return Err("SVN_ADMIN_USERNAME and SVN_ADMIN_PASSWORD must be configured together".into());
    }
    install_svn_admin_credentials(username.trim(), &password)?;
    unsafe {
        std::env::remove_var("SVN_ADMIN_USERNAME");
        std::env::remove_var("SVN_ADMIN_PASSWORD");
    }
    Ok(true)
}

pub(crate) fn install_svn_admin_credentials(
    username: &str,
    password: &str,
) -> Result<(), Box<dyn Error>> {
    if username.trim().is_empty() || password.is_empty() {
        return Err("SVN management credentials are incomplete".into());
    }
    save_local_svn_connection(
        SVN_ADMIN_CONNECTION_ID,
        "公司 SVN 管理",
        SVN_ADMIN_URL,
        username.trim(),
        password,
        "svnadmin_v2",
    )?;
    Ok(())
}

pub(crate) fn default_svn_username(display_name: &str) -> Result<String, Box<dyn Error>> {
    let mut username = display_name.trim();
    for suffix in ["（软件）", "(软件)"] {
        if let Some(value) = username.strip_suffix(suffix) {
            username = value.trim();
            break;
        }
    }
    if username.is_empty() || username.len() > 200 || username.contains(['\r', '\n']) {
        return Err("HiMind user name cannot be used as an SVN username".into());
    }
    Ok(username.to_string())
}

pub(crate) fn ensure_default_svn_credentials(username: &str) -> Result<bool, Box<dyn Error>> {
    let username = default_svn_username(username)?;
    if list_local_svn_connections()?.into_iter().any(|item| {
        item.id == SVN_CONNECTION_ID
            && item.username == username
            && !item.encrypted_password.is_empty()
    }) {
        return Ok(false);
    }
    let password = DEFAULT_SVN_USER_PASSWORD.to_string();
    login_svn_user(&username, &password)?;
    save_local_svn_connection(
        SVN_CONNECTION_ID,
        "公司 SVN",
        SVN_SERVICE_URL,
        &username,
        &password,
        "svn",
    )?;
    update_local_svn_connection_status(SVN_CONNECTION_ID, "ready", "")?;
    Ok(true)
}

pub(crate) fn svn_admin_ready() -> bool {
    load_local_svn_connection_secret(SVN_ADMIN_CONNECTION_ID).is_ok()
}

pub(crate) fn svn_admin_status() -> &'static str {
    let connections = match list_local_svn_connections() {
        Ok(connections) => connections,
        Err(_) => return "unreadable",
    };
    if !connections.iter().any(|item| {
        item.id == SVN_ADMIN_CONNECTION_ID && !item.encrypted_password.trim().is_empty()
    }) {
        return "missing";
    }
    if svn_admin_ready() {
        "ready"
    } else {
        "unreadable"
    }
}

pub(crate) fn provision_default_svn_user_account(username: &str) -> Result<Value, Box<dyn Error>> {
    let username = default_svn_username(username)?;
    if !ensure_svn_user_account(&username, DEFAULT_SVN_USER_PASSWORD)? {
        return Err("SvnAdmin credentials are not configured on this Agent".into());
    }
    Ok(json!({
        "ok": true,
        "svn_username": username,
        "verified": true,
        "password_policy": "default-v1"
    }))
}

pub(crate) fn list_connections() -> Result<Vec<SvnConnectionSummary>, Box<dyn Error>> {
    let connections = list_local_svn_connections()?;
    let selected = connections
        .into_iter()
        .find(|item| item.id == SVN_CONNECTION_ID)
        .or_else(|| {
            list_local_svn_connections()
                .ok()?
                .into_iter()
                .find(|item| item.provider == "svn")
        });
    Ok(selected
        .map(|item| SvnConnectionSummary {
            id: item.id,
            name: "公司 SVN".to_string(),
            base_url: SVN_SERVICE_URL.to_string(),
            username: item.username,
            provider: "svn".to_string(),
            credentials_configured: !item.encrypted_password.is_empty(),
            status: item.status,
            last_error: item.last_error,
        })
        .into_iter()
        .collect())
}

pub(crate) fn save_connection(
    request: SaveSvnConnectionRequest,
) -> Result<SvnConnectionSummary, Box<dyn Error>> {
    let username = required_value(&request.username, "SVN username")?;
    save_local_svn_connection(
        SVN_CONNECTION_ID,
        "公司 SVN",
        SVN_SERVICE_URL,
        &username,
        &request.password,
        "svn",
    )?;
    Ok(SvnConnectionSummary {
        id: SVN_CONNECTION_ID.to_string(),
        name: "公司 SVN".to_string(),
        base_url: SVN_SERVICE_URL.to_string(),
        username,
        provider: "svn".to_string(),
        credentials_configured: true,
        status: "configured".to_string(),
        last_error: String::new(),
    })
}

pub(crate) fn remove_connection() -> Result<bool, Box<dyn Error>> {
    if remove_local_svn_connection(SVN_CONNECTION_ID)? {
        return Ok(true);
    }
    let legacy_id = list_local_svn_connections()?
        .into_iter()
        .find(|item| item.provider == "svn")
        .map(|item| item.id);
    match legacy_id {
        Some(id) => remove_local_svn_connection(&id),
        None => Ok(false),
    }
}

pub(crate) fn test_connection() -> Result<Value, Box<dyn Error>> {
    let (connection, password) = match load_company_svn_secret() {
        Ok(value) => value,
        Err(error) => {
            let _ =
                update_local_svn_connection_status(SVN_CONNECTION_ID, "invalid", "本地凭据不可用");
            return Err(error);
        }
    };
    if let Err(error) = login_svn_user(&connection.username, &password) {
        let message = error.to_string();
        let unreachable = message.starts_with("SVN service returned HTTP")
            || message.contains("timed out")
            || message.contains("connect");
        let _ = update_local_svn_connection_status(
            SVN_CONNECTION_ID,
            if unreachable {
                "unreachable"
            } else {
                "invalid"
            },
            if unreachable {
                "SVN 服务不可用"
            } else {
                "SVN 账号或密码无效"
            },
        );
        return Err(error);
    }
    update_local_svn_connection_status(SVN_CONNECTION_ID, "ready", "")?;
    Ok(json!({
        "connection_id": SVN_CONNECTION_ID,
        "provider": "svn",
        "status": "ready",
        "authenticated": true,
        "username": connection.username
    }))
}

fn login_svn_user(username: &str, password: &str) -> Result<Value, Box<dyn Error>> {
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let mut child = Command::new(&executable)
        .args([
            "info".to_string(),
            SVN_SERVICE_URL.to_string(),
            "--non-interactive".to_string(),
            "--no-auth-cache".to_string(),
            "--username".to_string(),
            username.to_string(),
            "--password-from-stdin".to_string(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.as_bytes())?;
        stdin.write_all(b"\r\n")?;
    }
    let output = child.wait_with_output()?;
    let stderr = decode_svn_cli_output(&output.stderr);
    if output.status.success() {
        return Ok(json!({ "authenticated": true, "username": username }));
    }
    if stderr.contains("E215004")
        || stderr.contains("E170001")
        || stderr.contains("Authentication failed")
    {
        return Err("SVN account or password is invalid".into());
    }
    // The repository root URL is not itself a repository, so an info probe
    // legitimately fails after authentication succeeded (E190001/E170013).
    Ok(json!({ "authenticated": true, "username": username }))
}

fn ensure_svn_user_account(username: &str, password: &str) -> Result<bool, Box<dyn Error>> {
    let Ok((admin, admin_password)) = load_local_svn_connection_secret(SVN_ADMIN_CONNECTION_ID)
    else {
        return Ok(false);
    };
    let token = login_svnadmin(&admin.username, &admin_password)?;
    let list = svnadmin_post_read(
        "Svnuser",
        "GetUserList",
        Some(&token),
        json!({
            "pageSize": 10000,
            "currentPage": 1,
            "searchKeyword": "",
            "sortName": "svn_user_name",
            "sortType": "asc",
            "sync": false,
            "page": true
        }),
    )?;
    ensure_svnadmin_success(&list)?;
    let existing = list
        .pointer("/data/data")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("svn_user_name").and_then(Value::as_str) == Some(username))
        });
    if login_svn_user(username, password).is_ok() {
        return Ok(true);
    }
    if existing.is_some() {
        return Err(
            "SVN account already exists but its password is not the initial default; refusing to reset it"
                .into(),
        );
    }
    let mutation_error = svnadmin_post(
        "Svnuser",
        "CreateUser",
        Some(&token),
        json!({
            "svn_user_name": username,
            "svn_user_pass": password,
            "svn_user_note": "HiMind 用户"
        }),
    )
    .and_then(|response| ensure_svnadmin_success(&response))
    .err();
    if let Some(error) = mutation_error {
        if login_svn_user(username, password).is_ok() {
            record_svn_diagnostic_event(
                "svnadmin",
                "CreateUser",
                "recovered_by_login",
                "write_response_uncertain",
                1,
                1,
                None,
                0,
            );
            return Ok(true);
        }
        return Err(error);
    }
    login_svn_user(username, password)?;
    Ok(true)
}

pub(crate) fn checkout_workspace(request: SvnCheckoutRequest) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let (connection, password) = load_company_svn_secret()?;
    let repository_url =
        checkout_repository_url(request.repository_url.as_deref(), &project_id, &exhibit_id)?;
    let candidate = absolute_path(&request.target_path)?;
    reject_sensitive_path(&candidate)?;
    let target_uuid = svn_remote_item(
        &repository_url,
        "repos-uuid",
        &connection.username,
        &password,
    )?;
    if target_uuid.trim().is_empty() {
        return Err("SVN target repository did not return a repository UUID".into());
    }
    let (target, output, checkout_mode, backup_retained, preserved_property_count) =
        if is_valid_svn_working_copy(&candidate) {
            let status = workspace_status_path(&candidate)?;
            let current_url = status
                .get("repository_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let current_uuid = status
                .get("repository_uuid")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if current_uuid == target_uuid {
                if same_svn_url(current_url, &repository_url) {
                    let output = run_svn_authenticated(
                        [
                            "update".to_string(),
                            "--ignore-externals".to_string(),
                            candidate.to_string_lossy().to_string(),
                        ],
                        &connection.username,
                        &password,
                    )?;
                    (candidate, output, "update", false, 0)
                } else {
                    if status
                        .get("change_count")
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
                        > 0
                    {
                        return Err(
                            "same SVN repository but different path; commit or back up local changes before switching"
                                .into(),
                        );
                    }
                    let output = switch_same_repository_workspace(
                        &candidate,
                        current_url,
                        &repository_url,
                        &connection.username,
                        &password,
                    )?;
                    (candidate, output, "switch", false, 0)
                }
            } else {
                takeover_non_empty_workspace(
                    &candidate,
                    &repository_url,
                    &target_uuid,
                    &connection.username,
                    &password,
                )?
            }
        } else {
            let target = validate_checkout_target(&request.target_path)?;
            let non_empty = target.is_dir()
                && (target.join(".svn").is_dir() || working_copy_contains_content(&target)?);
            if non_empty {
                takeover_non_empty_workspace(
                    &target,
                    &repository_url,
                    &target_uuid,
                    &connection.username,
                    &password,
                )?
            } else {
                let output = run_svn_authenticated(
                    [
                        "checkout".to_string(),
                        "--ignore-externals".to_string(),
                        repository_url.clone(),
                        target.to_string_lossy().to_string(),
                    ],
                    &connection.username,
                    &password,
                )?;
                (target, output, "checkout", false, 0)
            }
        };
    let external_sync = sync_workspace_externals(&target, &connection.username, &password);
    let status = workspace_status_path(&target)?;
    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "exhibit_id": exhibit_id,
        "repository_url": repository_url,
        "repository_uuid": target_uuid,
        "checkout_mode": checkout_mode,
        "backup_retained": backup_retained,
        "preserved_property_count": preserved_property_count,
        "external_sync": external_sync,
        "target_path": target,
        "output": output,
        "workspace": status
    }))
}

fn sync_workspace_externals(target: &Path, username: &str, password: &str) -> Value {
    match run_svn_authenticated(
        workspace_externals_update_arguments(target),
        username,
        password,
    ) {
        Ok(output) => json!({ "status": "ready", "output": output }),
        Err(error) => json!({
            "status": "warning",
            "error": error.to_string(),
            "message": "主工程已检出，但部分外部依赖未能更新；本地已有 external 目录保持不变"
        }),
    }
}

fn workspace_externals_update_arguments(target: &Path) -> [String; 2] {
    ["update".to_string(), target.to_string_lossy().to_string()]
}

fn svn_remote_item(
    repository_url: &str,
    item: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn Error>> {
    run_svn_authenticated(
        [
            "info".to_string(),
            "--show-item".to_string(),
            item.to_string(),
            repository_url.to_string(),
        ],
        username,
        password,
    )
    .map(|value| value.trim().to_string())
}

fn same_svn_url(left: &str, right: &str) -> bool {
    left.trim_end_matches('/')
        .eq_ignore_ascii_case(right.trim_end_matches('/'))
}

fn switch_same_repository_workspace(
    target: &Path,
    current_url: &str,
    target_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn Error>> {
    let current_root = svn_item(target, "repos-root-url")?;
    let target_root =
        svn_remote_item(target_url, "repos-root-url", username, password).unwrap_or_default();
    let current_suffix = svn_repository_relative_url(current_url, &current_root);
    let target_suffix = svn_repository_relative_url(target_url, &target_root);
    let arguments = if !current_root.trim().is_empty()
        && !target_root.trim().is_empty()
        && current_suffix.is_some()
        && current_suffix == target_suffix
        && !same_svn_url(&current_root, &target_root)
    {
        vec![
            "switch".to_string(),
            "--ignore-externals".to_string(),
            "--relocate".to_string(),
            current_root,
            target_root,
            target.to_string_lossy().to_string(),
        ]
    } else {
        vec![
            "switch".to_string(),
            "--ignore-externals".to_string(),
            target_url.to_string(),
            target.to_string_lossy().to_string(),
        ]
    };
    run_svn_authenticated(arguments, username, password)
}

fn svn_repository_relative_url(url: &str, root: &str) -> Option<String> {
    let url = Url::parse(url.trim_end_matches('/')).ok()?;
    let root = Url::parse(root.trim_end_matches('/')).ok()?;
    if url.scheme() != root.scheme()
        || url.host_str() != root.host_str()
        || url.port_or_known_default() != root.port_or_known_default()
    {
        return None;
    }
    let root_path = root.path().trim_end_matches('/');
    let url_path = url.path().trim_end_matches('/');
    let suffix = url_path.strip_prefix(root_path)?;
    Some(suffix.trim_matches('/').to_string())
}

fn takeover_non_empty_workspace(
    target: &Path,
    repository_url: &str,
    target_uuid: &str,
    username: &str,
    password: &str,
) -> Result<(PathBuf, String, &'static str, bool, usize), Box<dyn Error>> {
    let had_svn_metadata = target.join(".svn").is_dir();
    let had_working_copy = is_valid_svn_working_copy(target);
    let old_paths = workspace_path_manifest(target)?;
    let snapshot = if had_working_copy {
        snapshot_migration_metadata(target, false, None)?
    } else {
        MigrationMetadataSnapshot::default()
    };
    let mut backup = if had_svn_metadata {
        Some(WorkingCopyAdminBackup::create(target)?)
    } else {
        None
    };
    let result = (|| -> Result<(String, usize), Box<dyn Error>> {
        let output = run_svn_authenticated(
            [
                "checkout".to_string(),
                "--force".to_string(),
                "--ignore-externals".to_string(),
                repository_url.to_string(),
                target.to_string_lossy().to_string(),
            ],
            username,
            password,
        )?;
        let switched_url = svn_item(target, "url")?;
        let switched_uuid = svn_item(target, "repos-uuid")?;
        if !same_svn_url(&switched_url, repository_url) || switched_uuid.trim() != target_uuid {
            return Err("local workspace switched to an unexpected SVN repository".into());
        }
        let preserved_property_count =
            preserve_missing_migration_properties(target, &snapshot.properties)?;
        Ok((output, preserved_property_count))
    })();
    match result {
        Ok((output, preserved_property_count)) => {
            if let Some(backup) = backup.as_mut() {
                backup.retain();
            }
            Ok((
                target.to_path_buf(),
                output,
                if had_working_copy {
                    "takeover-old-svn"
                } else {
                    "takeover-existing"
                },
                had_svn_metadata,
                preserved_property_count,
            ))
        }
        Err(error) => {
            cleanup_partial_checkout(target, &old_paths)?;
            if let Some(backup) = backup.as_mut() {
                backup.rollback()?;
            }
            Err(error)
        }
    }
}

fn preserve_missing_migration_properties(
    working_copy: &Path,
    properties: &[MigrationProperty],
) -> Result<usize, Box<dyn Error>> {
    let mut pending = Vec::new();
    for property in properties {
        if property.value.is_empty() {
            continue;
        }
        let target = working_copy.join(&property.relative_path);
        if !target.exists() {
            continue;
        }
        let current = run_svn_in_directory_owned(
            &target,
            vec![
                "propget".to_string(),
                "--strict".to_string(),
                property.name.clone(),
                ".".to_string(),
            ],
        )
        .unwrap_or_default();
        if current.trim().is_empty() {
            pending.push(property.clone());
        }
    }
    let count = pending.len();
    run_migration_propset_batches(working_copy, pending.iter())?;
    Ok(count)
}

fn workspace_path_manifest(root: &Path) -> Result<BTreeSet<PathBuf>, Box<dyn Error>> {
    if !root.is_dir() {
        return Ok(BTreeSet::new());
    }
    let mut paths = BTreeSet::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(root)?;
        if relative.as_os_str().is_empty() || relative == Path::new(".svn") {
            continue;
        }
        if relative.starts_with(".svn") {
            continue;
        }
        paths.insert(relative.to_path_buf());
    }
    Ok(paths)
}

fn cleanup_partial_checkout(
    root: &Path,
    original_paths: &BTreeSet<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    if !root.is_dir() {
        return Ok(());
    }
    let admin = root.join(".svn");
    if admin.exists() {
        std::fs::remove_dir_all(admin)?;
    }
    for entry in WalkDir::new(root)
        .contents_first(true)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry?;
        let relative = entry.path().strip_prefix(root)?;
        if relative.as_os_str().is_empty()
            || relative.starts_with(".svn")
            || original_paths.contains(relative)
        {
            continue;
        }
        if entry.file_type().is_dir() {
            let _ = std::fs::remove_dir(entry.path());
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn create_exhibit_repository_path(
    request: CreateExhibitRepositoryPathRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let response = svnadmin_post_with_attempts(
        "Svnrep",
        "CreateRepFolder",
        Some(&token),
        json!({
            "rep_name": project_id,
            "path": "/trunk/exhibits/",
            "folder_name": exhibit_id
        }),
        SVN_ADMIN_CREATE_PATH_ATTEMPTS,
    )?;
    if let Err(error) = ensure_svnadmin_success(&response) {
        let message = error.to_string();
        if !is_existing_repository_path_error(&message) {
            return Err(error);
        }
        return Ok(json!({
            "ok": true,
            "created": false,
            "already_exists": true,
            "project_id": project_id,
            "exhibit_id": exhibit_id,
            "repository_url": repository_url,
            "revision": 0,
            "result": response.get("data").cloned().unwrap_or(Value::Null)
        }));
    }
    let revision = response
        .pointer("/data/revision")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Ok(json!({
        "ok": true,
        "created": true,
        "project_id": project_id,
        "exhibit_id": exhibit_id,
        "repository_url": repository_url,
        "revision": revision,
        "result": response.get("data").cloned().unwrap_or(Value::Null)
    }))
}

pub(crate) fn initialize_exhibit_repository(
    request: InitializeExhibitRepositoryRequest,
) -> Result<Value, Box<dyn Error>> {
    initialize_exhibit_repository_with_cancel(request, &mut || Ok(()))
}

pub(crate) fn clone_exhibit_repository(
    request: CloneExhibitRepositoryRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let source_url = request.source_repository_url.trim().trim_end_matches('/');
    let expected_prefix = format!("{SVN_SERVICE_URL}/");
    if !source_url.starts_with(&expected_prefix) || !source_url.contains("/trunk/exhibits/") {
        return Err("source_repository_url must be a HiMind exhibit SVN URL".into());
    }
    let target_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    if source_url.eq_ignore_ascii_case(target_url.trim_end_matches('/')) {
        return Err("source and target exhibit repositories must be different".into());
    }
    let (connection, password) = load_company_svn_secret()?;
    let commit_message = format!("Clone exhibit {exhibit_id} from {source_url}");
    let copy_result = run_svn_authenticated(
        [
            "copy".to_string(),
            source_url.to_string(),
            target_url.clone(),
            "-m".to_string(),
            commit_message.clone(),
        ],
        &connection.username,
        &password,
    );
    let (revision, output, recovered_after_error) = match copy_result {
        Ok(output) => {
            let revision = run_svn_authenticated(
                [
                    "info".to_string(),
                    "--show-item".to_string(),
                    "revision".to_string(),
                    target_url.clone(),
                ],
                &connection.username,
                &password,
            )?;
            (
                revision.trim().parse::<u64>().unwrap_or_default(),
                output,
                false,
            )
        }
        Err(copy_error) => {
            record_svn_diagnostic_event(
                "svn_cli",
                "clone_exhibit",
                "verifying_uncertain_result",
                "copy_response_error",
                1,
                1,
                None,
                0,
            );
            let (revision, remote_message) = svn_remote_latest_log(
                &target_url,
                &connection.username,
                &password,
            )
            .map_err(|verification_error| {
                format!(
                    "SVN copy result is uncertain: {copy_error}; target verification failed: {verification_error}"
                )
            })?;
            if remote_message != commit_message {
                return Err(format!(
                    "SVN copy result is uncertain: {copy_error}; target commit message did not match this clone request"
                )
                .into());
            }
            record_svn_diagnostic_event(
                "svn_cli",
                "clone_exhibit",
                "recovered_by_server_verification",
                "copy_response_error",
                1,
                1,
                None,
                0,
            );
            (
                revision,
                "SVN copy response was interrupted; target commit verified".to_string(),
                true,
            )
        }
    };
    Ok(json!({
        "ok": true,
        "cloned": true,
        "project_id": project_id,
        "exhibit_id": exhibit_id,
        "source_repository_url": source_url,
        "repository_url": target_url,
        "revision": revision,
        "recovered_after_error": recovered_after_error,
        "output": output
    }))
}

#[derive(Debug, Deserialize)]
struct SvnLogDocument {
    #[serde(rename = "logentry", default)]
    entries: Vec<SvnLogEntry>,
}

#[derive(Debug, Deserialize)]
struct SvnLogEntry {
    #[serde(rename = "@revision")]
    revision: u64,
    #[serde(default)]
    msg: String,
}

fn svn_remote_latest_log(
    repository_url: &str,
    username: &str,
    password: &str,
) -> Result<(u64, String), Box<dyn Error>> {
    let output = run_svn_authenticated(
        [
            "log".to_string(),
            "--xml".to_string(),
            "--limit".to_string(),
            "1".to_string(),
            repository_url.to_string(),
        ],
        username,
        password,
    )?;
    let document: SvnLogDocument = quick_xml::de::from_str(&output)?;
    let entry = document
        .entries
        .into_iter()
        .next()
        .ok_or("target SVN path has no commit history")?;
    Ok((entry.revision, entry.msg))
}

pub(crate) fn import_local_exhibit_with_cancel_and_progress<F, P>(
    request: ImportLocalExhibitRequest,
    cancel: &mut F,
    progress: &mut P,
) -> Result<Value, Box<dyn Error>>
where
    F: FnMut() -> Result<(), Box<dyn Error>>,
    P: FnMut(i32, &str) -> Result<(), Box<dyn Error>>,
{
    let ignore_policy = normalized_ignore_policy(&request.ignore_policy);
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let source = absolute_path(&request.source_path)?;
    reject_sensitive_path(&source)?;
    if !source.is_dir() {
        return Err("source_path must be an existing directory".into());
    }

    cancel()?;
    progress(12, "正在检查本地工程和旧 SVN 工作副本")?;
    let source_has_svn_metadata = source.join(".svn").is_dir();
    let source_is_working_copy = is_valid_svn_working_copy(&source);
    if source_is_working_copy {
        let source_repository_url = svn_item(&source, "url")?;
        let target_repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
        if request.force_migration
            && source_repository_url
                .trim_end_matches('/')
                .eq_ignore_ascii_case(target_repository_url.trim_end_matches('/'))
        {
            return Err("source and target exhibit repositories must be different".into());
        }
        let old_remote_status = probe_svn_remote(&source_repository_url, Duration::from_secs(5));
        match old_remote_status.as_str() {
			"reachable" if !request.force_migration => {
				let revision = svn_item(&source, "revision")?;
				let change_count = svn_status_change_count(&source)?;
				progress(100, "旧 SVN 仓库有效，已直接保留并关联当前工作副本")?;
				return Ok(json!({
					"ok": true,
					"imported": false,
					"linked_existing": true,
					"adopted_existing_working_copy": false,
					"project_id": project_id,
					"exhibit_id": exhibit_id,
					"repository_url": source_repository_url,
					"source_repository_url": source_repository_url,
					"workspace_path": source,
					"revision": revision.trim().parse::<u64>().unwrap_or_default(),
					"change_count": change_count,
					"old_remote_status": old_remote_status,
					"backup_retained": false
				}));
			}
			"reachable" => {
				progress(18, "旧 SVN 仓库有效，正在按强制迁移请求重建到目标展项仓库")?;
			}
			"missing" => {}
			"temporarily_unreachable" | "authorization_unknown" if request.force_migration => {
				progress(
					18,
					"无法验证旧 SVN 仓库；正在按明确的强制迁移请求重建到目标展项仓库",
				)?;
			}
			"temporarily_unreachable" | "authorization_unknown" => {
				return Err(
					"cannot confirm whether the existing SVN repository is valid; refusing to rebuild automatically"
						.into(),
				)
			}
			_ => return Err("unexpected existing SVN repository status".into()),
		}
    }
    progress(19, "正在读取旧 SVN 忽略规则、文件属性和外部依赖")?;
    let snapshot = if source_is_working_copy {
        snapshot_migration_metadata(&source, false, Some(&ignore_policy))?
    } else {
        MigrationMetadataSnapshot::default()
    };
    let transformed_paths = migration_transform_paths(&snapshot.properties);
    // Include locally retained files in the source fingerprint so an ignored
    // archive cannot change unnoticed while the repository is being prepared.
    let source_stability_before =
        migration_source_stability_summary(&source, &snapshot.external_roots, &transformed_paths)?;
    progress(
        20,
        &format!(
            "已核对 {} 个工程文件，正在确认工程未发生变化",
            source_stability_before.file_count
        ),
    )?;
    if !request.expected_source_fingerprint.trim().is_empty()
        && request
            .expected_source_fingerprint
            .trim()
            .trim_start_matches("sha256:")
            != source_stability_before.digest
    {
        return Err(
            "source project changed after scanning; scan the directory again before migration"
                .into(),
        );
    }

    progress(22, "本地工程预检完成，正在创建目标展项仓库")?;
    create_exhibit_repository_path(CreateExhibitRepositoryPathRequest {
        project_id: project_id.clone(),
        exhibit_id: exhibit_id.clone(),
    })?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let (connection, password) = load_company_svn_secret()?;
    ensure_exhibit_writer_access(&project_id, &exhibit_id, &connection.username)?;
    let target_uuid = svn_remote_item(
        &repository_url,
        "repos-uuid",
        &connection.username,
        &password,
    )?;
    if target_uuid.trim().is_empty() {
        return Err("target exhibit repository did not return a repository UUID".into());
    }
    let temp_root = std::env::temp_dir().join(format!(
        "himind-local-import-{}-{}-{}",
        std::process::id(),
        exhibit_id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    ));
    let working_copy = temp_root.join("target");
    std::fs::create_dir_all(&temp_root)?;
    let result = (|| -> Result<Value, Box<dyn Error>> {
        cancel()?;
        progress(32, "目标展项仓库已就绪，正在检出临时工作副本")?;
        run_svn_authenticated_cancelable(
            [
                "checkout".to_string(),
                "--ignore-externals".to_string(),
                repository_url.clone(),
                working_copy.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
            cancel,
        )?;
        let mut target_was_empty = !working_copy_contains_content(&working_copy)?;
        if request.force_migration && !target_was_empty {
            progress(40, "正在清空目标展项仓库内容并准备强制重建")?;
            clear_working_copy_content(&working_copy)?;
            if working_copy_contains_content(&working_copy)? {
                return Err(
                    "failed to clear the target exhibit repository before forced migration".into(),
                );
            }
            target_was_empty = true;
        }

        progress(
            45,
            &format!(
                "正在复制 {} 个工程文件并保留本地修改",
                source_stability_before.file_count
            ),
        )?;
        let source_summary = copy_migration_tree(
            &source,
            &working_copy,
            &snapshot.external_roots,
            &transformed_paths,
            &ignore_policy,
        )?;
        let source_stability_after = migration_source_stability_summary(
            &source,
            &snapshot.external_roots,
            &transformed_paths,
        )?;
        if source_stability_before != source_stability_after {
            return Err(
                "source project changed while preparing migration; refusing to continue".into(),
            );
        }
        cancel()?;
        progress(54, "工程文件复制完成，正在登记需要提交的文件")?;
        apply_migration_directory_properties(&working_copy, &snapshot.properties)?;
        run_svn_in_directory(
            &working_copy,
            [
                "add",
                "--force",
                "--no-ignore",
                "--parents",
                "--depth",
                "infinity",
                ".",
            ],
        )?;
        add_previously_versioned_paths(&working_copy, &snapshot.versioned_paths, &ignore_policy)?;
        progress(58, "正在恢复 SVN 忽略规则、文件属性和外部依赖")?;
        apply_migration_properties(&working_copy, &snapshot.properties)?;
        apply_migration_ignore_policy(&source, &working_copy, &ignore_policy)?;
        progress(64, "正在校验待提交文件与原工程是否一致")?;
        let staged_summary = migration_tree_summary(
            &working_copy,
            &snapshot.external_roots,
            &transformed_paths,
            &ignore_policy,
        )?;
        if staged_summary != source_summary {
            return Err(
                "staged target exhibit repository does not match the source project; refusing to commit"
                    .into(),
            );
        }
        let pending_change_count = svn_status_change_count(&working_copy)?;
        if !target_was_empty && pending_change_count > 0 {
            return Err("target exhibit repository already contains different content; refusing to overwrite it".into());
        }
        let mut commit_response_error = None;
        if pending_change_count > 0 {
            progress(70, "正在提交工程到目标展项仓库")?;
            if let Err(error) = run_svn_authenticated_cancelable(
                [
                    "commit".to_string(),
                    working_copy.to_string_lossy().to_string(),
                    "-m".to_string(),
                    format!("Import local exhibit {exhibit_id}"),
                ],
                &connection.username,
                &password,
                cancel,
            ) {
                commit_response_error = Some(error.to_string());
                record_svn_diagnostic_event(
                    "svn_cli",
                    "commit",
                    "verifying_uncertain_result",
                    "commit_response_error",
                    1,
                    1,
                    None,
                    0,
                );
                progress(74, "SVN 提交响应异常，正在核验服务端是否已经提交")?;
            } else {
                progress(76, "SVN 提交完成，正在读取服务端版本")?;
            }
        }

        progress(80, "目标仓库提交完成，正在校验文件和版本")?;
        if let Err(error) = run_svn_authenticated_cancelable(
            [
                "update".to_string(),
                "--ignore-externals".to_string(),
                working_copy.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
            cancel,
        ) {
            if let Some(commit_error) = commit_response_error.as_ref() {
                return Err(format!(
                    "SVN commit result is uncertain: {commit_error}; server verification update failed: {error}"
                )
                .into());
            }
            return Err(error);
        }
        if let Err(error) = verify_migration_working_copy(
            &working_copy,
            &repository_url,
            &target_uuid,
            &source_summary,
            &snapshot.external_roots,
            &transformed_paths,
            &ignore_policy,
            "target exhibit repository",
        ) {
            if let Some(commit_error) = commit_response_error.as_ref() {
                return Err(format!(
                    "SVN commit result is uncertain: {commit_error}; server content verification failed: {error}"
                )
                .into());
            }
            return Err(error);
        }
        if commit_response_error.is_some() {
            record_svn_diagnostic_event(
                "svn_cli",
                "commit",
                "recovered_by_server_verification",
                "commit_response_error",
                1,
                1,
                None,
                0,
            );
            progress(84, "已确认服务端提交成功，正在继续接管本地工程")?;
        }

        // Commit already wrote the authoritative content to the server and the
        // working copy was re-verified from the server via `update` above. A
        // second full checkout only re-downloads every file and times out on
        // large projects, so confirm the server HEAD instead.
        let committed_revision = svn_item(&working_copy, "revision")?;
        let committed_revision: u64 = committed_revision.trim().parse()?;
        let server_revision_result = run_svn_authenticated_cancelable(
            [
                "info".to_string(),
                "--show-item".to_string(),
                "revision".to_string(),
                repository_url.clone(),
            ],
            &connection.username,
            &password,
            cancel,
        );
        let server_revision = match server_revision_result {
            Ok(value) => match value.trim().parse::<u64>() {
                Ok(revision) => revision,
                Err(_) => {
                    record_svn_diagnostic_event(
                        "svn_cli",
                        "read_server_revision",
                        "recovered_from_working_copy",
                        "invalid_server_revision",
                        1,
                        1,
                        None,
                        0,
                    );
                    committed_revision
                }
            },
            Err(_) => {
                record_svn_diagnostic_event(
                    "svn_cli",
                    "read_server_revision",
                    "recovered_from_working_copy",
                    "server_revision_unavailable",
                    1,
                    1,
                    None,
                    0,
                );
                committed_revision
            }
        };
        if server_revision < committed_revision {
            return Err("server revision fell behind the committed working copy".into());
        }

        let mut backup = if source_has_svn_metadata {
            progress(88, "目标仓库已验证，正在安全接管原工程目录")?;
            Some(WorkingCopyAdminBackup::create(&source)?)
        } else {
            None
        };
        let switch_result = (|| -> Result<String, Box<dyn Error>> {
            // The temporary working copy already contains the exact source
            // tree and has been verified against the server. Replacing only
            // the SVN admin metadata avoids downloading the whole project a
            // second time during local workspace adoption.
            cancel()?;
            progress(92, "正在写入新的 SVN 工作副本信息")?;
            copy_working_copy_admin(&working_copy, &source)?;
            progress(96, "正在验证原目录的新 SVN 关联")?;
            let switched_url = svn_item(&source, "url")?;
            let switched_uuid = svn_item(&source, "repos-uuid")?;
            if !same_svn_url(&switched_url, &repository_url) || switched_uuid.trim() != target_uuid
            {
                return Err("local workspace switched to an unexpected SVN repository".into());
            }
            let switched_summary = migration_tree_summary(
                &source,
                &snapshot.external_roots,
                &transformed_paths,
                &ignore_policy,
            )?;
            if source_summary != switched_summary {
                return Err(
                    "local workspace verification failed after switching SVN metadata".into(),
                );
            }
            let switched_source_stability = migration_source_stability_summary(
                &source,
                &snapshot.external_roots,
                &transformed_paths,
            )?;
            if source_stability_before != switched_source_stability {
                return Err(
                    "local files changed while switching SVN metadata; restoring the previous association"
                        .into(),
                );
            }
            let switched_change_count = svn_status_change_count(&source)?;
            if switched_change_count != 0 {
                return Err(format!(
                    "local workspace is not clean after switching SVN metadata ({switched_change_count} pending SVN changes)"
                )
                .into());
            }
            progress(98, "正在确认忽略文件和外部依赖保持不变")?;
            verify_migration_ignored_paths_unversioned(&source, &ignore_policy)?;
            svn_item(&source, "revision")
        })();
        let revision = match switch_result {
            Ok(revision) => revision,
            Err(error) => {
                let recovery_error = if let Some(backup) = backup.as_mut() {
                    restore_adopted_workspace(
                        backup,
                        &working_copy,
                        &source,
                        &snapshot.external_roots,
                        &transformed_paths,
                        &ignore_policy,
                    )
                    .err()
                } else {
                    let partial_admin = source.join(".svn");
                    if partial_admin.exists() {
                        std::fs::remove_dir_all(partial_admin)
                            .err()
                            .map(|error| -> Box<dyn Error> { Box::new(error) })
                    } else {
                        None
                    }
                };
                let local_recovery_succeeded = recovery_error.is_none();
                let message = if let Some(recovery_error) = recovery_error {
                    format!(
                        "目标展项仓库已成功导入到 r{server_revision}，但原目录接管失败且本地恢复未完成：{error}；恢复错误：{recovery_error}"
                    )
                } else {
                    format!(
                        "目标展项仓库已成功导入到 r{server_revision}，但原目录接管失败，已恢复原 SVN 关联：{error}"
                    )
                };
                record_svn_diagnostic_event(
                    "svn_workspace",
                    "adopt_local_workspace",
                    "partial_failure",
                    if local_recovery_succeeded {
                        "remote_imported_local_restored"
                    } else {
                        "remote_imported_local_recovery_failed"
                    },
                    1,
                    1,
                    None,
                    0,
                );
                return Err(Box::new(ExhibitImportPartialFailure {
                    message,
                    result: json!({
                        "ok": false,
                        "imported": true,
                        "remote_imported": true,
                        "repository_revision": server_revision,
                        "revision": server_revision,
                        "local_adoption_pending": true,
                        "local_recovery_succeeded": local_recovery_succeeded,
                        "failure_code": "local_adoption_failed",
                        "project_id": project_id,
                        "exhibit_id": exhibit_id,
                        "repository_url": repository_url,
                        "workspace_path": source,
                        "backup_retained": false,
                        "force_migration": request.force_migration
                    }),
                }));
            }
        };
        if let Some(backup) = backup.as_mut() {
            backup.retain();
        }
        Ok(json!({
            "ok": true,
            "imported": true,
            "adopted_existing_working_copy": source_is_working_copy,
            "project_id": project_id,
            "exhibit_id": exhibit_id,
            "repository_url": repository_url,
            "workspace_path": source,
            "revision": revision.trim().parse::<u64>().unwrap_or_default(),
            "preserved_property_count": snapshot.properties.len(),
            "external_count": snapshot.external_count,
            "external_local_checkout_count": snapshot.external_local_checkout_count,
            "backup_retained": source_has_svn_metadata,
            "force_migration": request.force_migration
        }))
    })();
    if std::fs::remove_dir_all(&temp_root).is_err() {
        record_svn_diagnostic_event(
            "svn_workspace",
            "cleanup_import_staging",
            "warning",
            "temporary_directory_cleanup_failed",
            1,
            1,
            None,
            0,
        );
    }
    result
}

pub(crate) fn initialize_exhibit_repository_with_cancel<F>(
    request: InitializeExhibitRepositoryRequest,
    cancel: &mut F,
) -> Result<Value, Box<dyn Error>>
where
    F: FnMut() -> Result<(), Box<dyn Error>>,
{
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let (engine_type, template_id, template_url) =
        resolve_template(&request.engine_type, &request.template_id)?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let (connection, password) = load_company_svn_secret()?;
    ensure_exhibit_writer_access(&project_id, &exhibit_id, &connection.username)?;
    let temp_root = std::env::temp_dir().join(format!(
        "himind-svn-template-{}-{}-{}",
        std::process::id(),
        exhibit_id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    ));
    let working_copy = temp_root.join("target");
    let template_export = temp_root.join("template");
    std::fs::create_dir_all(&temp_root)?;

    let result = (|| -> Result<Value, Box<dyn Error>> {
        run_svn_authenticated_cancelable(
            [
                "checkout".to_string(),
                repository_url.clone(),
                working_copy.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
            cancel,
        )?;

        let marker_path = working_copy.join(TEMPLATE_MARKER_FILE);
        if marker_path.is_file() {
            let marker: Value = serde_json::from_slice(&std::fs::read(&marker_path)?)?;
            if marker.get("template_id").and_then(Value::as_str) != Some(template_id)
                || marker.get("engine_type").and_then(Value::as_str) != Some(engine_type)
            {
                return Err("exhibit repository was initialized with a different template".into());
            }
            let revision = svn_item(&working_copy, "revision")?;
            return Ok(json!({
                "ok": true,
                "initialized": false,
                "already_initialized": true,
                "project_id": project_id,
                "exhibit_id": exhibit_id,
                "repository_url": repository_url,
                "engine_type": engine_type,
                "template_id": template_id,
                "template_version": marker.get("template_version").cloned().unwrap_or(Value::Null),
                "revision": revision.parse::<u64>().unwrap_or_default()
            }));
        }
        if working_copy_contains_content(&working_copy)? {
            return Err("exhibit repository is not empty and has no HiMind template marker".into());
        }

        let template_version = run_svn_authenticated_cancelable(
            [
                "info".to_string(),
                "--show-item".to_string(),
                "revision".to_string(),
                template_url.to_string(),
            ],
            &connection.username,
            &password,
            cancel,
        )?;
        run_svn_authenticated_cancelable(
            [
                "checkout".to_string(),
                template_url.to_string(),
                template_export.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
            cancel,
        )?;
        copy_template_tree(&template_export, &working_copy)?;
        if engine_type == "Unreal Engine" {
            normalize_unreal_project_file(&working_copy, &exhibit_id)?;
        }

        let mut ignored_rule_count =
            migrate_template_ignore_properties(&template_export, &working_copy)?;
        ignored_rule_count += apply_svnignore_files(&working_copy)?;
        let marker = json!({
            "template_id": template_id,
            "template_version": template_version.trim(),
            "engine_type": engine_type
        });
        std::fs::write(&marker_path, serde_json::to_vec_pretty(&marker)?)?;
        run_svn_in_directory(
            &working_copy,
            [
                "add",
                "--force",
                "--no-ignore",
                "--parents",
                "--depth",
                "infinity",
                ".",
            ],
        )?;
        let commit_error = run_svn_authenticated_cancelable(
            [
                "commit".to_string(),
                working_copy.to_string_lossy().to_string(),
                "-m".to_string(),
                format!("Initialize {exhibit_id} from template {template_id}"),
            ],
            &connection.username,
            &password,
            cancel,
        )
        .err();
        if let Some(commit_error) = commit_error.as_ref() {
            record_svn_diagnostic_event(
                "svn_cli",
                "initialize_template_commit",
                "verifying_uncertain_result",
                "commit_response_error",
                1,
                1,
                None,
                0,
            );
            run_svn_authenticated_cancelable(
                [
                    "update".to_string(),
                    "--ignore-externals".to_string(),
                    working_copy.to_string_lossy().to_string(),
                ],
                &connection.username,
                &password,
                cancel,
            )
            .map_err(|verification_error| {
                format!(
                    "SVN template commit result is uncertain: {commit_error}; verification update failed: {verification_error}"
                )
            })?;
            let pending_changes = svn_status_change_count(&working_copy)?;
            if pending_changes != 0 {
                return Err(format!(
                    "SVN template commit result is uncertain: {commit_error}; server readback left {pending_changes} pending working-copy changes"
                )
                .into());
            }
            let verified_marker: Value = serde_json::from_slice(&std::fs::read(&marker_path)?)?;
            if verified_marker.get("template_id").and_then(Value::as_str) != Some(template_id)
                || verified_marker.get("engine_type").and_then(Value::as_str) != Some(engine_type)
                || verified_marker
                    .get("template_version")
                    .and_then(Value::as_str)
                    != Some(template_version.trim())
            {
                return Err(format!(
                    "SVN template commit result is uncertain: {commit_error}; marker readback did not match the requested template"
                )
                .into());
            }
            record_svn_diagnostic_event(
                "svn_cli",
                "initialize_template_commit",
                "recovered_by_server_verification",
                "commit_response_error",
                1,
                1,
                None,
                0,
            );
        }
        let revision = svn_item(&working_copy, "revision")?;
        Ok(json!({
            "ok": true,
            "initialized": true,
            "project_id": project_id,
            "exhibit_id": exhibit_id,
            "repository_url": repository_url,
            "engine_type": engine_type,
            "template_id": template_id,
            "template_version": template_version.trim(),
            "ignored_rule_count": ignored_rule_count,
            "revision": revision.parse::<u64>().unwrap_or_default()
        }))
    })();
    if std::fs::remove_dir_all(&temp_root).is_err() {
        record_svn_diagnostic_event(
            "svn_workspace",
            "cleanup_template_staging",
            "warning",
            "temporary_directory_cleanup_failed",
            1,
            1,
            None,
            0,
        );
    }
    result
}

fn resolve_template<'a>(
    engine_type: &str,
    template_id: &'a str,
) -> Result<(&'static str, &'a str, String), Box<dyn Error>> {
    match (engine_type.trim(), template_id.trim()) {
        ("Unity3D", "unity-uniart") => {
            Ok(("Unity3D", "unity-uniart", UNITY_TEMPLATE_URL.to_string()))
        }
        ("Unreal Engine", "unreal-blank-4.27") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/UE_Blank (4.27)"),
        )),
        ("Unreal Engine", "unreal-blank-5.3") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/UE_Blank (5.3)"),
        )),
        ("Unreal Engine", "unreal-blank-5.4") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/UE_Blank (5.4)"),
        )),
        ("Unreal Engine", "unreal-blank-5.5") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/UE_Blank (5.5)"),
        )),
        ("Unreal Engine", "unreal-picoxr-5.3") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/PicoXR_Template (UE5.3)"),
        )),
        ("Unreal Engine", "unreal-picoxr-5.5") => Ok((
            "Unreal Engine",
            template_id,
            format!("{UNREAL_TEMPLATE_ROOT_URL}/PicoXR_Template (UE5.5)"),
        )),
        _ => Err("template_id does not match the exhibit engine_type".into()),
    }
}

fn working_copy_contains_content(path: &Path) -> Result<bool, Box<dyn Error>> {
    for entry in std::fs::read_dir(path)? {
        if entry?.file_name() != ".svn" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn clear_working_copy_content(path: &Path) -> Result<(), Box<dyn Error>> {
    let entries = std::fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| entry.file_name().is_none_or(|name| name != ".svn"))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(());
    }
    let mut arguments = vec!["delete".to_string(), "--force".to_string()];
    arguments.extend(
        entries
            .iter()
            .map(|entry| entry.to_string_lossy().to_string()),
    );
    run_svn_in_directory_owned(path, arguments)?;
    if working_copy_contains_content(path)? {
        return Err("SVN delete completed but target working copy is not empty".into());
    }
    Ok(())
}

fn copy_template_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty()
            || relative.components().any(|part| part.as_os_str() == ".svn")
        {
            continue;
        }
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct MigrationTreeSummary {
    file_count: u64,
    digest: String,
}

const DEFAULT_ROOT_LARGE_FILE_THRESHOLD_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_ROOT_ARCHIVE_PATTERNS: [&str; 10] = [
    "*.zip", "*.rar", "*.7z", "*.tar", "*.tar.gz", "*.tgz", "*.gz", "*.bz2", "*.xz", "*.iso",
];

fn normalized_ignore_policy(policy: &MigrationIgnorePolicy) -> MigrationIgnorePolicy {
    let mut result = policy.clone();
    if result.version == 0 {
        result.version = 1;
    }
    if result.root_large_file_threshold_bytes == 0 {
        result.root_large_file_threshold_bytes = DEFAULT_ROOT_LARGE_FILE_THRESHOLD_BYTES;
    }
    if result.root_archive_patterns.is_empty() {
        result.root_archive_patterns = DEFAULT_ROOT_ARCHIVE_PATTERNS
            .iter()
            .map(|v| (*v).to_string())
            .collect();
    }
    result.root_archive_patterns = result
        .root_archive_patterns
        .iter()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect();
    result.excluded_relative_paths = result
        .excluded_relative_paths
        .iter()
        .map(|v| v.replace('\\', "/").trim_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .collect();
    result.included_relative_paths = result
        .included_relative_paths
        .iter()
        .map(|v| v.replace('\\', "/").trim_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .collect();
    result
}

fn migration_policy_excludes(
    relative: &Path,
    file_size: Option<u64>,
    policy: &MigrationIgnorePolicy,
) -> bool {
    let normalized = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if policy.included_relative_paths.iter().any(|value| {
        value
            .replace('\\', "/")
            .trim_matches('/')
            .eq_ignore_ascii_case(&normalized)
    }) {
        return false;
    }
    migration_policy_candidate(relative, file_size, policy)
}

fn migration_policy_candidate(
    relative: &Path,
    file_size: Option<u64>,
    policy: &MigrationIgnorePolicy,
) -> bool {
    let normalized = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if policy.excluded_relative_paths.iter().any(|value| {
        value
            .replace('\\', "/")
            .trim_matches('/')
            .eq_ignore_ascii_case(&normalized)
    }) {
        return true;
    }
    if relative.components().count() != 1 {
        return false;
    }
    let name = relative
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let archive = policy
        .root_archive_patterns
        .iter()
        .any(|pattern| wildcard_match(pattern, &name));
    archive || file_size.is_some_and(|size| size >= policy.root_large_file_threshold_bytes)
}

fn migration_entry_excluded(
    entry: &DirEntry,
    source: &Path,
    policy: &MigrationIgnorePolicy,
) -> bool {
    let Ok(relative) = entry.path().strip_prefix(source) else {
        return false;
    };
    migration_relative_path_excluded(source, relative, policy)
}

fn migration_relative_path_excluded(
    source: &Path,
    relative: &Path,
    policy: &MigrationIgnorePolicy,
) -> bool {
    if relative.components().any(|component| {
        is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
    }) {
        return true;
    }
    migration_policy_excludes(
        relative,
        source
            .join(relative)
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len()),
        policy,
    )
}

fn apply_migration_ignore_policy(
    source: &Path,
    working_copy: &Path,
    policy: &MigrationIgnorePolicy,
) -> Result<(), Box<dyn Error>> {
    let mut rules = policy.root_archive_patterns.clone();
    for entry in WalkDir::new(source)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        let relative = entry.path().strip_prefix(source)?;
        if migration_policy_excludes(
            relative,
            entry
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len()),
            policy,
        ) {
            if let Some(name) = relative.file_name().and_then(|v| v.to_str()) {
                rules.push(name.to_string());
            }
        }
    }
    rules.sort();
    rules.dedup();
    let existing =
        run_svn_in_directory(working_copy, ["propget", "svn:ignore", "."]).unwrap_or_default();
    let mut lines = existing
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    lines.extend(rules);
    lines.sort();
    lines.dedup();
    run_svn_in_directory(
        working_copy,
        ["propset", "svn:ignore", &lines.join("\n"), "."],
    )?;
    Ok(())
}

impl PartialEq for MigrationTreeSummary {
    fn eq(&self, other: &Self) -> bool {
        // Keyword and EOL properties legitimately rewrite bytes on checkout; the digest
        // marks those paths while still checking every path and every stable file's bytes.
        self.file_count == other.file_count && self.digest == other.digest
    }
}

impl Eq for MigrationTreeSummary {}

#[derive(Debug, Clone)]
struct MigrationProperty {
    relative_path: PathBuf,
    name: String,
    value: String,
}

#[derive(Debug, Default)]
struct MigrationMetadataSnapshot {
    properties: Vec<MigrationProperty>,
    versioned_paths: Vec<PathBuf>,
    external_roots: Vec<PathBuf>,
    external_count: u64,
    external_local_checkout_count: u64,
    external_local_revision_count: u64,
    external_status_counts: BTreeMap<String, u64>,
}

fn copy_migration_tree(
    source: &Path,
    target: &Path,
    external_roots: &[PathBuf],
    transformed_paths: &BTreeSet<PathBuf>,
    ignore_policy: &MigrationIgnorePolicy,
) -> Result<MigrationTreeSummary, Box<dyn Error>> {
    let mut fingerprint = Sha256::new();
    let mut file_count = 0_u64;
    for item in WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            !migration_entry_excluded(entry, source, ignore_policy)
                && entry
                    .path()
                    .strip_prefix(source)
                    .ok()
                    .is_none_or(|relative| !path_is_within_roots(relative, external_roots))
        })
    {
        let entry = item?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() || entry.file_type().is_symlink() {
            continue;
        }
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            update_migration_digest(
                &mut fingerprint,
                relative,
                entry.path(),
                transformed_paths.contains(relative),
            )?;
            file_count += 1;
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(MigrationTreeSummary {
        file_count,
        digest: format!("{:x}", fingerprint.finalize()),
    })
}

fn migration_tree_summary(
    source: &Path,
    external_roots: &[PathBuf],
    transformed_paths: &BTreeSet<PathBuf>,
    ignore_policy: &MigrationIgnorePolicy,
) -> Result<MigrationTreeSummary, Box<dyn Error>> {
    let mut fingerprint = Sha256::new();
    let mut file_count = 0_u64;
    for item in WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            !migration_entry_excluded(entry, source, ignore_policy)
                && entry
                    .path()
                    .strip_prefix(source)
                    .ok()
                    .is_none_or(|relative| !path_is_within_roots(relative, external_roots))
        })
    {
        let entry = item?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty()
            || !entry.file_type().is_file()
            || entry.file_type().is_symlink()
        {
            continue;
        }
        update_migration_digest(
            &mut fingerprint,
            relative,
            entry.path(),
            transformed_paths.contains(relative),
        )?;
        file_count += 1;
    }
    Ok(MigrationTreeSummary {
        file_count,
        digest: format!("{:x}", fingerprint.finalize()),
    })
}

fn migration_source_stability_summary(
    source: &Path,
    external_roots: &[PathBuf],
    transformed_paths: &BTreeSet<PathBuf>,
) -> Result<MigrationTreeSummary, Box<dyn Error>> {
    let mut fingerprint = Sha256::new();
    let mut file_count = 0_u64;
    for item in WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(source)
                .ok()
                .is_none_or(|relative| {
                    !relative.components().any(|component| {
                        is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
                    }) && !path_is_within_roots(relative, external_roots)
                })
        })
    {
        let entry = item?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty()
            || !entry.file_type().is_file()
            || entry.file_type().is_symlink()
        {
            continue;
        }
        update_migration_digest(
            &mut fingerprint,
            relative,
            entry.path(),
            transformed_paths.contains(relative),
        )?;
        file_count += 1;
    }
    Ok(MigrationTreeSummary {
        file_count,
        digest: format!("{:x}", fingerprint.finalize()),
    })
}

fn verify_migration_working_copy(
    working_copy: &Path,
    expected_url: &str,
    expected_uuid: &str,
    expected_summary: &MigrationTreeSummary,
    external_roots: &[PathBuf],
    transformed_paths: &BTreeSet<PathBuf>,
    ignore_policy: &MigrationIgnorePolicy,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    let actual_url = svn_item(working_copy, "url")?;
    let actual_uuid = svn_item(working_copy, "repos-uuid")?;
    if !same_svn_url(&actual_url, expected_url) || actual_uuid.trim() != expected_uuid.trim() {
        return Err(format!("{label} points to an unexpected SVN location").into());
    }
    let change_count = svn_status_change_count(working_copy)?;
    if change_count != 0 {
        return Err(format!(
            "{label} is not clean after verification ({change_count} pending SVN changes)"
        )
        .into());
    }
    verify_migration_ignored_paths_unversioned(working_copy, ignore_policy)?;
    let actual_summary = migration_tree_summary(
        working_copy,
        external_roots,
        transformed_paths,
        ignore_policy,
    )?;
    if actual_summary != *expected_summary {
        return Err(format!(
            "{label} file verification failed (expected {} files, received {} files)",
            expected_summary.file_count, actual_summary.file_count
        )
        .into());
    }
    Ok(())
}

fn verify_migration_ignored_paths_unversioned(
    working_copy: &Path,
    ignore_policy: &MigrationIgnorePolicy,
) -> Result<(), Box<dyn Error>> {
    let output = run_svn_in_directory(
        working_copy,
        ["status", "--xml", "--verbose", "--ignore-externals", "."],
    )?;
    let status: SvnStatusDocument = quick_xml::de::from_str(&output)?;
    for entry in status.targets.into_iter().flat_map(|target| target.entries) {
        if matches!(
            entry.status.item.as_str(),
            "unversioned" | "ignored" | "external" | "none"
        ) {
            continue;
        }
        let relative = migration_property_relative_path(working_copy, &entry.path)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if migration_policy_excludes(
            &relative,
            working_copy
                .join(&relative)
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len()),
            ignore_policy,
        ) {
            return Err(format!(
                "target repository contains a file that must remain local: {}",
                relative.display()
            )
            .into());
        }
    }
    Ok(())
}

fn update_migration_digest(
    fingerprint: &mut Sha256,
    relative: &Path,
    file: &Path,
    transformed: bool,
) -> Result<(), Box<dyn Error>> {
    fingerprint.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
    fingerprint.update([0]);
    if transformed {
        fingerprint.update([1]);
        return Ok(());
    }
    fingerprint.update([0]);
    let mut input = std::fs::File::open(file)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        fingerprint.update(&buffer[..read]);
    }
    fingerprint.update([0]);
    Ok(())
}

fn migration_transform_paths(properties: &[MigrationProperty]) -> BTreeSet<PathBuf> {
    properties
        .iter()
        .filter(|property| matches!(property.name.as_str(), "svn:keywords" | "svn:eol-style"))
        .map(|property| property.relative_path.clone())
        .collect()
}

fn path_is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| !root.as_os_str().is_empty() && (path == root || path.starts_with(root)))
}

#[derive(Debug, Deserialize)]
struct SvnProperties {
    #[serde(rename = "target", default)]
    targets: Vec<SvnPropertyTarget>,
}

#[derive(Debug, Deserialize)]
struct SvnPropertyTarget {
    #[serde(rename = "@path")]
    path: String,
    #[serde(rename = "property", default)]
    properties: Vec<SvnProperty>,
}

#[derive(Debug, Deserialize)]
struct SvnProperty {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@encoding", default)]
    encoding: String,
    #[serde(rename = "$text", default)]
    value: String,
}

#[derive(Debug, Deserialize)]
struct SvnStatusDocument {
    #[serde(rename = "target", default)]
    targets: Vec<SvnStatusTarget>,
}

#[derive(Debug, Deserialize)]
struct SvnStatusTarget {
    #[serde(rename = "entry", default)]
    entries: Vec<SvnStatusEntry>,
}

#[derive(Debug, Deserialize)]
struct SvnStatusEntry {
    #[serde(rename = "@path")]
    path: String,
    #[serde(rename = "wc-status")]
    status: SvnWorkingCopyStatus,
}

#[derive(Debug, Deserialize)]
struct SvnWorkingCopyStatus {
    #[serde(rename = "@item")]
    item: String,
    #[serde(rename = "@props", default)]
    props: String,
}

fn snapshot_migration_metadata(
    source: &Path,
    probe_externals: bool,
    ignore_policy: Option<&MigrationIgnorePolicy>,
) -> Result<MigrationMetadataSnapshot, Box<dyn Error>> {
    let output = run_svn_in_directory(
        source,
        ["proplist", "--xml", "--verbose", "--recursive", "."],
    )?;
    let parsed: SvnProperties = quick_xml::de::from_str(&output)?;
    let repository_root = svn_item(source, "repos-root-url").unwrap_or_default();
    let source_url = svn_item(source, "url").unwrap_or_default();
    let mut snapshot = MigrationMetadataSnapshot::default();

    for target in parsed.targets {
        let relative_path = migration_property_relative_path(source, &target.path)?;
        if relative_path.components().any(|component| {
            is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
        }) || ignore_policy
            .is_some_and(|policy| migration_relative_path_excluded(source, &relative_path, policy))
        {
            continue;
        }
        // A working copy may still carry svn:* metadata for paths that were
        // deleted locally (svn status "!"). Such targets cannot be copied, so
        // skip them instead of failing the whole migration later.
        if !relative_path.as_os_str().is_empty() && !source.join(&relative_path).exists() {
            continue;
        }
        for property in target.properties {
            if !MIGRATION_PROPERTY_NAMES.contains(&property.name.as_str()) {
                continue;
            }
            let mut value = decode_svn_property_value(&property)?;
            if property.name == "svn:externals" {
                let property_url =
                    svn_item(&source.join(&relative_path), "url").unwrap_or_else(|_| {
                        append_url_path(&source_url, &relative_path)
                            .unwrap_or_else(|| source_url.clone())
                    });
                let normalized = normalize_external_property(
                    source,
                    &relative_path,
                    &property_url,
                    &repository_root,
                    &value,
                    probe_externals,
                )?;
                value = normalized.value;
                snapshot.external_roots.extend(normalized.local_roots);
                snapshot.external_count += normalized.external_count;
                snapshot.external_local_checkout_count += normalized.local_checkout_count;
                snapshot.external_local_revision_count += normalized.local_revision_count;
                for (status, count) in normalized.status_counts {
                    *snapshot.external_status_counts.entry(status).or_default() += count;
                }
            }
            snapshot.properties.push(MigrationProperty {
                relative_path: relative_path.clone(),
                name: property.name,
                value,
            });
        }
    }
    snapshot.external_roots.sort();
    snapshot.external_roots.dedup();
    snapshot.versioned_paths =
        snapshot_migration_versioned_paths(source, &snapshot.external_roots, ignore_policy)?;
    Ok(snapshot)
}

fn decode_svn_property_value(property: &SvnProperty) -> Result<String, Box<dyn Error>> {
    if property.encoding.is_empty() {
        return Ok(property.value.clone());
    }
    if property.encoding != "base64" {
        return Err(format!("unsupported SVN property encoding: {}", property.encoding).into());
    }
    let encoded = property
        .value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let bytes = BASE64_STANDARD.decode(encoded)?;
    match String::from_utf8(bytes) {
        Ok(value) => Ok(value),
        Err(error) => {
            let bytes = error.into_bytes();
            let (value, had_errors) = encoding_rs::GBK.decode_without_bom_handling(&bytes);
            if had_errors {
                return Err("SVN property is neither valid UTF-8 nor valid GBK text".into());
            }
            Ok(value.into_owned())
        }
    }
}

fn snapshot_migration_versioned_paths(
    source: &Path,
    external_roots: &[PathBuf],
    ignore_policy: Option<&MigrationIgnorePolicy>,
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let output = run_svn_in_directory(
        source,
        ["status", "--xml", "--verbose", "--ignore-externals", "."],
    )?;
    let status: SvnStatusDocument = quick_xml::de::from_str(&output)?;
    let mut paths = BTreeSet::new();
    for entry in status.targets.into_iter().flat_map(|target| target.entries) {
        if matches!(
            entry.status.item.as_str(),
            "unversioned" | "ignored" | "external" | "none"
        ) {
            continue;
        }
        let relative = migration_property_relative_path(source, &entry.path)?;
        if relative.as_os_str().is_empty()
            || path_is_within_roots(&relative, external_roots)
            || relative.components().any(|component| {
                is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
            })
            || ignore_policy
                .is_some_and(|policy| migration_relative_path_excluded(source, &relative, policy))
        {
            continue;
        }
        paths.insert(relative);
    }
    Ok(paths.into_iter().collect())
}

fn migration_property_relative_path(
    source: &Path,
    target_path: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let path = PathBuf::from(target_path);
    let relative = if path.is_absolute() {
        path.strip_prefix(source)?.to_path_buf()
    } else {
        path.strip_prefix(".").unwrap_or(&path).to_path_buf()
    };
    if relative.components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    }) {
        return Err("SVN property target escapes the migration source".into());
    }
    Ok(relative)
}

#[derive(Debug, Default)]
struct NormalizedExternalProperty {
    value: String,
    local_roots: Vec<PathBuf>,
    external_count: u64,
    local_checkout_count: u64,
    local_revision_count: u64,
    status_counts: BTreeMap<String, u64>,
}

fn normalize_external_property(
    source: &Path,
    property_relative: &Path,
    property_url: &str,
    repository_root: &str,
    value: &str,
    probe_externals: bool,
) -> Result<NormalizedExternalProperty, Box<dyn Error>> {
    let mut result = NormalizedExternalProperty::default();
    let mut output_lines = Vec::new();
    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            output_lines.push(line.to_string());
            continue;
        }
        let mut tokens = split_external_tokens(trimmed)?;
        let Some((url_index, local_index)) = external_url_and_local_indexes(&tokens) else {
            output_lines.push(line.to_string());
            *result
                .status_counts
                .entry("definition_unknown".to_string())
                .or_default() += 1;
            continue;
        };
        let Some(local_relative) =
            resolve_external_local_path(property_relative, &tokens[local_index])
        else {
            return Err("svn:externals local path escapes the migration source".into());
        };
        result.external_count += 1;
        result.local_roots.push(local_relative.clone());

        let local_path = source.join(&local_relative);
        let local_url = if local_path.exists() {
            let url = svn_item(&local_path, "url").ok();
            if url.as_ref().is_some_and(|value| !value.trim().is_empty()) {
                result.local_checkout_count += 1;
            }
            let revision = svn_item(&local_path, "revision").unwrap_or_default();
            if !revision.trim().is_empty() {
                result.local_revision_count += 1;
            }
            url
        } else {
            None
        };
        let local_checkout_available = local_url.as_ref().is_some_and(|url| !url.trim().is_empty());
        let peg_revision = external_peg_revision(&tokens[url_index]);
        let definition_url = peg_revision
            .and_then(|revision| tokens[url_index].strip_suffix(&format!("@{revision}")))
            .unwrap_or(&tokens[url_index]);
        let normalized_url = local_url
            .filter(|url| !url.trim().is_empty())
            .or_else(|| normalize_external_url(definition_url, property_url, repository_root));
        if let Some(mut url) = normalized_url {
            if let Some(peg_revision) = peg_revision {
                url.push('@');
                url.push_str(peg_revision);
            }
            tokens[url_index] = url;
        }
        if probe_externals {
            let status = tokens
                .get(url_index)
                .map(|url| probe_svn_remote(url, Duration::from_secs(4)))
                .unwrap_or_else(|| "definition_unknown".to_string());
            *result.status_counts.entry(status).or_default() += 1;
        } else if local_checkout_available {
            *result
                .status_counts
                .entry("local_checkout_available".to_string())
                .or_default() += 1;
        } else {
            *result
                .status_counts
                .entry("local_checkout_missing".to_string())
                .or_default() += 1;
        }
        output_lines.push(
            tokens
                .into_iter()
                .map(|token| quote_external_token(&token))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    result.value = output_lines.join("\n");
    if value.ends_with('\n') {
        result.value.push('\n');
    }
    Ok(result)
}

fn split_external_tokens(line: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if quote.is_some() {
        return Err("svn:externals contains an unterminated quoted value".into());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn external_url_and_local_indexes(tokens: &[String]) -> Option<(usize, usize)> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token == "-r" || token == "--revision" {
            index += 2;
            continue;
        }
        if token.starts_with("-r") || token.starts_with("--revision=") {
            index += 1;
            continue;
        }
        positional.push(index);
        index += 1;
    }
    if positional.len() != 2 {
        return None;
    }
    let first = positional[0];
    let second = positional[1];
    if is_external_url_token(&tokens[first]) {
        Some((first, second))
    } else if is_external_url_token(&tokens[second]) {
        Some((second, first))
    } else {
        None
    }
}

fn is_external_url_token(value: &str) -> bool {
    value.contains("://")
        || value.starts_with("^/")
        || value.starts_with("//")
        || value.starts_with('/')
        || value.starts_with("../")
        || value.starts_with("./")
}

fn external_peg_revision(value: &str) -> Option<&str> {
    let (_, revision) = value.rsplit_once('@')?;
    (!revision.is_empty()
        && revision
            .chars()
            .all(|character| character.is_ascii_digit() || character.is_ascii_alphabetic()))
    .then_some(revision)
}

fn resolve_external_local_path(property_relative: &Path, value: &str) -> Option<PathBuf> {
    let local = Path::new(value);
    if local.is_absolute() {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in property_relative.components().chain(local.components()) {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => resolved.push(value),
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some(resolved)
}

fn normalize_external_url(
    value: &str,
    property_url: &str,
    repository_root: &str,
) -> Option<String> {
    if Url::parse(value).is_ok() {
        return Some(value.to_string());
    }
    if let Some(suffix) = value.strip_prefix("^/") {
        return Some(format!(
            "{}/{}",
            repository_root.trim_end_matches('/'),
            suffix
        ));
    }
    let base = Url::parse(property_url).ok()?;
    if value.starts_with("//") {
        return Some(format!("{}:{value}", base.scheme()));
    }
    if value.starts_with('/') {
        let host = base.host_str()?;
        let authority = match base.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        return Some(format!("{}://{}{}", base.scheme(), authority, value));
    }
    let mut directory_url = property_url.trim_end_matches('/').to_string();
    directory_url.push('/');
    Url::parse(&directory_url)
        .ok()?
        .join(value)
        .ok()
        .map(|url| url.to_string())
}

fn append_url_path(base: &str, path: &Path) -> Option<String> {
    let mut url = Url::parse(&format!("{}/", base.trim_end_matches('/'))).ok()?;
    for component in path.components() {
        if let Component::Normal(value) = component {
            url.path_segments_mut().ok()?.push(value.to_str()?);
        }
    }
    Some(url.to_string())
}

fn quote_external_token(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn apply_migration_properties(
    working_copy: &Path,
    properties: &[MigrationProperty],
) -> Result<(), Box<dyn Error>> {
    for property in properties {
        if !working_copy.join(&property.relative_path).exists() {
            return Err(format!(
                "SVN property target was not copied: {}",
                property.relative_path.display()
            )
            .into());
        }
    }
    run_migration_propset_batches(working_copy, properties.iter())
}

fn apply_migration_directory_properties(
    working_copy: &Path,
    properties: &[MigrationProperty],
) -> Result<(), Box<dyn Error>> {
    let directory_properties = properties
        .iter()
        .filter(|property| working_copy.join(&property.relative_path).is_dir())
        .collect::<Vec<_>>();
    let targets = directory_properties
        .iter()
        .filter(|property| !property.relative_path.as_os_str().is_empty())
        .map(|property| property.relative_path.to_string_lossy().to_string())
        .collect::<BTreeSet<_>>();
    run_svn_target_batches(
        working_copy,
        &["add", "--force", "--parents", "--depth", "empty"],
        targets.into_iter(),
    )?;
    run_migration_propset_batches(working_copy, directory_properties.into_iter())
}

fn add_previously_versioned_paths(
    working_copy: &Path,
    versioned_paths: &[PathBuf],
    ignore_policy: &MigrationIgnorePolicy,
) -> Result<(), Box<dyn Error>> {
    let targets = versioned_paths
        .iter()
        .filter(|relative| working_copy.join(relative).exists())
        .filter(|relative| !migration_relative_path_excluded(working_copy, relative, ignore_policy))
        .map(|relative| relative.to_string_lossy().to_string());
    run_svn_target_batches(
        working_copy,
        &["add", "--force", "--no-ignore", "--parents"],
        targets,
    )
}

fn run_migration_propset_batches<'a, I>(
    working_copy: &Path,
    properties: I,
) -> Result<(), Box<dyn Error>>
where
    I: IntoIterator<Item = &'a MigrationProperty>,
{
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for property in properties {
        let target = if property.relative_path.as_os_str().is_empty() {
            ".".to_string()
        } else {
            property.relative_path.to_string_lossy().to_string()
        };
        groups
            .entry((property.name.clone(), property.value.clone()))
            .or_default()
            .push(target);
    }
    for ((name, value), targets) in groups {
        run_svn_target_batches(
            working_copy,
            &["propset", &name, &value],
            targets.into_iter(),
        )?;
    }
    Ok(())
}

fn run_svn_target_batches<I, S>(
    working_copy: &Path,
    prefix: &[&str],
    targets: I,
) -> Result<(), Box<dyn Error>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    const MAX_ARGUMENT_CHARS: usize = 20_000;
    let batch_prefix = || {
        let mut arguments = prefix
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        arguments.push("--".to_string());
        arguments
    };
    let mut arguments = batch_prefix();
    let target_start = arguments.len();
    let prefix_length = arguments.iter().map(|value| value.len() + 3).sum::<usize>();
    let mut argument_length = prefix_length;
    for target in targets {
        let target = svn_local_target_argument(target.into());
        let next_length = target.len() + 3;
        if arguments.len() > target_start && argument_length + next_length > MAX_ARGUMENT_CHARS {
            run_svn_in_directory_owned(
                working_copy,
                std::mem::replace(&mut arguments, batch_prefix()),
            )?;
            argument_length = prefix_length;
        }
        argument_length += next_length;
        arguments.push(target);
    }
    if arguments.len() > target_start {
        run_svn_in_directory_owned(working_copy, arguments)?;
    }
    Ok(())
}

fn svn_local_target_argument(mut target: String) -> String {
    // An empty peg revision makes a literal '@' unambiguous to the SVN CLI.
    if target.contains('@') {
        target.push('@');
    }
    target
}

fn migrate_template_ignore_properties(
    template_working_copy: &Path,
    target_working_copy: &Path,
) -> Result<usize, Box<dyn Error>> {
    let output = run_svn_in_directory(
        template_working_copy,
        ["proplist", "--xml", "--verbose", "--recursive", "."],
    )?;
    let properties = parse_template_ignore_properties(&output)?;
    let mut count = 0;
    for target in properties.targets {
        let Some(property) = target
            .properties
            .into_iter()
            .find(|property| property.name == "svn:ignore")
        else {
            continue;
        };
        let source_path = PathBuf::from(&target.path);
        let relative = if source_path.is_absolute() {
            source_path.strip_prefix(template_working_copy)?
        } else {
            source_path.strip_prefix(".").unwrap_or(&source_path)
        };
        let target_path = target_working_copy.join(relative);
        if target_path != target_working_copy {
            run_svn_in_directory_owned(
                target_working_copy,
                vec![
                    "add".to_string(),
                    "--force".to_string(),
                    "--parents".to_string(),
                    "--depth".to_string(),
                    "empty".to_string(),
                    relative.to_string_lossy().to_string(),
                ],
            )?;
        }
        run_svn_in_directory_owned(
            &target_path,
            vec![
                "propset".to_string(),
                "svn:ignore".to_string(),
                property.value.clone(),
                ".".to_string(),
            ],
        )?;
        count += property
            .value
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
    }
    Ok(count)
}

fn parse_template_ignore_properties(xml: &str) -> Result<SvnProperties, Box<dyn Error>> {
    Ok(quick_xml::de::from_str(xml)?)
}

fn apply_svnignore_files(working_copy: &Path) -> Result<usize, Box<dyn Error>> {
    let ignore_files = walkdir::WalkDir::new(working_copy)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == ".svnignore")
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    let mut count = 0;
    for ignore_file in ignore_files {
        let parent = ignore_file
            .parent()
            .ok_or(".svnignore has no parent directory")?;
        if parent != working_copy {
            let relative = parent
                .strip_prefix(working_copy)?
                .to_string_lossy()
                .to_string();
            run_svn_in_directory_owned(
                working_copy,
                vec![
                    "add".to_string(),
                    "--force".to_string(),
                    "--parents".to_string(),
                    "--depth".to_string(),
                    "empty".to_string(),
                    relative,
                ],
            )?;
        }
        run_svn_in_directory(parent, ["propset", "svn:ignore", "-F", ".svnignore", "."])?;
        let rules = std::fs::read_to_string(&ignore_file)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect::<Vec<_>>();
        count += rules.len();
        for child in std::fs::read_dir(parent)? {
            let child = child?;
            let name = child.file_name().to_string_lossy().to_string();
            if name != ".svn"
                && name != ".svnignore"
                && rules.iter().any(|rule| wildcard_match(rule, &name))
            {
                if child.file_type()?.is_dir() {
                    std::fs::remove_dir_all(child.path())?;
                } else {
                    std::fs::remove_file(child.path())?;
                }
            }
        }
    }
    Ok(count)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim().as_bytes();
    let value = value.as_bytes();
    if pattern.is_empty() {
        return false;
    }
    let mut pattern_index = 0;
    let mut value_index = 0;
    let mut star_index = None;
    let mut retry_value_index = 0;
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            retry_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            retry_value_index += 1;
            value_index = retry_value_index;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn normalize_unreal_project_file(path: &Path, exhibit_id: &str) -> Result<(), Box<dyn Error>> {
    let files = walkdir::WalkDir::new(path)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("uproject"))
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    if files.len() != 1 {
        return Err("Unreal template must contain exactly one .uproject file".into());
    }
    let destination = path.join(format!("{exhibit_id}.uproject"));
    if files[0] != destination {
        std::fs::rename(&files[0], destination)?;
    }
    Ok(())
}

fn run_svn_in_directory<const N: usize>(
    directory: &Path,
    arguments: [&str; N],
) -> Result<String, Box<dyn Error>> {
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let output = Command::new(executable)
        .args(arguments)
        .current_dir(directory)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        return Err(decode_svn_cli_output(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(decode_svn_cli_output(&output.stdout).trim().to_string())
}

fn run_svn_in_directory_owned(
    directory: &Path,
    arguments: Vec<String>,
) -> Result<String, Box<dyn Error>> {
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let output = Command::new(executable)
        .args(arguments)
        .current_dir(directory)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        return Err(decode_svn_cli_output(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(decode_svn_cli_output(&output.stdout).trim().to_string())
}

fn decode_svn_cli_output(bytes: &[u8]) -> String {
    if let Ok(value) = std::str::from_utf8(bytes) {
        return value.to_string();
    }
    let (value, had_errors) = encoding_rs::GBK.decode_without_bom_handling(bytes);
    if had_errors {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        value.into_owned()
    }
}

fn svn_status_change_count(working_copy: &Path) -> Result<usize, Box<dyn Error>> {
    let output =
        run_svn_in_directory(working_copy, ["status", "--xml", "--ignore-externals", "."])?;
    svn_status_change_count_from_xml(&output)
}

fn svn_status_change_count_from_xml(output: &str) -> Result<usize, Box<dyn Error>> {
    let status: SvnStatusDocument = quick_xml::de::from_str(&output)?;
    Ok(status
        .targets
        .into_iter()
        .flat_map(|target| target.entries)
        .filter(|entry| {
            // SVN externals and explicitly ignored files are not pending
            // repository changes. Ordinary unversioned files still count so
            // an incomplete adoption cannot be reported as clean.
            let item = entry.status.item.as_str();
            let props = entry.status.props.as_str();
            !matches!(item, "normal" | "ignored" | "external" | "none")
                || (item == "normal" && !matches!(props, "" | "normal" | "none"))
        })
        .count())
}

fn probe_svn_remote(url: &str, timeout: Duration) -> String {
    let Some(executable) = find_svn_executable() else {
        return "temporarily_unreachable".to_string();
    };
    let credentials = load_company_svn_secret().ok();
    let arguments = svn_remote_probe_arguments(
        url,
        credentials
            .as_ref()
            .map(|(connection, _)| connection.username.as_str()),
    );
    let child = Command::new(executable)
        .args(arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(if credentials.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return "temporarily_unreachable".to_string();
    };
    if let Some((_, password)) = credentials {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return "temporarily_unreachable".to_string();
        };
        if stdin.write_all(password.as_bytes()).is_err() || stdin.write_all(b"\r\n").is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return "temporarily_unreachable".to_string();
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = match child.wait_with_output() {
                    Ok(output) => output,
                    Err(_) => return "temporarily_unreachable".to_string(),
                };
                if output.status.success() {
                    return "reachable".to_string();
                }
                return classify_svn_remote_error(&decode_svn_cli_output(&output.stderr));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return "temporarily_unreachable".to_string();
            }
            Err(_) => return "temporarily_unreachable".to_string(),
        }
    }
}

fn svn_remote_probe_arguments(url: &str, username: Option<&str>) -> Vec<String> {
    let mut arguments = vec![
        "info".to_string(),
        url.to_string(),
        "--non-interactive".to_string(),
    ];
    if let Some(username) = username.filter(|value| !value.trim().is_empty()) {
        arguments.extend([
            "--no-auth-cache".to_string(),
            "--username".to_string(),
            username.to_string(),
            "--password-from-stdin".to_string(),
        ]);
    }
    arguments
}

fn classify_svn_remote_error(message: &str) -> String {
    let value = message.to_ascii_lowercase();
    if value.contains("e170001")
        || value.contains("e215004")
        || value.contains("e220004")
        || value.contains("authorization failed")
        || value.contains("authentication failed")
        || value.contains("forbidden")
        || value.contains("401")
        || value.contains("403")
    {
        "authorization_unknown".to_string()
    } else if value.contains("e160013")
        || value.contains("path not found")
        || value.contains("not found in revision")
        || value.contains("does not exist")
    {
        "missing".to_string()
    } else {
        "temporarily_unreachable".to_string()
    }
}

struct WorkingCopyAdminBackup {
    source: PathBuf,
    backup: PathBuf,
    active: bool,
}

impl WorkingCopyAdminBackup {
    fn create(source: &Path) -> Result<Self, Box<dyn Error>> {
        let admin = source.join(".svn");
        if !admin.is_dir() {
            return Err("source is not an SVN working copy".into());
        }
        let parent = source.parent().ok_or("source directory has no parent")?;
        let backup_root = parent.join(". himind-svn-backups");
        std::fs::create_dir_all(&backup_root)?;
        let _ = Command::new("attrib.exe")
            .args(["+H", backup_root.to_string_lossy().as_ref()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let source_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let backup = backup_root.join(format!(
            "{}-{}-{}-svn",
            source_name,
            std::process::id(),
            timestamp
        ));
        std::fs::rename(&admin, &backup)?;
        Ok(Self {
            source: source.to_path_buf(),
            backup,
            active: true,
        })
    }

    fn rollback(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.active {
            return Ok(());
        }
        let current = self.source.join(".svn");
        if current.exists() {
            std::fs::remove_dir_all(&current)?;
        }
        std::fs::rename(&self.backup, &current)?;
        self.active = false;
        Ok(())
    }

    fn retain(&mut self) {
        self.active = false;
    }
}

impl Drop for WorkingCopyAdminBackup {
    fn drop(&mut self) {
        if self.active {
            let _ = self.rollback();
        }
    }
}

fn restore_adopted_workspace(
    backup: &mut WorkingCopyAdminBackup,
    verified_working_copy: &Path,
    source: &Path,
    external_roots: &[PathBuf],
    transformed_paths: &BTreeSet<PathBuf>,
    ignore_policy: &MigrationIgnorePolicy,
) -> Result<(), Box<dyn Error>> {
    backup.rollback()?;
    copy_migration_tree(
        verified_working_copy,
        source,
        external_roots,
        transformed_paths,
        ignore_policy,
    )?;
    Ok(())
}

fn copy_working_copy_admin(
    verified_working_copy: &Path,
    source: &Path,
) -> Result<(), Box<dyn Error>> {
    let source_admin = source.join(".svn");
    if source_admin.exists() {
        std::fs::remove_dir_all(&source_admin)?;
    }
    copy_directory_tree(&verified_working_copy.join(".svn"), &source_admin)
}

fn copy_directory_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    if !source.is_dir() {
        return Err(format!("SVN working copy metadata is missing: {}", source.display()).into());
    }
    std::fs::create_dir_all(target)?;
    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

fn svn_item(working_copy: &Path, item: &str) -> Result<String, Box<dyn Error>> {
    run_svn_in_directory(working_copy, ["info", "--show-item", item])
}

fn is_valid_svn_working_copy(path: &Path) -> bool {
    path.join(".svn").is_dir() && svn_item(path, "url").is_ok_and(|url| !url.trim().is_empty())
}

pub(crate) fn ensure_project_exhibits_access(
    request: EnsureProjectExhibitsAccessRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let mut paths = Vec::new();
    for (path, access) in PROJECT_REPOSITORY_BROAD_ACL {
        let action = ensure_authenticated_path_access(&project_id, path, access, &token)?;
        paths.push(json!({ "path": path, "access": access, "action": action }));
    }
    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "principal": "$authenticated",
        "paths": paths,
        "tortoise_log_compatible": true,
        "exhibit_default_access": "no",
        "verified": true
    }))
}

fn ensure_authenticated_path_access(
    project_id: &str,
    path: &str,
    access: &str,
    token: &str,
) -> Result<&'static str, Box<dyn Error>> {
    ensure_principal_path_access(
        project_id,
        path,
        "$authenticated",
        "$authenticated",
        access,
        token,
    )
}

fn ensure_principal_path_access(
    project_id: &str,
    path: &str,
    object_type: &str,
    object_name: &str,
    access: &str,
    token: &str,
) -> Result<&'static str, Box<dyn Error>> {
    if !matches!(access, "r" | "rw" | "no") {
        return Err("unsupported SVN access value".into());
    }
    let query_body = json!({ "rep_name": project_id, "path": path, "svnn_user_pri_path_id": -1 });
    let before = svnadmin_post_read(
        "Svnrep",
        "GetRepPathAllPri",
        Some(token),
        query_body.clone(),
    )?;
    ensure_svnadmin_success(&before)?;
    let existing = before
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("objectType").and_then(Value::as_str) == Some(object_type)
                    && item.get("objectName").and_then(Value::as_str) == Some(object_name)
            })
        });
    let existing_matches = existing.is_some_and(|item| {
        item.get("objectPri").and_then(Value::as_str) == Some(access)
            && !item
                .get("invert")
                .is_some_and(|value| value == true || value == 1)
    });
    let mut mutation_error: Option<Box<dyn Error>> = None;
    let action = if existing_matches {
        "unchanged"
    } else {
        let endpoint_action = if existing.is_some() {
            "UpdRepPathPri"
        } else {
            "CreateRepPathPri"
        };
        let mut body = json!({
            "rep_name": project_id,
            "path": path,
            "objectType": object_type,
            "objectName": object_name,
            "objectPri": access,
            "svnn_user_pri_path_id": -1
        });
        if endpoint_action == "UpdRepPathPri" {
            body["invert"] = Value::Bool(false);
        }
        mutation_error = svnadmin_post("Svnrep", endpoint_action, Some(token), body)
            .and_then(|response| ensure_svnadmin_success(&response))
            .err();
        if endpoint_action == "UpdRepPathPri" {
            if mutation_error.is_some() {
                "updated_after_readback"
            } else {
                "updated"
            }
        } else {
            if mutation_error.is_some() {
                "created_after_readback"
            } else {
                "created"
            }
        }
    };
    let after = svnadmin_post_read("Svnrep", "GetRepPathAllPri", Some(token), query_body)?;
    ensure_svnadmin_success(&after)?;
    let verified = after
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("objectType").and_then(Value::as_str) == Some(object_type)
                    && item.get("objectName").and_then(Value::as_str) == Some(object_name)
                    && item.get("objectPri").and_then(Value::as_str) == Some(access)
                    && !item
                        .get("invert")
                        .is_some_and(|value| value == true || value == 1)
            })
        });
    if !verified {
        if let Some(error) = mutation_error.as_ref() {
            return Err(format!(
                "{error}; SvnAdmin ACL readback did not confirm the requested change"
            )
            .into());
        }
        return Err(
            format!("SvnAdmin did not persist {object_name} {access} access for {path}").into(),
        );
    }
    if mutation_error.is_some() {
        record_svn_diagnostic_event(
            "svnadmin",
            "AclMutation",
            "recovered_by_readback",
            "write_response_uncertain",
            1,
            1,
            None,
            0,
        );
    }
    Ok(action)
}

fn ensure_exhibit_writer_access(
    project_id: &str,
    exhibit_id: &str,
    username: &str,
) -> Result<(), Box<dyn Error>> {
    let path = format!("/trunk/exhibits/{exhibit_id}");
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    ensure_principal_path_access(project_id, &path, "user", username, "rw", &token)?;
    Ok(())
}

pub(crate) fn preview_project_acl(
    request: PreviewProjectAclRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    validate_plan_id(&request.plan_id)?;
    let managed_paths = validate_managed_acl_paths(&request.managed_paths)?;
    let desired = validate_desired_acl_entries(&request.desired_entries, &managed_paths)?;
    let current = read_project_acl(&project_id, &managed_paths)?;
    Ok(acl_plan_result(
        &request.plan_id,
        &project_id,
        &managed_paths,
        &desired,
        &current,
    ))
}

pub(crate) fn apply_project_acl(request: ApplyProjectAclRequest) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    validate_plan_id(&request.plan_id)?;
    let managed_paths = validate_managed_acl_paths(&request.managed_paths)?;
    let desired = validate_desired_acl_entries(&request.desired_entries, &managed_paths)?;
    let before = read_project_acl(&project_id, &managed_paths)?;
    let before_digest = acl_digest(&before)?;
    if before_digest != request.expected_current_digest {
        return Err("SVN ACL changed after preview; generate a new plan".into());
    }
    let repository_policy = ensure_project_exhibits_access(EnsureProjectExhibitsAccessRequest {
        project_id: project_id.clone(),
    })?;
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let current_users = user_acl_map(&before);
    let desired_users = desired_acl_map(&desired);
    let mut applied = Vec::new();
    let mut uncertain_write_count = 0usize;
    let mut first_write_error = None;
    for (key, access) in &desired_users {
        let action = match current_users.get(key) {
            None => "CreateRepPathPri",
            Some(current_access) if current_access != access => "UpdRepPathPri",
            Some(_) => continue,
        };
        let mut body = json!({
            "rep_name": project_id,
            "path": key.0,
            "objectType": "user",
            "objectName": key.1,
            "objectPri": access,
            "svnn_user_pri_path_id": -1
        });
        if action == "UpdRepPathPri" {
            body["invert"] = Value::Bool(false);
        }
        let mutation = svnadmin_post("Svnrep", action, Some(&token), body)
            .and_then(|response| ensure_svnadmin_success(&response));
        let response_confirmed = mutation.is_ok();
        if !response_confirmed {
            uncertain_write_count += 1;
            if first_write_error.is_none() {
                first_write_error = mutation.err().map(|error| error.to_string());
            }
        }
        applied.push(json!({ "action": if action == "CreateRepPathPri" { "create" } else { "update" }, "path": key.0, "username": key.1, "access": access, "response_confirmed": response_confirmed }));
    }
    for (key, access) in &current_users {
        if desired_users.contains_key(key) {
            continue;
        }
        let mutation = svnadmin_post(
            "Svnrep",
            "DelRepPathPri",
            Some(&token),
            json!({
                "rep_name": project_id,
                "path": key.0,
                "objectType": "user",
                "objectName": key.1,
                "svnn_user_pri_path_id": -1
            }),
        )
        .and_then(|response| ensure_svnadmin_success(&response));
        let response_confirmed = mutation.is_ok();
        if !response_confirmed {
            uncertain_write_count += 1;
            if first_write_error.is_none() {
                first_write_error = mutation.err().map(|error| error.to_string());
            }
        }
        applied.push(
            json!({ "action": "delete", "path": key.0, "username": key.1, "access": access, "response_confirmed": response_confirmed }),
        );
    }
    let after = read_project_acl(&project_id, &managed_paths)?;
    if user_acl_map(&after) != desired_users {
        return Err(format!(
            "SvnAdmin ACL readback did not match the approved plan ({uncertain_write_count} write responses were uncertain; first_error={})",
            first_write_error.as_deref().unwrap_or("none")
        )
        .into());
    }
    if uncertain_write_count > 0 {
        record_svn_diagnostic_event(
            "svnadmin",
            "ApplyAclPlan",
            "recovered_by_readback",
            "write_response_uncertain",
            uncertain_write_count,
            uncertain_write_count,
            None,
            0,
        );
    }
    Ok(json!({
        "ok": true,
        "plan_id": request.plan_id,
        "project_id": project_id,
        "before_digest": before_digest,
        "after_digest": acl_digest(&after)?,
        "applied": applied,
        "repository_policy": repository_policy,
        "verified": true,
        "broad_access": broad_acl_entries(&after)
    }))
}

pub(crate) fn reconcile_project_acl(
    request: ReconcileProjectAclRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let managed_paths = validate_managed_acl_paths(&request.managed_paths)?;
    let desired_entries = validate_desired_acl_entries(&request.desired_entries, &managed_paths)?;
    let current = read_project_acl(&project_id, &managed_paths)?;
    apply_project_acl(ApplyProjectAclRequest {
        plan_id: "system-reconcile".to_string(),
        project_id,
        managed_paths,
        desired_entries,
        expected_current_digest: acl_digest(&current)?,
    })
}

fn validate_plan_id(value: &str) -> Result<(), Box<dyn Error>> {
    if value.len() < 8
        || value.len() > 80
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("invalid ACL plan id".into());
    }
    Ok(())
}

fn validate_managed_acl_paths(paths: &[String]) -> Result<Vec<String>, Box<dyn Error>> {
    if paths.is_empty() || paths.len() > 500 {
        return Err("ACL plan requires 1 to 500 managed paths".into());
    }
    let mut result = BTreeSet::new();
    for raw in paths {
        let path = raw.trim().trim_end_matches('/');
        let valid = path == "/trunk"
            || path
                .strip_prefix("/trunk/exhibits/")
                .is_some_and(|id| normalize_repository_name(id).is_ok());
        if !valid {
            return Err(format!("unmanaged SVN ACL path: {path}").into());
        }
        result.insert(path.to_string());
    }
    Ok(result.into_iter().collect())
}

fn validate_desired_acl_entries(
    entries: &[ProjectAclEntry],
    managed_paths: &[String],
) -> Result<Vec<ProjectAclEntry>, Box<dyn Error>> {
    if entries.len() > 2000 {
        return Err("ACL plan contains too many entries".into());
    }
    let paths = managed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for entry in entries {
        let path = entry.path.trim().trim_end_matches('/').to_string();
        let username = entry.username.trim().to_string();
        if !paths.contains(&path)
            || username.is_empty()
            || username.len() > 200
            || entry.access != "rw"
        {
            return Err("invalid desired SVN ACL entry".into());
        }
        if username.starts_with('$') || username.starts_with('@') || username.contains(['\r', '\n'])
        {
            return Err("ACL plan only accepts explicit SVN users".into());
        }
        result.insert(
            (path.clone(), username.clone()),
            ProjectAclEntry {
                path,
                username,
                access: "rw".to_string(),
            },
        );
    }
    Ok(result.into_values().collect())
}

fn read_project_acl(project_id: &str, paths: &[String]) -> Result<Vec<Value>, Box<dyn Error>> {
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let mut entries = Vec::new();
    for path in paths {
        let response = svnadmin_post_read(
            "Svnrep",
            "GetRepPathAllPri",
            Some(&token),
            json!({ "rep_name": project_id, "path": path, "svnn_user_pri_path_id": -1 }),
        )?;
        ensure_svnadmin_success(&response)?;
        for item in response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            entries.push(json!({
                "path": path,
                "object_type": item.get("objectType").and_then(Value::as_str).unwrap_or(""),
                "object_name": item.get("objectName").and_then(Value::as_str).unwrap_or(""),
                "access": item.get("objectPri").and_then(Value::as_str).unwrap_or(""),
                "invert": item.get("invert").is_some_and(|value| value == true || value == 1)
            }));
        }
    }
    entries.sort_by_key(|item| item.to_string());
    Ok(entries)
}

fn acl_digest(entries: &[Value]) -> Result<String, Box<dyn Error>> {
    let encoded = serde_json::to_vec(entries)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn user_acl_map(entries: &[Value]) -> BTreeMap<(String, String), String> {
    entries
        .iter()
        .filter_map(|item| {
            if item.get("object_type").and_then(Value::as_str) != Some("user")
                || item.get("invert").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            Some((
                (
                    item.get("path")?.as_str()?.to_string(),
                    item.get("object_name")?.as_str()?.to_string(),
                ),
                item.get("access")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

fn desired_acl_map(entries: &[ProjectAclEntry]) -> BTreeMap<(String, String), String> {
    entries
        .iter()
        .map(|item| {
            (
                (item.path.clone(), item.username.clone()),
                item.access.clone(),
            )
        })
        .collect()
}

fn broad_acl_entries(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|item| item.get("object_type").and_then(Value::as_str) != Some("user"))
        .cloned()
        .collect()
}

fn acl_plan_result(
    plan_id: &str,
    project_id: &str,
    managed_paths: &[String],
    desired: &[ProjectAclEntry],
    current: &[Value],
) -> Value {
    let current_users = user_acl_map(current);
    let desired_users = desired_acl_map(desired);
    let mut changes = Vec::new();
    for (key, access) in &desired_users {
        match current_users.get(key) {
            None => changes.push(json!({ "action": "create", "path": key.0, "username": key.1, "access": access })),
            Some(existing) if existing != access => changes.push(json!({ "action": "update", "path": key.0, "username": key.1, "from_access": existing, "access": access })),
            Some(_) => {}
        }
    }
    for (key, access) in &current_users {
        if !desired_users.contains_key(key) {
            changes.push(json!({ "action": "delete", "path": key.0, "username": key.1, "from_access": access }));
        }
    }
    json!({
        "ok": true,
        "plan_id": plan_id,
        "project_id": project_id,
        "managed_paths": managed_paths,
        "current_digest": acl_digest(current).unwrap_or_default(),
        "changes": changes,
        "broad_access": broad_acl_entries(current)
    })
}

pub(crate) fn workspace_status(request: SvnWorkspaceRequest) -> Result<Value, Box<dyn Error>> {
    let target = validate_working_copy(&request.target_path)?;
    workspace_status_path(&target)
}

pub(crate) fn scan_migration_source(
    request: MigrationSourceScanRequest,
) -> Result<Value, Box<dyn Error>> {
    let ignore_policy = normalized_ignore_policy(&request.ignore_policy);
    let target = absolute_path(&request.target_path)?;
    reject_sensitive_path(&target)?;
    if !target.is_dir() {
        return Err("target_path must be an existing directory".into());
    }

    let has_svn_metadata = target.join(".svn").is_dir();
    let is_svn = is_valid_svn_working_copy(&target);
    let snapshot = if is_svn {
        snapshot_migration_metadata(&target, true, Some(&ignore_policy))?
    } else {
        MigrationMetadataSnapshot::default()
    };
    let mut file_count = 0_u64;
    let mut total_bytes = 0_u64;
    let mut excluded_count = 0_u64;
    let mut unity = target.join("ProjectSettings").is_dir();
    let mut unreal = false;
    let walker = WalkDir::new(&target)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    for item in walker.filter_entry(|entry| {
        !migration_entry_excluded(entry, &target, &ignore_policy)
            && entry
                .path()
                .strip_prefix(&target)
                .ok()
                .is_none_or(|relative| !path_is_within_roots(relative, &snapshot.external_roots))
    }) {
        let entry = item?;
        if entry.path() == target || entry.file_type().is_dir() {
            continue;
        }
        if entry.file_type().is_symlink() {
            excluded_count += 1;
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        unity |= extension == "unity";
        unreal |= extension == "uproject";
        let metadata = entry.metadata()?;
        file_count += 1;
        total_bytes = total_bytes.saturating_add(metadata.len());
    }

    let mut ignored_files = Vec::new();
    let mut ignore_candidates = Vec::new();
    let mut ignored_bytes = 0_u64;
    for entry in WalkDir::new(&target)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(&target)
                .ok()
                .is_none_or(|relative| {
                    !relative.components().any(|component| {
                        is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
                    })
                })
        })
        .filter_map(Result::ok)
    {
        let Ok(relative) = entry.path().strip_prefix(&target) else {
            continue;
        };
        if relative.components().any(|component| {
            is_migration_excluded_name(component.as_os_str().to_string_lossy().as_ref())
        }) {
            excluded_count += 1;
            continue;
        }
        if entry.file_type().is_file() {
            let size = entry
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or_default();
            if migration_policy_candidate(relative, Some(size), &ignore_policy) {
                let ignored = migration_policy_excludes(relative, Some(size), &ignore_policy);
                ignore_candidates.push(json!({
                    "path": relative.to_string_lossy().replace('\\', "/"),
                    "size_bytes": size,
                    "ignored": ignored,
                }));
                if ignored {
                    excluded_count += 1;
                    ignored_bytes = ignored_bytes.saturating_add(size);
                    ignored_files.push(json!({ "path": relative.to_string_lossy().replace('\\', "/"), "size_bytes": size }));
                }
            }
        }
    }

    let source_fingerprint = migration_source_stability_summary(
        &target,
        &snapshot.external_roots,
        &migration_transform_paths(&snapshot.properties),
    )?;

    let mut repository_url = String::new();
    let mut revision = String::new();
    let mut change_count = 0_u64;
    let mut blocking_reasons: Vec<String> = Vec::new();
    let mut old_remote_status = "not_applicable".to_string();
    let mut warnings = Vec::new();
    if has_svn_metadata && !is_svn {
        warnings.push(
            "根目录包含无效或残留的 .svn 元数据，将按普通本地工程处理；迁移时不会复制该目录"
                .to_string(),
        );
    }
    if is_svn {
        let status = workspace_status_path(&target)?;
        repository_url = status["repository_url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        revision = status["revision"].as_str().unwrap_or_default().to_string();
        change_count = status["change_count"].as_u64().unwrap_or_default();
        old_remote_status = probe_svn_remote(&repository_url, Duration::from_secs(5));
        match old_remote_status.as_str() {
            "missing" => {
                warnings.push("旧 SVN 地址已失效；仍可使用当前本地内容接管新仓库".to_string())
            }
            "temporarily_unreachable" => {
                warnings.push("旧 SVN 当前不可达；为避免误重建，请恢复访问后重新扫描".to_string())
            }
            "authorization_unknown" => warnings
                .push("无法确认旧 SVN 访问权限；为避免误重建，请恢复权限后重新扫描".to_string()),
            _ => {}
        }
        if old_remote_status == "temporarily_unreachable"
            || old_remote_status == "authorization_unknown"
        {
            blocking_reasons.push("旧 SVN 仓库状态尚未确认，已暂停自动重建".to_string());
        }
        let unavailable_externals = snapshot
            .external_status_counts
            .get("missing")
            .copied()
            .unwrap_or_default()
            + snapshot
                .external_status_counts
                .get("temporarily_unreachable")
                .copied()
                .unwrap_or_default()
            + snapshot
                .external_status_counts
                .get("authorization_unknown")
                .copied()
                .unwrap_or_default();
        if unavailable_externals > 0 {
            warnings.push(format!(
                "有 {unavailable_externals} 个外部依赖当前无法确认可重新检出；本地 external 目录将保留"
            ));
        }
    }

    Ok(json!({
        "source_kind": if is_svn { "svn_working_copy" } else { "local_directory" },
        "source_display_name": target.file_name().and_then(|value| value.to_str()).unwrap_or("历史工程"),
        "source_repository_url": repository_url,
        "source_revision": revision,
        "source_fingerprint": format!("sha256:{}", source_fingerprint.digest),
        "file_count": file_count,
        "total_bytes": total_bytes,
        "excluded_count": excluded_count,
        "ignored_files": ignored_files,
        "ignore_candidates": ignore_candidates,
        "ignored_bytes": ignored_bytes,
        "ignore_policy": ignore_policy,
        "change_count": change_count,
        "old_remote_status": old_remote_status,
        "external_count": snapshot.external_count,
        "external_local_checkout_count": snapshot.external_local_checkout_count,
        "external_local_revision_count": snapshot.external_local_revision_count,
        "external_status_counts": snapshot.external_status_counts,
        "preserved_property_count": snapshot.properties.len(),
        "engine_type": if unity { "Unity3D" } else if unreal { "Unreal Engine" } else { "unknown" },
        "blocking_reasons": blocking_reasons,
        "force_migration_available": is_svn && old_remote_status != "missing",
        "warnings": warnings,
    }))
}

fn is_migration_excluded_name(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    if matches!(
        name.as_str(),
        ".svn"
            | "library"
            | "temp"
            | "obj"
            | "logs"
            | "binaries"
            | "deriveddatacache"
            | "intermediate"
            | "saved"
            | "usersettings"
            | ".vs"
    ) {
        return true;
    }
    false
}

pub(crate) fn update_workspace(request: SvnWorkspaceRequest) -> Result<Value, Box<dyn Error>> {
    let target = validate_working_copy(&request.target_path)?;
    let (connection, password) = load_company_svn_secret()?;
    let output = run_svn_authenticated(
        ["update".to_string(), target.to_string_lossy().to_string()],
        &connection.username,
        &password,
    )?;
    Ok(json!({ "ok": true, "output": output, "workspace": workspace_status_path(&target)? }))
}

pub(crate) fn open_workspace(request: SvnWorkspaceRequest) -> Result<Value, Box<dyn Error>> {
    let target = validate_working_copy(&request.target_path)?;
    let working_url = svn_item(&target, "url")?;
    if !working_url.contains("/trunk/exhibits/") {
        return Err(format!(
            "当前工作副本不是展项目录（{}），请重新检出具体展项后再查看提交日志",
            working_url
        )
        .into());
    }
    let (connection, password) = load_company_svn_secret()?;
    run_svn_authenticated(
        [
            "log".to_string(),
            "--limit".to_string(),
            "1".to_string(),
            target.to_string_lossy().to_string(),
        ],
        &connection.username,
        &password,
    )
    .map_err(|error| format!("当前 SVN 账号无法读取该展项提交日志: {error}"))?;
    let executable = find_tortoise_executable().ok_or("TortoiseProc.exe was not found")?;
    Command::new(&executable)
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "/command:log",
            &format!("/path:{}", target.to_string_lossy()),
        ])
        .spawn()?;
    Ok(json!({ "ok": true, "target_path": target, "client": executable }))
}

pub(crate) fn create_repository(request: CreateRepositoryRequest) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    if svnadmin_repository_exists(&token, &project_id)? {
        return Ok(json!({
            "ok": true,
            "created": false,
            "already_exists": true,
            "project_id": project_id,
            "repository_url": project_repository_url(&project_id)?
        }));
    }
    let response = svnadmin_post(
        "Svnrep",
        "CreateRep",
        Some(&token),
        json!({
            "rep_name": project_id,
            "rep_note": request.project_name.trim(),
            "rep_type": "2"
        }),
    );
    let (result_data, mutation_error) = match response {
        Ok(response) => (
            response.get("data").cloned().unwrap_or(Value::Null),
            ensure_svnadmin_success(&response).err(),
        ),
        Err(error) => (Value::Null, Some(error)),
    };
    let recovered_after_error = mutation_error.is_some();
    if recovered_after_error {
        if !svnadmin_repository_exists(&token, &project_id)? {
            return Err(mutation_error
                .unwrap_or_else(|| "SvnAdmin did not persist the new repository".into()));
        }
        record_svn_diagnostic_event(
            "svnadmin",
            "CreateRep",
            "recovered_by_readback",
            "write_response_uncertain",
            1,
            1,
            None,
            0,
        );
    }
    Ok(json!({
        "ok": true,
        "created": true,
        "recovered_after_error": recovered_after_error,
        "project_id": project_id,
        "repository_url": project_repository_url(&project_id)?,
        "result": result_data
    }))
}

const HIMIND_SVN_POST_COMMIT_HOOK_VERSION: &str = "HiMind SVN post-commit v2";

pub(crate) fn create_repository_with_post_commit_hook(
    request: CreateRepositoryRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let hook_endpoint = validate_repository_hook_endpoint(&request.hook_endpoint)?;
    let mut repository = create_repository(request)?;
    let hook_credential = generate_repository_hook_credential();
    let hook = install_repository_post_commit_hook(&project_id, &hook_endpoint, &hook_credential)?;
    let result = repository
        .as_object_mut()
        .ok_or("repository creation result must be an object")?;
    result.insert("post_commit_hook".to_string(), hook);
    Ok(repository)
}

pub(crate) fn install_repository_post_commit_hook(
    project_id: &str,
    hook_endpoint: &str,
    hook_credential: &str,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(project_id)?;
    let hook_endpoint = validate_repository_hook_endpoint(hook_endpoint)?;
    if hook_credential.trim().is_empty() {
        return Err("repository Hook credential is required".into());
    }
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let content = repository_post_commit_hook_content(&hook_endpoint, hook_credential);
    let content_sha256 = repository_post_commit_hook_sha256(&content);
    let credential_sha256 = repository_post_commit_hook_sha256(hook_credential);

    let mutation_error = svnadmin_post(
        "Svnrep",
        "UpdRepHook",
        Some(&token),
        json!({
            "rep_name": project_id,
            "fileName": "post-commit",
            "content": content,
        }),
    )
    .and_then(|response| ensure_svnadmin_success(&response))
    .err();

    let verify = svnadmin_post_read(
        "Svnrep",
        "GetRepHooks",
        Some(&token),
        json!({ "rep_name": project_id }),
    )?;
    ensure_svnadmin_success(&verify)?;
    let hook = verify
        .pointer("/data/post_commit")
        .ok_or("SvnAdmin hook readback did not include post-commit")?;
    let has_file = hook
        .get("hasFile")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let file_name = hook
        .get("fileName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let installed_content = hook.get("con").and_then(Value::as_str).unwrap_or_default();
    if !has_file || file_name != "post-commit" || installed_content != content {
        if let Some(error) = mutation_error.as_ref() {
            return Err(format!(
                "{error}; SvnAdmin Hook readback did not confirm the requested content"
            )
            .into());
        }
        return Err("SvnAdmin post-commit Hook readback verification failed".into());
    }
    let recovered_after_error = mutation_error.is_some();
    if recovered_after_error {
        record_svn_diagnostic_event(
            "svnadmin",
            "UpdRepHook",
            "recovered_by_readback",
            "write_response_uncertain",
            1,
            1,
            None,
            0,
        );
    }

    Ok(json!({
        "installed": true,
        "recovered_after_error": recovered_after_error,
        "repository": project_id,
        "hook_version": HIMIND_SVN_POST_COMMIT_HOOK_VERSION,
        "content_sha256": content_sha256,
        "credential_sha256": credential_sha256,
    }))
}

fn validate_repository_hook_endpoint(value: &str) -> Result<String, Box<dyn Error>> {
    let endpoint = Url::parse(value.trim())?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err("repository Hook endpoint must be an absolute HTTP(S) URL without credentials, query, or fragment".into());
    }
    Ok(endpoint.to_string())
}

fn generate_repository_hook_credential() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn repository_post_commit_hook_sha256(content: &str) -> String {
    Sha256::digest(content.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn repository_post_commit_hook_content(endpoint: &str, credential: &str) -> String {
    let endpoint_literal = php_single_quoted_literal(endpoint);
    let credential_literal = php_single_quoted_literal(credential);
    format!(
        r#"#!/usr/bin/php
<?php
// {HIMIND_SVN_POST_COMMIT_HOOK_VERSION}
// This Hook must never make an SVN commit fail.
try {{
    $repositoryPath = $argv[1] ?? '';
    $revision = trim((string)($argv[2] ?? ''));
    $endpoint = {endpoint_literal};
    $credential = {credential_literal};
    $endpointParts = $endpoint === '' ? false : parse_url($endpoint);
    if ($repositoryPath === '' || !ctype_digit($revision) || (int)$revision < 1 || $endpointParts === false || !isset($endpointParts['scheme']) || !in_array(strtolower($endpointParts['scheme']), ['http', 'https'], true) || $credential === '') {{
        exit(0);
    }}

    $truncate = static function (string $value, int $limit): string {{
        if (strlen($value) <= $limit) {{
            return $value;
        }}
        if (function_exists('mb_strcut')) {{
            return mb_strcut($value, 0, $limit, 'UTF-8');
        }}
        return substr($value, 0, $limit);
    }};
    $svnlook = static function (string $action) use ($repositoryPath, $revision): string {{
        $output = [];
        $status = 0;
        $command = '/usr/bin/svnlook ' . escapeshellarg($action) . ' -r ' . escapeshellarg($revision) . ' ' . escapeshellarg($repositoryPath) . ' 2>/dev/null';
        exec($command, $output, $status);
        return $status === 0 ? trim(implode("\n", $output)) : '';
    }};

    $author = $truncate($svnlook('author'), 200);
    if ($author === '') {{
        exit(0);
    }}
    $paths = [];
    foreach (preg_split('/\\r?\\n/', $svnlook('changed')) as $line) {{
        if (strlen($line) < 5 || count($paths) >= 500) {{
            continue;
        }}
        $path = trim(substr($line, 4));
        if ($path !== '') {{
            $paths[] = '/' . ltrim($truncate($path, 1000), '/');
        }}
    }}
    $date = preg_replace('/\\s+\\(.*/', '', $svnlook('date'));
    $committedAt = gmdate('c');
    if (is_string($date) && $date !== '') {{
        try {{
            $committedAt = (new DateTimeImmutable($date))->setTimezone(new DateTimeZone('UTC'))->format('Y-m-d\\TH:i:s.u\\Z');
        }} catch (Throwable $ignored) {{
        }}
    }}
    $payload = json_encode([
        'repository_name' => basename(rtrim($repositoryPath, '/')),
        'revision' => (int)$revision,
        'svn_username' => $author,
        'commit_message' => $truncate($svnlook('log'), 4000),
        'changed_paths' => $paths,
        'committed_at' => $committedAt,
    ], JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
    if ($payload === false) {{
        exit(0);
    }}
    $timestamp = (string)time();
    $command = '/usr/bin/curl --fail --silent --show-error --connect-timeout 3 --max-time 5 -X POST'
        . ' -H ' . escapeshellarg('Content-Type: application/json')
        . ' -H ' . escapeshellarg('X-Himind-Hook-Timestamp: ' . $timestamp)
        . ' -H ' . escapeshellarg('X-Himind-Hook-Token: ' . $credential)
        . ' --data-binary ' . escapeshellarg($payload)
        . ' ' . escapeshellarg($endpoint)
        . ' >/dev/null 2>&1';
    exec($command, $ignoredOutput, $ignoredStatus);
}} catch (Throwable $ignored) {{
}}
exit(0);
"#
    )
}

fn php_single_quoted_literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn svnadmin_repository_exists(token: &str, project_id: &str) -> Result<bool, Box<dyn Error>> {
    let response = svnadmin_post_read(
        "Svnrep",
        "GetRepCon",
        Some(token),
        json!({
            "rep_name": project_id,
            "path": "/",
            "svnn_user_pri_path_id": -1
        }),
    )?;
    svnadmin_repository_exists_response(&response)
}

fn svnadmin_repository_exists_response(response: &Value) -> Result<bool, Box<dyn Error>> {
    if response.get("status").and_then(Value::as_i64) == Some(1) {
        return Ok(true);
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("SvnAdmin repository lookup failed");
    let normalized = message.to_ascii_lowercase();
    if message.contains("仓库不存在")
        || normalized.contains("repository does not exist")
        || normalized.contains("repository not found")
    {
        return Ok(false);
    }
    Err(message.to_string().into())
}

fn workspace_status_path(target: &Path) -> Result<Value, Box<dyn Error>> {
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let info = Command::new(&executable)
        .args(["info", "--show-item", "url"])
        .current_dir(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !info.status.success() {
        return Err(decode_svn_cli_output(&info.stderr)
            .trim()
            .to_string()
            .into());
    }
    let revision = Command::new(&executable)
        .args(["info", "--show-item", "revision"])
        .current_dir(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    let changes = Command::new(&executable)
        .args(["status", "--xml"])
        .current_dir(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !revision.status.success() || !changes.status.success() {
        return Err("failed to read SVN working copy status".into());
    }
    let status_xml = decode_svn_cli_output(&changes.stdout);
    let change_count = status_xml.matches("<entry").count();
    Ok(json!({
        "target_path": target,
        "repository_url": decode_svn_cli_output(&info.stdout).trim(),
        "repository_uuid": svn_item(target, "repos-uuid").unwrap_or_default(),
        "repository_root_url": svn_item(target, "repos-root-url").unwrap_or_default(),
        "revision": decode_svn_cli_output(&revision.stdout).trim(),
        "change_count": change_count,
        "clean": change_count == 0
    }))
}

fn run_svn_authenticated<I>(
    arguments: I,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn Error>>
where
    I: IntoIterator<Item = String>,
{
    run_svn_authenticated_cancelable(arguments, username, password, &mut || Ok(()))
}

fn run_svn_authenticated_cancelable<I, F>(
    arguments: I,
    username: &str,
    password: &str,
    cancel: &mut F,
) -> Result<String, Box<dyn Error>>
where
    I: IntoIterator<Item = String>,
    F: FnMut() -> Result<(), Box<dyn Error>>,
{
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let mut command_arguments: Vec<String> = arguments.into_iter().collect();
    command_arguments.extend([
        "--non-interactive".to_string(),
        "--no-auth-cache".to_string(),
        "--username".to_string(),
        username.to_string(),
        "--password-from-stdin".to_string(),
    ]);
    let mut child = Command::new(&executable)
        .args(&command_arguments)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.as_bytes())?;
        stdin.write_all(b"\r\n")?;
    }
    let mut stdout = child.stdout.take().ok_or("failed to capture SVN stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture SVN stderr")?;
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = loop {
        if let Err(error) = cancel() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(error);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(250));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "failed to join SVN stdout reader")??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "failed to join SVN stderr reader")??;
    if !status.success() {
        return Err(decode_svn_cli_output(&stderr).trim().to_string().into());
    }
    Ok(decode_svn_cli_output(&stdout).trim().to_string())
}

fn load_company_svn_secret(
) -> Result<(crate::store::types::StoredSvnConnection, String), Box<dyn Error>> {
    if let Ok(secret) = load_local_svn_connection_secret(SVN_CONNECTION_ID) {
        return Ok(secret);
    }
    let legacy = list_local_svn_connections()?
        .into_iter()
        .find(|item| item.provider == "svn")
        .ok_or("SVN account is not configured")?;
    load_local_svn_connection_secret(&legacy.id)
}

fn load_svn_admin_secret(
) -> Result<(crate::store::types::StoredSvnConnection, String), Box<dyn Error>> {
    if let Ok(secret) = load_local_svn_connection_secret(SVN_ADMIN_CONNECTION_ID) {
        return Ok(secret);
    }
    let username = std::env::var("SVN_ADMIN_USERNAME").unwrap_or_default();
    let password = std::env::var("SVN_ADMIN_PASSWORD").unwrap_or_default();
    if username.trim().is_empty() || password.is_empty() {
        return Err("SvnAdmin credentials are not configured on this Agent".into());
    }
    bootstrap_svn_admin_credentials()?;
    load_local_svn_connection_secret(SVN_ADMIN_CONNECTION_ID)
}

fn login_svnadmin(username: &str, password: &str) -> Result<String, Box<dyn Error>> {
    let verify_option = svnadmin_post_read("Setting", "GetVerifyOption", None, json!({}))?;
    ensure_svnadmin_success(&verify_option)?;
    if verify_option
        .pointer("/data/enable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(
            "SvnAdmin verification code is enabled; disable it for Agent automation".into(),
        );
    }
    let response = svnadmin_post_read(
        "Common",
        "Login",
        None,
        json!({ "user_name": username, "user_pass": password, "user_role": "1" }),
    )?;
    ensure_svnadmin_success(&response)?;
    response
        .pointer("/data/token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "SvnAdmin login response did not include a token".into())
}

fn svnadmin_post(
    controller: &str,
    action: &str,
    token: Option<&str>,
    body: Value,
) -> Result<Value, Box<dyn Error>> {
    svnadmin_post_with_attempts(controller, action, token, body, 1)
}

fn svnadmin_post_read(
    controller: &str,
    action: &str,
    token: Option<&str>,
    body: Value,
) -> Result<Value, Box<dyn Error>> {
    svnadmin_post_with_attempts(controller, action, token, body, SVN_ADMIN_READ_ATTEMPTS)
}

fn svnadmin_post_with_attempts(
    controller: &str,
    action: &str,
    token: Option<&str>,
    body: Value,
    max_attempts: usize,
) -> Result<Value, Box<dyn Error>> {
    let max_attempts = max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match svnadmin_post_once(controller, action, token, &body, attempt, max_attempts) {
            Ok(response) => {
                if attempt > 1 {
                    record_svn_diagnostic_event(
                        "svnadmin",
                        action,
                        "recovered",
                        "",
                        attempt,
                        max_attempts,
                        None,
                        0,
                    );
                }
                return Ok(response);
            }
            Err(error) => {
                let should_retry = error.retryable && attempt < max_attempts;
                record_svn_diagnostic_event(
                    "svnadmin",
                    action,
                    if should_retry { "retrying" } else { "failed" },
                    error.category,
                    attempt,
                    max_attempts,
                    error.http_status,
                    error.elapsed_ms,
                );
                if !should_retry {
                    return Err(Box::new(error));
                }
                thread::sleep(svnadmin_retry_delay(attempt));
            }
        }
    }
    Err("SvnAdmin request exhausted all attempts".into())
}

fn svnadmin_post_once(
    controller: &str,
    action: &str,
    token: Option<&str>,
    body: &Value,
    attempt: usize,
    max_attempts: usize,
) -> Result<Value, SvnAdminRequestError> {
    let endpoint = format!("{SVN_ADMIN_URL}/api.php?c={controller}&a={action}&t=web");
    let started = Instant::now();
    let client = SVN_ADMIN_CLIENT.get_or_init(reqwest::blocking::Client::new);
    let mut request = client
        .post(endpoint)
        .timeout(Duration::from_secs(20))
        .json(body);
    if let Some(token) = token {
        request = request.header("Token", token);
    }
    let response = request.send().map_err(|error| {
        let category = if error.is_timeout() {
            "timeout"
        } else if error.is_connect() {
            "connect"
        } else {
            "transport"
        };
        SvnAdminRequestError {
            action: action.to_string(),
            category,
            attempt,
            max_attempts,
            http_status: error.status().map(|status| status.as_u16()),
            elapsed_ms: started.elapsed().as_millis(),
            retryable: error.is_timeout() || error.is_connect() || error.is_request(),
            message: error.to_string(),
        }
    })?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(SvnAdminRequestError {
            action: action.to_string(),
            category: "http",
            attempt,
            max_attempts,
            http_status: Some(status.as_u16()),
            elapsed_ms: started.elapsed().as_millis(),
            retryable: is_retryable_svnadmin_status(status.as_u16()),
            message: format!("SvnAdmin returned HTTP {status}"),
        });
    }
    response.json().map_err(|error| SvnAdminRequestError {
        action: action.to_string(),
        category: "invalid_response",
        attempt,
        max_attempts,
        http_status: error.status().map(|status| status.as_u16()),
        elapsed_ms: started.elapsed().as_millis(),
        retryable: true,
        message: "SvnAdmin returned an invalid JSON response".to_string(),
    })
}

fn is_retryable_svnadmin_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn svnadmin_retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(match attempt {
        1 => 250,
        _ => 750,
    })
}

fn record_svn_diagnostic_event(
    component: &str,
    action: &str,
    outcome: &str,
    category: &str,
    attempt: usize,
    max_attempts: usize,
    http_status: Option<u16>,
    elapsed_ms: u128,
) {
    let context = SVN_DIAGNOSTIC_CONTEXT.with(|value| value.borrow().clone());
    let event = json!({
        "timestamp": svn_diagnostic_unix_now(),
        "component": component,
        "action": action,
        "outcome": outcome,
        "category": category,
        "attempt": attempt,
        "max_attempts": max_attempts,
        "http_status": http_status,
        "elapsed_ms": elapsed_ms,
        "task_id": context.task_id,
        "execution_id": context.execution_id,
    });
    let lock = SVN_DIAGNOSTIC_LOG_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.lock() else {
        return;
    };
    let path = crate::store::paths::agent_home()
        .join("logs")
        .join("svn-events.jsonl");
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    rotate_svn_diagnostic_logs(&path);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    if let Ok(line) = serde_json::to_string(&event) {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

fn rotate_svn_diagnostic_logs(path: &Path) {
    const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
    if path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or_default()
        < MAX_LOG_BYTES
    {
        return;
    }
    let second = path.with_extension("jsonl.2");
    let first = path.with_extension("jsonl.1");
    let _ = std::fs::remove_file(&second);
    let _ = std::fs::rename(&first, &second);
    let _ = std::fs::rename(path, &first);
}

fn svn_diagnostic_unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn ensure_svnadmin_success(response: &Value) -> Result<(), Box<dyn Error>> {
    if response.get("status").and_then(Value::as_i64) == Some(1) {
        return Ok(());
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("SvnAdmin operation failed");
    Err(message.to_string().into())
}

fn is_existing_repository_path_error(message: &str) -> bool {
    let value = message.to_ascii_lowercase();
    value.contains("file already exists")
        || value.contains("path already exists")
        || value.contains("e160020")
}

fn validate_checkout_target(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let target = absolute_path(value)?;
    reject_sensitive_path(&target)?;
    if target.exists() {
        if !target.is_dir() {
            return Err("checkout target must be a directory".into());
        }
        if is_valid_svn_working_copy(&target) {
            return Err("checkout target is already an SVN working copy".into());
        }
    } else if let Some(parent) = target.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(target)
}

fn validate_working_copy(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let target = absolute_path(value)?;
    reject_sensitive_path(&target)?;
    if !target.is_dir() || !is_valid_svn_working_copy(&target) {
        return Err("target_path is not an SVN working copy".into());
    }
    Ok(target)
}

fn absolute_path(value: &str) -> Result<PathBuf, Box<dyn Error>> {
    let target = PathBuf::from(value.trim());
    if !target.is_absolute() {
        return Err("target_path must be an absolute path".into());
    }
    Ok(target)
}

fn reject_sensitive_path(target: &Path) -> Result<(), Box<dyn Error>> {
    let normalized = target
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let windows = std::env::var("WINDIR")
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .to_ascii_lowercase();
    let program_files = [
        r"c:\program files",
        r"c:\program files (x86)",
        r"c:\programdata",
    ];
    if normalized == r"c:\"
        || normalized.starts_with(&(windows + "\\"))
        || program_files
            .iter()
            .any(|root| normalized == *root || normalized.starts_with(&format!("{root}\\")))
    {
        return Err("target_path points to a protected system directory".into());
    }
    Ok(())
}

fn normalize_repository_name(value: &str) -> Result<String, Box<dyn Error>> {
    let name = value.trim();
    if name.is_empty()
        || name.len() > 80
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(
            "repository_name must use 1-80 letters, numbers, dots, hyphens, or underscores".into(),
        );
    }
    Ok(name.to_string())
}

fn project_repository_url(project_id: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{SVN_SERVICE_URL}/{}",
        normalize_repository_name(project_id)?
    ))
}

fn exhibit_repository_url(project_id: &str, exhibit_id: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{}/trunk/exhibits/{}",
        project_repository_url(project_id)?,
        normalize_repository_name(exhibit_id)?
    ))
}

fn checkout_repository_url(
    repository_url: Option<&str>,
    project_id: &str,
    exhibit_id: &str,
) -> Result<String, Box<dyn Error>> {
    let Some(value) = repository_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return exhibit_repository_url(project_id, exhibit_id);
    };
    let parsed =
        Url::parse(value).map_err(|_| "repository_url must be a valid HiMind exhibit SVN URL")?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("svn.andcrane.com")
        || parsed.port_or_known_default() != Some(80)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path().contains('%')
    {
        return Err("repository_url must be a HiMind exhibit SVN URL".into());
    }
    let segments = parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if segments.len() != 5
        || segments[0] != "repo"
        || normalize_repository_name(segments[1]).is_err()
        || segments[2] != "trunk"
        || segments[3] != "exhibits"
        || normalize_repository_name(segments[4]).is_err()
    {
        return Err("repository_url must be a HiMind exhibit SVN URL".into());
    }
    Ok(format!(
        "{SVN_SERVICE_URL}/{}/trunk/exhibits/{}",
        segments[1], segments[4]
    ))
}

fn required_value(value: &str, label: &str) -> Result<String, Box<dyn Error>> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(format!("{label} is required").into());
    }
    Ok(normalized.to_string())
}

fn find_svn_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SVN_EXECUTABLE") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let tortoise = PathBuf::from(r"C:\Program Files\TortoiseSVN\bin\svn.exe");
    if tortoise.is_file() {
        return Some(tortoise);
    }
    Some(PathBuf::from("svn.exe"))
}

fn find_tortoise_executable() -> Option<PathBuf> {
    let candidate = PathBuf::from(r"C:\Program Files\TortoiseSVN\bin\TortoiseProc.exe");
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_svn_username_from_himind_name() {
        assert_eq!(default_svn_username("马宝全").unwrap(), "马宝全");
        assert_eq!(default_svn_username(" 李鹏（软件） ").unwrap(), "李鹏");
        assert_eq!(default_svn_username("陈晨(软件)").unwrap(), "陈晨");
        assert!(default_svn_username("\n").is_err());
    }

    #[test]
    fn validates_repository_paths_and_names() {
        assert_eq!(normalize_repository_name("Project_1").unwrap(), "Project_1");
        assert!(normalize_repository_name("bad/name").is_err());
        assert!(normalize_repository_name(&"a".repeat(81)).is_err());
        assert_eq!(
            project_repository_url("prj_123").unwrap(),
            "http://svn.andcrane.com/repo/prj_123"
        );
        assert!(project_repository_url("bad/name").is_err());
        assert_eq!(
            exhibit_repository_url("prj_123", "EXH-000001").unwrap(),
            "http://svn.andcrane.com/repo/prj_123/trunk/exhibits/EXH-000001"
        );
        assert!(exhibit_repository_url("prj_123", "bad/name").is_err());
    }

    #[test]
    fn checkout_prefers_a_registered_himind_exhibit_url() {
        assert_eq!(
            checkout_repository_url(
                Some("http://svn.andcrane.com/repo/prj_legacy/trunk/exhibits/EX-0088/"),
                "prj_current",
                "EX-0088",
            )
            .unwrap(),
            "http://svn.andcrane.com/repo/prj_legacy/trunk/exhibits/EX-0088"
        );
        assert_eq!(
            checkout_repository_url(None, "prj_current", "EX-0088").unwrap(),
            "http://svn.andcrane.com/repo/prj_current/trunk/exhibits/EX-0088"
        );
    }

    #[test]
    fn checkout_rejects_untrusted_or_malformed_repository_urls() {
        for url in [
            "https://svn.andcrane.com/repo/prj_1/trunk/exhibits/EX-1",
            "http://evil.example/repo/prj_1/trunk/exhibits/EX-1",
            "http://svn.andcrane.com/repo/prj_1/branches/exhibits/EX-1",
            "http://svn.andcrane.com/repo/prj_1/trunk/exhibits/EX-1/child",
            "http://svn.andcrane.com/repo/prj_1/trunk/exhibits/%2e%2e",
            "http://user@svn.andcrane.com/repo/prj_1/trunk/exhibits/EX-1",
        ] {
            assert!(checkout_repository_url(Some(url), "prj_current", "EX-1").is_err());
        }
    }

    #[test]
    fn parses_svnadmin_repository_lookup_response() {
        assert!(svnadmin_repository_exists_response(&json!({"status": 1, "data": {}})).unwrap());
        assert!(!svnadmin_repository_exists_response(
            &json!({"status": 0, "message": "仓库不存在", "data": []})
        )
        .unwrap());
        assert!(svnadmin_repository_exists_response(
            &json!({"status": 0, "message": "管理员权限不足"})
        )
        .is_err());
    }

    #[test]
    fn builds_a_repository_scoped_post_commit_hook() {
        let endpoint = "https://dashboard.example.com/api/internal/svn/repository-events";
        let credential = "repository-scoped-test-credential";
        let content = repository_post_commit_hook_content(endpoint, credential);
        assert!(content.starts_with("#!/usr/bin/php\n<?php\n"));
        assert!(content.contains(HIMIND_SVN_POST_COMMIT_HOOK_VERSION));
        assert!(content.contains(endpoint));
        assert!(content.contains(credential));
        assert!(!content.contains("getenv("));
        assert!(content.contains("X-Himind-Hook-Token"));
        assert!(content.contains("/usr/bin/svnlook"));
        assert!(content.contains("/usr/bin/curl"));
        assert!(content.contains("exit(0);"));
        assert_eq!(
            repository_post_commit_hook_sha256(&content),
            repository_post_commit_hook_sha256(&repository_post_commit_hook_content(
                endpoint, credential
            ))
        );
    }

    #[test]
    fn generates_unique_repository_hook_credentials() {
        let first = generate_repository_hook_credential();
        let second = generate_repository_hook_credential();
        assert_eq!(first.len(), 43);
        assert_eq!(second.len(), 43);
        assert_ne!(first, second);
        assert!(!first.contains(['+', '/', '=']));
    }

    #[test]
    fn validates_repository_hook_endpoints() {
        assert!(validate_repository_hook_endpoint(
            "https://dashboard.example.com/api/internal/svn/repository-events"
        )
        .is_ok());
        for invalid in [
            "relative/path",
            "ftp://dashboard.example.com/hook",
            "https://user@dashboard.example.com/hook",
            "https://dashboard.example.com/hook?token=secret",
            "https://dashboard.example.com/hook#fragment",
        ] {
            assert!(validate_repository_hook_endpoint(invalid).is_err());
        }
    }

    #[test]
    fn rejects_relative_and_system_checkout_targets() {
        assert!(absolute_path("relative/path").is_err());
        assert!(reject_sensitive_path(Path::new(r"C:\Windows\Temp\repo")).is_err());
        assert!(reject_sensitive_path(Path::new(r"D:\Projects\repo")).is_ok());
    }

    #[test]
    fn accepts_non_empty_checkout_targets_for_controlled_takeover() {
        let target = std::env::temp_dir().join(format!(
            " himind-non-empty-checkout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("local.txt"), "keep").unwrap();

        assert_eq!(
            validate_checkout_target(target.to_string_lossy().as_ref()).unwrap(),
            target
        );

        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn cleanup_partial_checkout_only_removes_new_paths_and_svn_metadata() {
        let target = std::env::temp_dir().join(format!(
            " himind-checkout-rollback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(target.join("existing")).unwrap();
        std::fs::write(target.join("existing/local.txt"), "keep").unwrap();
        let manifest = workspace_path_manifest(&target).unwrap();
        std::fs::create_dir_all(target.join(".svn")).unwrap();
        std::fs::write(target.join(".svn/wc.db"), "new metadata").unwrap();
        std::fs::create_dir_all(target.join("downloaded")).unwrap();
        std::fs::write(target.join("downloaded/remote.txt"), "remove").unwrap();

        cleanup_partial_checkout(&target, &manifest).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.join("existing/local.txt")).unwrap(),
            "keep"
        );
        assert!(!target.join("downloaded").exists());
        assert!(!target.join(".svn").exists());
        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn compares_repository_locations_without_confusing_uuid_and_url() {
        assert!(same_svn_url(
            "http://svn.example/repo/project/",
            "HTTP://SVN.EXAMPLE/repo/project"
        ));
        assert_eq!(
            svn_repository_relative_url(
                "http://svn.example/repo/project/trunk/exhibits/EX-1",
                "http://svn.example/repo/project"
            ),
            Some("trunk/exhibits/EX-1".to_string())
        );
        assert_eq!(
            PROJECT_REPOSITORY_BROAD_ACL,
            [("/", "r"), ("/trunk", "r"), ("/trunk/exhibits", "no")]
        );
    }

    #[test]
    fn external_update_uses_svn_update_default_behavior() {
        assert_eq!(
            workspace_externals_update_arguments(Path::new(r"D:\Projects\Exhibit")),
            ["update".to_string(), r"D:\Projects\Exhibit".to_string()]
        );
    }

    #[test]
    fn migration_scan_is_read_only_and_does_not_expose_source_path() {
        let target =
            std::env::temp_dir().join(format!(" himind-migration-scan-{}", std::process::id()));
        let assets = target.join("Assets");
        let library = target.join("Library");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(assets.join("Main.unity"), "scene").unwrap();
        std::fs::write(library.join("cache.bin"), "cache").unwrap();

        let result = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
            ignore_policy: MigrationIgnorePolicy::default(),
        })
        .unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        assert_eq!(result["source_kind"], "local_directory");
        assert_eq!(result["engine_type"], "Unity3D");
        assert_eq!(result["file_count"], 1);
        assert!(!serialized.contains(&target.to_string_lossy().to_string()));

        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn migration_scan_treats_stale_svn_metadata_as_a_local_directory() {
        let target = std::env::temp_dir().join(format!(
            " himind-stale-svn-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(target.join(".svn")).unwrap();
        std::fs::create_dir_all(target.join("Assets")).unwrap();
        std::fs::write(target.join(".svn/wc.db"), "stale metadata").unwrap();
        std::fs::write(target.join("Assets/Main.unity"), "scene").unwrap();

        let result = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
            ignore_policy: MigrationIgnorePolicy::default(),
        })
        .unwrap();

        assert_eq!(result["source_kind"], "local_directory");
        assert_eq!(result["old_remote_status"], "not_applicable");
        assert!(result["warnings"]
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty()));
        assert!(validate_checkout_target(target.to_string_lossy().as_ref()).is_ok());

        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn migration_scan_ignores_only_root_archives_and_large_files() {
        let target = std::env::temp_dir().join(format!(
            " himind-ignore-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(target.join("Assets")).unwrap();
        std::fs::write(target.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(target.join("backup.zip"), "archive").unwrap();
        std::fs::write(target.join("Assets/required.zip"), "asset").unwrap();
        std::fs::write(target.join("large.bin"), vec![0_u8; 16]).unwrap();

        let result = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
            ignore_policy: MigrationIgnorePolicy {
                root_large_file_threshold_bytes: 8,
                ..MigrationIgnorePolicy::default()
            },
        })
        .unwrap();
        let ignored = result["ignored_files"].as_array().unwrap();
        assert!(ignored.iter().any(|item| item["path"] == "backup.zip"));
        assert!(ignored.iter().any(|item| item["path"] == "large.bin"));
        assert!(!ignored
            .iter()
            .any(|item| item["path"] == "Assets/required.zip"));
        assert_eq!(result["file_count"], 2);
        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn explicit_include_overrides_root_ignore_policy() {
        let policy = normalized_ignore_policy(&MigrationIgnorePolicy {
            included_relative_paths: vec!["backup.zip".to_string()],
            ..MigrationIgnorePolicy::default()
        });
        assert!(!migration_policy_excludes(
            Path::new("backup.zip"),
            Some(200 * 1024 * 1024),
            &policy
        ));
        assert!(migration_policy_excludes(
            Path::new("other.zip"),
            Some(1),
            &policy
        ));
    }

    #[test]
    fn migration_fingerprint_detects_changes_to_locally_retained_files() {
        let target = std::env::temp_dir().join(format!(
            " himind-retained-fingerprint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        std::fs::create_dir_all(target.join("Assets")).unwrap();
        std::fs::write(target.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(target.join("backup.zip"), "archive-v1").unwrap();

        let first = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
            ignore_policy: MigrationIgnorePolicy::default(),
        })
        .unwrap();
        std::fs::write(target.join("backup.zip"), "archive-v2").unwrap();
        let second = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
            ignore_policy: MigrationIgnorePolicy::default(),
        })
        .unwrap();

        assert_ne!(first["source_fingerprint"], second["source_fingerprint"]);
        std::fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn migration_ignore_policy_keeps_local_files_out_of_a_real_svn_repository() {
        let svn = find_svn_executable().unwrap();
        let svnadmin = svn.with_file_name("svnadmin.exe");
        if !svnadmin.is_file() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            " himind-ignore-svn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let source = root.join("source");
        let repository = root.join("repository");
        let working_copy = root.join("working-copy");
        std::fs::create_dir_all(source.join("Assets")).unwrap();
        std::fs::write(source.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(source.join("Assets/required.zip"), "required asset").unwrap();
        std::fs::write(source.join("backup.zip"), "local archive").unwrap();
        std::fs::write(source.join("large.bin"), vec![7_u8; 16]).unwrap();
        let retained_before =
            migration_source_stability_summary(&source, &[], &BTreeSet::new()).unwrap();

        let created = Command::new(&svnadmin)
            .args(["create", repository.to_string_lossy().as_ref()])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();
        assert!(created.success());
        let repository_url = Url::from_file_path(&repository).unwrap().to_string();
        let checked_out = Command::new(&svn)
            .args([
                "checkout",
                &repository_url,
                working_copy.to_string_lossy().as_ref(),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();
        assert!(checked_out.success());

        let policy = normalized_ignore_policy(&MigrationIgnorePolicy {
            root_large_file_threshold_bytes: 8,
            ..MigrationIgnorePolicy::default()
        });
        let source_summary =
            copy_migration_tree(&source, &working_copy, &[], &BTreeSet::new(), &policy).unwrap();
        run_svn_in_directory(
            &working_copy,
            [
                "add",
                "--force",
                "--no-ignore",
                "--parents",
                "--depth",
                "infinity",
                ".",
            ],
        )
        .unwrap();
        apply_migration_ignore_policy(&source, &working_copy, &policy).unwrap();
        run_svn_in_directory(&working_copy, ["commit", "-m", "migration ignore test"]).unwrap();

        std::fs::copy(source.join("backup.zip"), working_copy.join("backup.zip")).unwrap();
        std::fs::copy(source.join("large.bin"), working_copy.join("large.bin")).unwrap();
        let ignore = run_svn_in_directory(&working_copy, ["propget", "svn:ignore", "."]).unwrap();
        assert!(ignore.lines().any(|line| line == "*.zip"));
        assert!(ignore.lines().any(|line| line == "large.bin"));
        assert!(run_svn_in_directory(&working_copy, ["status"])
            .unwrap()
            .is_empty());

        let listing = Command::new(&svn)
            .args(["list", "--recursive", &repository_url])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let listing = decode_svn_cli_output(&listing.stdout);
        assert!(listing.contains("Assets/Main.unity"));
        assert!(listing.contains("Assets/required.zip"));
        assert!(!listing.contains("backup.zip"));
        assert!(!listing.contains("large.bin"));

        let uuid = svn_item(&working_copy, "repos-uuid").unwrap();
        verify_migration_working_copy(
            &working_copy,
            &repository_url,
            &uuid,
            &source_summary,
            &[],
            &BTreeSet::new(),
            &policy,
            "test repository",
        )
        .unwrap();
        let retained_after =
            migration_source_stability_summary(&source, &[], &BTreeSet::new()).unwrap();
        assert_eq!(retained_before, retained_after);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adopts_verified_working_copy_metadata_without_a_second_checkout() {
        let svn = find_svn_executable().unwrap();
        let svnadmin = svn.with_file_name("svnadmin.exe");
        if !svnadmin.is_file() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "himind-adopt-svn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let repository = root.join("repository");
        let verified = root.join("verified");
        let source = root.join("source");
        std::fs::create_dir_all(&root).unwrap();
        assert!(Command::new(&svnadmin)
            .args(["create", repository.to_string_lossy().as_ref()])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap()
            .success());
        let repository_url = Url::from_file_path(&repository).unwrap().to_string();
        assert!(Command::new(&svn)
            .args([
                "checkout",
                &repository_url,
                verified.to_string_lossy().as_ref()
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap()
            .success());
        std::fs::write(verified.join("project.txt"), "verified content").unwrap();
        run_svn_in_directory(&verified, ["add", "project.txt"]).unwrap();
        run_svn_in_directory(&verified, ["commit", "-m", "initial"]).unwrap();

        std::fs::create_dir_all(&source).unwrap();
        std::fs::copy(verified.join("project.txt"), source.join("project.txt")).unwrap();
        copy_working_copy_admin(&verified, &source).unwrap();

        assert!(same_svn_url(
            &svn_item(&source, "url").unwrap(),
            &repository_url
        ));
        assert_eq!(svn_status_change_count(&source).unwrap(), 0);
        std::fs::write(source.join("project.txt"), "changed").unwrap();
        assert_eq!(svn_status_change_count(&source).unwrap(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_external_urls_and_preserves_revision_options() {
        let source =
            std::env::temp_dir().join(format!(" himind-external-normalize-{}", std::process::id()));
        std::fs::create_dir_all(source.join("Packages")).unwrap();
        let result = normalize_external_property(
            &source,
            Path::new("Packages"),
            "http://legacy.example/repo/project/Packages",
            "http://legacy.example/repo",
            "libs/core -r42 ^/shared/core\n../common \"Common Lib\"\n",
            false,
        )
        .unwrap();

        assert_eq!(result.external_count, 2);
        assert!(result
            .local_roots
            .contains(&PathBuf::from("Packages/libs/core")));
        assert!(result
            .local_roots
            .contains(&PathBuf::from("Packages/Common Lib")));
        assert!(result
            .value
            .contains("libs/core -r42 http://legacy.example/repo/shared/core"));
        assert!(result
            .value
            .contains("http://legacy.example/repo/project/common \"Common Lib\""));

        std::fs::remove_dir_all(source).unwrap();
    }

    #[test]
    fn parses_quoted_external_tokens_and_peg_revisions() {
        assert_eq!(
            split_external_tokens("-r 12 \"http://example/repo/My Lib@12\" 'Local Lib'").unwrap(),
            vec!["-r", "12", "http://example/repo/My Lib@12", "Local Lib"]
        );
        assert_eq!(
            external_peg_revision("http://example/repo/lib@123"),
            Some("123")
        );
        assert_eq!(external_peg_revision("http://user@example/repo/lib"), None);
    }

    #[test]
    fn escapes_literal_at_signs_in_local_svn_targets() {
        assert_eq!(
            svn_local_target_argument(
                "Assets/ArtAssets/Textures/二级-切图/下一关@点击_二级-切图_063.png".to_string()
            ),
            "Assets/ArtAssets/Textures/二级-切图/下一关@点击_二级-切图_063.png@"
        );
        assert_eq!(
            svn_local_target_argument("Assets/icon@".to_string()),
            "Assets/icon@@"
        );
        assert_eq!(
            svn_local_target_argument("Assets/icon.png".to_string()),
            "Assets/icon.png"
        );
    }

    #[test]
    fn decodes_svn_cli_output_as_utf8_or_gbk() {
        assert_eq!(decode_svn_cli_output("路径正常".as_bytes()), "路径正常");
        assert_eq!(decode_svn_cli_output(&[0xD6, 0xD0, 0xCE, 0xC4]), "中文");
    }

    #[test]
    fn svn_status_count_ignores_externals_but_keeps_real_changes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<status><target path=".">
<entry path="Assets/Main.unity"><wc-status item="normal" props="none" revision="12" /></entry>
<entry path="Packages/External"><wc-status item="external" props="none" /></entry>
<entry path="backup.zip"><wc-status item="unversioned" props="none" /></entry>
<entry path="Assets/Changed.asset"><wc-status item="modified" props="none" revision="12" /></entry>
<entry path="ProjectSettings"><wc-status item="normal" props="modified" revision="12" /></entry>
</target></status>"#;

        assert_eq!(svn_status_change_count_from_xml(xml).unwrap(), 3);
    }

    #[test]
    fn migration_tree_skips_generated_and_external_directories() {
        let root =
            std::env::temp_dir().join(format!(" himind-migration-tree-{}", std::process::id()));
        let source = root.join("source");
        let target = root.join("target");
        std::fs::create_dir_all(source.join("Assets")).unwrap();
        std::fs::create_dir_all(source.join("Library")).unwrap();
        std::fs::create_dir_all(source.join("Packages/External")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(source.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(source.join("Library/cache.bin"), "cache").unwrap();
        std::fs::write(
            source.join("Packages/External/dependency.txt"),
            "dependency",
        )
        .unwrap();

        let summary = copy_migration_tree(
            &source,
            &target,
            &[PathBuf::from("Packages/External")],
            &BTreeSet::new(),
            &normalized_ignore_policy(&MigrationIgnorePolicy::default()),
        )
        .unwrap();
        assert_eq!(summary.file_count, 1);
        assert!(target.join("Assets/Main.unity").is_file());
        assert!(!target.join("Library").exists());
        assert!(!target.join("Packages/External").exists());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_tree_summary_detects_template_residue_and_missing_source_files() {
        let root = std::env::temp_dir().join(format!(
            " himind-migration-compare-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let source = root.join("source");
        let target = root.join("target");
        std::fs::create_dir_all(source.join("Assets")).unwrap();
        std::fs::create_dir_all(target.join("Assets")).unwrap();
        std::fs::write(source.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(source.join("Assets/SourceOnly.asset"), "source").unwrap();
        std::fs::write(target.join("Assets/Main.unity"), "scene").unwrap();
        std::fs::write(target.join("Assets/TemplateOnly.asset"), "template").unwrap();

        let policy = normalized_ignore_policy(&MigrationIgnorePolicy::default());
        let source_summary =
            migration_tree_summary(&source, &[], &BTreeSet::new(), &policy).unwrap();
        let target_summary =
            migration_tree_summary(&target, &[], &BTreeSet::new(), &policy).unwrap();
        assert_eq!(source_summary.file_count, 2);
        assert_eq!(target_summary.file_count, 2);
        assert_ne!(source_summary, target_summary);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn working_copy_admin_backup_restores_on_drop() {
        let root = std::env::temp_dir().join(format!(
            " himind-svn-backup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let source = root.join("project");
        std::fs::create_dir_all(source.join(".svn")).unwrap();
        std::fs::write(source.join(".svn/wc.db"), "old metadata").unwrap();
        {
            let _backup = WorkingCopyAdminBackup::create(&source).unwrap();
            assert!(!source.join(".svn").exists());
        }
        assert_eq!(
            std::fs::read_to_string(source.join(".svn/wc.db")).unwrap(),
            "old metadata"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn classifies_remote_probe_failures_without_exposing_details() {
        assert_eq!(
            classify_svn_remote_error("svn: E170001: Authentication failed"),
            "authorization_unknown"
        );
        assert_eq!(
            classify_svn_remote_error("svn: E160013: path not found"),
            "missing"
        );
        assert_eq!(
            classify_svn_remote_error("svn: E170013: unable to connect"),
            "temporarily_unreachable"
        );
    }

    #[test]
    fn remote_probe_uses_username_and_password_stdin_without_exposing_password() {
        let arguments =
            svn_remote_probe_arguments("http://svn.example/repo/project", Some("SVN User"));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--username", "SVN User"]));
        assert!(arguments.contains(&"--password-from-stdin".to_string()));
        assert!(!arguments.iter().any(|argument| argument == "secret"));
    }

    #[test]
    fn rejects_unmanaged_acl_paths_and_non_user_entries() {
        assert!(validate_managed_acl_paths(&["/trunk/shared".to_string()]).is_err());
        assert!(validate_desired_acl_entries(
            &[ProjectAclEntry {
                path: "/trunk".into(),
                username: "$authenticated".into(),
                access: "rw".into(),
            }],
            &["/trunk".into()]
        )
        .is_err());
    }

    #[test]
    fn computes_explicit_user_acl_changes_without_broad_access_removal() {
        let current = vec![
            json!({"path":"/trunk","object_type":"user","object_name":"Alice","access":"rw","invert":false}),
            json!({"path":"/trunk/exhibits/EXH-1","object_type":"$authenticated","object_name":"$authenticated","access":"rw","invert":false}),
        ];
        let desired = vec![ProjectAclEntry {
            path: "/trunk".into(),
            username: "Bob".into(),
            access: "rw".into(),
        }];
        let result = acl_plan_result(
            "plan_12345678",
            "project",
            &["/trunk".into()],
            &desired,
            &current,
        );
        assert_eq!(result["changes"][0]["action"], "create");
        assert_eq!(result["changes"][1]["action"], "delete");
        assert_eq!(result["broad_access"][0]["object_name"], "$authenticated");
    }

    #[test]
    fn acl_digest_is_stable_for_equal_entries() {
        let first = vec![
            json!({"path":"/trunk","object_type":"user","object_name":"Alice","access":"rw","invert":false}),
        ];
        assert_eq!(acl_digest(&first).unwrap(), acl_digest(&first).unwrap());
    }

    #[test]
    fn matches_template_ignore_patterns() {
        assert!(wildcard_match("Library", "Library"));
        assert!(wildcard_match("*.log", "editor.log"));
        assert!(wildcard_match("Build*", "Build-2026"));
        assert!(!wildcard_match("*.log", "editor.txt"));
    }

    #[test]
    fn parses_recursive_template_ignore_properties() {
        let properties = parse_template_ignore_properties(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<properties>
<target path="."><property name="svn:ignore">Library
Temp
</property></target>
<target path="Assets"><property name="svn:ignore">Develop
Develop.meta
</property></target>
</properties>"#,
        )
        .unwrap();
        assert_eq!(properties.targets.len(), 2);
        assert_eq!(properties.targets[0].path, ".");
        assert_eq!(properties.targets[0].properties[0].value, "Library\nTemp\n");
        assert_eq!(properties.targets[1].path, "Assets");
        assert_eq!(properties.targets[1].properties[0].name, "svn:ignore");
    }

    #[test]
    fn decodes_legacy_gbk_base64_svn_properties() {
        let property = SvnProperty {
            name: "svn:externals".to_string(),
            encoding: "base64".to_string(),
            value: "Y29tLnBhcmZ1bC5jb2xsYWJodWIvye7b2r/GvLy53Q==".to_string(),
        };
        assert_eq!(
            decode_svn_property_value(&property).unwrap(),
            "com.parful.collabhub/深圳科技馆"
        );
    }

    #[test]
    fn parses_verbose_status_paths_without_external_entries() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<status><target path=".">
<entry path="."><wc-status item="normal" revision="1" /></entry>
<entry path="Assets/Main.unity"><wc-status item="modified" revision="1" /></entry>
<entry path="Packages/External"><wc-status item="external" /></entry>
<entry path="Temp/cache"><wc-status item="ignored" /></entry>
</target></status>"#;
        let parsed: SvnStatusDocument = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(parsed.targets[0].entries.len(), 4);
        assert_eq!(parsed.targets[0].entries[1].status.item, "modified");
        assert_eq!(parsed.targets[0].entries[2].status.item, "external");
    }

    #[test]
    fn retries_only_transient_svnadmin_http_statuses() {
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert!(is_retryable_svnadmin_status(status), "status={status}");
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(!is_retryable_svnadmin_status(status), "status={status}");
        }
        assert_eq!(svnadmin_retry_delay(1), Duration::from_millis(250));
        assert_eq!(svnadmin_retry_delay(2), Duration::from_millis(750));
    }

    #[test]
    fn exposes_remote_import_state_from_partial_failure() {
        let error: Box<dyn Error> = Box::new(ExhibitImportPartialFailure {
            message: "local adoption failed".to_string(),
            result: json!({
                "remote_imported": true,
                "repository_revision": 12,
                "local_adoption_pending": true,
            }),
        });
        let result = task_failure_result(error.as_ref()).unwrap();
        assert_eq!(result["repository_revision"], 12);
        assert_eq!(result["local_adoption_pending"], true);
    }

    #[test]
    fn parses_clone_verification_log_entry() {
        let document: SvnLogDocument = quick_xml::de::from_str(
            r#"<?xml version="1.0"?><log><logentry revision="42"><author>tester</author><msg>Clone exhibit EX-1 from http://svn.example/source</msg></logentry></log>"#,
        )
        .unwrap();
        assert_eq!(document.entries[0].revision, 42);
        assert_eq!(
            document.entries[0].msg,
            "Clone exhibit EX-1 from http://svn.example/source"
        );
    }
}
