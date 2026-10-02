//! GitHub 分发落点。
//!
//! 落点定义为「tag + Release + 构建好的制品资产 + 发布清单」：插件 entry 指向
//! 编译产物，所以不能用源码树分发。tag 不可变，因此冲突时直接失败，不覆盖。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::github_credentials;

pub(crate) const RELEASE_MANIFEST_SCHEMA: &str = "himind_extension_release.v1";
const DEFAULT_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "himind-agent";
const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagDecision {
    Create,
    Reuse,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssetDecision {
    Upload,
    Skip,
    Conflict,
}

/// 制品扩展名与制品类型一一对应，避免同一 ID 的插件包与技能包在同一次发布里重名。
pub(crate) fn asset_extension(kind: &str) -> Result<&'static str, Box<dyn Error>> {
    match kind.trim() {
        "plugin" => Ok("hmpkg"),
        "skill" => Ok("hmskill"),
        "workflow" => Ok("hmwf"),
        other => Err(format!("扩展类型无效: {other}").into()),
    }
}

/// tag 命名：`<kind>/<id>@<version>`。聚合仓库下多扩展互不冲突，
/// 与精确 pin、溯源一一对应。
pub(crate) fn tag_name(kind: &str, id: &str, version: &str) -> Result<String, Box<dyn Error>> {
    ensure_tag_safe(kind)?;
    ensure_tag_safe(id)?;
    ensure_tag_safe(version)?;
    Ok(format!("{}/{id}@{version}", kind.trim()))
}

pub(crate) fn asset_name(kind: &str, id: &str, version: &str) -> Result<String, Box<dyn Error>> {
    Ok(format!("{id}-{version}.{}", asset_extension(kind)?))
}

pub(crate) fn manifest_name(id: &str, version: &str) -> String {
    format!("{id}@{version}.json")
}

/// 归一化仓库标识：项目里可能存着完整 URL，而 GitHub API 路径只接受 `owner/repo`。
pub(crate) fn normalize_repository_slug(repository: &str) -> Result<String, Box<dyn Error>> {
    let value = repository.trim().trim_end_matches('/');
    if value.is_empty() {
        return Err("GitHub 仓库不能为空".into());
    }
    let path = if let Some(rest) = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("http://github.com/"))
        .or_else(|| value.strip_prefix("git@github.com:"))
    {
        rest
    } else if value.contains("://") || value.contains('@') {
        return Err(format!("只支持 github.com 仓库地址，请改用 owner/repo 形式: {value}").into());
    } else {
        value
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || segments.next().is_some() {
        return Err(format!("GitHub 仓库必须是 owner/repo 形式: {value}").into());
    }
    Ok(format!("{owner}/{name}"))
}

/// tag 会进入 git ref，禁止空格、`~^:?*[\`、`..`、`@{`、以 `/` 开头或结尾等。
fn ensure_tag_safe(value: &str) -> Result<(), Box<dyn Error>> {
    let value = value.trim();
    if value.is_empty() {
        return Err("tag 组成不能为空".into());
    }
    let invalid = value.is_empty()
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || "~^:?*[\\".contains(ch) || ch.is_control());
    if invalid {
        return Err(format!("tag 包含非法字符: {value}").into());
    }
    Ok(())
}

/// 已存在同名 tag 时的判定：指向同一提交才允许复用，否则是版本占用。
pub(crate) fn decide_tag(existing_commit: Option<&str>, expected_commit: &str) -> TagDecision {
    match existing_commit
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => TagDecision::Create,
        Some(existing) => {
            let expected = expected_commit.trim();
            if expected.is_empty() || existing.eq_ignore_ascii_case(expected) {
                TagDecision::Reuse
            } else {
                TagDecision::Conflict
            }
        }
    }
}

/// 待判定资产的本地身份。
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssetIdentity<'a> {
    pub name: &'a str,
    pub sha256: &'a str,
}

/// 台账基线：上一次发布（或尝试）留下的事实。
#[derive(Debug, Clone, Copy)]
pub(crate) struct LedgerBaseline<'a> {
    pub asset_name: &'a str,
    pub sha256: &'a str,
    pub published: bool,
}

