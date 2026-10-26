use crate::store::ai_service_balances::{AISourceBalanceSnapshot, BalanceCurrency};
use reqwest::blocking::Client;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Queries only when the user clicks refresh. Credentials stay in Rust and are never returned to UI.
pub(crate) fn refresh_saved_balance(id: &str) -> Result<AISourceBalanceSnapshot, String> {
    let (service, api_key, balance_access_token, balance_user_id) =
        crate::store::ai_services::load_balance_credentials(id)
            .map_err(|_| "无法读取此模型来源的凭据".to_string())?;
    let profile_id = service
        .balance_profile_id
        .as_deref()
        .ok_or_else(|| "此模型来源未配置受支持的余额查询接口".to_string())?;
    let access_token = balance_access_token.as_deref().unwrap_or(api_key.as_str());
    let user_id = balance_user_id.as_deref();
    let endpoint = balance_endpoint(profile_id, &service.base_url)?;
    if profile_id == "new-api" && user_id.is_none() {
        return Err("New API 余额查询需要填写 User ID".to_string());
    }
    let payload = request_balance_payload(profile_id, &endpoint, &api_key, access_token, user_id)?;
    let mut snapshot = parse_balance_payload(profile_id, &payload)?;
    snapshot.observed_at = crate::app::ai_provider_import::unix_now_seconds().to_string();
    // The live result should remain usable if the non-sensitive cache is temporarily unwritable.
    let _ = crate::store::ai_service_balances::save(id, snapshot.clone());
    Ok(snapshot)
}

pub(crate) fn suggest_balance_profile(base_url: &str) -> Option<&'static str> {
    let url = url::Url::parse(base_url).ok()?;
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let path = url.path().trim_end_matches('/');
    match host.as_str() {
        "api.deepseek.com" => Some("deepseek"),
        "api.moonshot.cn" => Some("kimi"),
        "api.kimi.com" if path == "/coding" || path == "/coding/v1" => Some("kimi-coding"),
        "open.bigmodel.cn" | "api.z.ai" if path.contains("/paas/v4") => Some("zhipu-plan"),
        "api.minimaxi.com" | "api.minimax.cn" => Some("minimax-plan-cn"),
        "api.minimax.io" => Some("minimax-plan-en"),
        "api.stepfun.ai" | "api.stepfun.com" => Some("stepfun"),
        "api.siliconflow.cn" => Some("siliconflow"),
        "api.siliconflow.com" => Some("siliconflow-en"),
        "openrouter.ai" | "api.openrouter.ai" => Some("openrouter"),
        "api.novita.ai" => Some("novita"),
        "opencode.ai" if path == "/zen/go" || path == "/zen/go/v1" => Some("opencode-go"),
        _ => None,
    }
}

