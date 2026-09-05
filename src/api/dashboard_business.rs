use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use serde_json::Value;
use std::error::Error;
use std::time::Duration;
use url::Url;

use crate::api::client::load_agent_state;
use crate::api::oauth::{
    platform_access_token, BUSINESS_CONTEXT_READ_SCOPE, BUSINESS_EXHIBIT_READ_SCOPE,
    BUSINESS_EXHIBIT_WRITE_SCOPE, BUSINESS_PEOPLE_READ_SCOPE, BUSINESS_PEOPLE_WRITE_SCOPE,
    BUSINESS_PROJECT_READ_SCOPE, BUSINESS_PROJECT_WRITE_SCOPE, BUSINESS_REQUIREMENT_READ_SCOPE,
    BUSINESS_REQUIREMENT_WRITE_SCOPE, BUSINESS_WORKSPACE_READ_SCOPE,
    BUSINESS_WORKSPACE_WRITE_SCOPE, KNOWLEDGE_SEARCH_SCOPE, OPERATION_CANCEL_SCOPE,
    OPERATION_READ_SCOPE,
};
use crate::approval::remote::ApprovalProof;
use crate::Options;

const BUSINESS_CLIENT_ID: &str = "himind-agent-business";

pub(crate) fn resolve_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request(
        options,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "context",
            "resolve",
        ],
        Some(input),
    )
}

pub(crate) fn project_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    request_with_scope(
        options,
        BUSINESS_PROJECT_READ_SCOPE,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
        ],
        None,
    )
}

pub(crate) fn exhibit_context(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_EXHIBIT_READ_SCOPE,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
        ],
        None,
    )
}

pub(crate) fn my_work_summary(options: &Options) -> Result<Value, Box<dyn Error>> {
    request(
        options,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "my-work",
            "summary",
        ],
        None,
    )
}

pub(crate) fn search_knowledge(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request_with_scope(
        options,
        KNOWLEDGE_SEARCH_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "knowledge",
            "search",
        ],
        Some(input),
    )
}

pub(crate) fn project_list(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    business_list(
        options,
        BUSINESS_PROJECT_READ_SCOPE,
        &["api", "integrations", "ai", "business", "projects"],
        input,
    )
}

pub(crate) fn project_create(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request_with_scope(
        options,
        BUSINESS_PROJECT_WRITE_SCOPE,
        Method::POST,
        &["api", "integrations", "ai", "business", "projects"],
        Some(input),
    )
}

pub(crate) fn project_update(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    request_with_scope(
        options,
        BUSINESS_PROJECT_WRITE_SCOPE,
        Method::PUT,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
        ],
        Some(without_key(input, "project_id")),
    )
}

pub(crate) fn project_delete(
    options: &Options,
    input: Value,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    request_with_scope_proof(
        options,
        BUSINESS_PROJECT_WRITE_SCOPE,
        Method::DELETE,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
        ],
        None,
        proof,
    )
}

pub(crate) fn exhibit_list(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    business_list(
        options,
        BUSINESS_EXHIBIT_READ_SCOPE,
        &["api", "integrations", "ai", "business", "exhibits"],
        input,
    )
}

pub(crate) fn exhibit_create(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    request_with_scope(
        options,
        BUSINESS_EXHIBIT_WRITE_SCOPE,
        Method::POST,
        &["api", "integrations", "ai", "business", "exhibits"],
        Some(input),
    )
}

pub(crate) fn exhibit_update(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_EXHIBIT_WRITE_SCOPE,
        Method::PUT,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
        ],
        Some(without_key(input, "exhibit_id")),
    )
}

pub(crate) fn exhibit_delete(
    options: &Options,
    input: Value,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope_proof(
        options,
        BUSINESS_EXHIBIT_WRITE_SCOPE,
        Method::DELETE,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
        ],
        None,
        proof,
    )
}

