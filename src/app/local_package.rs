use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(crate) const CHECKSUMS_FILE: &str = "checksums.sha256";

/// 本地扩展源直接指向开发工作区，包体必须在安装时现场物化。依赖缓存与点前缀条目
/// （node_modules、.git、.idea、.vs 等）永远不属于扩展包，否则包体会随依赖缓存失控。
const PRUNED_COMPONENTS: [&str; 1] = ["node_modules"];

pub(crate) struct PackageLimits {
    pub(crate) max_files: usize,
    pub(crate) max_bytes: u64,
    pub(crate) label: &'static str,
}

fn is_pruned(relative: &str) -> bool {
    relative
        .split('/')
        .any(|component| component.starts_with('.') || PRUNED_COMPONENTS.contains(&component))
}

/// 把开发工作区目录物化成可安装的扩展包：按 `select` 收集文件复制到 `staging`，
/// 并生成规范 `checksums.sha256`（按小写路径排序，保证本地产出确定），返回纳入的相对路径。
/// 官方制品的清单行序并不固定（取决于打包机枚举顺序），因此清单只用于描述内容，
/// 判断「同一版本内容是否一致」必须比较解析后的映射。
pub(crate) fn stage_local_package(
    source: &Path,
    staging: &Path,
    limits: &PackageLimits,
    select: impl Fn(&str) -> bool,
) -> Result<Vec<String>, Box<dyn Error>> {
    if staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    fs::create_dir_all(staging)?;
    let mut entries: Vec<(String, PathBuf, u64)> = Vec::new();
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(source)?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == CHECKSUMS_FILE || is_pruned(&relative) || !select(&relative) {
            continue;
        }
        entries.push((relative, entry.path().to_path_buf(), entry.metadata()?.len()));
    }
    if entries.len() > limits.max_files {
        return Err(format!(
            "{}文件数量超过 {} 个限制",
            limits.label, limits.max_files
        )
        .into());
    }
    let total_bytes = entries
        .iter()
        .try_fold(0_u64, |sum, entry| sum.checked_add(entry.2))
        .ok_or("本地扩展包大小溢出")?;
    if total_bytes > limits.max_bytes {
        return Err(format!(
            "{}内容超过 {} MiB 限制",
            limits.label,
            limits.max_bytes / 1024 / 1024
        )
        .into());
    }
    entries.sort_by(|left, right| left.0.to_lowercase().cmp(&right.0.to_lowercase()));
    let mut manifest = String::new();
    for (relative, path, _) in &entries {
        let target = staging.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(path, &target)?;
        manifest.push_str(&format!(
            "{:x}  {relative}\n",
            Sha256::digest(fs::read(path)?)
        ));
    }
    fs::write(staging.join(CHECKSUMS_FILE), manifest)?;
    Ok(entries.into_iter().map(|entry| entry.0).collect())
}

/// 只纳入 Manifest 声明的文件，用于 Skill 这类自带内容清单的扩展。
pub(crate) fn select_declared(
    source: &Path,
    staging: &Path,
    limits: &PackageLimits,
    declared: &[String],
) -> Result<(), Box<dyn Error>> {
    let selected = declared
        .iter()
        .map(|value| value.replace('\\', "/"))
        .collect::<HashSet<_>>();
    let staged = stage_local_package(source, staging, limits, |relative| {
        selected.contains(relative)
    })?;
    let staged = staged.into_iter().collect::<HashSet<_>>();
    if let Some(missing) = declared
        .iter()
        .map(|value| value.replace('\\', "/"))
        .find(|value| !staged.contains(value))
    {
        return Err(format!("{}缺少 Manifest 声明的文件: {missing}", limits.label).into());
    }
    Ok(())
}