fn validate_https_service_url(base_url: &str) -> Result<url::Url, String> {
    let mut url =
        url::Url::parse(base_url).map_err(|_| "模型来源地址无效，无法查询余额".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err("余额查询仅允许使用服务本身的 HTTPS 主机".to_string());
    }
    // A configured custom endpoint must not be able to redirect a credential
    // to a local service or raw IP. The balance request stays on the model host.
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return Err("余额查询不允许使用本地主机或 IP 地址".to_string());
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn balance_endpoint(profile_id: &str, base_url: &str) -> Result<String, String> {
    let profile = profile_id.trim().to_ascii_lowercase();
    let mut url = validate_https_service_url(base_url)?;
    let official = match profile.as_str() {
        "deepseek" => Some((
            &["api.deepseek.com"][..],
            "https://api.deepseek.com/user/balance",
        )),
        "kimi" => Some((
            &["api.moonshot.cn"][..],
            "https://api.moonshot.cn/v1/users/me/balance",
        )),
        "kimi-coding" => Some((
            &["api.kimi.com"][..],
            "https://api.kimi.com/coding/v1/usages",
        )),
        "zhipu-plan" => {
            let (host, base) = match url.host_str() {
                Some("open.bigmodel.cn") => ("open.bigmodel.cn", "https://open.bigmodel.cn"),
                Some("api.z.ai") => ("api.z.ai", "https://api.z.ai"),
                _ => return Err("智谱套餐查询只允许使用官方 HTTPS 主机".to_string()),
            };
            let _ = host;
            return Ok(format!("{base}/api/monitor/usage/quota/limit"));
        }
        "minimax-plan-cn" => Some((
            &["api.minimaxi.com", "api.minimax.cn"][..],
            "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains",
        )),
        "minimax-plan-en" => Some((
            &["api.minimax.io"][..],
            "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
        )),
        "stepfun" => Some((
            &["api.stepfun.ai", "api.stepfun.com"][..],
            "https://api.stepfun.com/v1/accounts",
        )),
        "siliconflow" => Some((
            &["api.siliconflow.cn"][..],
            "https://api.siliconflow.cn/v1/user/info",
        )),
        "siliconflow-en" => Some((
            &["api.siliconflow.com"][..],
            "https://api.siliconflow.com/v1/user/info",
        )),
        "openrouter" => Some((
            &["openrouter.ai", "api.openrouter.ai"][..],
            "https://openrouter.ai/api/v1/credits",
        )),
        "novita" => Some((
            &["api.novita.ai"][..],
            "https://api.novita.ai/v3/user/balance",
        )),
        _ => None,
    };
    if let Some((allowed_hosts, endpoint)) = official {
        if !allowed_hosts.contains(&url.host_str().unwrap_or_default()) {
            return Err("余额查询仅允许连接所选供应商的官方 HTTPS 地址".to_string());
        }
        return Ok(endpoint.to_string());
    }

    let base_path = url.path().trim_end_matches('/');
    let root = base_path
        .strip_suffix("/v1")
        .unwrap_or(base_path)
        .trim_end_matches('/');
    let path = match profile.as_str() {
        // Match the generic CC Switch template literally: append to Base URL.
        "relay-balance" => format!("{base_path}/user/balance"),
        "sub2api" => format!("{root}/v1/usage"),
        "new-api" => format!("{base_path}/api/user/self"),
        "opencode-go" => {
            if url.host_str() != Some("opencode.ai")
                || !(base_path == "/zen/go" || base_path == "/zen/go/v1")
            {
                return Err("OpenCode Go 查询仅允许使用官方套餐地址".to_string());
            }
            "/zen/go/v1/usage".to_string()
        }
        _ => return Err("此模型来源未配置受支持的余额查询接口".to_string()),
    };
    url.set_path(if path.is_empty() { "/" } else { &path });
    Ok(url.to_string())
}

trait LoopbackEndpoint {
    fn is_loopback_http_url(&self) -> bool;
}

impl LoopbackEndpoint for str {
    fn is_loopback_http_url(&self) -> bool {
        let Ok(url) = url::Url::parse(self) else {
            return false;
        };
        match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(host)) => {
                host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost")
            }
            None => false,
        }
    }
}
fn request_balance_payload(
    profile_id: &str,
    endpoint: &str,
    api_key: &str,
    access_token: &str,
    user_id: Option<&str>,
) -> Result<Value, String> {
    let mut client_builder = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if endpoint.is_loopback_http_url() {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|_| "无法初始化余额查询连接".to_string())?;
    let mut builder = client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json");
    match profile_id {
        "new-api" => {
            builder = builder
                .bearer_auth(access_token.trim())
                .header("New-Api-User", user_id.unwrap_or_default());
        }
        "zhipu-plan" => {
            builder = builder.header("Authorization", api_key.trim());
        }
        "deepseek" | "kimi" | "kimi-coding" | "minimax-plan-cn" | "minimax-plan-en" | "stepfun"
        | "siliconflow" | "siliconflow-en" | "openrouter" | "novita" | "relay-balance"
        | "sub2api" | "opencode-go" => {
            builder = builder.bearer_auth(api_key.trim());
        }
        _ => return Err("此模型来源未配置受支持的余额查询接口".to_string()),
    }
    let request = builder
        .build()
        .map_err(|_| "无法创建余额查询请求".to_string())?;
    let response = client
        .execute(request)
        .map_err(|_| "连接余额接口失败，请检查网络后重试".to_string())?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN && profile_id == "opencode-go" {
        return Err("API Key 有效，但当前 OpenCode 工作区没有 Go 套餐".to_string());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("余额查询未获授权，请检查 API Key 及供应商账户权限".to_string());
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err("余额查询过于频繁，请稍后重试".to_string());
    }
    if !status.is_success() {
        return Err(format!(
            "供应商余额接口暂不可用（HTTP {}）",
            status.as_u16()
        ));
    }

    if let Some(content_type) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        if is_html_content_type(content_type) {
            return Err("余额接口返回网页而不是 JSON；请检查所选模板或 Base URL".to_string());
        }
    }

    let mut body = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| "读取供应商余额响应失败".to_string())?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("供应商余额响应超出大小限制".to_string());
    }
    serde_json::from_slice(&body).map_err(|_| "供应商余额响应格式无法识别".to_string())
}

fn is_html_content_type(content_type: &str) -> bool {
    content_type.to_ascii_lowercase().contains("text/html")
}