/// 已存在同名 Release 资产时的判定。
///
/// 判据按可靠性排序：
/// 1. **服务端摘要**（`digest: sha256:…`）与本地制品摘要一致 → 内容已在位，跳过。
///    这一条同时覆盖「上传成功但响应超时」的真实场景：资产其实已经建好，重试不该报冲突。
/// 2. 老版本 GitHub 不返回 `digest` 时，退回台账：主制品名与批次摘要都对得上，
///    或该版本已标记发布（清单等同批文件）→ 跳过。
/// 3. 其余情况一律冲突，宁可失败也不覆盖别人已经发布的制品。
pub(crate) fn decide_asset(
    existing: &[GithubAssetRef],
    local: AssetIdentity<'_>,
    batch_sha256: &str,
    baseline: &LedgerBaseline<'_>,
) -> AssetDecision {
    let Some(remote) = existing.iter().find(|asset| asset.name == local.name) else {
        return AssetDecision::Upload;
    };
    let remote_digest = remote
        .digest
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(remote.digest.trim());
    // 服务端摘要存在时它就是权威结论：一致即跳过，不一致即冲突，不再回退台账——
    // 否则「远端是别的内容、台账却说发过」会被错误放行。
    if !remote_digest.is_empty() {
        return if remote_digest.eq_ignore_ascii_case(local.sha256.trim()) {
            AssetDecision::Skip
        } else {
            AssetDecision::Conflict
        };
    }
    let previous = baseline.sha256.trim();
    let batch_matches = !previous.is_empty() && previous.eq_ignore_ascii_case(batch_sha256.trim());
    if batch_matches && (local.name == baseline.asset_name.trim() || baseline.published) {
        return AssetDecision::Skip;
    }
    AssetDecision::Conflict
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubIdentity {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubRepositoryInfo {
    pub full_name: String,
    pub default_branch: String,
    pub can_push: bool,
    pub private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubAssetRef {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub browser_download_url: String,
    /// GitHub 返回的 `sha256:<hex>` 内容摘要，是幂等判定最可靠的依据。
    #[serde(default)]
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubReleaseRef {
    pub id: u64,
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    /// 上传地址含 `{?name,label}` 模板，取用前需要剥离。
    #[serde(default)]
    pub upload_url: String,
    #[serde(default)]
    pub assets: Vec<GithubAssetRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubArtifact {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub content_type: String,
}

/// 一次 GitHub 发布所需的全部输入。`commit` 为空时按分支 HEAD 解析。
#[derive(Debug, Clone)]
pub(crate) struct GithubPublishPlan {
    pub kind: String,
    pub id: String,
    pub version: String,
    pub repository: String,
    pub commit: String,
    pub branch: String,
    pub release_name: String,
    pub release_notes: String,
    pub channel: String,
    pub prerelease: bool,
    pub artifacts: Vec<GithubArtifact>,
    /// 制品分离签名元数据。None 表示本机未配置签名私钥，这次按未签名发布。
    pub signature: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GithubPublishOutcome {
    pub repository: String,
    pub tag: String,
    pub release_id: String,
    pub html_url: String,
    pub commit: String,
    pub asset_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// 已存在且摘要一致、本次跳过的资产名。
    #[serde(default)]
    pub reused_assets: Vec<String>,
}

/// 上一次发布留在台账里的事实，用于判断能否复用已有 tag / Release / 资产。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PreviousRelease {
    /// 台账记录的主制品名。
    pub asset_name: String,
    /// 台账记录的主制品摘要，充当这次发布的「批次身份」。
    pub sha256: String,
    /// 台账是否已经标记为已发布。
    pub published: bool,
}

/// 构造发布清单。清单是消费侧唯一事实源：它同时承载制品摘要与依赖精确 pin。
///
/// 清单**不含发布时间**：同一版本重复发布会得到完全相同的字节，这是幂等重试与
/// 内容寻址的前提；权威发布时间由 GitHub Release 与本机台账各自记录。
pub(crate) fn build_release_manifest(
    plan: &GithubPublishPlan,
    tag: &str,
    artifact: &GithubArtifact,
    dependencies: &Value,
    min_agent_version: &str,
) -> Value {
    let mut manifest = json!({
        "schema_version": RELEASE_MANIFEST_SCHEMA,
        "repository": plan.repository,
        "tag": tag,
        "kind": plan.kind,
        "id": plan.id,
        "version": plan.version,
        "channel": plan.channel,
        "source_commit": plan.commit,
        "min_agent_version": min_agent_version,
        "artifact": {
            "name": artifact.name,
            "size_bytes": artifact.size_bytes,
            "sha256": artifact.sha256,
        },
        "dependencies": dependencies,
    });
    // 签名只在配置了私钥时出现：消费侧看到该字段就必须校验通过，看不到则按未签名
    // 制品处理。这样「有没有签名」在清单里是显式事实，而不是靠某个开关推断。
    if let Some(signature) = plan.signature.clone() {
        if let Some(object) = manifest.as_object_mut() {
            object.insert("signature".to_string(), signature);
        }
    }
    manifest
}

struct GithubClient {
    client: reqwest::blocking::Client,
    api_base: String,
    token: String,
}

impl GithubClient {
    /// `token` 为 `None` 时不带认证头，用于公开仓库的只读读取。
    fn new(token: Option<&str>) -> Result<Self, Box<dyn Error>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            client,
            api_base: api_base(),
            token: token.unwrap_or_default().to_string(),
        })
    }

    fn get(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        let request = self
            .client
            .get(format!("{}{}", self.api_base, path))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", USER_AGENT);
        if self.token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.token)
        }
    }

    fn post(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        let request = self
            .client
            .post(format!("{}{}", self.api_base, path))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", USER_AGENT);
        if self.token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.token)
        }
    }
}

