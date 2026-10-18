//! 软件分发的内核实现：项目/制品检查与渠道/版本解析。
//!
//! 与仓库 `plugins/software-distribution`（Go）保持同一份语义：发布（工作台/GitHub）
//! 本来就在内核，这里把剩下的 4 条能力收回内核，让「软件分发」只有一个家。
//! 对外能力 ID 与请求/响应形状保持不变。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

const IDENTIFIER: &str = "稳定标识";

/// 内置软件分发插件自身的契约版本，与旧外置插件 1.3.0 的能力契约保持一致。
pub(crate) const SOFTWARE_DISTRIBUTION_PLUGIN_VERSION: &str = "1.3.0";

fn text(input: &Value, field: &str) -> String {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn flag(input: &Value, field: &str) -> bool {
    input.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn default_value(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_identifier(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn is_repository(value: &str) -> bool {
    let mut parts = value.trim().split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repo), None) if !owner.is_empty() && !repo.is_empty())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn matches_package_type(value: &str) -> bool {
    matches!(
        value.trim(),
        "directory-zip" | "apk" | "unity-addressables" | "content"
    )
}

fn is_directory(path: &Path) -> bool {
    path.is_dir()
}

fn is_regular_file(path: &Path) -> bool {
    path.is_file()
}

fn validate_identity(input: &Value) -> Result<(), Box<dyn Error>> {
    for (name, value) in [
        ("product_id", text(input, "product_id")),
        ("platform", text(input, "platform")),
        ("architecture", text(input, "architecture")),
    ] {
        if !is_identifier(&value) {
            return Err(format!("{name} 不是有效的{IDENTIFIER}").into());
        }
    }
    if text(input, "version").is_empty() && text(input, "current_version").is_empty() {
        return Err("version 或 current_version 至少填写一个".into());
    }
    let package_type = text(input, "package_type");
    if package_type.is_empty() {
        if !text(input, "artifact_path").is_empty() {
            return Err("package_type 不能为空".into());
        }
    } else if !matches_package_type(&package_type) {
        return Err("不支持的 package_type".into());
    }
    Ok(())
}

fn validate_package_extension(package_type: &str, extension: &str) -> Result<(), Box<dyn Error>> {
    let extension = extension.to_ascii_lowercase();
    if package_type == "apk" && extension != ".apk" {
        return Err("apk 包类型必须使用 .apk 文件".into());
    }
    if matches!(package_type, "directory-zip" | "unity-addressables") && extension != ".zip" {
        return Err("ZIP 包类型必须使用 .zip 文件".into());
    }
    Ok(())
}

/// 目标必须位于显式指定的 workspace_root 内（先词法判定，再规范化判定）。
fn workspace_path(input: &Value) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let workspace = text(input, "workspace_root");
    let target = text(input, "target_path");
    let target = if target.is_empty() {
        text(input, "project_path")
    } else {
        target
    };
    let target = if target.is_empty() {
        text(input, "artifact_path")
    } else {
        target
    };
    if workspace.is_empty() || target.is_empty() {
        return Err("workspace_root 和目标路径不能为空".into());
    }
    let root = PathBuf::from(&workspace);
    if !is_directory(&root) {
        return Err("workspace_root 必须是可访问的本机目录".into());
    }
    let root = root.canonicalize()?;
    let candidate = PathBuf::from(&target);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        root.join(&candidate)
    };
    // 词法判定：即使目标不存在，也要给出确定性的越界诊断。
    let lexical = crate::extension_workspace::display_path(&candidate);
    let lexical_root = crate::extension_workspace::display_path(&root);
    if lexical != lexical_root
        && !lexical.starts_with(&(lexical_root.clone() + "\\"))
        && !lexical.starts_with(&(lexical_root + "/"))
    {
        return Err("目标路径必须位于显式指定的 workspace_root 内".into());
    }
    let target = candidate.canonicalize()?;
    if !target.starts_with(&root) {
        return Err("目标路径必须位于显式指定的 workspace_root 内".into());
    }
    Ok((root, target))
}