fn parse_balance_payload(
    profile_id: &str,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    let profile_id = profile_id.trim().to_ascii_lowercase();
    if profile_id == "kimi-coding" {
        return parse_kimi_coding_quota(profile_id, payload);
    }
    if profile_id == "zhipu-plan" {
        return parse_zhipu_plan_quota(profile_id, payload);
    }
    if profile_id == "minimax-plan-cn" || profile_id == "minimax-plan-en" {
        return parse_minimax_plan_quota(profile_id, payload);
    }
    if profile_id == "relay-balance" {
        return parse_generic_balance(profile_id, payload);
    }
    if matches!(
        profile_id.as_str(),
        "stepfun" | "siliconflow" | "siliconflow-en" | "openrouter" | "novita"
    ) {
        return parse_provider_balance(&profile_id, payload);
    }
    if profile_id == "sub2api" {
        return parse_sub2api_usage(profile_id, payload);
    }
    if profile_id == "new-api" {
        return parse_new_api_usage(profile_id, payload);
    }
    if profile_id == "opencode-go" {
        return parse_opencode_go_quota(profile_id, payload);
    }
    let (balances, is_available) = match profile_id.as_str() {
        "deepseek" => {
            let infos = payload
                .get("balance_infos")
                .and_then(Value::as_array)
                .ok_or_else(|| "DeepSeek 余额响应缺少账户明细".to_string())?;
            if infos.is_empty() || infos.len() > 16 {
                return Err("DeepSeek 未返回有效的余额账户".to_string());
            }
            let mut balances = Vec::with_capacity(infos.len());
            for info in infos {
                let currency = info
                    .get("currency")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && value.len() <= 16)
                    .ok_or_else(|| "DeepSeek 余额响应缺少有效币种".to_string())?;
                let amount = number(info.get("total_balance"))
                    .ok_or_else(|| "DeepSeek 余额响应包含无效金额".to_string())?;
                balances.push(BalanceCurrency {
                    currency: currency.to_ascii_uppercase(),
                    available: amount,
                    total: Some(amount),
                    components: collect_components(info, &["granted_balance", "topped_up_balance"]),
                });
            }
            (
                balances,
                payload.get("is_available").and_then(Value::as_bool),
            )
        }
        "kimi" => {
            if payload.get("status").and_then(Value::as_bool) == Some(false)
                || payload
                    .get("code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0)
            {
                return Err("Moonshot 账户未返回可用余额".to_string());
            }
            let data = payload
                .get("data")
                .ok_or_else(|| "Moonshot 余额响应缺少账户数据".to_string())?;
            let amount = number(data.get("available_balance"))
                .ok_or_else(|| "Moonshot 余额响应包含无效金额".to_string())?;
            (
                vec![BalanceCurrency {
                    currency: "CNY".to_string(),
                    available: amount,
                    total: Some(amount),
                    components: collect_components(data, &["cash_balance", "voucher_balance"]),
                }],
                None,
            )
        }
        _ => return Err("此模型来源未配置受支持的余额查询接口".to_string()),
    };

    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "balance".to_string(),
        balances,
        plan_name: None,
        quota_windows: Vec::new(),
        is_available,
        observed_at: String::new(),
    })
}

