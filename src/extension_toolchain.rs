//! 扩展开发工具链：能力归属内核，编译交给受控执行器。
//!
//! 「扩展开发」是产品的一等功能，但它长期依赖一个可选的市场制品
//! （`com.himind.extension-development-tools`）：没装就直接失败，而这个插件
//! 偏偏又是它自己的构建依赖（bootstrap 死结）——开发版 profile 只配本地来源时
//! 连装都装不上，于是 28 个扩展一次性报「请先安装扩展开发工具」。
//!
//! 这里把**校验、打包、preflight**收回内核：它们不依赖任何外部语言工具链，
//! 内核自己就能做（工作流/专家/项目规则早就是原生实现，只有插件与技能走了插件）。
//! **编译**（`go build`）仍是受控执行器：装了插件优先用插件（上游可以独立演进，
//! 模板与语言矩阵都留给他们），没装就用内核的默认执行器。
//!
//! 分层约定：插件是「执行器提供方」，不是「能力所有者」。能力 ID 与语义由内核
//! 定义，任何一层都不该因为一个可选制品缺失就让整条创作链路不可用。

use crate::runtime::process::{
    hidden_command, remove_himind_secret_environment, resolve_executable, terminate_process_tree,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub(crate) const DEVELOPMENT_TOOLS_PLUGIN_ID: &str = "com.himind.extension-development-tools";
/// 内置插件自身的契约版本。它跟随对外能力契约，而不是 Agent 版本：
/// Skill/Workflow 依赖声明里的 min_version 比较的是这个值。
pub(crate) const DEVELOPMENT_TOOLS_PLUGIN_VERSION: &str = "1.4.3";

/// 第一方创作能力的完整清单：内核实现覆盖其中一部分，其余仍由扩展开发工具插件
/// 提供。这份清单是唯一事实来源（审批白名单、调用上限、内核覆盖都从它派生），
/// 之前它散在 capability/service.rs 与 plugin.json 里，已经漂移过一次
/// （`extension.environment.preflight` 漏登记）。
pub(crate) const AUTHORING_CAPABILITIES: [&str; 12] = [
    "extension.environment.preflight",
    "extension.plugin.scaffold",
    "extension.plugin.validate",
    "extension.plugin.build",
    "extension.plugin.package",
    "extension.skill.scaffold",
    "extension.skill.validate",
    "extension.skill.package",
    "extension.workflow.scaffold",
    "extension.workflow.validate",
    "extension.workflow.build",
    "extension.workflow.package",
];

/// 是不是第一方创作能力。
pub(crate) fn is_authoring_capability(capability_id: &str) -> bool {
    AUTHORING_CAPABILITIES.contains(&capability_id)
}

/// 创作能力的调用上限。
///
/// 它们内部会跑 `go test`/`go build`：工具自己封顶 10 分钟，并在注释里明确要求
/// 「构建上限要小于 Agent 侧给本能力声明的 timeout_seconds，否则进程会先被 Agent
/// 杀掉，调用方拿到的是一句『超时』而不是能定位问题的编译输出」。插件清单没声明
/// 上限时，Agent 的 30 秒默认值会让一个 52 秒的单测套件永远建不出来
/// （wechat-miniprogram-tools 就是这样）。
pub(crate) const AUTHORING_CAPABILITY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 内核独立提供的能力：新建骨架、校验、构建、打包、preflight。
/// 更合适（`extension_projects::create` 对工作流已经有原生骨架兜底）；
/// 而构建/校验/打包是每次开发都要走的动作，不能依赖可选制品。
pub(crate) const BUILTIN_CAPABILITIES: [&str; 12] = [
    "extension.environment.preflight",
    "extension.plugin.scaffold",
    "extension.plugin.validate",
    "extension.plugin.build",
    "extension.plugin.package",
    "extension.skill.scaffold",
    "extension.skill.validate",
    "extension.skill.package",
    "extension.workflow.scaffold",
    "extension.workflow.validate",
    "extension.workflow.build",
    "extension.workflow.package",
];

/// 内核实现的能力 → 是否覆盖。用于「插件缺失时是否还能继续」的判定。
pub(crate) fn covers(capability_id: &str) -> bool {
    BUILTIN_CAPABILITIES.contains(&capability_id)
}

/// 构建上限。要小于能力侧声明的 timeout，否则调用方先被杀进程，拿到的只是
/// 一句「超时」而不是能定位问题的编译输出。
const GO_TIMEOUT: Duration = Duration::from_secs(600);
/// 回传给调用方的命令输出上限（超限时保留首尾两段）。
const COMMAND_OUTPUT_LIMIT: usize = 16_000;

pub(crate) fn invoke(capability_id: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    match capability_id {
        "extension.environment.preflight" => Ok(preflight(input)),
        "extension.plugin.scaffold" => crate::extension_scaffold::create_plugin(input),
        "extension.skill.scaffold" => crate::extension_scaffold::create_skill(input),
        "extension.workflow.scaffold" => crate::extension_scaffold::create_workflow(input),
        "extension.workflow.validate" => crate::extension_scaffold::validate_workflow(input),
        "extension.workflow.build" | "extension.workflow.package" => {
            crate::extension_scaffold::build_workflow(input)
        }
        "extension.plugin.validate" => {
            let path = target_path(input, "path")?;
            validate_plugin_path(&path)?;
            Ok(json!({"ok": true, "path": display(&path), "provider": "kernel"}))
        }
        "extension.plugin.build" => plugin_build(input),
        "extension.plugin.package" => {
            let path = target_path(input, "path")?;
            let output = target_path(input, "output")?;
            package_plugin(&path, &output)?;
            Ok(json!({"ok": true, "output": display(&output), "provider": "kernel"}))
        }
        "extension.skill.validate" => {
            let path = target_path(input, "path")?;
            validate_skill_path(&path)?;
            Ok(json!({"ok": true, "path": display(&path), "provider": "kernel"}))
        }
        "extension.skill.package" => {
            let path = target_path(input, "path")?;
            let output = target_path(input, "output")?;
            package_skill(&path, &output)?;
            Ok(json!({"ok": true, "output": display(&output), "provider": "kernel"}))
        }
        other => Err(format!("内核未实现该能力: {other}").into()),
    }
}

/// 工具链就绪度：给界面与预检用，回答「现在这套环境能不能构建」。
pub(crate) fn readiness() -> Value {
    let plugin = crate::capability::plugin::find_plugin(DEVELOPMENT_TOOLS_PLUGIN_ID)
        .ok()
        .flatten();
    let (installed, enabled, version) = match plugin {
        Some(item) => (
            true,
            item.enabled && !item.circuit_open,
            item.version.clone(),
        ),
        None => (false, false, String::new()),
    };
    let go = go_toolchain();
    json!({
        "plugin": { "installed": installed, "usable": enabled, "version": version },
        "kernel": {
            "capabilities": BUILTIN_CAPABILITIES,
            "go": { "installed": go.installed, "path": go.path.as_ref().map(|value| display(value)), "version": go.version.clone(), "error": go.error.clone() },
        },
        // 同一 ID 只有内置身份生效：外部同 ID 副本会被注册表忽略。
        "builtin": { "installed": true, "id": DEVELOPMENT_TOOLS_PLUGIN_ID, "version": DEVELOPMENT_TOOLS_PLUGIN_VERSION },
        "provider": "builtin",
        // 脚手架已由内核提供（extension_scaffold）；插件只影响模板的丰富程度。
        "scaffold_requires_plugin": false,
    })
}

// ---------------------------------------------------------------------------
// 输入解析与路径守卫
// ---------------------------------------------------------------------------

fn string_field(input: &Value, field: &str) -> Result<String, Box<dyn Error>> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{field} is required").into())
}

