//! Bounded metadata-only lookup cache. Body reads and project checks are never cached.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Key(PathBuf, String, String, String);
struct Entry {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    inserted: Instant,
}
static CACHE: OnceLock<Mutex<HashMap<Key, Entry>>> = OnceLock::new();
fn cache() -> &'static Mutex<HashMap<Key, Entry>> {
    CACHE.get_or_init(Default::default)
}
pub(super) fn key(root: &Path, source: &str, project: &str, native_id: &str) -> Key {
    Key(
        root.to_path_buf(),
        source.into(),
        project.into(),
        native_id.into(),
    )
}

pub(super) fn get(key: &Key) -> Option<PathBuf> {
    let mut cache = cache().lock().ok()?;
    let entry = cache.get(key)?;
    let canonical = std::fs::canonicalize(&entry.path).ok();
    let metadata = canonical
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok());
    let valid = entry.inserted.elapsed() < Duration::from_secs(5)
        && canonical
            .as_ref()
            .is_some_and(|path| path.starts_with(&key.0) && path == &entry.path)
        && metadata.is_some_and(|m| {
            m.is_file() && m.len() == entry.size && m.modified().ok() == entry.modified
        });
    if valid {
        Some(entry.path.clone())
    } else {
        cache.remove(key);
        None
    }
}

pub(super) fn remember(key: Key, path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    let modified = metadata.modified().ok();
    if cache
        .get(&key)
        .is_some_and(|entry| entry.modified > modified)
    {
        return;
    }
    if cache.len() >= 2048 {
        cache.retain(|_, entry| entry.inserted.elapsed() < Duration::from_secs(5));
    }
    if cache.len() >= 2048 {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, e)| e.inserted)
            .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        Entry {
            path: path.into(),
            size: metadata.len(),
            modified,
            inserted: Instant::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_history_path_cache_invalidates_on_signature_and_scope() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("history-path-cache-{:x}", rand::random::<u128>()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let path = root.join("s.jsonl");
        std::fs::write(&path, "before").unwrap();
        let key = key(&root, "claude_code", "p", "s");
        remember(key.clone(), &path);
        assert_eq!(get(&key), Some(path.clone()));
        assert_eq!(get(&super::key(&root, "codex", "p", "s")), None);
        assert_eq!(
            get(&super::key(&root, "claude_code", "other-project", "s")),
            None
        );
        std::fs::write(&path, "changed-size").unwrap();
        assert_eq!(get(&key), None);
        remember(key.clone(), &path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(get(&key), None);
        std::fs::remove_dir(&root).unwrap();
    }
}