fn parse_provider_balance(
    profile_id: &str,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    let balance = match profile_id {
        "stepfun" => {
            let amount = number(payload.get("balance"))
                .ok_or_else(|| "StepFun 余额响应缺少有效 balance".to_string())?;
            BalanceCurrency {
                currency: "CNY".to_string(),
                available: amount,
                total: None,
                components: collect_components(
                    payload,
                    &["total_cash_balance", "total_voucher_balance"],
                ),
            }
        }
        "siliconflow" | "siliconflow-en" => {
            let data = payload
                .get("data")
                .ok_or_else(|| "SiliconFlow 余额响应缺少 data".to_string())?;
            let amount = number(data.get("totalBalance"))
                .ok_or_else(|| "SiliconFlow 余额响应缺少有效 totalBalance".to_string())?;
            BalanceCurrency {
                currency: if profile_id == "siliconflow" {
                    "CNY"
                } else {
                    "USD"
                }
                .to_string(),
                available: amount,
                total: None,
                components: collect_components(data, &["balance", "chargeBalance"]),
            }
        }
        "openrouter" => {
            let data = payload.get("data").unwrap_or(payload);
            let total = number(data.get("total_credits"))
                .ok_or_else(|| "OpenRouter 余额响应缺少 total_credits".to_string())?;
            let used = number(data.get("total_usage"))
                .ok_or_else(|| "OpenRouter 余额响应缺少 total_usage".to_string())?;
            let remaining = total - used;
            if !remaining.is_finite() {
                return Err("OpenRouter 余额超出有效数值范围".to_string());
            }
            BalanceCurrency {
                currency: "USD".to_string(),
                available: remaining,
                total: Some(total),
                components: BTreeMap::from([("used".to_string(), used)]),
            }
        }
        "novita" => {
            let raw = number(payload.get("availableBalance"))
                .ok_or_else(|| "Novita 余额响应缺少 availableBalance".to_string())?;
            let available = raw / 10_000.0;
            if !available.is_finite() {
                return Err("Novita 余额超出有效数值范围".to_string());
            }
            BalanceCurrency {
                currency: "USD".to_string(),
                available,
                total: None,
                components: BTreeMap::new(),
            }
        }
        _ => return Err("此模型来源未配置受支持的余额查询接口".to_string()),
    };
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id.to_string(),
        kind: "balance".to_string(),
        balances: vec![balance],
        plan_name: Some(
            match profile_id {
                "stepfun" => "StepFun",
                "siliconflow" => "SiliconFlow",
                "siliconflow-en" => "SiliconFlow (EN)",
                "openrouter" => "OpenRouter",
                "novita" => "Novita AI",
                _ => "余额",
            }
            .to_string(),
        ),
        quota_windows: Vec::new(),
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_generic_balance(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    if payload.get("is_active").and_then(Value::as_bool) == Some(false)
        || payload.get("isValid").and_then(Value::as_bool) == Some(false)
    {
        return Err("中转账户当前不可用".to_string());
    }
    let amount = number(payload.get("balance"))
        .filter(|value| *value >= 0.0)
        .ok_or_else(|| "余额响应缺少有效 balance 数值".to_string())?;
    let currency = payload
        .get("unit")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 16)
        .unwrap_or("USD")
        .to_ascii_uppercase();
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "balance".to_string(),
        balances: vec![BalanceCurrency {
            currency,
            available: amount,
            total: None,
            components: BTreeMap::new(),
        }],
        plan_name: None,
        quota_windows: Vec::new(),
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_new_api_usage(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    if payload.get("success").and_then(Value::as_bool) != Some(true) {
        return Err("New API 未返回成功的账户额度".to_string());
    }
    let data = payload
        .get("data")
        .ok_or_else(|| "New API 额度响应缺少 data".to_string())?;
    let quota =
        number(data.get("quota")).ok_or_else(|| "New API 额度响应缺少有效 quota".to_string())?;
    let used_quota = number(data.get("used_quota"))
        .filter(|value| *value >= 0.0)
        .ok_or_else(|| "New API 额度响应缺少有效 used_quota".to_string())?;
    if quota < 0.0 {
        return Err("New API 额度响应包含无效 quota".to_string());
    }
    const NEW_API_QUOTA_PER_USD: f64 = 500_000.0;
    let remaining = quota / NEW_API_QUOTA_PER_USD;
    let used = used_quota / NEW_API_QUOTA_PER_USD;
    let total = remaining + used;
    if !remaining.is_finite() || !used.is_finite() || !total.is_finite() {
        return Err("New API 额度响应超出有效数值范围".to_string());
    }
    let plan_name = data
        .get("group")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 128)
        .map(str::to_string);
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "usage".to_string(),
        balances: vec![BalanceCurrency {
            currency: "USD".to_string(),
            available: remaining,
            total: Some(total),
            components: BTreeMap::from([("used".to_string(), used)]),
        }],
        plan_name,
        quota_windows: Vec::new(),
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_sub2api_usage(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    if payload.get("isValid").and_then(Value::as_bool) == Some(false) {
        return Err("Sub2API 账户当前不可用".to_string());
    }
    let remaining = number(payload.get("remaining"))
        .filter(|value| *value >= 0.0)
        .ok_or_else(|| "Sub2API 额度响应缺少有效 remaining".to_string())?;
    let quota = payload.get("quota").unwrap_or(&Value::Null);
    let unit = payload
        .get("unit")
        .or_else(|| quota.get("unit"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 16)
        .unwrap_or("USD")
        .to_ascii_uppercase();
    let limit = number(quota.get("limit")).filter(|value| *value > 0.0);
    let used = number(quota.get("used"));
    let total = limit;
    let plan_name = payload
        .get("planName")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 128)
        .map(str::to_string);
    let mut components = BTreeMap::new();
    if let Some(value) = used {
        components.insert("used".to_string(), value);
    }
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "usage".to_string(),
        balances: vec![BalanceCurrency {
            currency: unit,
            available: remaining,
            total,
            components,
        }],
        plan_name,
        quota_windows: Vec::new(),
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_kimi_coding_quota(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    let mut quota_windows = Vec::new();
    if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
        for (index, item) in limits.iter().enumerate() {
            let detail = item.get("detail").unwrap_or(item);
            let Some(limit) = number(detail.get("limit")).filter(|value| *value > 0.0) else {
                continue;
            };
            let Some(remaining) = number(detail.get("remaining")).filter(|value| *value >= 0.0)
            else {
                continue;
            };
            let used_percent = (((limit - remaining).max(0.0) / limit) * 100.0).clamp(0.0, 100.0);
            quota_windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
                name: if limits.len() > 1 {
                    format!("5 小时 {}", index + 1)
                } else {
                    "5 小时".to_string()
                },
                used_percent,
                remaining_percent: 100.0 - used_percent,
                resets_at: detail.get("resetTime").and_then(parse_reset_time),
            });
        }
    }
    if let Some(usage) = payload.get("usage") {
        if let (Some(limit), Some(remaining)) = (
            number(usage.get("limit")).filter(|value| *value > 0.0),
            number(usage.get("remaining")).filter(|value| *value >= 0.0),
        ) {
            let used_percent = (((limit - remaining).max(0.0) / limit) * 100.0).clamp(0.0, 100.0);
            quota_windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
                name: "每周".to_string(),
                used_percent,
                remaining_percent: 100.0 - used_percent,
                resets_at: usage.get("resetTime").and_then(parse_reset_time),
            });
        }
    }
    if quota_windows.is_empty() {
        return Err("Kimi Coding Plan 额度响应没有可识别的使用窗口".to_string());
    }
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "quota".to_string(),
        balances: Vec::new(),
        plan_name: Some("Kimi Coding Plan".to_string()),
        quota_windows,
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_zhipu_plan_quota(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        return Err("智谱套餐接口返回失败".to_string());
    }
    let data = payload
        .get("data")
        .ok_or_else(|| "智谱套餐响应缺少 data".to_string())?;
    let limits = data
        .get("limits")
        .and_then(Value::as_array)
        .ok_or_else(|| "智谱套餐响应缺少 limits".to_string())?;
    let mut windows = Vec::new();
    for limit in limits.iter().filter(|item| {
        item.get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| {
                kind.eq_ignore_ascii_case("TOKENS_LIMIT")
                    || kind.eq_ignore_ascii_case("CREDIT_LIMIT")
            })
    }) {
        let Some(percent) =
            number(limit.get("percentage")).filter(|value| (0.0..=100.0).contains(value))
        else {
            continue;
        };
        let name = match limit.get("unit").and_then(Value::as_i64) {
            Some(3) => "5 小时",
            Some(6) => "每周",
            _ if windows.is_empty() => "5 小时",
            _ => "每周",
        };
        windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
            name: name.to_string(),
            used_percent: percent,
            remaining_percent: 100.0 - percent,
            resets_at: limit.get("nextResetTime").and_then(parse_reset_time),
        });
        if windows.len() >= 2 {
            break;
        }
    }
    if windows.is_empty() {
        return Err("智谱套餐响应没有可识别的额度窗口".to_string());
    }
    let plan_name = data
        .get("level")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 128)
        .map(str::to_string);
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "quota".to_string(),
        balances: Vec::new(),
        plan_name,
        quota_windows: windows,
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_minimax_plan_quota(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    let items = payload
        .get("model_remains")
        .and_then(Value::as_array)
        .ok_or_else(|| "MiniMax 套餐响应缺少 model_remains".to_string())?;
    let item = items
        .iter()
        .find(|entry| entry.get("model_name").and_then(Value::as_str) == Some("general"))
        .ok_or_else(|| "MiniMax 套餐响应没有 general 模型额度".to_string())?;
    let mut windows = Vec::new();
    if let Some(remaining) = number(item.get("current_interval_remaining_percent"))
        .filter(|value| (0.0..=100.0).contains(value))
    {
        windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
            name: "5 小时".to_string(),
            used_percent: 100.0 - remaining,
            remaining_percent: remaining,
            resets_at: item.get("end_time").and_then(parse_reset_time),
        });
    }
    if item.get("current_weekly_status").and_then(Value::as_i64) == Some(1) {
        if let Some(remaining) = number(item.get("current_weekly_remaining_percent"))
            .filter(|value| (0.0..=100.0).contains(value))
        {
            windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
                name: "每周".to_string(),
                used_percent: 100.0 - remaining,
                remaining_percent: remaining,
                resets_at: item.get("weekly_end_time").and_then(parse_reset_time),
            });
        }
    }
    if windows.is_empty() {
        return Err("MiniMax 套餐响应没有可识别的额度窗口".to_string());
    }
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "quota".to_string(),
        balances: Vec::new(),
        plan_name: Some("MiniMax Coding Plan".to_string()),
        quota_windows: windows,
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn parse_reset_time(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        if let Ok(timestamp) = text.trim().parse::<i64>() {
            return timestamp_to_iso(timestamp);
        }
        return (text.len() <= 64).then(|| text.to_string());
    }
    value.as_i64().and_then(timestamp_to_iso)
}