fn api_base() -> String {
    std::env::var("HIMIND_GITHUB_API_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

/// GitHub 返回的错误里绝不带上 token；这里只透出状态码与 GitHub 的消息体。
fn describe_failure(status: reqwest::StatusCode, body: &str, action: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().chars().take(200).collect());
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return format!("{action}失败：GitHub 拒绝了凭据（401），请重新授权。{message}");
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return format!(
            "{action}失败：凭据权限不足（403），需要 Contents: Read and write。{message}"
        );
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return format!(
            "{action}失败：GitHub 未找到目标（404），请确认仓库地址与凭据可见范围。{message}"
        );
    }
    format!("{action}失败：GitHub 返回 {}。{message}", status.as_u16())
}

pub(crate) fn verify_token(token: &str) -> Result<GithubIdentity, Box<dyn Error>> {
    let client = GithubClient::new(Some(token))?;
    parse_identity(client.get("/user").send()?)
}

fn parse_identity(response: reqwest::blocking::Response) -> Result<GithubIdentity, Box<dyn Error>> {
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "校验 GitHub 凭据").into());
    }
    let value: Value = serde_json::from_str(&body)?;
    Ok(GithubIdentity {
        login: value
            .get("login")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        id: value.get("id").and_then(Value::as_u64).unwrap_or_default(),
    })
}

pub(crate) fn repository_info(
    token: &str,
    repository: &str,
) -> Result<GithubRepositoryInfo, Box<dyn Error>> {
    let client = GithubClient::new(Some(token))?;
    let slug = normalize_repository_slug(repository)?;
    repository_info_with_capability(
        &client,
        &slug,
        crate::store::github_credentials::cached_contents_write().unwrap_or(None),
    )
}

/// 写权限判定。安装令牌与个人令牌的判据不同：
/// - 个人令牌 / 用户令牌：`GET /repos/{slug}` 的 `permissions.push`。
/// - 安装令牌：`permissions` 字段恒为 false（它只反映登录用户），因此改用「签发安装
///   令牌时响应里的 contents 权限」+「该仓库是否在安装覆盖范围内」两个事实。
pub(crate) fn decide_can_push(
    installation_contents_write: Option<bool>,
    repo_permissions_push: bool,
    covered_by_installation: bool,
) -> bool {
    match installation_contents_write {
        Some(write) => write && covered_by_installation,
        None => repo_permissions_push,
    }
}

fn repository_info_with_capability(
    client: &GithubClient,
    slug: &str,
    installation_contents_write: Option<bool>,
) -> Result<GithubRepositoryInfo, Box<dyn Error>> {
    let mut info = repository_status(client, slug)?;
    if installation_contents_write.is_some() {
        // 安装令牌：再确认仓库确实在这个安装的覆盖范围内。
        info.can_push = decide_can_push(
            installation_contents_write,
            info.can_push,
            installation_covers_repository(client, slug)?,
        );
    }
    Ok(info)
}

/// 指定的 App 安装是否覆盖该仓库。
fn installation_covers_repository(
    client: &GithubClient,
    slug: &str,
) -> Result<bool, Box<dyn Error>> {
    const PAGE_SIZE: usize = 100;
    const MAX_PAGES: usize = 10;
    for page in 1..=MAX_PAGES {
        let response = client
            .get(&format!(
                "/installation/repositories?per_page={PAGE_SIZE}&page={page}"
            ))
            .send()?;
        let status = response.status();
        let body = response.text()?;
        if !status.is_success() {
            return Err(describe_failure(status, &body, "读取安装覆盖的仓库").into());
        }
        let value: Value = serde_json::from_str(&body)?;
        let repositories = value
            .get("repositories")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if repositories.iter().any(|repository| {
            repository
                .get("full_name")
                .and_then(Value::as_str)
                .map(|name| name.eq_ignore_ascii_case(slug))
                .unwrap_or(false)
        }) {
            return Ok(true);
        }
        if repositories.len() < PAGE_SIZE {
            return Ok(false);
        }
    }
    Ok(false)
}