pub(crate) fn people_search(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let mut query = Vec::new();
    for key in ["q", "project_id", "exhibit_id", "page", "page_size"] {
        if let Some(value) = input.get(key) {
            if let Some(value) = value.as_str() {
                if !value.trim().is_empty() {
                    if key == "exhibit_id" && is_exhibit_display_id(value) {
                        return Err(exhibit_route_id_error(value).into());
                    }
                    query.push((key.to_string(), value.to_string()));
                }
            } else if value.is_number() {
                query.push((key.to_string(), value.to_string()));
            }
        }
    }
    request_with_scope_query(
        options,
        BUSINESS_PEOPLE_READ_SCOPE,
        Method::GET,
        &["api", "integrations", "ai", "business", "people", "search"],
        None,
        Some(query),
    )
}

pub(crate) fn requirement_list(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    let mut query = Vec::new();
    for key in ["status", "mine", "page", "page_size"] {
        if let Some(value) = input.get(key) {
            if let Some(value) = value.as_str() {
                if !value.trim().is_empty() {
                    query.push((key.to_string(), value.to_string()));
                }
            } else if value.is_number() || value.is_boolean() {
                query.push((key.to_string(), value.to_string()));
            }
        }
    }
    query.push(("exhibit_id".to_string(), exhibit_id));
    request_with_scope_query(
        options,
        BUSINESS_REQUIREMENT_READ_SCOPE,
        Method::GET,
        &["api", "integrations", "ai", "business", "requirements"],
        None,
        Some(query),
    )
}

pub(crate) fn requirement_get(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let requirement_id = required_string(&input, "requirement_id")?;
    request_with_scope(
        options,
        BUSINESS_REQUIREMENT_READ_SCOPE,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "requirements",
            &requirement_id,
        ],
        None,
    )
}

pub(crate) fn requirement_create(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_REQUIREMENT_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "requirements",
        ],
        Some(without_key(input, "exhibit_id")),
    )
}

pub(crate) fn requirement_update(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let requirement_id = required_string(&input, "requirement_id")?;
    request_with_scope(
        options,
        BUSINESS_REQUIREMENT_WRITE_SCOPE,
        Method::PATCH,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "requirements",
            &requirement_id,
        ],
        Some(without_key(input, "requirement_id")),
    )
}

pub(crate) fn requirement_assignment_update(
    options: &Options,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let requirement_id = required_string(&input, "requirement_id")?;
    request_with_scope(
        options,
        BUSINESS_REQUIREMENT_WRITE_SCOPE,
        Method::PATCH,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "requirements",
            &requirement_id,
            "assignment",
        ],
        Some(without_key(input, "requirement_id")),
    )
}

pub(crate) fn requirement_action(
    options: &Options,
    input: Value,
    action: &str,
) -> Result<Value, Box<dyn Error>> {
    let requirement_id = required_string(&input, "requirement_id")?;
    request_with_scope(
        options,
        BUSINESS_REQUIREMENT_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "requirements",
            &requirement_id,
            action,
        ],
        if action == "cancel" {
            None
        } else {
            Some(without_key(input, "requirement_id"))
        },
    )
}

pub(crate) fn project_people_replace(
    options: &Options,
    input: Value,
    role: &str,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    let path_role = match role {
        "managers" | "owners" => role,
        _ => return Err("role must be managers or owners".into()),
    };
    let mut body = input
        .get("user_ids")
        .cloned()
        .map(|user_ids| serde_json::json!({"user_ids": user_ids}))
        .unwrap_or_else(|| serde_json::json!({"user_ids": []}));
    if let Some(expected) = input.get("expected_user_ids") {
        if let Some(object) = body.as_object_mut() {
            object.insert("expected_user_ids".to_string(), expected.clone());
        }
    }
    request_with_scope_proof(
        options,
        BUSINESS_PEOPLE_WRITE_SCOPE,
        Method::PUT,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
            path_role,
        ],
        Some(body),
        proof,
    )
}

pub(crate) fn exhibit_crew_replace(
    options: &Options,
    input: Value,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope_proof(
        options,
        BUSINESS_PEOPLE_WRITE_SCOPE,
        Method::PUT,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "crew",
        ],
        Some(without_key(input, "exhibit_id")),
        proof,
    )
}

pub(crate) fn exhibit_crew_append(
    options: &Options,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_PEOPLE_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "crew",
            "append",
        ],
        Some(without_key(input, "exhibit_id")),
    )
}

