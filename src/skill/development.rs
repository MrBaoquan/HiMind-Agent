//! 免安装直挂的 Skill 开发注册表。
//!
//! 与 `capability::plugin` 的开发插件注册表对称：本地开发工作区构建出的
//! Skill 候选包在这里登记源码/候选目录后立即生效，不需要先安装到
//! managed/user 作用域。这样「构建 → 本机生效 → 测试 → 提交」与插件保持
//! 同一条闭环，本地开发工作区始终是本机最新权威。

use crate::skill::manifest::{load_skill_manifest, validate_skill_id};
use crate::skill::types::SkillRecord;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DevelopmentSkill {
    id: String,
    path: String,
}

pub(crate) fn registry_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HIMIND_SKILL_DEVELOPMENT_REGISTRY") {
        return PathBuf::from(path);
    }
    fallback_registry_path()
}

// 测试进程并行运行，默认注册表必须落在线程私有的临时文件里：测试用例跑在
// 各自线程上，共享同一份注册表会让 authoring 测试登记的直挂 Skill 污染其它
// 测试。MCP registry generation 会把直挂 Skill 计入投影哈希，一旦被污染就会
// 在并行执行中途触发 generation 变化并清空已激活能力。
#[cfg(test)]
fn fallback_registry_path() -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};

    thread_local! {
        static PATH: PathBuf = {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            std::env::temp_dir().join(format!(
                "himind-skill-development-{}-{}.json",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        };
    }
    PATH.with(PathBuf::clone)
}

#[cfg(not(test))]
fn fallback_registry_path() -> PathBuf {
    crate::store::paths::agent_home().join("skill-development.json")
}

pub(crate) fn register_skill(path: &Path) -> Result<String, Box<dyn Error>> {
    register_skill_at(path, &registry_path())
}

fn register_skill_at(path: &Path, registry: &Path) -> Result<String, Box<dyn Error>> {
    let root = path.canonicalize()?;
    let manifest = load_skill_manifest(&root)?;
    validate_skill_id(&manifest.id)?;
    // 同一个 Agent 可能同时服务多个工作区会话，每个会话都在登记自己的开发
    // Skill。读改写必须整体串行化，否则先登记的会话会被后登记的整表覆盖。
    let _lock = crate::store::atomic_file::lock(registry)?;
    let mut entries = entries_at(registry);
    entries.retain(|entry| entry.id != manifest.id);
    entries.push(DevelopmentSkill {
        id: manifest.id.clone(),
        path: root.to_string_lossy().to_string(),
    });
    write_entries_at(registry, &entries)?;
    Ok(manifest.id)
}

pub(crate) fn unregister_skill(skill_id: &str) -> Result<(), Box<dyn Error>> {
    unregister_skill_at(skill_id, &registry_path())
}

fn unregister_skill_at(skill_id: &str, registry: &Path) -> Result<(), Box<dyn Error>> {
    let _lock = crate::store::atomic_file::lock(registry)?;
    let mut entries = entries_at(registry);
    entries.retain(|entry| entry.id != skill_id);
    write_entries_at(registry, &entries)
}

/// 已登记的开发 Skill：`(skill_id, 源码或候选包目录)`。
pub(crate) fn entries() -> Vec<(String, PathBuf)> {
    entries_at(&registry_path())
        .into_iter()
        .map(|entry| (entry.id, PathBuf::from(entry.path)))
        .collect()
}

/// 开发直挂的 Skill 记录。目录已失效时静默跳过，避免残留登记项阻断整个列表。
pub(crate) fn records() -> Vec<SkillRecord> {
    entries()
        .into_iter()
        .filter_map(|(_, path)| record_at(&path))
        .collect()
}

pub(crate) fn record(skill_id: &str) -> Option<SkillRecord> {
    entries()
        .into_iter()
        .find(|(id, _)| id == skill_id)
        .and_then(|(_, path)| record_at(&path))
}

pub(crate) fn is_development_skill(skill_id: &str) -> bool {
    entries().iter().any(|(id, _)| id == skill_id)
}

fn record_at(root: &Path) -> Option<SkillRecord> {
    if !root.is_dir() {
        return None;
    }
    let manifest = load_skill_manifest(root).ok()?;
    Some(SkillRecord {
        manifest,
        root: root.to_path_buf(),
        version_root: root.to_path_buf(),
        current: true,
        previous_version: None,
    })
}

fn entries_at(path: &Path) -> Vec<DevelopmentSkill> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn write_entries_at(path: &Path, entries: &[DevelopmentSkill]) -> Result<(), Box<dyn Error>> {
    // 换用原子替换：既让并发读永远看到完整内容，也避免多个会话共用同一个
    // `.json.tmp` 互相截断。调用方负责持有注册表锁。
    crate::store::atomic_file::atomic_write(path, &serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, id: &str, version: &str) {
        fs::create_dir_all(root).unwrap();
        let manifest = crate::skill::types::SkillManifest {
            id: id.to_string(),
            name: "开发直挂测试".to_string(),
            author: "马宝全".to_string(),
            categories: vec!["开发工具".to_string()],
            version: version.to_string(),
            scope: crate::skill::types::SkillScope::User,
            description: "development mount test".to_string(),
            release_notes: String::new(),
            min_agent_version: String::new(),
            supported_clients: vec!["codex".to_string()],
            capabilities: Vec::new(),
            plugin_dependencies: Vec::new(),
            risk_summary: "read_only".to_string(),
            contents: vec!["skill.json".to_string(), "SKILL.md".to_string()],
        };
        fs::write(
            root.join("skill.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(root.join("SKILL.md"), "# demo").unwrap();
    }

    #[test]
    fn registers_replaces_and_unregisters_development_skill() {
        let base = std::env::temp_dir().join(format!(
            "himind-skill-development-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let registry = base.join("skill-development.json");
        let source = base.join("workspace").join("skills").join("demo");
        write_skill(&source, "com.himind.skill.demo", "0.1.0");

        let id = register_skill_at(&source, &registry).unwrap();
        assert_eq!(id, "com.himind.skill.demo");
        assert_eq!(entries_at(&registry).len(), 1);

        // 同名重复登记必须覆盖而不是追加，否则列表会出现重复项。
        register_skill_at(&source, &registry).unwrap();
        assert_eq!(entries_at(&registry).len(), 1);

        unregister_skill_at("com.himind.skill.demo", &registry).unwrap();
        assert!(entries_at(&registry).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn skips_registry_entries_whose_directory_disappeared() {
        let base = std::env::temp_dir().join(format!(
            "himind-skill-development-stale-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let registry = base.join("skill-development.json");
        let source = base.join("demo");
        write_skill(&source, "com.himind.skill.stale", "0.1.0");
        register_skill_at(&source, &registry).unwrap();
        fs::remove_dir_all(&source).unwrap();

        let entries = entries_at(&registry);
        assert_eq!(entries.len(), 1);
        assert!(record_at(Path::new(&entries[0].path)).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    /// 同一个 Agent 会被多个工作区会话共用。多个会话各自登记开发 Skill 时，
    /// 读改写整体串行化后必须一条不丢；共用 `.json.tmp` 的旧写法会互相截断。
    #[test]
    fn concurrent_development_skill_registrations_lose_nothing() {
        let base = std::env::temp_dir().join(format!(
            "himind-skill-development-concurrent-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let registry = base.join("skill-development.json");
        let sources = (0..6)
            .map(|index| {
                let source = base
                    .join(format!("workspace-{index}"))
                    .join("skills")
                    .join(format!("demo-{index}"));
                write_skill(
                    &source,
                    &format!("com.himind.skill.concurrent.{index}"),
                    "0.1.0",
                );
                source
            })
            .collect::<Vec<_>>();

        let barrier = std::sync::Barrier::new(sources.len());
        std::thread::scope(|scope| {
            for source in &sources {
                let registry = registry.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    register_skill_at(source, &registry).unwrap();
                });
            }
        });

        let mut ids = entries_at(&registry)
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(
            ids,
            (0..6)
                .map(|index| format!("com.himind.skill.concurrent.{index}"))
                .collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(&base);
    }
}