pub(crate) fn archive_directory(root: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    let mut archive = zip::ZipWriter::new(File::create(target)?);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        archive.start_file(relative, options)?;
        std::io::copy(&mut File::open(entry.path())?, &mut archive)?;
    }
    archive.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        source: PathBuf,
        staging: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let root = std::env::temp_dir().join(format!(
                "himind-local-package-test-{name}-{}",
                unique_suffix()
            ));
            let source = root.join("workspace");
            let staging = root.join("staging");
            fs::create_dir_all(&source).unwrap();
            Fixture { source, staging }
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.source.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn cleanup(&self) {
            let _ = fs::remove_dir_all(self.source.parent().unwrap());
        }
    }

    fn limits(label: &'static str) -> PackageLimits {
        PackageLimits {
            max_files: 100,
            max_bytes: 8 * 1024 * 1024,
            label,
        }
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default()
    }

    #[test]
    fn staging_prunes_dependency_and_vcs_directories() {
        let fixture = Fixture::new("prune");
        fixture.write("plugin.json", "{}");
        fixture.write("src/main.go", "package main");
        fixture.write("node_modules/dep/index.js", "module.exports = {}");
        fixture.write(".git/config", "[core]");
        let staged = stage_local_package(
            &fixture.source,
            &fixture.staging,
            &limits("本地插件"),
            |_| true,
        )
        .unwrap();
        assert_eq!(staged, vec!["plugin.json", "src/main.go"]);
        assert!(!fixture.staging.join("node_modules").exists());
        assert!(!fixture.staging.join(".git").exists());
        let manifest = fs::read_to_string(fixture.staging.join(CHECKSUMS_FILE)).unwrap();
        assert!(manifest.ends_with('\n'));
        assert_eq!(manifest.lines().count(), 2);
        fixture.cleanup();
    }

    #[test]
    fn staging_writes_checksums_sorted_by_lowercase_path() {
        let fixture = Fixture::new("order");
        fixture.write("skill.json", "{}");
        fixture.write("SKILL.md", "# skill");
        fixture.write("agents/openai.yaml", "name: skill");
        let staged = stage_local_package(
            &fixture.source,
            &fixture.staging,
            &limits("Skill 包"),
            |_| true,
        )
        .unwrap();
        assert_eq!(staged, vec!["agents/openai.yaml", "skill.json", "SKILL.md"]);
        let manifest = fs::read_to_string(fixture.staging.join(CHECKSUMS_FILE)).unwrap();
        let order = manifest
            .lines()
            .map(|line| line.split_once("  ").unwrap().1.to_string())
            .collect::<Vec<_>>();
        assert_eq!(order, vec!["agents/openai.yaml", "skill.json", "SKILL.md"]);
        fixture.cleanup();
    }

    #[test]
    fn select_declared_ignores_undeclared_files_and_reports_missing_ones() {
        let fixture = Fixture::new("declared");
        fixture.write("skill.json", "{}");
        fixture.write("SKILL.md", "# skill");
        fixture.write("dist/old.hmskill", "archive");
        let declared = vec!["skill.json".to_string(), "SKILL.md".to_string()];
        select_declared(
            &fixture.source,
            &fixture.staging,
            &limits("Skill 包"),
            &declared,
        )
        .unwrap();
        assert!(!fixture.staging.join("dist").exists());
        assert!(fixture.staging.join("skill.json").exists());

        let missing = vec!["skill.json".to_string(), "agents/openai.yaml".to_string()];
        let error = select_declared(
            &fixture.source,
            &fixture.staging,
            &limits("Skill 包"),
            &missing,
        )
        .unwrap_err();
        assert!(error.to_string().contains("agents/openai.yaml"));
        fixture.cleanup();
    }

    #[test]
    fn staging_rejects_oversized_workspaces() {
        let fixture = Fixture::new("oversized");
        fixture.write("plugin.json", "{}");
        fixture.write("src/main.go", "package main");
        let tiny = PackageLimits {
            max_files: 100,
            max_bytes: 1,
            label: "本地插件",
        };
        let error =
            stage_local_package(&fixture.source, &fixture.staging, &tiny, |_| true).unwrap_err();
        assert!(error.to_string().contains("本地插件内容超过 0 MiB 限制"));
        let few = PackageLimits {
            max_files: 1,
            max_bytes: 8 * 1024 * 1024,
            label: "本地插件",
        };
        let error =
            stage_local_package(&fixture.source, &fixture.staging, &few, |_| true).unwrap_err();
        assert!(error.to_string().contains("本地插件文件数量超过 1 个限制"));
        fixture.cleanup();
    }
}