pub(crate) fn exhibit_crew_remove(
    options: &Options,
    input: Value,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope_proof(
        options,
        BUSINESS_PEOPLE_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "crew",
            "remove",
        ],
        Some(without_key(input, "exhibit_id")),
        proof,
    )
}

pub(crate) fn project_exhibit_association(
    options: &Options,
    input: Value,
    action: &str,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let project_id = required_string(&input, "project_id")?;
    let exhibit_id = required_string(&input, "exhibit_id")?;
    if action != "attach" && action != "detach" {
        return Err("action must be attach or detach".into());
    }
    request_with_scope_proof(
        options,
        BUSINESS_PROJECT_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "projects",
            &project_id,
            "exhibits",
            &exhibit_id,
            action,
        ],
        None,
        proof,
    )
}

pub(crate) fn exhibit_workspace_get(
    options: &Options,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    let agent_id = required_string(&input, "agent_id")?;
    request_with_scope_query(
        options,
        BUSINESS_WORKSPACE_READ_SCOPE,
        Method::GET,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "workspace",
        ],
        None,
        Some(vec![("agent_id".to_string(), agent_id)]),
    )
}

pub(crate) fn exhibit_workspace_bind(
    options: &Options,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_WORKSPACE_WRITE_SCOPE,
        Method::PUT,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "workspace",
        ],
        Some(without_key(input, "exhibit_id")),
    )
}

pub(crate) fn exhibit_workspace_checkout(
    options: &Options,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let exhibit_id = required_string(&input, "exhibit_id")?;
    request_with_scope(
        options,
        BUSINESS_WORKSPACE_WRITE_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "business",
            "exhibits",
            &exhibit_id,
            "workspace",
            "checkout",
        ],
        Some(without_key(input, "exhibit_id")),
    )
}

pub(crate) fn operation_get(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let operation_id = required_string(&input, "operation_id")?;
    let path = [
        "api",
        "integrations",
        "ai",
        "operations",
        operation_id.as_str(),
    ];
    request_with_scope(options, OPERATION_READ_SCOPE, Method::GET, &path, None)
}

pub(crate) fn operation_cancel(options: &Options, input: Value) -> Result<Value, Box<dyn Error>> {
    let operation_id = required_string(&input, "operation_id")?;
    request_with_scope(
        options,
        OPERATION_CANCEL_SCOPE,
        Method::POST,
        &[
            "api",
            "integrations",
            "ai",
            "operations",
            operation_id.as_str(),
            "cancel",
        ],
        None,
    )
}

fn business_list(
    options: &Options,
    scope: &str,
    path: &[&str],
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let mut query = Vec::new();
    for key in [
        "q",
        "status",
        "scope",
        "project",
        "engine",
        "page",
        "page_size",
    ] {
        if let Some(value) = input.get(key) {
            if let Some(value) = value.as_str() {
                if !value.trim().is_empty() {
                    query.push((key.to_string(), value.to_string()));
                }
            } else if value.is_number() {
                query.push((key.to_string(), value.to_string()));
            }
        }
    }
    request_with_scope_query(options, scope, Method::GET, path, None, Some(query))
}

fn without_key(mut input: Value, key: &str) -> Value {
    if let Some(object) = input.as_object_mut() {
        object.remove(key);
    }
    input
}

fn request(
    options: &Options,
    method: Method,
    path: &[&str],
    body: Option<Value>,
) -> Result<Value, Box<dyn Error>> {
    request_with_scope(options, BUSINESS_CONTEXT_READ_SCOPE, method, path, body)
}

fn request_with_scope(
    options: &Options,
    required_scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
) -> Result<Value, Box<dyn Error>> {
    request_with_scope_proof(options, required_scope, method, path, body, None)
}

fn request_with_scope_proof(
    options: &Options,
    required_scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    request_with_scope_query_proof(options, required_scope, method, path, body, None, proof)
}

fn request_with_scope_query(
    options: &Options,
    required_scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
    query: Option<Vec<(String, String)>>,
) -> Result<Value, Box<dyn Error>> {
    request_with_scope_query_proof(options, required_scope, method, path, body, query, None)
}