fn repository_status(
    client: &GithubClient,
    repository: &str,
) -> Result<GithubRepositoryInfo, Box<dyn Error>> {
    let response = client.get(&format!("/repos/{repository}")).send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "读取 GitHub 仓库").into());
    }
    let value: Value = serde_json::from_str(&body)?;
    Ok(GithubRepositoryInfo {
        full_name: value
            .get("full_name")
            .and_then(Value::as_str)
            .unwrap_or(repository)
            .to_string(),
        default_branch: value
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        can_push: value
            .get("permissions")
            .and_then(|permissions| permissions.get("push"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        private: value
            .get("private")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn resolve_commit(
    client: &GithubClient,
    repository: &str,
    reference: &str,
) -> Result<String, Box<dyn Error>> {
    let response = client
        .get(&format!("/repos/{repository}/commits/{reference}"))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "解析发布提交").into());
    }
    let value: Value = serde_json::from_str(&body)?;
    value
        .get("sha")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "GitHub 未返回提交 SHA".into())
}

/// 读取 tag 指向的提交。轻量标签直接给提交 SHA；附注标签需要再取一次对象。
fn tag_commit(
    client: &GithubClient,
    repository: &str,
    tag: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    let response = client
        .get(&format!(
            "/repos/{repository}/git/ref/tags/{}",
            urlencode(tag)
        ))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(describe_failure(status, &body, "读取 GitHub tag").into());
    }
    let value: Value = serde_json::from_str(&body)?;
    let object = value.get("object").cloned().unwrap_or(Value::Null);
    let sha = object
        .get("sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if object.get("type").and_then(Value::as_str) == Some("commit") {
        return Ok(Some(sha));
    }
    // 附注标签：继续解析到它指向的提交。
    let response = client
        .get(&format!("/repos/{repository}/git/tags/{sha}"))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "解析 GitHub 标签对象").into());
    }
    let value: Value = serde_json::from_str(&body)?;
    Ok(value
        .get("object")
        .and_then(|object| object.get("sha"))
        .and_then(Value::as_str)
        .map(str::to_string))
}

fn create_tag(
    client: &GithubClient,
    repository: &str,
    tag: &str,
    commit: &str,
) -> Result<(), Box<dyn Error>> {
    let response = client
        .post(&format!("/repos/{repository}/git/refs"))
        .json(&json!({ "ref": format!("refs/tags/{tag}"), "sha": commit }))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "创建 GitHub tag").into());
    }
    Ok(())
}

fn release_by_tag(
    client: &GithubClient,
    repository: &str,
    tag: &str,
) -> Result<Option<GithubReleaseRef>, Box<dyn Error>> {
    let response = client
        .get(&format!(
            "/repos/{repository}/releases/tags/{}",
            urlencode(tag)
        ))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(describe_failure(status, &body, "读取 GitHub Release").into());
    }
    Ok(Some(serde_json::from_str(&body)?))
}

fn create_release(
    client: &GithubClient,
    plan: &GithubPublishPlan,
    repository: &str,
    tag: &str,
) -> Result<GithubReleaseRef, Box<dyn Error>> {
    let response = client
        .post(&format!("/repos/{repository}/releases"))
        .json(&json!({
            "tag_name": tag,
            "name": plan.release_name,
            "body": plan.release_notes,
            "draft": false,
            "prerelease": plan.prerelease,
        }))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, "创建 GitHub Release").into());
    }
    Ok(serde_json::from_str(&body)?)
}

/// 资产上传的独立超时：制品可能几十 MiB，经本机代理（VPN/加速器）时 120 秒的
/// API 超时会先于上传完成触发，表现为「error sending request」这类传输错误。
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(900);

fn upload_asset(
    client: &GithubClient,
    release: &GithubReleaseRef,
    artifact: &GithubArtifact,
    repository: &str,
) -> Result<GithubAssetRef, Box<dyn Error>> {
    let upload_url = release
        .upload_url
        .split('{')
        .next()
        .unwrap_or_default()
        .to_string();
    if upload_url.trim().is_empty() {
        // 复用已有 Release 时 GitHub 也会返回 upload_url；缺字段说明响应异常。
        return Err("GitHub Release 未返回资产上传地址".into());
    }
    let bytes = std::fs::read(&artifact.path)?;
    let started = Instant::now();
    let response = client
        .client
        .post(format!("{upload_url}?name={}", urlencode(&artifact.name)))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", artifact.content_type.clone())
        .bearer_auth(&client.token)
        .timeout(UPLOAD_TIMEOUT)
        .body(bytes)
        .send()
        .map_err(|error| {
            let elapsed = started.elapsed().as_secs();
            format!(
                "上传制品 {} 失败（已耗时 {elapsed} 秒，{} 字节）：{error}。请检查网络或本机代理设置后重试；已创建的 tag 与 Release 会被复用，不会重复创建。",
                artifact.name, artifact.size_bytes
            )
        })?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(describe_failure(status, &body, &format!("上传资产到 {repository}")).into());
    }
    Ok(serde_json::from_str(&body)?)
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod urlencode_tests {
    use super::urlencode;

    #[test]
    fn tag_segments_are_percent_encoded() {
        assert_eq!(
            urlencode("skill/com.himind.x@1.0.0"),
            "skill%2Fcom.himind.x%401.0.0"
        );
    }
}