fn timestamp_to_iso(timestamp: i64) -> Option<String> {
    if timestamp <= 0 {
        return None;
    }
    let seconds = if timestamp >= 1_000_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0).map(|value| value.to_rfc3339())
}

fn parse_opencode_go_quota(
    profile_id: String,
    payload: &Value,
) -> Result<AISourceBalanceSnapshot, String> {
    let usage = payload
        .get("usage")
        .ok_or_else(|| "OpenCode Go 配额响应缺少 usage".to_string())?;
    let windows = [
        ("rolling", "5 小时"),
        ("weekly", "每周"),
        ("monthly", "每月"),
    ];
    let mut quota_windows = Vec::new();
    for (key, name) in windows {
        let Some(window) = usage.get(key) else {
            continue;
        };
        let Some(used_percent) = number(window.get("percent")) else {
            continue;
        };
        if !(0.0..=100.0).contains(&used_percent) {
            continue;
        }
        let resets_at = if used_percent > 0.0 {
            window
                .get("resetsAt")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        quota_windows.push(crate::store::ai_service_balances::BalanceQuotaWindow {
            name: name.to_string(),
            used_percent,
            remaining_percent: 100.0 - used_percent,
            resets_at,
        });
    }
    if quota_windows.is_empty() {
        return Err("OpenCode Go 配额响应中没有可识别的使用窗口".to_string());
    }
    Ok(AISourceBalanceSnapshot {
        provider_profile_id: profile_id,
        kind: "quota".to_string(),
        balances: Vec::new(),
        plan_name: Some("OpenCode Go".to_string()),
        quota_windows,
        is_available: Some(true),
        observed_at: String::new(),
    })
}

fn number(value: Option<&Value>) -> Option<f64> {
    let number = value.and_then(|value| {
        value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<f64>().ok())
        })
    })?;
    number.is_finite().then_some(number)
}

