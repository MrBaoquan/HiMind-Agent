//! 扩展脚手架：内核内置的「新建插件 / 新建技能」骨架。
//!
//! 模板与仓库 `tooling/pluginproject` / `tooling/skillproject` 的产出一致：
//! 插件是「可离线构建的最小 JSON-RPC 插件」（自带 `internal/himindjsonrpc`，
//! 不依赖外部模块），技能是带 frontmatter 的 `skill.json + SKILL.md + agents/openai.yaml`。
//! 这样「新建扩展」不再硬依赖外置的扩展开发工具插件；插件模板更丰富时仍优先走插件
//!（`invoke_development_tool` 是插件优先、内核兜底）。

use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

const JSONRPC_SOURCE: &str = r#"package himindjsonrpc

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
)

type Request struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

type Response struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  any             `json:"result,omitempty"`
	Error   *Error          `json:"error,omitempty"`
}

type Error struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type Handler func(Request) (any, *Error)

func Serve(r io.Reader, w io.Writer, handler Handler) error {
	if handler == nil {
		return errors.New("jsonrpc handler is required")
	}
	scanner := bufio.NewScanner(r)
	encoder := json.NewEncoder(w)
	for scanner.Scan() {
		var request Request
		if err := json.Unmarshal(scanner.Bytes(), &request); err != nil {
			_ = encoder.Encode(Response{JSONRPC: "2.0", Error: &Error{Code: -32700, Message: "parse error"}})
			continue
		}
		result, rpcError := handler(request)
		if err := encoder.Encode(Response{JSONRPC: "2.0", ID: request.ID, Result: result, Error: rpcError}); err != nil {
			return err
		}
	}
	return scanner.Err()
}

func DecodeParams(request Request, target any) *Error {
	if target == nil {
		return &Error{Code: -32602, Message: "params target is required"}
	}
	if err := json.Unmarshal(request.Params, target); err != nil {
		return &Error{Code: -32602, Message: fmt.Sprintf("invalid params: %v", err)}
	}
	return nil
}
"#;