fn target_path(input: &Value, field: &str) -> Result<PathBuf, Box<dyn Error>> {
    let workspace = PathBuf::from(string_field(input, "workspace_root")?);
    let raw = string_field(input, field)?;
    let candidate = PathBuf::from(&raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        workspace.join(&candidate)
    };
    ensure_within(&workspace, &absolute)?;
    Ok(absolute)
}

/// 与插件侧 `ensureWithin` 同义：目标必须留在 workspace_root 之内。
/// 输出文件可能还不存在，所以走词法判定而不是 canonicalize。
fn ensure_within(workspace: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    let workspace = normalize_lexically(workspace);
    let target = normalize_lexically(target);
    if target == workspace || target.starts_with(&workspace) {
        return Ok(());
    }
    Err(format!(
        "path must remain inside workspace_root: {}",
        display(&target)
    )
    .into())
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => parts.push(prefix.as_os_str().to_os_string()),
            Component::RootDir => parts.push(std::ffi::OsString::from(
                std::path::MAIN_SEPARATOR.to_string(),
            )),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            Component::Normal(value) => parts.push(value.to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for part in parts {
        result.push(part);
    }
    result
}

fn display(path: &Path) -> String {
    crate::extension_workspace::display_path(path)
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

// ---------------------------------------------------------------------------
// 外部语言工具链（受控执行器）
// ---------------------------------------------------------------------------

struct Toolchain {
    installed: bool,
    path: Option<PathBuf>,
    version: String,
    error: Option<String>,
}

impl Toolchain {
    fn as_json(&self) -> Value {
        json!({
            "installed": self.installed,
            "path": self.path.as_ref().map(|value| display(value)),
            "version": self.version,
            "required": true,
        })
    }
}

fn go_toolchain() -> Toolchain {
    let Some(path) = resolve_executable("go") else {
        return Toolchain {
            installed: false,
            path: None,
            version: String::new(),
            error: Some("未在 PATH 中找到 Go 工具链".to_string()),
        };
    };
    let mut command = hidden_command(&path);
    command.arg("version");
    remove_himind_secret_environment(&mut command);
    match command.output() {
        Ok(output) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let version = first_line(&text);
            Toolchain {
                installed: true,
                path: Some(path),
                version,
                error: (!output.status.success())
                    .then(|| format!("go version 退出码 {}", output.status.code().unwrap_or(-1))),
            }
        }
        Err(error) => Toolchain {
            installed: true,
            path: Some(path),
            version: String::new(),
            error: Some(format!("Go 工具链无法执行: {error}")),
        },
    }
}

/// 跑一条 go 命令：隐藏窗口、隔离密钥环境、超时后杀进程树、输出做首尾保留裁剪。
fn run_go(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    let Some(go) = resolve_executable("go") else {
        return Err("未在 PATH 中找到 Go 工具链".to_string());
    };
    let mut command = hidden_command(&go);
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    remove_himind_secret_environment(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 Go 工具链: {error}"))?;
    let stdout = crate::runtime::process::capture_output(
        child
            .stdout
            .take()
            .ok_or("stdout unavailable".to_string())?,
    );
    let stderr = crate::runtime::process::capture_output(
        child
            .stderr
            .take()
            .ok_or("stderr unavailable".to_string())?,
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(format!("等待 Go 工具链失败: {error}")),
        }
        if started.elapsed() >= GO_TIMEOUT {
            terminate_process_tree(&mut child);
            break Err(format!(
                "go 命令超过 {} 秒未完成，已终止",
                GO_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = trim_command_output(&format!(
        "{}\n{}",
        crate::runtime::process::join_output(Some(stdout)),
        crate::runtime::process::join_output(Some(stderr))
    ));
    match status {
        Ok(status) if status.success() => Ok(text),
        Ok(status) => Err(if text.trim().is_empty() {
            format!("go 命令退出码 {}", status.code().unwrap_or(-1))
        } else {
            text
        }),
        Err(error) => Err(if text.trim().is_empty() {
            error
        } else {
            format!("{text}\n{error}")
        }),
    }
}

fn first_line(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// 超限时保留首尾两段。只留尾部会把最该看的内容丢掉：go build 先报失败的包，
/// 末尾往往只剩一句 `exit status 1`。
fn trim_command_output(text: &str) -> String {
    let text = text.trim();
    if text.len() <= COMMAND_OUTPUT_LIMIT {
        return text.to_string();
    }
    let head_end = floor_char_boundary(text, COMMAND_OUTPUT_LIMIT / 2);
    let tail_start = ceil_char_boundary(
        text,
        text.len() - (COMMAND_OUTPUT_LIMIT - COMMAND_OUTPUT_LIMIT / 2),
    );
    format!(
        "{}\n...（中间省略 {} 字节，完整输出请在本机重新执行该命令）...\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

// ---------------------------------------------------------------------------
// preflight
// ---------------------------------------------------------------------------

fn preflight(input: &Value) -> Value {
    let kind = input
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let kind = if kind.is_empty() {
        "plugin".to_string()
    } else {
        kind
    };
    let go = go_toolchain();
    let mut blockers: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut next_steps: Vec<String> = Vec::new();
    let mut state = "ready";
    let mut go_field = json!({ "installed": false, "required": false });

    match kind.as_str() {
        "skill" => {
            next_steps.push("继续执行校验与打包；打包由 Agent 内核完成，不依赖外部插件".to_string())
        }
        "workflow" => {
            next_steps.push("继续执行扩展构建；工作流的候选与打包由 Agent 内核完成".to_string())
        }
        "plugin" => {
            go_field = go.as_json();
            if go.installed && go.error.is_none() {
                next_steps
                    .push("继续执行扩展构建；内核会先跑 go test，再编译入口二进制".to_string());
            } else {
                state = "blocked";
                blockers.push(json!({
                    "code": if go.installed { "toolchain_unavailable" } else { "toolchain_missing" },
                    "stage": "toolchain",
                    "severity": "error",
                    "message": go.error.clone().unwrap_or_else(|| "Go 工具链不可用".to_string()),
                    "remediation": "安装 Go 并确保 go 在 PATH 中，然后重新预检",
                    "retryable": true,
                }));
                next_steps
                    .push("修复 Go 工具链后重新调用 extension.environment.preflight".to_string());
            }
        }
        other => {
            state = "blocked";
            blockers.push(json!({
                "code": "invalid_extension_kind",
                "stage": "preflight",
                "severity": "error",
                "message": format!("kind 必须是 plugin、skill 或 workflow，收到: {other}"),
                "remediation": "使用 kind=plugin、kind=skill 或 kind=workflow 重新调用",
                "retryable": false,
            }));
        }
    }
    if !plugin_usable() {
        warnings.push(json!({
            "message": "内置扩展开发工具未加载：新建骨架、构建、校验与打包由 Agent 内核完成",
        }));
    }
    json!({
        "checked_at": now_stamp(),
        "kind": kind,
        "state": state,
        "ready": state == "ready",
        "blockers": blockers,
        "warnings": warnings,
        "next_steps": next_steps,
        "go": go_field,
        "provider": if plugin_usable() { "builtin" } else { "kernel" },
    })
}

fn plugin_usable() -> bool {
    crate::capability::plugin::find_plugin(DEVELOPMENT_TOOLS_PLUGIN_ID)
        .ok()
        .flatten()
        .is_some_and(|item| item.enabled && !item.circuit_open)
}

// ---------------------------------------------------------------------------
// 插件：校验、编译、打包
// ---------------------------------------------------------------------------

fn validate_plugin_path(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        let manifest = read_plugin_manifest(path)?;
        crate::capability::plugin::validate_manifest_contributions(path, &manifest)?;
        if manifest.runtime != "process-jsonrpc-stdio" {
            return Err(format!("unsupported plugin runtime: {}", manifest.runtime).into());
        }
        if manifest.entry.trim().is_empty() {
            return Err("plugin entry is required".into());
        }
        return Ok(());
    }
    let entries = read_archive_entries(path)?;
    let content = entries.get("plugin.json").ok_or("插件包缺少 plugin.json")?;
    let manifest = crate::capability::plugin::parse_plugin_manifest(content)?;
    for name in entries.keys() {
        // checksums.sha256 是打包时自己生成的清单，不算「非载荷文件」。
        if name != "plugin.json" && name != "checksums.sha256" && !plugin_payload_path(name) {
            return Err(format!("插件包包含非载荷文件: {name}").into());
        }
    }
    verify_checksums(&entries)?;
    let entry = manifest.entry.trim();
    if entry.is_empty() {
        return Err("plugin entry is required".into());
    }
    if !entries.contains_key(entry) {
        return Err(format!("插件包缺少入口文件: {entry}").into());
    }
    Ok(())
}

fn read_plugin_manifest(
    directory: &Path,
) -> Result<crate::capability::plugin::PluginManifest, Box<dyn Error>> {
    let content = fs::read_to_string(directory.join("plugin.json"))
        .map_err(|error| format!("插件目录缺少 plugin.json: {error}"))?;
    Ok(crate::capability::plugin::parse_plugin_manifest(&content)?)
}

fn plugin_build(input: &Value) -> Result<Value, Box<dyn Error>> {
    let path = target_path(input, "path")?;
    if !path.is_dir() {
        return Err(format!("插件构建需要目录: {}", display(&path)).into());
    }
    let manifest = read_plugin_manifest(&path)?;
    let entry = manifest.entry.trim().to_string();
    if entry.is_empty() {
        return Err("plugin entry is required".into());
    }
    let relative = Path::new(&entry);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err("plugin entry must remain inside the project".into());
    }
    // 与插件侧一致：先跑单测，再编译入口。失败时把输出原样回传，调用方才能定位。
    let test_output = run_go(&path, &["test", "./..."]).map_err(Box::<dyn Error>::from)?;
    if let Some(parent) = path.join(relative).parent() {
        fs::create_dir_all(parent)?;
    }
    let target = path.join(relative).to_string_lossy().to_string();
    let build_output =
        run_go(&path, &["build", "-o", &target, "."]).map_err(Box::<dyn Error>::from)?;
    if !path.join(relative).is_file() {
        return Err(format!(
            "go build 未产出入口文件: {}{}",
            display(&path.join(relative)),
            if build_output.is_empty() {
                String::new()
            } else {
                format!("\n{build_output}")
            }
        )
        .into());
    }
    Ok(json!({
        "ok": true,
        "entry": display(&path.join(relative)),
        "output": format!("{test_output}{build_output}").trim().to_string(),
        "provider": "kernel",
    }))
}

fn package_plugin(directory: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    validate_plugin_path(directory)?;
    let manifest = read_plugin_manifest(directory)?;
    let entry = manifest.entry.trim();
    if entry.is_empty() {
        return Err("plugin entry is required".into());
    }
    if !directory.join(entry).is_file() {
        return Err(format!("package input must contain built entry {entry}").into());
    }
    let files = plugin_payload_files(directory, output)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut archive = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Stored);
    let mut checksums = Vec::with_capacity(files.len());
    for relative in &files {
        let source = directory.join(relative);
        archive.start_file(relative.as_str(), options)?;
        std::io::copy(&mut File::open(&source)?, &mut archive)?;
        checksums.push(format!(
            "{:x}  {relative}\n",
            Sha256::digest(fs::read(&source)?)
        ));
    }
    archive.start_file("checksums.sha256", options)?;
    archive.write_all(checksums.concat().as_bytes())?;
    archive.finish()?;
    Ok(())
}

/// 插件载荷文件：默认「目录里的一切都进包」，只有构建输入与生成物被排除。
///
/// 这是与仓库侧 `pluginpack.PayloadFiles` 同一份排除规则的实现——两处口径一旦
/// 分叉，就会出现「本地能跑、发布装不上」。
fn plugin_payload_files(root: &Path, output: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let output = normalize_lexically(output);
    let mut files = Vec::new();
    collect_payload_files(root, root, &output, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_payload_files(
    root: &Path,
    directory: &Path,
    output: &Path,
    files: &mut Vec<String>,
) -> Result<(), Box<dyn Error>> {
    let mut entries: Vec<_> = fs::read_dir(directory)?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if plugin_skips_directory(&name) {
                continue;
            }
            collect_payload_files(root, &path, output, files)?;
            continue;
        }
        if plugin_skips_file(&name) || normalize_lexically(&path) == output {
            continue;
        }
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        files.push(relative);
    }
    Ok(())
}

fn plugin_skips_directory(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git" | ".github" | "node_modules" | "dist" | "target" | "test-output"
    )
}

fn plugin_skips_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".go") {
        return true;
    }
    if [
        ".hmpkg",
        ".hmskill",
        ".hmwf",
        ".release-manifest.json",
        ".pdb",
        ".log",
        ".tmp",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
    {
        return true;
    }
    matches!(
        lower.as_str(),
        "go.mod"
            | "go.sum"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "checksums.sha256"
            | "extension-lock.json"
            | ".gitignore"
            | ".gitattributes"
            | ".ds_store"
            | "thumbs.db"
    )
}

/// 归档内路径是否属于可移植载荷（与目录排除规则同源，逐段判定）。
fn plugin_payload_path(relative: &str) -> bool {
    let normalized = relative.replace('\\', "/");
    if normalized.is_empty() {
        return false;
    }
    let components: Vec<&str> = normalized.split('/').collect();
    let last = components.len().saturating_sub(1);
    for (index, component) in components.iter().enumerate() {
        if component.is_empty() || *component == "." || *component == ".." {
            return false;
        }
        if index == last {
            return !plugin_skips_file(component);
        }
        if plugin_skips_directory(component) {
            return false;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// 技能：校验、打包
// ---------------------------------------------------------------------------

fn validate_skill_path(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        let manifest = crate::skill::manifest::load_skill_manifest(path)?;
        crate::skill::manifest::validate_skill_manifest(&manifest)?;
        return Ok(());
    }
    let entries = read_archive_entries(path)?;
    let content = entries.get("skill.json").ok_or("技能包缺少 skill.json")?;
    let manifest = crate::skill::manifest::parse_skill_manifest(content)?;
    crate::skill::manifest::validate_skill_manifest(&manifest)?;
    for name in manifest.contents.iter() {
        if !entries.contains_key(name) {
            return Err(format!("skill content is missing: {name}").into());
        }
    }
    for name in entries.keys() {
        if name != "checksums.sha256" && !manifest.contents.contains(name) {
            return Err(format!("skill package contains undeclared content: {name}").into());
        }
    }
    verify_checksums(&entries)?;
    Ok(())
}

fn package_skill(directory: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let manifest = crate::skill::manifest::load_skill_manifest(directory)?;
    crate::skill::manifest::validate_skill_manifest(&manifest)?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for name in manifest.contents.iter() {
        crate::skill::manifest::validate_relative_package_path(name)?;
        let content = fs::read(directory.join(name))
            .map_err(|_| format!("skill content is missing: {name}"))?;
        files.push((name.clone(), content));
    }
    let mut rows: Vec<String> = files
        .iter()
        .map(|(name, content)| format!("{:x}  {name}\n", Sha256::digest(content)))
        .collect();
    rows.sort();
    files.push(("checksums.sha256".to_string(), rows.concat().into_bytes()));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut archive = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, content) in files {
        archive.start_file(name, options)?;
        archive.write_all(&content)?;
    }
    archive.finish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 归档读取与校验和
// ---------------------------------------------------------------------------

fn read_archive_entries(
    path: &Path,
) -> Result<std::collections::BTreeMap<String, String>, Box<dyn Error>> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut entries = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .enclosed_name()
            .ok_or("归档包含非法路径")?
            .to_string_lossy()
            .replace('\\', "/");
        let mut content = String::new();
        if std::io::Read::read_to_string(&mut entry, &mut content).is_ok() {
            entries.insert(name, content);
        }
    }
    Ok(entries)
}

/// 归档自带的 checksums.sha256 必须与内容吻合：写包时算一遍，读包时也要验一遍。
/// 这里每次读原始字节重算，不依赖文本编码，二进制载荷同样适用。
fn verify_checksums(
    entries: &std::collections::BTreeMap<String, String>,
) -> Result<(), Box<dyn Error>> {
    let Some(content) = entries.get("checksums.sha256") else {
        return Ok(());
    };
    let expected = crate::skill::manifest::parse_checksums(content)?;
    for (name, digest) in expected.iter() {
        let Some(value) = entries.get(name) else {
            return Err(format!("checksums.sha256 指向不存在的文件: {name}").into());
        };
        let actual = format!("{:x}", Sha256::digest(value.as_bytes()));
        if actual != *digest {
            return Err(format!("校验和不匹配: {name}").into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(prefix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn plugin_fixture(root: &Path) {
        write(
            &root.join("plugin.json"),
            r#"{"id":"com.test.fixture","name":"测试插件","version":"1.0.0","release_notes":"初始版本。","runtime":"process-jsonrpc-stdio","entry":"bin/fixture.exe","capabilities":[]}"#,
        );
        write(&root.join("bin/fixture.exe"), "binary-payload");
        write(&root.join("ui/app.js"), "console.log(1)");
        // 构建输入与生成物：都不该进包。
        write(&root.join("main.go"), "package main");
        write(&root.join("go.mod"), "module fixture");
        write(&root.join("checksums.sha256"), "stale");
        write(&root.join("dist/bundle.js"), "generated");
        write(&root.join("ui/node_modules/react/index.js"), "dependency");
    }

    fn skill_fixture(root: &Path) {
        write(
            &root.join("skill.json"),
            r#"{"id":"com.test.fixture-skill","name":"测试技能","version":"1.0.0","scope":"user","release_notes":"初始版本。","supported_clients":["agent-skills"],"contents":["SKILL.md","reference/notes.md"]}"#,
        );
        write(
            &root.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: 用于打包用例的技能\n---\n\n# 测试技能\n",
        );
        write(&root.join("reference/notes.md"), "参考说明\n");
        write(&root.join("notes.local.md"), "未声明的内容不应进包");
    }

    fn archive_entries(path: &Path) -> Vec<String> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut names = Vec::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index).unwrap();
            if !entry.is_dir() {
                names.push(entry.name().to_string());
            }
        }
        names
    }

    fn read_entry(path: &Path, name: &str) -> String {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
        content
    }

    #[test]
    fn package_plugin_keeps_payload_and_writes_checksums() {
        let root = temp_root("himind-toolchain-plugin");
        let directory = root.join("plugins/demo");
        plugin_fixture(&directory);
        let output = root.join("out/demo.hmpkg");

        invoke(
            "extension.plugin.package",
            &json!({"workspace_root": root, "path": directory, "output": output}),
        )
        .unwrap();

        let names = archive_entries(&output);
        assert_eq!(
            names,
            vec![
                "bin/fixture.exe".to_string(),
                "plugin.json".to_string(),
                "ui/app.js".to_string(),
                "checksums.sha256".to_string(),
            ],
            "载荷只应包含运行期文件（构建输入与生成物必须排除），校验和固定写在最后"
        );
        let checksums = read_entry(&output, "checksums.sha256");
        let expected: Vec<String> = ["bin/fixture.exe", "plugin.json", "ui/app.js"]
            .iter()
            .map(|name| {
                format!(
                    "{:x}  {name}\n",
                    Sha256::digest(fs::read(directory.join(name)).unwrap())
                )
            })
            .collect();
        assert_eq!(
            checksums,
            expected.concat(),
            "校验和行序与内容必须与载荷一致"
        );

        // 内核自己的读取端必须认这份包（同一个归档校验链路）。
        invoke(
            "extension.plugin.validate",
            &json!({"workspace_root": root, "path": output}),
        )
        .unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn package_skill_keeps_declared_contents_only_and_passes_agent_verifiers() {
        let root = temp_root("himind-toolchain-skill");
        let directory = root.join("skills/demo");
        skill_fixture(&directory);
        let output = root.join("out/demo.hmskill");

        invoke(
            "extension.skill.package",
            &json!({"workspace_root": root, "path": directory, "output": output}),
        )
        .unwrap();

        assert_eq!(
            archive_entries(&output),
            vec![
                "SKILL.md".to_string(),
                "checksums.sha256".to_string(),
                "reference/notes.md".to_string(),
            ],
            "技能包只装清单声明的内容"
        );

        // 走 Agent 自己的安装校验链：解包 → 校验和 → 声明内容 → 清单。
        let staging = root.join("staging");
        crate::app::skill_manager::extract_archive(&output, &staging).unwrap();
        crate::app::skill_manager::verify_checksums(&staging).unwrap();
        crate::app::skill_manager::verify_declared_contents(&staging).unwrap();
        crate::skill::manifest::validate_skill_package_root(&staging).unwrap();

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_outputs_outside_the_workspace() {
        let root = temp_root("himind-toolchain-guard");
        let directory = root.join("skills/demo");
        skill_fixture(&directory);
        let outside = root.parent().unwrap().join("escape.hmskill");

        let error = invoke(
            "extension.skill.package",
            &json!({"workspace_root": root, "path": directory, "output": outside}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("inside workspace_root"),
            "越界输出必须被拒绝: {error}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preflight_reports_kind_specific_readiness() {
        let skill = preflight(&json!({"kind": "skill"}));
        assert_eq!(skill["ready"], json!(true));
        assert_eq!(skill["go"]["required"], json!(false));
        assert_eq!(
            skill["provider"],
            json!(if plugin_usable() { "builtin" } else { "kernel" })
        );

        let unknown = preflight(&json!({"kind": "expert"}));
        assert_eq!(unknown["state"], json!("blocked"));
        assert_eq!(
            unknown["blockers"][0]["code"],
            json!("invalid_extension_kind")
        );

        // 插件类扩展要 Go：装了 Go 就 ready，没装就必须给出可执行的阻断。
        let plugin = preflight(&json!({"kind": "plugin"}));
        if go_toolchain().installed {
            assert_eq!(plugin["ready"], json!(true), "{plugin}");
        } else {
            assert_eq!(plugin["state"], json!("blocked"));
            assert_eq!(plugin["blockers"][0]["stage"], json!("toolchain"));
        }

        assert!(covers("extension.plugin.package"));
        assert!(covers("extension.plugin.scaffold"), "新建骨架已由内核提供");
        assert!(covers("extension.skill.scaffold"));
    }

    #[test]
    fn build_requires_a_plugin_manifest() {
        let root = temp_root("himind-toolchain-build");
        let directory = root.join("plugins/empty");
        fs::create_dir_all(&directory).unwrap();
        let error = invoke(
            "extension.plugin.build",
            &json!({"workspace_root": root, "path": directory}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("plugin.json"),
            "缺少清单时要说明缺什么: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