/// 执行一次 GitHub 发布。返回的结论会写入分发台账。
///
/// 失败语义：tag/Release 一旦创建就保留（GitHub 生态中 tag 不可变），重试时
/// 只补传缺失资产，不删除已发布内容。
pub(crate) fn publish(
    token: &str,
    plan: &GithubPublishPlan,
    previous: &PreviousRelease,
) -> Result<GithubPublishOutcome, Box<dyn Error>> {
    if plan.artifacts.is_empty() {
        return Err("没有可发布的制品资产".into());
    }
    let tag = tag_name(&plan.kind, &plan.id, &plan.version)?;
    let previous_sha256 = previous.sha256.as_str();
    let previous_asset_name = previous.asset_name.as_str();
    let published_before = previous.published;
    // 批次身份：本地主制品的摘要。台账摘要与它一致才允许复用。
    let batch_sha256 = plan
        .artifacts
        .first()
        .map(|artifact| artifact.sha256.clone())
        .unwrap_or_default();
    let client = GithubClient::new(Some(token))?;
    let repository = normalize_repository_slug(&plan.repository)?;
    let repository = repository.as_str();

    // 权限判定必须按凭据形态分流：安装令牌看签发响应的 contents 权限 + 安装覆盖范围。
    let repo = repository_info_with_capability(
        &client,
        repository,
        crate::store::github_credentials::cached_contents_write().unwrap_or(None),
    )?;
    if !repo.can_push {
        return Err(format!(
            "GitHub 凭据对 {repository} 没有写权限。个人令牌需要 Contents: Read and write 且已授权该仓库；GitHub App 需要把该仓库加入安装的 Repository access，并给 App 开启 Contents: Read and write。"
        )
        .into());
    }
    let commit = if plan.commit.trim().is_empty() {
        let reference = if plan.branch.trim().is_empty() {
            repo.default_branch.clone()
        } else {
            plan.branch.trim().to_string()
        };
        resolve_commit(&client, repository, &reference)?
    } else {
        plan.commit.trim().to_string()
    };

    match decide_tag(tag_commit(&client, repository, &tag)?.as_deref(), &commit) {
        TagDecision::Create => create_tag(&client, repository, &tag, &commit)?,
        TagDecision::Reuse => {}
        TagDecision::Conflict => {
            return Err(format!(
            "GitHub tag {tag} 已存在且指向其它提交。同版本号不允许重复发布，请提升版本号后重试。"
        )
            .into())
        }
    }

    let release = match release_by_tag(&client, repository, &tag)? {
        Some(release) => release,
        None => create_release(&client, plan, repository, &tag)?,
    };
    let baseline = LedgerBaseline {
        asset_name: previous_asset_name,
        sha256: previous_sha256,
        published: published_before,
    };

    let mut reused = Vec::new();
    let mut primary: Option<GithubAssetRef> = None;
    // 主制品固定是清单里的第一个（制品本身）；清单只是一并上传的辅助文件，
    // 台账与展示都应记录制品，否则会显示成清单的名字与大小。
    let primary_name = plan
        .artifacts
        .first()
        .map(|artifact| artifact.name.clone())
        .unwrap_or_default();
    for artifact in &plan.artifacts {
        match decide_asset(
            &release.assets,
            AssetIdentity {
                name: &artifact.name,
                sha256: &artifact.sha256,
            },
            &batch_sha256,
            &baseline,
        ) {
            AssetDecision::Upload => {
                let uploaded = upload_asset(&client, &release, artifact, repository)?;
                if artifact.name == primary_name {
                    primary = Some(uploaded);
                }
            }
            AssetDecision::Skip => {
                reused.push(artifact.name.clone());
                if artifact.name == primary_name {
                    primary = release
                        .assets
                        .iter()
                        .find(|asset| asset.name == artifact.name)
                        .cloned();
                }
            }
            AssetDecision::Conflict => {
                return Err(format!(
                    "GitHub Release {tag} 已存在同名资产 {} 且摘要不一致。请先确认该 Release 内容，或提升版本号后重试。",
                    artifact.name
                )
                .into())
            }
        }
    }

    Ok(GithubPublishOutcome {
        repository: repo.full_name,
        tag,
        release_id: release.id.to_string(),
        html_url: release.html_url,
        commit,
        asset_name: primary
            .as_ref()
            .map(|asset| asset.name.clone())
            .unwrap_or_default(),
        sha256: plan.artifacts[0].sha256.clone(),
        size_bytes: primary
            .as_ref()
            .map(|asset| asset.size)
            .unwrap_or(plan.artifacts[0].size_bytes),
        reused_assets: reused,
    })
}

