use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use walkdir::{DirEntry, WalkDir};

use crate::store::credentials::{
    list_local_svn_connections, load_local_svn_connection_secret, remove_local_svn_connection,
    save_local_svn_connection, update_local_svn_connection_status,
};
use crate::svn::types::{
    ApplyProjectAclRequest, CloneExhibitRepositoryRequest, CreateExhibitRepositoryPathRequest,
    CreateRepositoryRequest, EnsureProjectExhibitsAccessRequest, ImportLocalExhibitRequest,
    InitializeExhibitRepositoryRequest, MigrationSourceScanRequest, PreviewProjectAclRequest,
    ProjectAclEntry, SaveSvnConnectionRequest, SvnCheckoutRequest, SvnConnectionSummary,
    SvnWorkspaceRequest,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const SVN_CONNECTION_ID: &str = "company-svn";
const SVN_ADMIN_CONNECTION_ID: &str = "company-svn-admin";
const SVN_ADMIN_URL: &str = "http://svn.andcrane.com";
const SVN_SERVICE_URL: &str = "http://svn.andcrane.com/repo";
const UNITY_TEMPLATE_URL: &str = "http://svn.andcrane.com/repo/UNIArtTemplate";
const UNREAL_TEMPLATE_ROOT_URL: &str = "http://svn.andcrane.com/repo/repo_UETemplates";
const TEMPLATE_MARKER_FILE: &str = ".himind-template.json";

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
    save_local_svn_connection(
        SVN_ADMIN_CONNECTION_ID,
        "公司 SVN 管理",
        SVN_ADMIN_URL,
        username.trim(),
        &password,
        "svnadmin_v2",
    )?;
    unsafe {
        std::env::remove_var("SVN_ADMIN_USERNAME");
        std::env::remove_var("SVN_ADMIN_PASSWORD");
    }
    Ok(true)
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
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .post(format!("{SVN_ADMIN_URL}/api.php?c=Common&a=Login&t=web"))
        .json(&json!({
            "user_name": connection.username,
            "user_pass": password,
            "user_role": "2",
            "uuid": "",
            "code": ""
        }))
        .send()?;
    if !response.status().is_success() {
        let _ =
            update_local_svn_connection_status(SVN_CONNECTION_ID, "unreachable", "SVN 服务不可用");
        return Err(format!("SVN service returned HTTP {}", response.status()).into());
    }
    let payload: Value = response.json()?;
    if payload.get("status").and_then(Value::as_i64) != Some(1) {
        let _ =
            update_local_svn_connection_status(SVN_CONNECTION_ID, "invalid", "SVN 账号或密码无效");
        return Err(payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("SVN account or password is invalid")
            .to_string()
            .into());
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

pub(crate) fn checkout_workspace(request: SvnCheckoutRequest) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let (connection, password) = load_company_svn_secret()?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let candidate = absolute_path(&request.target_path)?;
    reject_sensitive_path(&candidate)?;
    let (target, output) = if candidate.join(".svn").is_dir() {
        let status = workspace_status_path(&candidate)?;
        let current_url = status
            .get("repository_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if current_url.trim_end_matches('/') != repository_url.trim_end_matches('/') {
            return Err("checkout target is an SVN working copy for a different repository".into());
        }
        let output = run_svn_authenticated(
            [
                "update".to_string(),
                candidate.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
        )?;
        (candidate, output)
    } else {
        let target = validate_checkout_target(&request.target_path)?;
        let output = run_svn_authenticated(
            [
                "checkout".to_string(),
                repository_url.clone(),
                target.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
        )?;
        (target, output)
    };
    let status = workspace_status_path(&target)?;
    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "exhibit_id": exhibit_id,
        "repository_url": repository_url,
        "target_path": target,
        "output": output,
        "workspace": status
    }))
}