fn request_with_scope_query_proof(
    options: &Options,
    required_scope: &str,
    method: Method,
    path: &[&str],
    body: Option<Value>,
    query: Option<Vec<(String, String)>>,
    proof: Option<&ApprovalProof>,
) -> Result<Value, Box<dyn Error>> {
    let delegated = platform_access_token(options, required_scope)?;
    let state = load_agent_state(&options.state_path)?;
    if delegated.agent_id.trim() != state.agent_id.trim() {
        return Err("Dashboard 授权与当前 Agent 实例不匹配，请重新授权".into());
    }
    let mut url = api_url(&options.api_base, path)?;
    if let Some(query) = query {
        url.query_pairs_mut().extend_pairs(query);
    }
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut builder = client
        .request(method, url)
        .bearer_auth(&delegated.token)
        .header("X-HiMind-Agent-ID", &delegated.agent_id)
        .header("X-HiMind-AI-Client", BUSINESS_CLIENT_ID);
    if let Some(proof) = proof {
        match proof {
            ApprovalProof::Approval(id) => {
                builder = builder.header("X-HiMind-Approval-ID", id);
            }
            ApprovalProof::Grant(id) => {
                builder = builder.header("X-HiMind-Grant-ID", id);
            }
        }
    }
    if let Some(value) = body {
        builder = builder.json(&value);
    }
    read_json(builder)
}

fn api_url(base: &str, path: &[&str]) -> Result<Url, Box<dyn Error>> {
    let mut url = Url::parse(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Dashboard API URL cannot be a base")?;
        segments.pop_if_empty();
        for segment in path {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn read_json(builder: RequestBuilder) -> Result<Value, Box<dyn Error>> {
    let response = builder.send()?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Dashboard 业务能力调用失败");
        return Err(format!("{message}（HTTP {}）", status.as_u16()).into());
    }
    Ok(value)
}

fn required_string(input: &Value, name: &str) -> Result<String, Box<dyn Error>> {
    let value = input
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        return Err(format!("{name} is required").into());
    }
    if name == "exhibit_id" && is_exhibit_display_id(value) {
        return Err(exhibit_route_id_error(value).into());
    }
    Ok(value.to_string())
}

fn is_exhibit_display_id(value: &str) -> bool {
    let normalized = value.trim();
    normalized
        .get(..3)
        .map(|prefix| {
            prefix.eq_ignore_ascii_case("ex-")
                && normalized
                    .get(3..)
                    .map(|suffix| {
                        !suffix.is_empty()
                            && suffix.chars().all(|character| character.is_ascii_digit())
                    })
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn exhibit_route_id_error(display_id: &str) -> String {
    serde_json::json!({
        "code": "EXHIBIT_ROUTE_ID_REQUIRED",
        "field": "exhibit_id",
        "display_id": display_id.trim(),
        "message": format!("展项参数使用了展示编号 {}。请先调用 business.exhibit.list 或 context.resolve，并使用返回项的 pid 作为 exhibit_id。", display_id.trim()),
        "hint": "EX-xxxx 仅用于展示；后续展项读取、人员、需求和工作区操作必须传入 list 返回的 pid。"
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{api_url, required_string};
    use serde_json::json;

    #[test]
    fn api_url_preserves_base_path_and_escapes_identifiers() {
        let url = api_url(
            "https://example.test/root/",
            &[
                "api",
                "integrations",
                "ai",
                "business",
                "exhibits",
                "展项/1",
            ],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.test/root/api/integrations/ai/business/exhibits/%E5%B1%95%E9%A1%B9%2F1"
        );
    }

    #[test]
    fn display_exhibit_number_returns_machine_readable_route_id_hint() {
        let error = required_string(&json!({ "exhibit_id": "EX-0021" }), "exhibit_id")
            .expect_err("display number must not be sent to the route");
        let payload: serde_json::Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(payload["code"], "EXHIBIT_ROUTE_ID_REQUIRED");
        assert_eq!(payload["display_id"], "EX-0021");
        assert!(payload["message"].as_str().unwrap().contains("pid"));
    }

    #[test]
    fn non_ascii_exhibit_values_do_not_panic_identifier_validation() {
        let error = required_string(&json!({ "exhibit_id": "展项-1" }), "exhibit_id");
        assert_eq!(error.unwrap(), "展项-1");
    }
}
