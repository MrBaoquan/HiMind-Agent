//! 本机媒体任务表（异步媒体能力的作业事实源）。
//!
//! 生视频这类任务动辄几分钟：提交类能力只返回 `job_id`，状态与产物由本表维护，
//! 由 `media.job.get / wait / cancel` 查询与推进。表里只存本机元数据与产物路径，
//! 不含凭据（凭据由 `ai_services` 单独 DPAPI 保存）。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const STORE_FILE: &str = "media-jobs.json";
/// 只保留最近的任务，避免文件无限增长。
const MAX_JOBS: usize = 200;
const ID_PREFIX: &str = "media_job_";

pub(crate) const STATUS_QUEUED: &str = "queued";
pub(crate) const STATUS_RUNNING: &str = "running";
pub(crate) const STATUS_SUCCEEDED: &str = "succeeded";
pub(crate) const STATUS_FAILED: &str = "failed";
pub(crate) const STATUS_CANCELED: &str = "canceled";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MediaJobOutput {
    #[serde(default)]
    pub media_id: String,
    pub path: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub duration_seconds: f64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub cover_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MediaJob {
    pub id: String,
    /// 产出它的能力，例如 `video.generate`。
    pub capability: String,
    pub service_id: String,
    pub provider: String,
    pub adapter: String,
    pub model: String,
    /// Provider 侧任务 ID（提交后才有）。
    #[serde(default)]
    pub provider_job_id: String,
    pub status: String,
    #[serde(default)]
    pub progress: u8,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub output_dir: String,
    #[serde(default)]
    pub outputs: Vec<MediaJobOutput>,
    #[serde(default)]
    pub error: String,
    pub created_at: u64,
    pub updated_at: u64,
}

impl MediaJob {
    pub(crate) fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            STATUS_SUCCEEDED | STATUS_FAILED | STATUS_CANCELED
        )
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "job_id": self.id,
            "capability": self.capability,
            "service_id": self.service_id,
            "provider": self.provider,
            "model": self.model,
            "status": self.status,
            "progress": self.progress,
            "outputs": self.outputs,
            "error": self.error,
            "message": self.status_message(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }

    fn status_message(&self) -> &'static str {
        match self.status.as_str() {
            STATUS_QUEUED => "已提交，等待 provider 处理",
            STATUS_RUNNING => "生成中",
            STATUS_SUCCEEDED => "已完成",
            STATUS_FAILED => "失败",
            STATUS_CANCELED => "已取消",
            _ => "未知状态",
        }
    }
}

fn store_path() -> Result<PathBuf, Box<dyn Error>> {
    let dir = crate::store::paths::agent_home();
    fs::create_dir_all(&dir)?;
    Ok(dir.join(STORE_FILE))
}

fn load_all() -> Result<Vec<MediaJob>, Box<dyn Error>> {
    let path = store_path()?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn save_all(jobs: &[MediaJob]) -> Result<(), Box<dyn Error>> {
    crate::store::atomic_file::atomic_write(&store_path()?, &serde_json::to_vec_pretty(jobs)?)?;
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

/// 新建一条排队中的任务，返回带 `job_id` 的记录。
#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    capability: &str,
    service_id: &str,
    provider: &str,
    adapter: &str,
    model: &str,
    prompt: &str,
    output_dir: &str,
) -> Result<MediaJob, Box<dyn Error>> {
    let mut random = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let job = MediaJob {
        id: format!("{ID_PREFIX}{}_{suffix}", now()),
        capability: capability.to_string(),
        service_id: service_id.to_string(),
        provider: provider.to_string(),
        adapter: adapter.to_string(),
        model: model.to_string(),
        provider_job_id: String::new(),
        status: STATUS_QUEUED.to_string(),
        progress: 0,
        prompt: prompt.to_string(),
        output_dir: output_dir.to_string(),
        outputs: Vec::new(),
        error: String::new(),
        created_at: now(),
        updated_at: now(),
    };
    let mut jobs = load_all()?;
    jobs.push(job.clone());
    if jobs.len() > MAX_JOBS {
        let drop = jobs.len() - MAX_JOBS;
        jobs.drain(0..drop);
    }
    save_all(&jobs)?;
    Ok(job)
}

pub(crate) fn get(id: &str) -> Result<Option<MediaJob>, Box<dyn Error>> {
    let id = id.trim();
    Ok(load_all()?.into_iter().find(|job| job.id == id))
}

/// 覆盖保存一条任务（按 id 匹配）。
pub(crate) fn save(job: &MediaJob) -> Result<(), Box<dyn Error>> {
    let mut jobs = load_all()?;
    match jobs.iter_mut().find(|item| item.id == job.id) {
        Some(slot) => *slot = job.clone(),
        None => jobs.push(job.clone()),
    }
    save_all(&jobs)
}

/// 最近的媒体任务（新的在前）。
pub(crate) fn list(limit: usize) -> Result<Vec<MediaJob>, Box<dyn Error>> {
    let mut jobs = load_all()?;
    jobs.reverse();
    jobs.truncate(limit);
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_isolated_home<T>(run: impl FnOnce() -> T) -> T {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-media-jobs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let result = run();
        match previous {
            Some(value) => std::env::set_var("HIMIND_AGENT_HOME", value),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        }
        let _ = fs::remove_dir_all(&root);
        result
    }

    #[test]
    fn job_lifecycle_round_trips() {
        with_isolated_home(|| {
            let mut job = create(
                "video.generate",
                "ark-doubao",
                "ark.cn-beijing.volces.com",
                "ark-video",
                "doubao-seedance-2-5",
                "一只柯基在雪地奔跑",
                "F:\\\\tmp\\\\media",
            )
            .unwrap();
            assert!(job.id.starts_with("media_job_"));
            assert_eq!(job.status, STATUS_QUEUED);
            assert!(!job.terminal());

            job.provider_job_id = "cgt-123".to_string();
            job.status = STATUS_SUCCEEDED.to_string();
            job.outputs.push(MediaJobOutput {
                media_id: "media_1".to_string(),
                path: "F:\\\\tmp\\\\media\\\\a.mp4".to_string(),
                mime_type: "video/mp4".to_string(),
                size_bytes: 1024,
                duration_seconds: 5.0,
                width: 1920,
                height: 1080,
                cover_path: String::new(),
            });
            save(&job).unwrap();

            let reloaded = get(&job.id).unwrap().expect("job");
            assert!(reloaded.terminal());
            assert_eq!(reloaded.provider_job_id, "cgt-123");
            assert_eq!(reloaded.outputs.len(), 1);
            assert_eq!(list(10).unwrap().len(), 1);
        });
    }
}