fn validated_zip_entries(target: &Path) -> Result<BTreeMap<String, ()>, Box<dyn Error>> {
    let file = std::fs::File::open(target).map_err(|_| "ZIP 制品结构无效".to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| "ZIP 制品结构无效".to_string())?;
    if archive.is_empty() {
        return Err("ZIP 制品不能为空".into());
    }
    if archive.len() > 100_000 {
        return Err("ZIP 制品文件数量超过限制".into());
    }
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().replace('\\', "/");
        let cleaned = normalize_zip_path(&name);
        if cleaned.is_empty()
            || cleaned.starts_with("../")
            || cleaned.contains(':')
            || cleaned.starts_with('/')
        {
            return Err("ZIP 制品包含不安全路径".into());
        }
        if entry.is_dir() {
            continue;
        }
        let key = cleaned.trim_start_matches("./").to_ascii_lowercase();
        if entries.insert(key.clone(), ()).is_some() {
            return Err(format!("ZIP 制品包含重复路径: {cleaned}").into());
        }
    }
    if entries.is_empty() {
        return Err("ZIP 制品不包含文件".into());
    }
    Ok(entries)
}

fn normalize_zip_path(value: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
                parts.push("..");
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn validate_artifact_structure(input: &Value, target: &Path) -> Result<(), Box<dyn Error>> {
    match text(input, "package_type").as_str() {
        "directory-zip" | "unity-addressables" => {
            let entries = validated_zip_entries(target)?;
            if text(input, "product_id").eq_ignore_ascii_case("com.himind.media-resolver") {
                for required in [
                    "mediaresolver.exe",
                    "distribution.sample.json",
                    "updater/himind.distribution.updater.exe",
                ] {
                    if !entries.contains_key(required) {
                        return Err(format!("MediaResolver 发布包缺少必要文件: {required}").into());
                    }
                }
            }
        }
        "apk" => {
            let entries = validated_zip_entries(target)
                .map_err(|error| format!("APK 文件结构无效: {error}"))?;
            if !entries.contains_key("androidmanifest.xml") {
                return Err("APK 缺少 AndroidManifest.xml".into());
            }
        }
        _ => {}
    }
    Ok(())
}

/// 语义化版本比较：核心数字段逐段比较；预发布后缀视为低于同核心版本。
fn compare_versions(left: &str, right: &str) -> i32 {
    fn core(value: &str) -> (Vec<u64>, bool) {
        let trimmed = value.trim();
        let prerelease = trimmed.contains('-') || trimmed.contains('+');
        let base = trimmed
            .split('-')
            .next()
            .unwrap_or("")
            .split('+')
            .next()
            .unwrap_or("");
        let mut numbers = Vec::new();
        for part in base.split('.').filter(|part| !part.is_empty()) {
            match part.parse::<u64>() {
                Ok(number) => numbers.push(number),
                Err(_) => return (Vec::new(), prerelease),
            }
        }
        (numbers, prerelease)
    }
    let (left_parts, left_pre) = core(left);
    let (right_parts, right_pre) = core(right);
    let length = left_parts.len().max(right_parts.len());
    for index in 0..length {
        let left_value = left_parts.get(index).copied().unwrap_or(0);
        let right_value = right_parts.get(index).copied().unwrap_or(0);
        if left_value != right_value {
            return if left_value > right_value { 1 } else { -1 };
        }
    }
    match (left_pre, right_pre) {
        (a, b) if a == b => 0,
        (true, false) => -1,
        _ => 1,
    }
}

/// 灰度：稳定实例 ID 与版本号哈希取模，同一实例对同一版本结果稳定。
fn instance_in_rollout(instance_id: &str, version: &str, percent: i64) -> bool {
    if percent >= 100 {
        return true;
    }
    if percent <= 0 {
        return false;
    }
    let digest = Sha256::digest(format!("{}|{}", instance_id.trim(), version.trim()).as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    (bucket as i64) < percent
}

fn channel_document_url(input: &Value) -> Result<String, Box<dyn Error>> {
    let channel_url = text(input, "channel_url");
    if !channel_url.is_empty() {
        let parsed = url::Url::parse(&channel_url)
            .map_err(|_| "channel_url 必须是受信任的 GitHub HTTPS 地址".to_string())?;
        if parsed.scheme() != "https"
            || !matches!(
                parsed.host_str(),
                Some("raw.githubusercontent.com") | Some("github.com")
            )
        {
            return Err("channel_url 必须是受信任的 GitHub HTTPS 地址".into());
        }
        return Ok(channel_url);
    }
    let repository = text(input, "repository");
    if !is_repository(&repository) {
        return Err("repository 必须是 owner/repo".into());
    }
    let product_id = text(input, "product_id");
    if !is_identifier(&product_id) {
        return Err("product_id 不是有效的稳定标识".into());
    }
    let reference = default_value(&text(input, "reference"), "HEAD");
    let channel = default_value(&text(input, "channel"), "stable");
    if !is_identifier(&channel) {
        return Err("channel 不是有效的稳定标识".into());
    }
    Ok(format!(
        "https://raw.githubusercontent.com/{}/{}/distribution/{}/{}.json",
        repository.trim(),
        reference,
        product_id.trim(),
        channel
    ))
}

fn json_text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn json_i64(value: &Value, field: &str) -> i64 {
    value.get(field).and_then(Value::as_i64).unwrap_or_default()
}

fn json_bool(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool).unwrap_or(false)
}

/// 纯函数：渠道文档一致性与信任校验（fail-closed）。
fn evaluate_channel_document(
    input: &Value,
    body: &[u8],
    document_url: &str,
) -> Result<Value, Box<dyn Error>> {
    let channel: Value =
        serde_json::from_slice(body).map_err(|_| "channel 文档不是合法 JSON".to_string())?;
    let schema = json_text(&channel, "schema_version");
    if schema != "software_distribution_channel.v1" {
        return Err(format!("不支持的 channel schema_version: {schema}").into());
    }
    let product_id = json_text(&channel, "product_id");
    if product_id != text(input, "product_id") {
        return Err("channel 文档 product_id 与请求不一致".into());
    }
    let requested_channel = text(input, "channel");
    if !requested_channel.is_empty() && json_text(&channel, "channel") != requested_channel {
        return Err("channel 文档 channel 与请求不一致".into());
    }
    let platform = text(input, "platform");
    if !platform.is_empty() && json_text(&channel, "platform") != platform {
        return Err("channel 文档 platform 与请求不一致".into());
    }
    let architecture = text(input, "architecture");
    if !architecture.is_empty() && json_text(&channel, "architecture") != architecture {
        return Err("channel 文档 architecture 与请求不一致".into());
    }
    let package_type = text(input, "package_type");
    let channel_package_type = json_text(&channel, "package_type");
    if !package_type.is_empty()
        && !channel_package_type.is_empty()
        && channel_package_type != package_type
    {
        return Err("channel 文档 package_type 与请求不一致".into());
    }
    let release = channel.get("release").cloned().unwrap_or(Value::Null);
    let sha256 = json_text(&release, "sha256");
    if !is_sha256(&sha256) {
        return Err("channel 文档 sha256 无效".into());
    }
    let size_bytes = json_i64(&release, "size_bytes");
    if size_bytes <= 0 {
        return Err("channel 文档 size_bytes 无效".into());
    }
    let download_url = json_text(&release, "download_url");
    if !download_url.starts_with("https://github.com/") {
        return Err("channel 文档 download_url 必须指向 github.com".into());
    }
    let signature_required = !flag(input, "allow_unsigned");
    let signature = release.get("signature").cloned().unwrap_or(Value::Null);
    let signature_valid = !signature.is_null()
        && !json_text(&signature, "value").is_empty()
        && !json_text(&signature, "key_id").is_empty();
    if !signature_valid && signature_required {
        return Err("channel 文档缺少有效签名，已按 fail-closed 拒绝".into());
    }

    let current = default_value(&text(input, "current_version"), "0.0.0");
    let version = json_text(&release, "version");
    let revoked = json_bool(&release, "revoked");
    let mut update: Option<Value> = None;
    let mut reason = String::new();
    if revoked {
        reason = "该版本已被撤销".to_string();
    } else {
        match compare_versions(&version, &current) {
            1 => {
                update = Some(build_update_manifest(&channel, &release, true));
            }
            0 => reason = "已是最新版本".to_string(),
            _ => {
                if flag(input, "allow_downgrade") {
                    update = Some(build_update_manifest(&channel, &release, false));
                } else {
                    reason = "线上版本低于当前版本，默认拒绝降级".to_string();
                }
            }
        }
    }

    let rollout = json_i64(&release, "rollout_percent");
    if update.is_some() && rollout > 0 && rollout < 100 {
        let instance_id = text(input, "instance_id");
        if instance_id.is_empty() {
            update = None;
            reason = "该版本为灰度发布，缺少稳定实例 ID，已按未放量处理".to_string();
        } else if !instance_in_rollout(&instance_id, &version, rollout) {
            update = None;
            reason = format!("该版本正在灰度（{rollout}%），本实例暂未放量");
        }
    }

    let mut result = json!({
        "source": "github-channel",
        "channel_file": document_url,
        "product_id": product_id,
        "channel": json_text(&channel, "channel"),
        "current_version": current,
        "signature_required": signature_required,
        "update": update,
    });
    if !reason.is_empty() {
        result["reason"] = json!(reason);
    }
    Ok(result)
}

fn build_update_manifest(channel: &Value, release: &Value, with_signature: bool) -> Value {
    let signature = release.get("signature").cloned().unwrap_or(Value::Null);
    let mut manifest = json!({
        "productId": json_text(channel, "product_id"),
        "version": json_text(release, "version"),
        "releaseName": format!("{} {}", json_text(channel, "product_name"), json_text(release, "version")).trim(),
        "releaseNotes": json_text(release, "release_notes"),
        "channel": json_text(channel, "channel"),
        "artifactUrl": json_text(release, "download_url"),
        "fileName": json_text(release, "file_name"),
        "packageType": json_text(channel, "package_type"),
        "sha256": json_text(release, "sha256").to_ascii_lowercase(),
        "size": json_i64(release, "size_bytes"),
        "mandatory": json_bool(release, "mandatory"),
        "publishedAt": json_text(release, "published_at"),
        "signatureAlgorithm": "ed25519",
    });
    if with_signature && !signature.is_null() {
        manifest["signature"] = json!(json_text(&signature, "value"));
        manifest["signatureKeyId"] = json!(json_text(&signature, "key_id"));
        let algorithm = json_text(&signature, "algorithm");
        if !algorithm.is_empty() {
            manifest["signatureAlgorithm"] = json!(algorithm);
        }
    }
    manifest
}

fn resolve_channel(input: &Value) -> Result<Value, Box<dyn Error>> {
    let document_url = channel_document_url(input)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .get(&document_url)
        .header("User-Agent", "HiMind-Agent")
        .header("Accept", "application/json")
        .send()
        .map_err(|_| "GitHub channel 文档读取失败".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub channel 文档返回 HTTP {}",
            response.status().as_u16()
        )
        .into());
    }
    let body = response.bytes()?;
    if body.len() > 1 << 20 {
        return Err("GitHub channel 文档读取中断".into());
    }
    evaluate_channel_document(input, &body, &document_url)
}