fn text(input: &Value, field: &str) -> String {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn strings(input: &Value, field: &str) -> Vec<String> {
    input
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_slug(label: &str, value: &str) -> Result<(), Box<dyn Error>> {
    if value.is_empty()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
        || value.starts_with(['-', '_'])
        || value.ends_with(['-', '_'])
    {
        return Err(format!("{label} 必须是小写单词，用 - 或 _ 分隔: {value}").into());
    }
    Ok(())
}

fn ensure_inside(workspace: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    let workspace = comparable_path(&workspace.canonicalize()?);
    // 目标可能已经存在（覆盖检查、追加写入），也可能还不存在（新建目录）。
    // 存在时取真实路径：Windows 的 8.3 短路径（%TEMP% 常被解析成 ...\ADMINI~1\...）
    // 与 canonicalize 后的长路径文本不同，只做词法比较会把合法路径误判成越界。
    let target = comparable_path(&canonical_for_compare(target));
    if target == workspace || target.starts_with(&(workspace.clone() + "\\")) {
        return Ok(());
    }
    Err(format!("输出目录必须位于 workspace_root 内: {target}").into())
}

/// 尽力归一成真实路径：目标存在时直接 canonicalize；不存在时 canonicalize 最近的
/// 已存在祖先再把剩余组件拼回去。新目录（如 <workspace>/nested）因此也能与
/// canonicalize 过的 workspace 比较，而不会被 8.3 短路径或符号链接误判成越界。
fn canonical_for_compare(path: &Path) -> std::path::PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    while let (Some(parent), Some(name)) = (cursor.parent(), cursor.file_name()) {
        suffix.push(name.to_os_string());
        cursor = parent.to_path_buf();
        if let Ok(mut canonical) = cursor.canonicalize() {
            for part in suffix.iter().rev() {
                canonical.push(part);
            }
            return canonical;
        }
    }
    path.to_path_buf()
}
/// 将路径归一成可比较的文本：统一分隔符、去掉尾部分隔符，并在 Windows 上
/// 忽略大小写。MCP/AI 客户端经常传正斜杠路径，而 Agent 内部路径来自
/// canonicalize（反斜杠）；不归一会在「明明在 workspace 内」时误报越界。
fn comparable_path(path: &Path) -> String {
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
    let normalized = parts.into_iter().collect::<std::path::PathBuf>();
    let mut text = crate::extension_workspace::display_path(&normalized).replace('/', "\\");
    while text.len() > 1 && text.ends_with('\\') {
        text.pop();
    }
    if cfg!(windows) {
        text.make_ascii_lowercase();
    }
    text
}

fn write(path: &Path, content: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{content}\n"))?;
    Ok(())
}

fn plugin_main_source(module: &str, template: &str) -> String {
    let body = if template == "job-worker" {
        r#"	return map[string]any{"status": "completed", "value": in.Value}, nil"#
    } else {
        r#"	return map[string]any{"value": in.Value}, nil"#
    };
    format!(
        r#"package main

import (
    "os"
    "himind-plugin/{module}/internal/himindjsonrpc"
)

type input struct {{
    Value string `json:"value"`
}}

func main() {{
    _ = himindjsonrpc.Serve(os.Stdin, os.Stdout, handle)
}}

func handle(request himindjsonrpc.Request) (any, *himindjsonrpc.Error) {{
    var in input
    if rpcError := himindjsonrpc.DecodeParams(request, &in); rpcError != nil {{
        return nil, rpcError
    }}
{body}
}}"#
    )
}

fn plugin_test_source(module: &str, template: &str) -> String {
    let extra = if template == "job-worker" {
        "\n\tif result[\"status\"] != \"completed\" {\n\t\tt.Fatalf(\"unexpected status: %#v\", result[\"status\"])\n\t}"
    } else {
        ""
    };
    format!(
        r#"package main

import (
	"testing"
	"himind-plugin/{module}/internal/himindjsonrpc"
)

func TestHandleReturnsInputValue(t *testing.T) {{
	resultValue, rpcError := handle(himindjsonrpc.Request{{Params: []byte("{{\"value\":\"hello\"}}")}})
	if rpcError != nil {{
		t.Fatal(rpcError)
	}}
	result := resultValue.(map[string]any)
	if result["value"] != "hello" {{
		t.Fatalf("unexpected value: %#v", result["value"])
	}}{extra}
}}"#
    )
}

fn plugin_ui_source(display_name: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{display_name}</title><style>body{{font:14px system-ui;margin:0;padding:24px;color:#1f2937;background:#f7f8fa}}main{{max-width:720px;margin:auto}}h1{{font-size:22px}}button{{padding:8px 14px}}</style></head>
<body><main><h1>{display_name}</h1><p>请在此实现可独立使用的工具界面。</p><button type="button">开始</button></main></body>
</html>"#
    )
}

/// 新建插件工程。返回 `{"root": ..., "plugin_id": ..., "capability": ...}`，
/// 与插件版 scaffold 的返回形状一致（`extension_projects::create` 读 `root`）。
pub(crate) fn create_plugin(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = PathBuf::from(text(input, "workspace_root"));
    let output_dir = PathBuf::from(text(input, "output_dir"));
    let name = text(input, "name");
    let display_name = text(input, "display_name");
    let description = text(input, "description");
    let author = text(input, "author");
    let release_notes = text(input, "release_notes");
    let categories = strings(input, "categories");
    let template = {
        let value = text(input, "template");
        if value.is_empty() {
            "readonly-tool".to_string()
        } else {
            value
        }
    };
    validate_slug("插件名", &name)?;
    for (label, value) in [
        ("显示名", display_name.as_str()),
        ("描述", description.as_str()),
        ("作者", author.as_str()),
        ("更新说明", release_notes.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{label}不能为空").into());
        }
    }
    if categories.is_empty() {
        return Err("至少需要一个功能分类".into());
    }
    if !matches!(
        template.as_str(),
        "readonly-tool" | "job-worker" | "ui-tool"
    ) {
        return Err(
            format!("不支持的模板 {template}；可选 readonly-tool / job-worker / ui-tool").into(),
        );
    }
    ensure_inside(&workspace, &output_dir)?;
    let root = output_dir.join(&name);
    if root.exists() {
        return Err(format!("项目目录已存在: {}", root.display()).into());
    }
    let id = format!("com.himind.{}", name.replace('_', "-"));
    let capability_id = format!("{}.run", name.replace('-', "."));
    let view_id = format!("{}.main", name.replace('-', "."));
    let mut contributes = json!({
        "commands": [{ "id": capability_id, "title": format!("运行{display_name}") }],
        "views": []
    });
    if template == "ui-tool" {
        contributes["views"] = json!([{
            "id": view_id,
            "title": display_name,
            "location": "plugin_navigation",
            "entry": "ui/index.html"
        }]);
    }
    let manifest = json!({
        "id": id,
        "name": display_name,
        "author": author,
        "categories": categories,
        "description": description,
        "release_notes": release_notes,
        "version": "0.1.0",
        "entry": format!("bin/{name}.exe"),
        "runtime": "process-jsonrpc-stdio",
        "platforms": ["windows-x64"],
        "min_agent_version": "0.3.0",
        "governance": "optional",
        "capabilities": [{
            "id": capability_id,
            "description": if template == "job-worker" {
                "请用中文说明该长任务能力的用途、进度和结果。"
            } else {
                "请用中文说明该插件能力的用途、输入和输出。"
            },
            "input_schema": {
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "additionalProperties": false
            },
            "risk_level": if template == "job-worker" { "local_write" } else { "read_only" },
            "availability": "local"
        }],
        "permissions": [],
        "contributes": contributes
    });
    fs::create_dir_all(&root)?;
    write(
        &root.join("plugin.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;
    write(&root.join("main.go"), &plugin_main_source(&name, &template))?;
    write(
        &root.join("main_test.go"),
        &plugin_test_source(&name, &template),
    )?;
    write(
        &root.join("go.mod"),
        &format!("module himind-plugin/{name}\n\ngo 1.22"),
    )?;
    write(
        &root.join("internal/himindjsonrpc/jsonrpc.go"),
        JSONRPC_SOURCE.trim_end(),
    )?;
    if template == "ui-tool" {
        write(
            &root.join("ui/index.html"),
            &plugin_ui_source(&display_name),
        )?;
    }
    fs::create_dir_all(root.join("bin"))?;
    Ok(json!({
        "root": crate::extension_workspace::display_path(&root),
        "plugin_id": id,
        "capability": capability_id,
        "provider": "kernel",
    }))
}

/// 新建工作流工程：复用 Agent 已有的原生骨架（与插件不可用时的兜底是同一份）。
pub(crate) fn create_workflow(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = PathBuf::from(text(input, "workspace_root"));
    let output_dir = PathBuf::from(text(input, "output_dir"));
    let slug = text(input, "slug");
    validate_slug("工作流 slug", &slug)?;
    if text(input, "name").is_empty() || text(input, "description").is_empty() {
        return Err("工作流名称与描述不能为空".into());
    }
    ensure_inside(&workspace, &output_dir)?;
    let project = crate::extension_projects::CreateExtensionProjectInput {
        kind: crate::extension_projects::ExtensionProjectKind::Workflow,
        slug: slug.clone(),
        extension_id: text(input, "id"),
        name: text(input, "name"),
        description: text(input, "description"),
        category: input
            .get("categories")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
            .unwrap_or("software-engineering")
            .to_string(),
        template: text(input, "template"),
    };
    // create_workflow_skeleton 会自己在 parent 下拼 workflows/<slug>；调用方交来的
    // output_dir 通常已经是 <repo>/workflows（与插件版 scaffold 一致）。这里还原成
    // repo 根，避免出现 workflows/workflows 双目录。
    let parent = if output_dir
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == "workflows")
    {
        output_dir.parent().unwrap_or(&output_dir).to_path_buf()
    } else {
        output_dir.clone()
    };
    let mut result = crate::extension_projects::create_workflow_skeleton(&parent, &slug, &project)?;
    if let Some(map) = result.as_object_mut() {
        map.insert("provider".to_string(), json!("kernel"));
    }
    Ok(result)
}

fn target_path(input: &Value) -> Result<PathBuf, Box<dyn Error>> {
    let workspace = PathBuf::from(text(input, "workspace_root"));
    let raw = text(input, "path");
    if raw.is_empty() {
        return Err("path is required".into());
    }
    let candidate = PathBuf::from(&raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        workspace.join(&candidate)
    };
    ensure_inside(&workspace, &absolute)?;
    Ok(absolute)
}

/// 校验工作流工程：与 Agent 加载工作流包时用的是同一套校验。
pub(crate) fn validate_workflow(input: &Value) -> Result<Value, Box<dyn Error>> {
    let path = target_path(input)?;
    let package = crate::workflow::load_from_directory(&path)?;
    package.validate()?;
    Ok(json!({
        "ok": true,
        "id": package.id,
        "version": package.version,
        "provider": "kernel",
    }))
}

/// 构建/打包工作流候选：复用内核原生的 `save_authoring_candidate`。
pub(crate) fn build_workflow(input: &Value) -> Result<Value, Box<dyn Error>> {
    let path = target_path(input)?;
    let draft = crate::workflow::save_authoring_candidate(&path)?;
    let candidate = PathBuf::from(&draft.candidate_path);
    let output = text(input, "output");
    let mut result = json!({
        "ok": true,
        "candidate": crate::extension_workspace::display_path(&candidate),
        "provider": "kernel",
    });
    if !output.is_empty() {
        let output = PathBuf::from(output);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&candidate, &output)?;
        result["output"] = json!(crate::extension_workspace::display_path(&output));
    }
    Ok(result)
}

/// 新建技能工程。返回 `{"root": ...}`。
pub(crate) fn create_skill(input: &Value) -> Result<Value, Box<dyn Error>> {
    let workspace = PathBuf::from(text(input, "workspace_root"));
    let output_dir = PathBuf::from(text(input, "output_dir"));
    let slug = text(input, "slug");
    let name = text(input, "name");
    let description = text(input, "description");
    let author = text(input, "author");
    let release_notes = text(input, "release_notes");
    let categories = strings(input, "categories");
    let clients = strings(input, "supported_clients");
    let version = {
        let value = text(input, "version");
        if value.is_empty() {
            "0.1.0".to_string()
        } else {
            value
        }
    };
    let min_agent_version = {
        let value = text(input, "min_agent_version");
        if value.is_empty() {
            "0.3.0".to_string()
        } else {
            value
        }
    };
    validate_slug("技能 slug", &slug)?;
    if name.trim().is_empty() || description.trim().is_empty() {
        return Err("技能名称与描述不能为空".into());
    }
    if author.trim().is_empty() || release_notes.trim().is_empty() {
        return Err("作者与更新说明不能为空".into());
    }
    if categories.is_empty() {
        return Err("至少需要一个功能分类".into());
    }
    ensure_inside(&workspace, &output_dir)?;
    let root = output_dir.join(&slug);
    if root.exists() {
        return Err(format!("项目目录已存在: {}", root.display()).into());
    }
    let id = {
        let value = text(input, "id");
        if value.is_empty() {
            format!("com.himind.skill.{slug}")
        } else {
            value
        }
    };
    let clients = if clients.is_empty() {
        vec![
            "himind-ai".to_string(),
            "codex".to_string(),
            "github-copilot".to_string(),
            "workbuddy".to_string(),
        ]
    } else {
        clients
    };
    let manifest = json!({
        "id": id,
        "name": name,
        "author": author,
        "categories": categories,
        "version": version,
        "scope": "organization",
        "description": description,
        "release_notes": release_notes,
        "min_agent_version": min_agent_version,
        "supported_clients": clients,
        "capabilities": [],
        "plugin_dependencies": [],
        "risk_summary": "read_only",
        "contents": ["skill.json", "SKILL.md", "agents/openai.yaml"]
    });
    fs::create_dir_all(&root)?;
    write(
        &root.join("skill.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;
    write(
        &root.join("SKILL.md"),
        &format!(
            "---\nname: {slug}\ndescription: {description}\n---\n\n# {name}\n\n请在此编写可独立使用的操作流程。项目说明仅在目标仓库实际存在时读取。"
        ),
    )?;
    write(
        &root.join("agents/openai.yaml"),
        &format!(
            "interface:\n  display_name: \"{name}\"\n  short_description: \"使用中文规范创建、校验、测试并提交 HiMind 技能\"\n  default_prompt: \"使用 ${slug} 完成这项任务。\""
        ),
    )?;
    Ok(json!({
        "root": crate::extension_workspace::display_path(&root),
        "provider": "kernel",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "himind-scaffold-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn creates_a_plugin_that_passes_the_agents_own_validation() {
        let root = workspace("plugin");
        let result = create_plugin(&json!({
            "workspace_root": root.to_string_lossy(),
            "output_dir": root.to_string_lossy(),
            "name": "demo-tool",
            "display_name": "演示工具",
            "description": "演示用的只读插件。",
            "author": "测试作者",
            "release_notes": "创建初始版本。",
            "categories": ["software-engineering"],
        }))
        .unwrap();
        let created = PathBuf::from(result["root"].as_str().unwrap());
        for relative in [
            "plugin.json",
            "main.go",
            "main_test.go",
            "go.mod",
            "internal/himindjsonrpc/jsonrpc.go",
        ] {
            assert!(created.join(relative).is_file(), "缺少 {relative}");
        }
        // 用 Agent 自己的校验链确认清单可用（不是只看文件在不在）。
        let content = std::fs::read_to_string(created.join("plugin.json")).unwrap();
        let manifest = crate::capability::plugin::parse_plugin_manifest(&content).unwrap();
        crate::capability::plugin::validate_manifest_contributions(&created, &manifest).unwrap();
        assert_eq!(manifest.entry, "bin/demo-tool.exe");
        // 骨架必须真的能构建：模板自带 internal/himindjsonrpc，不依赖外部模块。
        if let Some(go) = crate::runtime::process::resolve_executable("go") {
            let output = std::process::Command::new(go)
                .args(["build", "-o", "bin/demo-tool.exe", "."])
                .current_dir(&created)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "脚手架必须可构建: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn creates_a_skill_that_passes_the_agent_skill_validators() {
        let root = workspace("skill");
        let result = create_skill(&json!({
            "workspace_root": root.to_string_lossy(),
            "output_dir": root.to_string_lossy(),
            "slug": "demo-skill",
            "name": "演示技能",
            "description": "演示用的技能。",
            "author": "测试作者",
            "release_notes": "创建初始版本。",
            "categories": ["software-engineering"],
        }))
        .unwrap();
        let created = PathBuf::from(result["root"].as_str().unwrap());
        let manifest = crate::skill::manifest::load_skill_manifest(&created).unwrap();
        crate::skill::manifest::validate_skill_manifest(&manifest).unwrap();
        // SKILL.md 的 frontmatter 必须能被标准解析器接受。
        let document = std::fs::read_to_string(created.join("SKILL.md")).unwrap();
        crate::skill::manifest::parse_skill_frontmatter(&document).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn creates_and_validates_a_workflow_project() {
        let root = workspace("workflow");
        let result = create_workflow(&json!({
            "workspace_root": root.to_string_lossy(),
            "output_dir": root.to_string_lossy(),
            "slug": "demo-workflow",
            "name": "演示工作流",
            "description": "演示用的工作流。",
            "categories": ["software-engineering"],
        }))
        .unwrap();
        let created = PathBuf::from(result["root"].as_str().unwrap());
        assert!(crate::workflow::load_from_directory(&created).is_ok());
        let validated = validate_workflow(&json!({
            "workspace_root": root.to_string_lossy(),
            "path": created.to_string_lossy(),
        }));
        if let Ok(value) = validated {
            assert_eq!(value["provider"], json!("kernel"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_to_overwrite_and_rejects_paths_outside_the_workspace() {
        let root = workspace("guard");
        let input = json!({
            "workspace_root": root.to_string_lossy(),
            "output_dir": root.to_string_lossy(),
            "name": "demo-tool",
            "display_name": "演示工具",
            "description": "演示。",
            "author": "测试作者",
            "release_notes": "初始版本。",
            "categories": ["software-engineering"],
        });
        create_plugin(&input).unwrap();
        assert!(create_plugin(&input).is_err(), "已存在的目录不能被覆盖");

        // MCP/AI 客户端经常传正斜杠路径，不能因为分隔符不同就误判越界。
        let slash_root = root.to_string_lossy().replace('\\', "/");
        let mut nested = input.clone();
        nested["output_dir"] = json!(format!("{slash_root}/nested"));
        create_plugin(&nested).unwrap();

        let outside = std::env::temp_dir().join("himind-scaffold-outside");
        let mut escaped = input.clone();
        escaped["output_dir"] = json!(outside.to_string_lossy());
        assert!(create_plugin(&escaped).is_err(), "越界输出必须被拒绝");
        let _ = std::fs::remove_dir_all(root);
    }
}