pub(crate) fn create_exhibit_repository_path(
    request: CreateExhibitRepositoryPathRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let response = svnadmin_post(
        "Svnrep",
        "CreateRepFolder",
        Some(&token),
        json!({
            "rep_name": project_id,
            "path": "/trunk/exhibits/",
            "folder_name": exhibit_id
        }),
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
    let output = run_svn_authenticated(
        [
            "copy".to_string(),
            source_url.to_string(),
            target_url.clone(),
            "-m".to_string(),
            format!("Clone exhibit {exhibit_id} from {source_url}"),
        ],
        &connection.username,
        &password,
    )?;
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
    Ok(json!({
        "ok": true,
        "cloned": true,
        "project_id": project_id,
        "exhibit_id": exhibit_id,
        "source_repository_url": source_url,
        "repository_url": target_url,
        "revision": revision.trim().parse::<u64>().unwrap_or_default(),
        "output": output
    }))
}

pub(crate) fn import_local_exhibit(
    request: ImportLocalExhibitRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let exhibit_id = normalize_repository_name(&request.exhibit_id)?;
    let source = absolute_path(&request.source_path)?;
    reject_sensitive_path(&source)?;
    if !source.is_dir() {
        return Err("source_path must be an existing directory".into());
    }
    if source.join(".svn").is_dir() {
        return Err("existing SVN working copies must use repository clone mode".into());
    }

    create_exhibit_repository_path(CreateExhibitRepositoryPathRequest {
        project_id: project_id.clone(),
        exhibit_id: exhibit_id.clone(),
    })?;
    let repository_url = exhibit_repository_url(&project_id, &exhibit_id)?;
    let (connection, password) = load_company_svn_secret()?;
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
        run_svn_authenticated(
            [
                "checkout".to_string(),
                repository_url.clone(),
                working_copy.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
        )?;
        copy_migration_tree(&source, &working_copy)?;
        run_svn_in_directory(
            &working_copy,
            ["add", "--force", "--parents", "--depth", "infinity", "."],
        )?;
        run_svn_authenticated(
            [
                "commit".to_string(),
                working_copy.to_string_lossy().to_string(),
                "-m".to_string(),
                format!("Import local exhibit {exhibit_id}"),
            ],
            &connection.username,
            &password,
        )?;
        run_svn_authenticated(
            [
                "checkout".to_string(),
                "--force".to_string(),
                repository_url.clone(),
                source.to_string_lossy().to_string(),
            ],
            &connection.username,
            &password,
        )?;
        let revision = svn_item(&source, "revision")?;
        Ok(json!({
            "ok": true,
            "imported": true,
            "project_id": project_id,
            "exhibit_id": exhibit_id,
            "repository_url": repository_url,
            "workspace_path": source,
            "revision": revision.trim().parse::<u64>().unwrap_or_default()
        }))
    })();
    let _ = std::fs::remove_dir_all(&temp_root);
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
        run_svn_authenticated_cancelable(
            [
                "commit".to_string(),
                working_copy.to_string_lossy().to_string(),
                "-m".to_string(),
                format!("Initialize {exhibit_id} from template {template_id}"),
            ],
            &connection.username,
            &password,
            cancel,
        )?;
        let revision = run_svn_authenticated_cancelable(
            [
                "info".to_string(),
                "--show-item".to_string(),
                "revision".to_string(),
                repository_url.clone(),
            ],
            &connection.username,
            &password,
            cancel,
        )?;
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
    let _ = std::fs::remove_dir_all(&temp_root);
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

fn copy_migration_tree(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    for item in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_migration_excluded(entry))
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
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
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
    #[serde(rename = "$text", default)]
    value: String,
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
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn svn_item(working_copy: &Path, item: &str) -> Result<String, Box<dyn Error>> {
    run_svn_in_directory(working_copy, ["info", "--show-item", item])
}

pub(crate) fn ensure_project_exhibits_access(
    request: EnsureProjectExhibitsAccessRequest,
) -> Result<Value, Box<dyn Error>> {
    let project_id = normalize_repository_name(&request.project_id)?;
    let path = "/trunk/exhibits";
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let query_body = json!({ "rep_name": project_id, "path": path, "svnn_user_pri_path_id": -1 });
    let before = svnadmin_post(
        "Svnrep",
        "GetRepPathAllPri",
        Some(&token),
        query_body.clone(),
    )?;
    ensure_svnadmin_success(&before)?;
    let existing = before
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("objectType").and_then(Value::as_str) == Some("$authenticated")
                    && item.get("objectName").and_then(Value::as_str) == Some("$authenticated")
            })
        });
    let action = if existing
        .and_then(|item| item.get("objectPri"))
        .and_then(Value::as_str)
        == Some("rw")
    {
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
            "objectType": "$authenticated",
            "objectName": "$authenticated",
            "objectPri": "rw",
            "svnn_user_pri_path_id": -1
        });
        if endpoint_action == "UpdRepPathPri" {
            body["invert"] = Value::Bool(false);
        }
        let response = svnadmin_post("Svnrep", endpoint_action, Some(&token), body)?;
        ensure_svnadmin_success(&response)?;
        if endpoint_action == "UpdRepPathPri" {
            "updated"
        } else {
            "created"
        }
    };
    let after = svnadmin_post("Svnrep", "GetRepPathAllPri", Some(&token), query_body)?;
    ensure_svnadmin_success(&after)?;
    let verified = after
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("objectType").and_then(Value::as_str) == Some("$authenticated")
                    && item.get("objectName").and_then(Value::as_str) == Some("$authenticated")
                    && item.get("objectPri").and_then(Value::as_str) == Some("rw")
                    && !item
                        .get("invert")
                        .is_some_and(|value| value == true || value == 1)
            })
        });
    if !verified {
        return Err("SvnAdmin did not persist authenticated read-write access".into());
    }
    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "path": path,
        "principal": "$authenticated",
        "access": "rw",
        "action": action,
        "verified": true
    }))
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
    let (connection, password) = load_svn_admin_secret()?;
    let token = login_svnadmin(&connection.username, &password)?;
    let current_users = user_acl_map(&before);
    let desired_users = desired_acl_map(&desired);
    let mut applied = Vec::new();
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
        let response = svnadmin_post("Svnrep", action, Some(&token), body)?;
        ensure_svnadmin_success(&response)?;
        applied.push(json!({ "action": if action == "CreateRepPathPri" { "create" } else { "update" }, "path": key.0, "username": key.1, "access": access }));
    }
    for (key, access) in &current_users {
        if desired_users.contains_key(key) {
            continue;
        }
        let response = svnadmin_post(
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
        )?;
        ensure_svnadmin_success(&response)?;
        applied.push(
            json!({ "action": "delete", "path": key.0, "username": key.1, "access": access }),
        );
    }
    let after = read_project_acl(&project_id, &managed_paths)?;
    if user_acl_map(&after) != desired_users {
        return Err("SvnAdmin ACL readback did not match the approved plan".into());
    }
    Ok(json!({
        "ok": true,
        "plan_id": request.plan_id,
        "project_id": project_id,
        "before_digest": before_digest,
        "after_digest": acl_digest(&after)?,
        "applied": applied,
        "verified": true,
        "broad_access": broad_acl_entries(&after)
    }))
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
        let response = svnadmin_post(
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
    let target = absolute_path(&request.target_path)?;
    reject_sensitive_path(&target)?;
    if !target.is_dir() {
        return Err("target_path must be an existing directory".into());
    }

    let mut fingerprint = Sha256::new();
    let mut file_count = 0_u64;
    let mut total_bytes = 0_u64;
    let mut excluded_count = 0_u64;
    let mut unity = target.join("ProjectSettings").is_dir();
    let mut unreal = false;
    let walker = WalkDir::new(&target)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    for item in walker.filter_entry(|entry| !is_migration_excluded(entry)) {
        let entry = item?;
        if entry.path() == target || entry.file_type().is_dir() {
            continue;
        }
        if entry.file_type().is_symlink() {
            excluded_count += 1;
            continue;
        }
        let relative = entry.path().strip_prefix(&target)?;
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        unity |= extension == "unity";
        unreal |= extension == "uproject";
        let metadata = entry.metadata()?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs())
            .unwrap_or_default();
        fingerprint.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        fingerprint.update([0]);
        fingerprint.update(metadata.len().to_le_bytes());
        fingerprint.update(modified.to_le_bytes());
        file_count += 1;
        total_bytes = total_bytes.saturating_add(metadata.len());
    }

    for entry in WalkDir::new(&target)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
    {
        if is_migration_excluded(&entry) {
            excluded_count += 1;
        }
    }

    let is_svn = target.join(".svn").is_dir();
    let mut repository_url = String::new();
    let mut revision = String::new();
    let mut change_count = 0_u64;
    let mut blocking_reasons: Vec<String> = Vec::new();
    if is_svn {
        let status = workspace_status_path(&target)?;
        repository_url = status["repository_url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        revision = status["revision"].as_str().unwrap_or_default().to_string();
        change_count = status["change_count"].as_u64().unwrap_or_default();
        if change_count > 0 {
            blocking_reasons.push("SVN 工作副本存在未提交变更，请先提交或清理后再迁移".to_string());
        }
    }

    Ok(json!({
        "source_kind": if is_svn { "svn_working_copy" } else { "local_directory" },
        "source_display_name": target.file_name().and_then(|value| value.to_str()).unwrap_or("历史工程"),
        "source_repository_url": repository_url,
        "source_revision": revision,
        "source_fingerprint": format!("sha256:{:x}", fingerprint.finalize()),
        "file_count": file_count,
        "total_bytes": total_bytes,
        "excluded_count": excluded_count,
        "change_count": change_count,
        "engine_type": if unity { "Unity3D" } else if unreal { "Unreal Engine" } else { "unknown" },
        "blocking_reasons": blocking_reasons,
    }))
}