/// 便捷入口：读取本机凭据后发布，缺少凭据时给出可执行的提示。
pub(crate) fn publish_with_stored_credential(
    plan: &GithubPublishPlan,
    previous: &PreviousRelease,
) -> Result<GithubPublishOutcome, Box<dyn Error>> {
    let account = github_credentials::account()?;
    let Some(account) = account else {
        return Err("尚未授权 GitHub 账号。请在设置 → 账号中保存具备 Contents: Read and write 的令牌后重试。".into());
    };
    // 账号显式绑定了仓库白名单时，只允许发布到列表内的仓库，避免误推到其它组织仓库。
    if !account.repositories.is_empty() {
        let slug = normalize_repository_slug(&plan.repository)?;
        if !account
            .repositories
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&slug))
        {
            return Err(format!(
                "GitHub 账号 {} 未绑定仓库 {slug}。请在设置 → 账号中把该仓库加入授权列表后重试。",
                account.login
            )
            .into());
        }
    }
    let Some(token) = github_credentials::resolve_token()? else {
        return Err("GitHub 凭据已失效，请重新授权后再试。".into());
    };
    publish(&token, plan, previous)
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// 读取某个 tag 的 Release（只读）。`token` 为 `None` 时按公开仓库处理。
pub(crate) fn release_for_tag(
    token: Option<&str>,
    repository: &str,
    tag: &str,
) -> Result<Option<GithubReleaseRef>, Box<dyn Error>> {
    let client = GithubClient::new(token)?;
    let slug = normalize_repository_slug(repository)?;
    release_by_tag(&client, &slug, tag)
}