fn dashboard_url() -> Result<String, Box<dyn Error>> {
    let value = std::env::var("HIMIND_DASHBOARD_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("HIMIND_API_BASE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .ok_or("Agent 未配置可信 Dashboard 地址")?;
    let parsed =
        url::Url::parse(&value).map_err(|_| "Agent 未配置可信 Dashboard 地址".to_string())?;
    let host = parsed.host_str().unwrap_or_default();
    let trusted = parsed.scheme() == "https"
        || (parsed.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1"));
    if !trusted {
        return Err("Agent 未配置可信 Dashboard 地址".into());
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn validate_resolve_manifest(input: &Value, manifest: &Value) -> Result<(), Box<dyn Error>> {
    if json_text(manifest, "productId") != text(input, "product_id") {
        return Err("Dashboard resolve 返回了不匹配的 productId".into());
    }
    if json_text(manifest, "version").is_empty() || json_text(manifest, "releaseName").is_empty() {
        return Err("Dashboard resolve Manifest 缺少版本或发布名称".into());
    }
    if json_text(manifest, "channel") != default_value(&text(input, "channel"), "stable") {
        return Err("Dashboard resolve 返回了不匹配的渠道".into());
    }
    let file_name = json_text(manifest, "fileName");
    if file_name.is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name == "."
        || file_name == ".."
    {
        return Err("Dashboard resolve Manifest 文件名无效".into());
    }
    if !matches_package_type(&json_text(manifest, "packageType")) {
        return Err("Dashboard resolve Manifest packageType 无效".into());
    }
    let size = json_i64(manifest, "size");
    if size <= 0 {
        return Err("Dashboard resolve Manifest 大小无效".into());
    }
    let sha256 = json_text(manifest, "sha256");
    if !is_sha256(&sha256) {
        return Err("Dashboard resolve Manifest sha256 无效".into());
    }
    let artifact_url = json_text(manifest, "artifactUrl");
    match url::Url::parse(&artifact_url) {
        Ok(parsed)
            if !parsed.host_str().unwrap_or_default().is_empty()
                && matches!(parsed.scheme(), "https" | "http") => {}
        _ => return Err("Dashboard resolve Manifest 下载地址无效".into()),
    }
    if json_text(manifest, "signature").is_empty()
        || json_text(manifest, "signatureKeyId").is_empty()
        || json_text(manifest, "signatureAlgorithm") != "rsa-pss-sha256"
    {
        return Err("Dashboard resolve Manifest 缺少可信 RSA-PSS 签名".into());
    }
    Ok(())
}

fn resolve_release(input: &Value) -> Result<Value, Box<dyn Error>> {
    validate_identity(input)?;
    let base = dashboard_url()?;
    let body = json!({
        "productId": text(input, "product_id"),
        "currentVersion": default_value(&text(input, "current_version"), "0.0.0"),
        "channel": default_value(&text(input, "channel"), "stable"),
        "platform": text(input, "platform"),
        "architecture": text(input, "architecture"),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .post(format!(
            "{base}/api/software-distribution/v1/updates/resolve"
        ))
        .json(&body)
        .send()
        .map_err(|_| "Dashboard resolve 请求失败".to_string())?;
    if !response.status().is_success() {
        return Err(format!("Dashboard resolve 返回 HTTP {}", response.status().as_u16()).into());
    }
    let payload: Value = response
        .json()
        .map_err(|_| "Dashboard resolve 响应无效".to_string())?;
    let update = payload.get("update").cloned().unwrap_or(Value::Null);
    if !update.is_null() {
        validate_resolve_manifest(input, &update)?;
    }
    Ok(json!({ "dashboard": base, "product_id": text(input, "product_id"), "update": update }))
}

fn inspect_project(input: &Value) -> Result<Value, Box<dyn Error>> {
    let (root, target) = workspace_path(input)?;
    if !is_directory(&target) {
        return Err("项目目录不存在".into());
    }
    let mut project_type = "unknown".to_string();
    let mut recommendation = json!({});
    let mut client_status = json!({
        "protocol_package_installed": false,
        "windows_adapter_installed": false,
        "configuration_detected": false,
        "updater_detected": false,
    });
    let csproj = std::fs::read_dir(&target)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("csproj"));
    if let Some(csproj) = csproj {
        let content =
            std::fs::read_to_string(&csproj).map_err(|_| "无法读取 .NET 项目文件".to_string())?;
        let lower = content.to_ascii_lowercase();
        project_type = if lower.contains("<usewpf>true</usewpf>") {
            "wpf"
        } else {
            "dotnet"
        }
        .to_string();
        recommendation = json!({
            "protocol_package": "HiMind.Distribution",
            "windows_adapter": "HiMind.Distribution.Windows",
            "package_type": "directory-zip",
            "platform": "windows",
            "architecture": "x64",
        });
        client_status["protocol_package_installed"] =
            json!(content.contains("HiMind.Distribution"));
        client_status["windows_adapter_installed"] =
            json!(content.contains("HiMind.Distribution.Windows"));
        client_status["configuration_detected"] = json!(has_distribution_configuration(&target));
        client_status["updater_detected"] = json!(is_regular_file(
            &target.join("updater/HiMind.Distribution.Updater.exe")
        ));
    } else if is_directory(&target.join("Assets")) && is_directory(&target.join("ProjectSettings"))
    {
        project_type = "unity".to_string();
        recommendation = json!({
            "windows_package_type": "directory-zip",
            "android_package_type": "apk",
            "runtime_note": "Unity 仅复用分发协议；Windows 与 Android 安装分别由平台适配器完成。",
        });
        client_status["configuration_detected"] = json!(has_distribution_configuration(&target));
        client_status["updater_detected"] = json!(is_regular_file(
            &target.join("updater/HiMind.Distribution.Updater.exe")
        ));
    }
    Ok(json!({
        "workspace_root": crate::extension_workspace::display_path(&root),
        "project_path": crate::extension_workspace::display_path(&target),
        "project_type": project_type,
        "product_id": text(input, "product_id"),
        "recommendation": recommendation,
        "client_status": client_status,
        "runtime_dependency": "应用运行时直接调用分发服务；Agent 仅用于 AI 接入检查和受控发布",
        "agent_runtime_required": false,
    }))
}

fn has_distribution_configuration(root: &Path) -> bool {
    for name in [
        "distribution.json",
        "distribution.sample.json",
        "appsettings.json",
        "appsettings.Development.json",
    ] {
        let Ok(content) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        let lower = content.to_ascii_lowercase();
        if lower.contains("software-distribution") || lower.contains("productid") {
            return true;
        }
    }
    false
}

fn inspect_artifact(input: &Value) -> Result<Value, Box<dyn Error>> {
    let (root, target) = workspace_path(input)?;
    validate_identity(input)?;
    let metadata = std::fs::metadata(&target).map_err(|_| "无法读取制品文件".to_string())?;
    if !metadata.is_file() {
        return Err("制品必须是普通文件".into());
    }
    let extension = target
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();
    validate_package_extension(&text(input, "package_type"), &extension)?;
    if metadata.len() == 0 {
        return Err("制品文件不能为空".into());
    }
    validate_artifact_structure(input, &target)?;
    let sha256 = format!("{:x}", Sha256::digest(std::fs::read(&target)?));
    Ok(json!({
        "ready": true,
        "artifact_path": crate::extension_workspace::display_path(&target),
        "file_name": target.file_name().map(|value| value.to_string_lossy().to_string()).unwrap_or_default(),
        "size": metadata.len(),
        "sha256": sha256,
        "product_id": text(input, "product_id"),
        "version": text(input, "version"),
        "channel": default_value(&text(input, "channel"), "stable"),
        "platform": text(input, "platform"),
        "architecture": text(input, "architecture"),
        "package_type": text(input, "package_type"),
        "workspace_root": crate::extension_workspace::display_path(&root),
    }))
}

pub(crate) fn invoke(capability_id: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    match capability_id {
        "distribution.project.inspect" => inspect_project(input),
        "distribution.artifact.inspect" => inspect_artifact(input),
        "distribution.channel.resolve" => resolve_channel(input),
        "distribution.release.resolve" => resolve_release(input),
        other => Err(format!("内核未实现该能力: {other}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semantic_versions_like_the_plugin() {
        assert_eq!(compare_versions("1.2.3", "1.2.2"), 1);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), 0);
        assert_eq!(compare_versions("1.2.3-beta", "1.2.3"), -1);
        assert_eq!(compare_versions("2.0", "1.9.9"), 1);
    }

    #[test]
    fn rollout_is_stable_per_instance_and_version() {
        let first = instance_in_rollout("instance-a", "1.0.0", 50);
        assert_eq!(first, instance_in_rollout("instance-a", "1.0.0", 50));
        assert!(instance_in_rollout("instance-a", "1.0.0", 100));
        assert!(!instance_in_rollout("instance-a", "1.0.0", 0));
    }

    #[test]
    fn channel_document_evaluation_is_fail_closed_on_missing_signature() {
        let body = serde_json::json!({
            "schema_version": "software_distribution_channel.v1",
            "product_id": "com.example.app",
            "product_name": "示例",
            "channel": "stable",
            "platform": "windows",
            "architecture": "x64",
            "package_type": "directory-zip",
            "release": {
                "version": "1.2.0",
                "file_name": "app.zip",
                "size_bytes": 1024,
                "sha256": "a".repeat(64),
                "download_url": "https://github.com/example/repo/releases/download/v1.2.0/app.zip",
                "rollout_percent": 100
            }
        });
        let input = serde_json::json!({
            "product_id": "com.example.app",
            "current_version": "1.0.0",
            "channel": "stable",
            "platform": "windows",
            "architecture": "x64",
            "package_type": "directory-zip",
        });
        let serialized = serde_json::to_vec(&body).unwrap();
        let error = evaluate_channel_document(&input, &serialized, "https://example/doc.json")
            .unwrap_err()
            .to_string();
        assert!(error.contains("签名"), "{error}");
    }

    #[test]
    fn inspect_artifact_rejects_zip_entries_outside_the_archive() {
        let root = std::env::temp_dir().join(format!("himind-dist-inspect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let target = root.join("artifact.zip");
        let file = std::fs::File::create(&target).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        archive.start_file("../escape.txt", options).unwrap();
        std::io::Write::write_all(&mut archive, b"x").unwrap();
        archive.finish().unwrap();
        let input = serde_json::json!({
            "workspace_root": root.to_string_lossy(),
            "artifact_path": target.to_string_lossy(),
            "product_id": "com.example.app",
            "version": "1.0.0",
            "platform": "windows",
            "architecture": "x64",
            "package_type": "directory-zip",
        });
        let error = inspect_artifact(&input).unwrap_err().to_string();
        assert!(error.contains("不安全路径"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