fn is_migration_excluded(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".svn"
            | "library"
            | "temp"
            | "obj"
            | "logs"
            | "userSettings"
            | "binaries"
            | "deriveddatacache"
            | "intermediate"
            | "saved"
            | "usersettings"
            | ".vs"
    )
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
    let response = svnadmin_post(
        "Svnrep",
        "CreateRep",
        Some(&token),
        json!({
            "rep_name": project_id,
            "rep_note": request.project_name.trim(),
            "rep_type": "2"
        }),
    )?;
    ensure_svnadmin_success(&response)?;
    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "repository_url": project_repository_url(&project_id)?,
        "result": response.get("data").cloned().unwrap_or(Value::Null)
    }))
}

fn workspace_status_path(target: &Path) -> Result<Value, Box<dyn Error>> {
    let executable = find_svn_executable().ok_or("SVN CLI was not found")?;
    let info = Command::new(&executable)
        .args(["info", "--show-item", "url"])
        .current_dir(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !info.status.success() {
        return Err(String::from_utf8_lossy(&info.stderr)
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
    let status_xml = String::from_utf8_lossy(&changes.stdout);
    let change_count = status_xml.matches("<entry").count();
    Ok(json!({
        "target_path": target,
        "repository_url": String::from_utf8_lossy(&info.stdout).trim(),
        "revision": String::from_utf8_lossy(&revision.stdout).trim(),
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
        return Err(String::from_utf8_lossy(&stderr).trim().to_string().into());
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
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
    let verify_option = svnadmin_post("Setting", "GetVerifyOption", None, json!({}))?;
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
    let response = svnadmin_post(
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
    let endpoint = format!("{SVN_ADMIN_URL}/api.php?c={controller}&a={action}&t=web");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let mut request = client.post(endpoint).json(&body);
    if let Some(token) = token {
        request = request.header("Token", token);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        return Err(format!("SvnAdmin returned HTTP {}", response.status()).into());
    }
    Ok(response.json()?)
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
        if target.join(".svn").exists() {
            return Err("checkout target is already an SVN working copy".into());
        }
        if fs_directory_not_empty(&target)? {
            return Err("checkout target directory must be empty".into());
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
    if !target.is_dir() || !target.join(".svn").is_dir() {
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

fn fs_directory_not_empty(path: &Path) -> Result<bool, Box<dyn Error>> {
    Ok(std::fs::read_dir(path)?.next().is_some())
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
    fn rejects_relative_and_system_checkout_targets() {
        assert!(absolute_path("relative/path").is_err());
        assert!(reject_sensitive_path(Path::new(r"C:\Windows\Temp\repo")).is_err());
        assert!(reject_sensitive_path(Path::new(r"D:\Projects\repo")).is_ok());
    }

    #[test]
    fn migration_scan_is_read_only_and_does_not_expose_source_path() {
        let target =
            std::env::temp_dir().join(format!("himind-migration-scan-{}", std::process::id()));
        let assets = target.join("Assets");
        let library = target.join("Library");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(assets.join("Main.unity"), "scene").unwrap();
        std::fs::write(library.join("cache.bin"), "cache").unwrap();

        let result = scan_migration_source(MigrationSourceScanRequest {
            target_path: target.to_string_lossy().to_string(),
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
}
