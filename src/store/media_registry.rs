//! 本机生成媒体登记表。
//!
//! 生成/编辑出来的图片、视频登记为稳定的 `media_id`，会话里可以直接用
//! `media:<id>` 引用它继续做图生图 / 图生视频，而不必让模型搬运长路径或
//! 大段 base64。登记表只保存本机元数据，不含凭据。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const STORE_FILE: &str = "media-registry.json";
/// 只保留最近的条目，避免登记表无限增长；文件本身仍然在磁盘上。
const MAX_ENTRIES: usize = 500;
const ID_PREFIX: &str = "media_";
/// 会话里引用登记媒体的前缀。
pub(crate) const REFERENCE_PREFIX: &str = "media:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct MediaRecord {
    pub id: String,
    /// 绝对路径；文件可能已被用户删除，取用时需要重新校验存在性。
    pub path: String,
    /// `image` / `video` / `audio`。
    pub kind: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub duration_seconds: f64,
    /// 产出它的能力，例如 `image.generate` / `image.edit`。
    #[serde(default)]
    pub capability: String,
    #[serde(default)]
    pub service_id: String,
    /// 截断后的提示词，便于用户回看"这张图是怎么来的"。
    #[serde(default)]
    pub prompt: String,
    pub created_at: String,
}

pub(crate) struct NewMedia<'a> {
    pub path: &'a str,
    pub kind: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: u64,
    pub sha256: &'a str,
    pub width: u32,
    pub height: u32,
    pub duration_seconds: f64,
    pub capability: &'a str,
    pub service_id: &'a str,
    pub prompt: &'a str,
}

fn store_path() -> Result<PathBuf, Box<dyn Error>> {
    let dir = crate::store::paths::agent_home();
    fs::create_dir_all(&dir)?;
    Ok(dir.join(STORE_FILE))
}

fn load_all() -> Result<Vec<MediaRecord>, Box<dyn Error>> {
    let path = store_path()?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let records: Vec<MediaRecord> = serde_json::from_slice(&fs::read(path)?)?;
    Ok(records)
}

fn save_all(records: &[MediaRecord]) -> Result<(), Box<dyn Error>> {
    let path = store_path()?;
    crate::store::atomic_file::atomic_write(&path, &serde_json::to_vec_pretty(records)?)?;
    Ok(())
}

fn prompt_digest(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut digest: String = trimmed.chars().take(200).collect();
    if trimmed.chars().count() > 200 {
        digest.push('…');
    }
    digest
}

/// 按内容哈希查已登记媒体。空哈希不参与匹配。
pub(crate) fn find_by_sha256(sha256: &str) -> Result<Option<MediaRecord>, Box<dyn Error>> {
    let sha256 = sha256.trim();
    if sha256.is_empty() {
        return Ok(None);
    }
    Ok(load_all()?
        .into_iter()
        .find(|item| item.sha256.eq_ignore_ascii_case(sha256)))
}

/// 登记一条本机生成的媒体，返回带 `media_id` 的记录。
///
/// 有 SHA-256 时按内容去重：同一张原图被多个工具或会话重复带入时，
/// 只保留一条登记和一份原文件，避免媒体历史继续放大。
pub(crate) fn register(item: NewMedia<'_>) -> Result<MediaRecord, Box<dyn Error>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    let mut random = [0u8; 6];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let record = MediaRecord {
        id: format!("{ID_PREFIX}{now}_{suffix}"),
        path: item.path.to_string(),
        kind: item.kind.to_string(),
        mime_type: item.mime_type.to_string(),
        size_bytes: item.size_bytes,
        sha256: item.sha256.to_string(),
        width: item.width,
        height: item.height,
        duration_seconds: item.duration_seconds,
        capability: item.capability.to_string(),
        service_id: item.service_id.to_string(),
        prompt: prompt_digest(item.prompt),
        created_at: now.to_string(),
    };
    let mut records = load_all()?;
    if !record.sha256.is_empty() {
        if let Some(existing) = records
            .iter()
            .find(|item| item.sha256.eq_ignore_ascii_case(&record.sha256))
            .cloned()
        {
            if std::path::Path::new(&existing.path).is_file() {
                return Ok(existing);
            }
        }
    }
    records.push(record.clone());
    if records.len() > MAX_ENTRIES {
        let drop = records.len() - MAX_ENTRIES;
        records.drain(0..drop);
    }
    save_all(&records)?;
    Ok(record)
}

/// 按 `media_id` 查记录。
pub(crate) fn get(id: &str) -> Result<Option<MediaRecord>, Box<dyn Error>> {
    let id = id.trim();
    Ok(load_all()?.into_iter().find(|item| item.id == id))
}

/// 解析 `media:<id>` / 裸 `media_<id>` 形式的引用。
pub(crate) fn resolve_reference(value: &str) -> Result<Option<MediaRecord>, Box<dyn Error>> {
    let value = value.trim();
    if let Some(id) = value
        .strip_prefix(REFERENCE_PREFIX)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return get(id);
    }
    // 也接受裸 `media_xxx`：模型常常直接复制返回值里的 media_id。
    if value.starts_with(ID_PREFIX) {
        return get(value);
    }
    Ok(None)
}

/// 最近的登记条目（新的在前），供界面/诊断列出"本机生成过什么"。
pub(crate) fn list(limit: usize) -> Result<Vec<MediaRecord>, Box<dyn Error>> {
    let mut records = load_all()?;
    records.reverse();
    records.truncate(limit);
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_isolated_home<T>(run: impl FnOnce() -> T) -> T {
        let _guard = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "himind-media-registry-{}-{}",
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
    fn registration_deduplicates_existing_content_hash() {
        with_isolated_home(|| {
            let root = crate::store::paths::agent_home();
            let path = root.join("same.png");
            std::fs::write(&path, b"same").unwrap();
            let first = register(NewMedia {
                path: &path.to_string_lossy(),
                kind: "image",
                mime_type: "image/png",
                size_bytes: 4,
                sha256: "same-hash",
                width: 1,
                height: 1,
                duration_seconds: 0.0,
                capability: "image.generate",
                service_id: "test",
                prompt: "",
            })
            .unwrap();
            let second = register(NewMedia {
                path: &path.to_string_lossy(),
                kind: "image",
                mime_type: "image/png",
                size_bytes: 4,
                sha256: "same-hash",
                width: 1,
                height: 1,
                duration_seconds: 0.0,
                capability: "image.generate",
                service_id: "test",
                prompt: "",
            })
            .unwrap();
            assert_eq!(first.id, second.id);
        });
    }
    #[test]
    fn registered_media_can_be_resolved_by_reference() {
        with_isolated_home(|| {
            let record = register(NewMedia {
                path: "F:\\tmp\\a.png",
                kind: "image",
                mime_type: "image/png",
                size_bytes: 12,
                sha256: "abc",
                width: 8,
                height: 6,
                duration_seconds: 0.0,
                capability: "image.generate",
                service_id: "ark-doubao",
                prompt: "一只猫",
            })
            .unwrap();
            assert!(record.id.starts_with("media_"));

            let by_reference = resolve_reference(&format!("media:{}", record.id)).unwrap();
            assert_eq!(by_reference.map(|item| item.id), Some(record.id.clone()));
            // 裸 id 也能解析
            assert!(resolve_reference(&record.id).unwrap().is_some());
            // 普通路径不会被误当成引用
            assert!(resolve_reference("F:\\tmp\\a.png").unwrap().is_none());

            let listed = list(10).unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].capability, "image.generate");
        });
    }
}