fn collect_components(value: &Value, keys: &[&str]) -> BTreeMap<String, f64> {
    keys.iter()
        .filter_map(|key| number(value.get(*key)).map(|amount| ((*key).to_string(), amount)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{balance_endpoint, parse_balance_payload};

    #[test]
    fn deepseek_endpoint_is_provider_specific_and_rejects_custom_hosts() {
        assert_eq!(
            balance_endpoint("deepseek", "https://api.deepseek.com/v1").unwrap(),
            "https://api.deepseek.com/user/balance"
        );
        assert!(balance_endpoint("deepseek", "https://proxy.example.com/v1").is_err());
        assert!(balance_endpoint("deepseek", "http://api.deepseek.com/v1").is_err());
    }

    #[test]
    fn identifies_html_fallback_responses_without_reading_or_echoing_the_body() {
        assert!(super::is_html_content_type("text/html; charset=utf-8"));
        assert!(!super::is_html_content_type("application/json"));
        assert!(!super::is_html_content_type("application/problem+json"));
    }

    #[test]
    fn generic_relay_path_appends_to_base_url_like_cc_switch_template() {
        assert_eq!(
            balance_endpoint("relay-balance", "https://relay.example/v1").unwrap(),
            "https://relay.example/v1/user/balance"
        );
    }

    #[test]
    fn detects_known_balance_templates_only_for_exact_provider_origins_and_paths() {
        assert_eq!(
            super::suggest_balance_profile("https://api.kimi.com/coding"),
            Some("kimi-coding")
        );
        assert_eq!(
            super::suggest_balance_profile("https://api.openrouter.ai/api/v1"),
            Some("openrouter")
        );
        assert_eq!(
            super::suggest_balance_profile("https://api.siliconflow.cn/v1"),
            Some("siliconflow")
        );
        assert_eq!(
            super::suggest_balance_profile("https://api.siliconflow.com/v1"),
            Some("siliconflow-en")
        );
        assert_eq!(
            super::suggest_balance_profile("https://api.stepfun.com/v1"),
            Some("stepfun")
        );
        assert_eq!(
            super::suggest_balance_profile("https://api.novita.ai/v3"),
            Some("novita")
        );
        assert_eq!(
            super::suggest_balance_profile("https://opencode.ai/zen/go/v1"),
            Some("opencode-go")
        );
        assert_eq!(
            super::suggest_balance_profile("https://opencode.ai/zen/v1"),
            None
        );
        assert_eq!(
            super::suggest_balance_profile("https://proxy.example/v1"),
            None
        );
        assert_eq!(
            super::suggest_balance_profile("http://api.openrouter.ai/v1"),
            None
        );
    }

    #[test]
    fn provider_official_endpoints_are_pinned_and_reject_host_confusion() {
        assert_eq!(
            balance_endpoint("stepfun", "https://api.stepfun.ai/v1").unwrap(),
            "https://api.stepfun.com/v1/accounts"
        );
        assert_eq!(
            balance_endpoint("siliconflow", "https://api.siliconflow.cn/v1").unwrap(),
            "https://api.siliconflow.cn/v1/user/info"
        );
        assert_eq!(
            balance_endpoint("siliconflow-en", "https://api.siliconflow.com/v1").unwrap(),
            "https://api.siliconflow.com/v1/user/info"
        );
        assert_eq!(
            balance_endpoint("openrouter", "https://api.openrouter.ai/api/v1").unwrap(),
            "https://openrouter.ai/api/v1/credits"
        );
        assert_eq!(
            balance_endpoint("novita", "https://api.novita.ai/v3").unwrap(),
            "https://api.novita.ai/v3/user/balance"
        );
        assert!(
            balance_endpoint("openrouter", "https://openrouter.ai.attacker.example/v1").is_err()
        );
    }

    #[test]
    fn coding_plan_routes_are_pinned_to_provider_origins() {
        assert_eq!(
            balance_endpoint("kimi-coding", "https://api.kimi.com/coding/v1").unwrap(),
            "https://api.kimi.com/coding/v1/usages"
        );
        assert_eq!(
            balance_endpoint("zhipu-plan", "https://open.bigmodel.cn/api/paas/v4").unwrap(),
            "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
        );
        assert_eq!(
            balance_endpoint("minimax-plan-cn", "https://api.minimax.cn/v1").unwrap(),
            "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains"
        );
        assert_eq!(
            balance_endpoint("minimax-plan-en", "https://api.minimax.io/v1").unwrap(),
            "https://api.minimax.io/v1/api/openplatform/coding_plan/remains"
        );
        assert!(
            balance_endpoint("zhipu-plan", "https://open.bigmodel.cn.attacker.example/v1").is_err()
        );
    }

    #[test]
    fn zhipu_plan_uses_raw_authorization_header() {
        use super::request_balance_payload;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let captured = Arc::new(Mutex::new(String::new()));
        let server_capture = captured.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            *server_capture.lock().expect("capture lock") =
                String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            let body = r#"{"success":false}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });
        let _ = request_balance_payload(
            "zhipu-plan",
            &format!("http://{address}/quota"),
            "raw-key",
            "raw-key",
            None,
        );
        server.join().expect("test server");
        let request = captured.lock().expect("capture lock").clone();
        assert!(request.contains("authorization: raw-key"));
        assert!(!request.contains("authorization: bearer"));
    }

    #[test]
    fn parses_zhipu_and_minimax_coding_plan_windows() {
        let zhipu = parse_balance_payload(
            "zhipu-plan",
            &serde_json::json!({"success":true,"data":{
                "level":"pro","limits":[
                    {"type":"TOKENS_LIMIT","unit":3,"percentage":40,"nextResetTime":"1792000000000"},
                    {"type":"TOKENS_LIMIT","unit":6,"percentage":10,"nextResetTime":"1792500000000"}
                ]
            }}),
        )
        .unwrap();
        assert_eq!(zhipu.plan_name.as_deref(), Some("pro"));
        assert_eq!(zhipu.quota_windows.len(), 2);
        assert_eq!(zhipu.quota_windows[1].name, "每周");

        let minimax = parse_balance_payload("minimax-plan-cn", &serde_json::json!({"model_remains":[{
            "model_name":"general","current_interval_remaining_percent":75,"end_time":"1792000000000",
            "current_weekly_status":1,"current_weekly_remaining_percent":50,"weekly_end_time":"1792500000000"
        }]})).unwrap();
        assert_eq!(minimax.quota_windows.len(), 2);
        assert_eq!(minimax.quota_windows[0].remaining_percent, 75.0);
        assert_eq!(minimax.quota_windows[1].used_percent, 50.0);
    }

    #[test]
    fn provider_specific_balance_adapters_retain_currency_and_component_semantics() {
        let stepfun = parse_balance_payload(
            "stepfun",
            &serde_json::json!({
                "balance": 12.5, "total_cash_balance": 10.0, "total_voucher_balance": 2.5
            }),
        )
        .unwrap();
        assert_eq!(stepfun.balances[0].currency, "CNY");
        assert_eq!(stepfun.balances[0].available, 12.5);

        let siliconflow = parse_balance_payload(
            "siliconflow",
            &serde_json::json!({
                "code": 20000, "data": {"totalBalance": 8.25}
            }),
        )
        .unwrap();
        assert_eq!(siliconflow.balances[0].currency, "CNY");
        assert_eq!(siliconflow.balances[0].available, 8.25);

        let openrouter = parse_balance_payload(
            "openrouter",
            &serde_json::json!({
                "data": {"total_credits": 20.0, "total_usage": 7.5}
            }),
        )
        .unwrap();
        assert_eq!(openrouter.balances[0].currency, "USD");
        assert_eq!(openrouter.balances[0].available, 12.5);
        assert_eq!(openrouter.balances[0].components["used"], 7.5);

        let novita = parse_balance_payload(
            "novita",
            &serde_json::json!({
                "availableBalance": 12500
            }),
        )
        .unwrap();
        assert_eq!(novita.balances[0].currency, "USD");
        assert_eq!(novita.balances[0].available, 1.25);
    }

    #[test]
    fn balance_endpoint_rejects_nonstandard_ports_and_embedded_credentials() {
        assert!(balance_endpoint("deepseek", "https://api.deepseek.com:8443/v1").is_err());
        assert!(balance_endpoint("deepseek", "https://user@api.deepseek.com/v1").is_err());
    }

    #[test]
    fn unauthorized_responses_do_not_echo_provider_body_or_api_key() {
        use super::request_balance_payload;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let body = r#"{"error":"provider-private-detail"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let error = request_balance_payload(
            "relay-balance",
            &format!("http://{address}/balance"),
            "sk-private",
            "sk-private",
            None,
        )
        .expect_err("unauthorized status should fail");
        server.join().expect("test server");
        assert!(error.contains("未获授权"));
        assert!(!error.contains("provider-private-detail"));
        assert!(!error.contains("sk-private"));
    }

    #[test]
    fn new_api_request_uses_access_token_and_user_id_header() {
        use super::request_balance_payload;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let captured = Arc::new(Mutex::new(String::new()));
        let server_capture = captured.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0u8; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            *server_capture.lock().expect("capture lock") =
                String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            let body = r#"{"error":"private response body"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let _ = request_balance_payload(
            "new-api",
            &format!("http://{address}/api/user/self"),
            "inference-key",
            "balance-access-secret",
            Some("user-42"),
        );
        server.join().expect("test server");
        let request = captured.lock().expect("capture lock").clone();
        assert!(request.contains("authorization: bearer balance-access-secret"));
        assert!(request.contains("new-api-user: user-42"));
        assert!(!request.contains("inference-key"));
    }

    #[test]
    fn parses_deepseek_currency_breakdown_without_flattening_currencies() {
        let snapshot = parse_balance_payload(
            "deepseek",
            &serde_json::json!({
                "is_available": true,
                "balance_infos": [
                    {"currency":"USD", "total_balance":"1.25", "granted_balance":0.25, "topped_up_balance":1.0},
                    {"currency":"CNY", "total_balance":0, "granted_balance":0, "topped_up_balance":0}
                ]
            }),
        )
        .unwrap();

        assert_eq!(snapshot.balances.len(), 2);
        assert_eq!(snapshot.balances[0].currency, "USD");
        assert_eq!(snapshot.balances[0].available, 1.25);
        assert_eq!(snapshot.balances[0].components["granted_balance"], 0.25);
        assert_eq!(snapshot.balances[1].available, 0.0);
        assert_eq!(snapshot.is_available, Some(true));
    }

    #[test]
    fn parses_moonshot_available_and_cash_voucher_components() {
        let snapshot = parse_balance_payload(
            "kimi",
            &serde_json::json!({
                "code": 0,
                "status": true,
                "data": {"available_balance":"8.75", "cash_balance":5.0, "voucher_balance":3.75}
            }),
        )
        .unwrap();

        assert_eq!(snapshot.balances.len(), 1);
        assert_eq!(snapshot.balances[0].currency, "CNY");
        assert_eq!(snapshot.balances[0].available, 8.75);
        assert_eq!(snapshot.balances[0].components["voucher_balance"], 3.75);
    }

    #[test]
    fn new_api_and_sub2api_endpoint_normalization_preserves_service_origin() {
        assert_eq!(
            balance_endpoint("kimi-coding", "https://api.kimi.com/coding").unwrap(),
            "https://api.kimi.com/coding/v1/usages"
        );
        assert_eq!(
            balance_endpoint("new-api", "https://relay.example/v1").unwrap(),
            "https://relay.example/v1/api/user/self"
        );
        assert_eq!(
            balance_endpoint("sub2api", "https://sub.example/v1/").unwrap(),
            "https://sub.example/v1/usage"
        );
        assert_eq!(
            balance_endpoint("opencode-go", "https://opencode.ai/zen/go/v1").unwrap(),
            "https://opencode.ai/zen/go/v1/usage"
        );
        assert!(balance_endpoint("opencode-go", "https://relay.example/zen/go/v1").is_err());
    }

    #[test]
    fn generic_relay_templates_allow_same_origin_https_only() {
        assert_eq!(
            balance_endpoint("relay-balance", "https://relay.example/v1").unwrap(),
            "https://relay.example/v1/user/balance"
        );
        assert!(balance_endpoint("relay-balance", "http://relay.example/v1").is_err());
        assert!(balance_endpoint("new-api", "https://127.0.0.1:8080").is_err());
    }

    #[test]
    fn parses_generic_relay_balance_as_money_without_assuming_openai_has_that_api() {
        let snapshot = parse_balance_payload(
            "relay-balance",
            &serde_json::json!({"balance": 12.5, "unit": "CNY", "is_active": true}),
        )
        .unwrap();
        assert_eq!(snapshot.kind, "balance");
        assert_eq!(snapshot.balances[0].currency, "CNY");
        assert_eq!(snapshot.balances[0].available, 12.5);
    }

    #[test]
    fn parses_new_api_access_token_quota_and_sub2api_usage_separately() {
        let new_api = parse_balance_payload(
            "new-api",
            &serde_json::json!({"success":true,"data":{"group":"Pro","quota":2500000,"used_quota":500000}}),
        )
        .unwrap();
        assert_eq!(new_api.kind, "usage");
        assert_eq!(new_api.plan_name.as_deref(), Some("Pro"));
        assert_eq!(new_api.balances[0].available, 5.0);
        assert_eq!(new_api.balances[0].total, Some(6.0));
        assert_eq!(new_api.balances[0].components["used"], 1.0);

        let sub2api = parse_balance_payload(
            "sub2api",
            &serde_json::json!({"isValid":true,"planName":"Team","remaining":8.0,"unit":"USD","quota":{"limit":10.0,"used":2.0}}),
        )
        .unwrap();
        assert_eq!(sub2api.kind, "usage");
        assert_eq!(sub2api.plan_name.as_deref(), Some("Team"));
        assert_eq!(sub2api.balances[0].available, 8.0);
        assert_eq!(sub2api.balances[0].total, Some(10.0));
    }

    #[test]
    fn parses_kimi_coding_five_hour_and_weekly_plan_windows() {
        let snapshot = parse_balance_payload(
            "kimi-coding",
            &serde_json::json!({
                "limits":[{"detail":{"limit":100,"remaining":60,"resetTime":1792000000}}],
                "usage":{"limit":1000,"remaining":850,"resetTime":"2026-10-20T00:00:00Z"}
            }),
        )
        .unwrap();
        assert_eq!(snapshot.kind, "quota");
        assert_eq!(snapshot.quota_windows.len(), 2);
        assert_eq!(snapshot.quota_windows[0].used_percent, 40.0);
        assert_eq!(snapshot.quota_windows[0].remaining_percent, 60.0);
        assert_eq!(snapshot.quota_windows[1].remaining_percent, 85.0);
        assert!(snapshot.quota_windows[0].resets_at.is_some());
    }

    #[test]
    fn parses_opencode_go_quota_windows_as_percentages_not_dollar_balances() {
        let snapshot = parse_balance_payload(
            "opencode-go",
            &serde_json::json!({"usage":{
                "rolling":{"status":"ok","percent":37,"resetsAt":"2026-10-14T18:00:00Z"},
                "weekly":{"status":"ok","percent":0,"resetsAt":"2026-10-20T00:00:00Z"},
                "monthly":{"status":"rate-limited","percent":100,"resetsAt":"2026-11-01T00:00:00Z"}
            }}),
        )
        .unwrap();
        assert_eq!(snapshot.kind, "quota");
        assert!(snapshot.balances.is_empty());
        assert_eq!(snapshot.quota_windows.len(), 3);
        assert_eq!(snapshot.quota_windows[0].used_percent, 37.0);
        assert_eq!(snapshot.quota_windows[0].remaining_percent, 63.0);
        assert_eq!(snapshot.quota_windows[1].resets_at, None);
        assert_eq!(snapshot.quota_windows[2].remaining_percent, 0.0);
    }

    #[test]
    fn relay_balance_payload_missing_amount_is_not_misreported_as_zero() {
        assert!(
            parse_balance_payload("relay-balance", &serde_json::json!({"unit":"USD"})).is_err()
        );
        assert!(parse_balance_payload(
            "new-api",
            &serde_json::json!({"success":false,"message":"no"})
        )
        .is_err());
        assert!(parse_balance_payload("sub2api", &serde_json::json!({"isValid":false})).is_err());
        assert!(parse_balance_payload("opencode-go", &serde_json::json!({"usage":{}})).is_err());
    }

    #[test]
    fn rejects_missing_or_non_finite_balances_instead_of_reporting_zero() {
        assert!(parse_balance_payload(
            "deepseek",
            &serde_json::json!({"is_available": true, "balance_infos": []}),
        )
        .is_err());
        assert!(parse_balance_payload(
            "kimi",
            &serde_json::json!({"data": {"available_balance": "NaN"}}),
        )
        .is_err());
    }
}