/// 下载 Release 资产并校验大小与摘要。私仓读取需要凭据。
pub(crate) fn download_asset_verified(
    token: Option<&str>,
    url: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if url.trim().is_empty() {
        return Err("Release 资产缺少下载地址".into());
    }
    let client = GithubClient::new(token)?;
    let request = client
        .client
        .get(url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", USER_AGENT);
    let request = if client.token.is_empty() {
        request
    } else {
        request.bearer_auth(&client.token)
    };
    let response = request.send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(describe_failure(status, &body, "下载 Release 资产").into());
    }
    let bytes = response.bytes()?.to_vec();
    if expected_size != 0 && bytes.len() as u64 != expected_size {
        return Err(format!(
            "Release 资产大小与清单不一致：清单 {expected_size} 字节，实际 {} 字节",
            bytes.len()
        )
        .into());
    }
    if !expected_sha256.trim().is_empty() {
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
            return Err(format!(
                "Release 资产摘要与清单不一致：清单 {expected_sha256}，实际 {actual}"
            )
            .into());
        }
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_and_asset_names_are_stable_and_kind_scoped() {
        assert_eq!(
            tag_name("plugin", "com.himind.software-distribution", "1.2.1").unwrap(),
            "plugin/com.himind.software-distribution@1.2.1"
        );
        assert_eq!(
            asset_name("plugin", "com.himind.software-distribution", "1.2.1").unwrap(),
            "com.himind.software-distribution-1.2.1.hmpkg"
        );
        assert_eq!(
            asset_name("skill", "com.himind.skill.example", "1.0.0").unwrap(),
            "com.himind.skill.example-1.0.0.hmskill"
        );
        assert_eq!(
            asset_name("workflow", "com.himind.workflow.example", "2.0.0").unwrap(),
            "com.himind.workflow.example-2.0.0.hmwf"
        );
        assert_eq!(
            manifest_name("com.himind.x", "1.0.0"),
            "com.himind.x@1.0.0.json"
        );
    }

    #[test]
    fn repository_slug_accepts_urls_and_shorthand() {
        assert_eq!(
            normalize_repository_slug("mrbaoquan/himind-extensions").unwrap(),
            "mrbaoquan/himind-extensions"
        );
        assert_eq!(
            normalize_repository_slug("https://github.com/mrbaoquan/himind-extensions").unwrap(),
            "mrbaoquan/himind-extensions"
        );
        assert_eq!(
            normalize_repository_slug("https://github.com/mrbaoquan/himind-extensions.git/")
                .unwrap(),
            "mrbaoquan/himind-extensions"
        );
        assert!(normalize_repository_slug("https://gitlab.com/a/b").is_err());
        assert!(normalize_repository_slug("owner").is_err());
        assert!(normalize_repository_slug("").is_err());
    }

    #[test]
    fn tag_rejects_unsafe_characters() {
        assert!(tag_name("plugin", "com.himind.x y", "1.0.0").is_err());
        assert!(tag_name("plugin", "com.himind.x", "1.0.0 ..").is_err());
        assert!(tag_name("plugin", "com.himind.x", "1.0.0^").is_err());
        assert!(tag_name("plugin", "com.himind.x", "").is_err());
        assert!(asset_extension("extension").is_err());
    }

    #[test]
    fn tag_decision_blocks_version_reuse_on_other_commit() {
        assert_eq!(decide_tag(None, "abc"), TagDecision::Create);
        assert_eq!(decide_tag(Some("abc"), "abc"), TagDecision::Reuse);
        assert_eq!(decide_tag(Some("ABC"), "abc"), TagDecision::Reuse);
        assert_eq!(decide_tag(Some("abc"), "def"), TagDecision::Conflict);
        // 未记录提交时按「已存在即可复用」处理，避免把人工建的同名 tag 误判为冲突。
        assert_eq!(decide_tag(Some("abc"), ""), TagDecision::Reuse);
    }

    fn asset(name: &str, digest: &str) -> GithubAssetRef {
        GithubAssetRef {
            name: name.to_string(),
            size: 1,
            browser_download_url: String::new(),
            digest: digest.to_string(),
        }
    }

    fn baseline<'a>(asset_name: &'a str, sha: &'a str, published: bool) -> LedgerBaseline<'a> {
        LedgerBaseline {
            asset_name,
            sha256: sha,
            published,
        }
    }

    #[test]
    fn asset_decision_prefers_remote_digest() {
        let local_sha = "a".repeat(64);
        let existing = vec![asset("p-1.0.0.hmpkg", &format!("sha256:{local_sha}"))];
        // 服务端摘要与本地一致：即使台账是失败的半成品，也判定为已就位并跳过。
        // 这正是「上传成功但响应超时」的重试场景。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.0.0.hmpkg",
                    sha256: &local_sha,
                },
                &local_sha,
                &baseline("", &local_sha, false),
            ),
            AssetDecision::Skip
        );
        // 服务端摘要是别的内容：即使台账说发布过，也必须冲突。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.0.0.hmpkg",
                    sha256: &"b".repeat(64),
                },
                &"b".repeat(64),
                &baseline("p-1.0.0.hmpkg", &"b".repeat(64), true),
            ),
            AssetDecision::Conflict
        );
        // 没有同名资产 → 直接上传。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.1.0.hmpkg",
                    sha256: &local_sha,
                },
                &local_sha,
                &baseline("", "", false),
            ),
            AssetDecision::Upload
        );
    }

    #[test]
    fn asset_decision_falls_back_to_ledger_without_remote_digest() {
        let local_sha = "a".repeat(64);
        let existing = vec![asset("p-1.0.0.hmpkg", ""), asset("p@1.0.0.json", "")];
        // 主制品名与批次摘要都对得上 → 跳过。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.0.0.hmpkg",
                    sha256: &local_sha,
                },
                &local_sha,
                &baseline("p-1.0.0.hmpkg", &local_sha, false),
            ),
            AssetDecision::Skip
        );
        // 同批次的清单：凭「该版本已发布」跳过。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p@1.0.0.json",
                    sha256: "manifest-sha",
                },
                &local_sha,
                &baseline("p-1.0.0.hmpkg", &local_sha, true),
            ),
            AssetDecision::Skip
        );
        // 台账摘要不是这一批（本地制品被改动、版本没升）→ 冲突。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.0.0.hmpkg",
                    sha256: &local_sha,
                },
                &local_sha,
                &baseline("p-1.0.0.hmpkg", "other", true),
            ),
            AssetDecision::Conflict
        );
        // 台账什么都没有，且服务端无摘要 → 一律冲突，绝不覆盖。
        assert_eq!(
            decide_asset(
                &existing,
                AssetIdentity {
                    name: "p-1.0.0.hmpkg",
                    sha256: &local_sha,
                },
                &local_sha,
                &baseline("", "", false),
            ),
            AssetDecision::Conflict
        );
    }

    #[test]
    fn manifest_carries_pin_and_provenance() {
        let artifact = GithubArtifact {
            name: "com.himind.x-1.0.0.hmpkg".to_string(),
            path: PathBuf::from("F:/tmp/package.hmpkg"),
            sha256: "b".repeat(64),
            size_bytes: 2048,
            content_type: "application/octet-stream".to_string(),
        };
        let plan = GithubPublishPlan {
            kind: "plugin".to_string(),
            id: "com.himind.x".to_string(),
            version: "1.0.0".to_string(),
            repository: "owner/repo".to_string(),
            commit: "deadbeef".to_string(),
            branch: "main".to_string(),
            release_name: "示例插件 v1.0.0".to_string(),
            release_notes: "首个版本".to_string(),
            channel: "stable".to_string(),
            prerelease: false,
            artifacts: vec![artifact.clone()],
            signature: None,
        };
        let manifest = build_release_manifest(
            &plan,
            "plugin/com.himind.x@1.0.0",
            &artifact,
            &json!([]),
            "0.3.30",
        );
        assert_eq!(manifest["schema_version"], RELEASE_MANIFEST_SCHEMA);
        assert_eq!(manifest["tag"], "plugin/com.himind.x@1.0.0");
        assert_eq!(manifest["artifact"]["sha256"], "b".repeat(64));
        assert_eq!(manifest["source_commit"], "deadbeef");
        assert_eq!(manifest["min_agent_version"], "0.3.30");
        assert_eq!(manifest["dependencies"], json!([]));
        // 未配置签名私钥时清单不带签名字段，消费侧据此按未签名制品处理。
        assert!(manifest.get("signature").is_none());
    }

    #[test]
    fn manifest_records_signature_when_the_release_is_signed() {
        let artifact = GithubArtifact {
            name: "com.himind.x-1.0.0.hmpkg".to_string(),
            path: PathBuf::from("F:/tmp/package.hmpkg"),
            sha256: "b".repeat(64),
            size_bytes: 2048,
            content_type: "application/octet-stream".to_string(),
        };
        let signature = json!({
            "sha256": "b".repeat(64),
            "signature": "c2ln",
            "signature_key_id": "himind-production-2026",
            "signature_algorithm": "rsa-pss-sha256",
        });
        let plan = GithubPublishPlan {
            kind: "plugin".to_string(),
            id: "com.himind.x".to_string(),
            version: "1.0.0".to_string(),
            repository: "owner/repo".to_string(),
            commit: "deadbeef".to_string(),
            branch: "main".to_string(),
            release_name: "示例插件 v1.0.0".to_string(),
            release_notes: "首个版本".to_string(),
            channel: "stable".to_string(),
            prerelease: false,
            artifacts: vec![artifact.clone()],
            signature: Some(signature.clone()),
        };
        let manifest = build_release_manifest(
            &plan,
            "plugin/com.himind.x@1.0.0",
            &artifact,
            &json!([]),
            "0.3.30",
        );
        assert_eq!(manifest["signature"], signature);
    }

    /// 发布清单是一份跨边界的契约：发布侧写、消费侧读。这里让真实产物对 schema
    /// 校验，避免「改了字段但没人发现」的静默漂移。
    #[test]
    fn release_manifest_matches_the_published_schema() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../contracts/agent-core/v1/extension-release-manifest.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let artifact = GithubArtifact {
            name: "com.himind.x-1.0.0.hmpkg".to_string(),
            path: PathBuf::from("F:/tmp/package.hmpkg"),
            sha256: "b".repeat(64),
            size_bytes: 2048,
            content_type: "application/octet-stream".to_string(),
        };
        let dependencies = json!([{
            "kind": "plugin",
            "id": "com.himind.y",
            "required": true,
            "min_version": "1.0.0",
            "version": "1.2.0",
            "sha256": "c".repeat(64),
            "source": {
                "kind": "github",
                "id": "src-1",
                "repository": "owner/repo",
                "reference": "plugin/com.himind.y@1.2.0",
                "artifact_url": "",
            },
            "pinned": true,
        }]);
        for signature in [
            None,
            Some(json!({
                "file_name": "com.himind.x-1.0.0.hmpkg",
                "file_size": 2048,
                "sha256": "b".repeat(64),
                "signature": "c2lnbmF0dXJl",
                "signature_key_id": "himind-production-2026",
                "signature_algorithm": "rsa-pss-sha256",
            })),
        ] {
            let plan = GithubPublishPlan {
                kind: "plugin".to_string(),
                id: "com.himind.x".to_string(),
                version: "1.0.0".to_string(),
                repository: "owner/repo".to_string(),
                commit: "deadbeef".to_string(),
                branch: "main".to_string(),
                release_name: "示例插件 v1.0.0".to_string(),
                release_notes: "首个版本".to_string(),
                channel: "stable".to_string(),
                prerelease: false,
                artifacts: vec![artifact.clone()],
                signature,
            };
            let manifest = build_release_manifest(
                &plan,
                "plugin/com.himind.x@1.0.0",
                &artifact,
                &dependencies,
                "0.3.30",
            );
            assert!(
                validator.is_valid(&manifest),
                "发布清单不符合契约: {manifest:#?}"
            );
        }
    }

    #[test]
    fn credential_errors_never_leak_the_token() {
        let message = describe_failure(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"message":"Bad credentials"}"#,
            "校验 GitHub 凭据",
        );
        assert!(message.contains("401"));
        assert!(!message.contains("ghp_"));
    }

    #[test]
    fn write_permission_follows_credential_shape() {
        // 个人令牌 / 用户令牌：看仓库的 permissions.push。
        assert!(decide_can_push(None, true, false));
        assert!(!decide_can_push(None, false, true));
        // 安装令牌：`GET /repos` 的 permissions 恒为 false，必须改用
        // 「签发令牌时的 contents 权限」+「安装覆盖范围」。
        assert!(decide_can_push(Some(true), false, true));
        assert!(!decide_can_push(Some(true), false, false));
        assert!(!decide_can_push(Some(false), true, true));
    }
}
