use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;
use zip::write::FileOptions;

pub(crate) struct ZipStats {
    pub included_files: usize,
    pub excluded_files: usize,
    pub included_bytes: u64,
}

pub(crate) struct PackageSnapshot {
    pub cache_key: String,
    pub included_files: usize,
    pub excluded_files: usize,
    pub included_bytes: u64,
}

pub(crate) fn zip_directories<F>(
    inputs: &[PathBuf],
    output: &Path,
    engine_type: &str,
    package_type: &str,
    mut progress: F,
) -> Result<ZipStats, Box<dyn Error>>
where
    F: FnMut(&ZipStats, &str) -> Result<(), Box<dyn Error>>,
{
    let file = File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut buffer = Vec::new();
    let mut stats = ZipStats {
        included_files: 0,
        excluded_files: 0,
        included_bytes: 0,
    };

    for (index, input) in inputs.iter().enumerate() {
        let prefix = package_root_name(input, index, inputs.len());
        for entry in WalkDir::new(input)
            .into_iter()
            .filter_entry(|entry| {
                let path = entry.path();
                if path == input {
                    return true;
                }
                match path.strip_prefix(input) {
                    Ok(relative_path) => {
                        let relative = relative_path.to_string_lossy().replace('\\', "/");
                        !should_skip_directory(path, &relative, engine_type, package_type)
                    }
                    Err(_) => true,
                }
            })
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path == input {
                continue;
            }
            let relative = path
                .strip_prefix(input)?
                .to_string_lossy()
                .replace('\\', "/");
            if should_exclude_file(path, &relative, package_type) {
                if path.is_file() {
                    stats.excluded_files += 1;
                }
                continue;
            }
            let archived_relative = if inputs.len() > 1 {
                format!("{}/{}", prefix, relative.trim_start_matches('/'))
            } else {
                relative
            };
            if path.is_dir() {
                zip.add_directory(
                    format!("{}/", archived_relative.trim_end_matches('/')),
                    options,
                )?;
                continue;
            }
            let mut source = File::open(path)?;
            buffer.clear();
            source.read_to_end(&mut buffer)?;
            zip.start_file(&archived_relative, options)?;
            zip.write_all(&buffer)?;
            stats.included_files += 1;
            stats.included_bytes += buffer.len() as u64;
            progress(&stats, &archived_relative)?;
        }
    }
    zip.finish()?;
    Ok(stats)
}

pub(crate) fn collect_package_snapshot(
    inputs: &[PathBuf],
    engine_type: &str,
    package_type: &str,
) -> Result<PackageSnapshot, Box<dyn Error>> {
    let mut hasher = DefaultHasher::new();
    package_type.hash(&mut hasher);
    for input in inputs {
        let normalized_input = input
            .canonicalize()
            .unwrap_or_else(|_| input.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        normalized_input.hash(&mut hasher);
    }
    let mut included_files = 0_usize;
    let mut excluded_files = 0_usize;
    let mut included_bytes = 0_u64;

    for input in inputs {
        for entry in WalkDir::new(input)
            .into_iter()
            .filter_entry(|entry| {
                let path = entry.path();
                if path == input {
                    return true;
                }
                match path.strip_prefix(input) {
                    Ok(relative_path) => {
                        let relative = relative_path.to_string_lossy().replace('\\', "/");
                        !should_skip_directory(path, &relative, engine_type, package_type)
                    }
                    Err(_) => true,
                }
            })
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path == input {
                continue;
            }
            let relative = path
                .strip_prefix(input)?
                .to_string_lossy()
                .replace('\\', "/");
            if should_exclude_file(path, &relative, package_type) {
                if path.is_file() {
                    excluded_files += 1;
                }
                continue;
            }
            if path.is_dir() {
                continue;
            }
            let metadata = entry.metadata()?;
            included_files += 1;
            included_bytes += metadata.len();
        }
    }

    Ok(PackageSnapshot {
        cache_key: format!("{:016x}", hasher.finish()),
        included_files,
        excluded_files,
        included_bytes,
    })
}

fn package_root_name(input: &Path, index: usize, total: usize) -> String {
    let base = input
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_file_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("folder-{}", index + 1));
    if total <= 1 {
        base
    } else {
        format!("{:02}-{}", index + 1, base)
    }
}

fn should_skip_directory(
    path: &Path,
    relative: &str,
    engine_type: &str,
    package_type: &str,
) -> bool {
    let relative_lower = relative.to_ascii_lowercase();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let common_dirs = [".git", ".svn", ".idea", "node_modules", ".vs"];
    if path.is_dir() && common_dirs.contains(&name.as_str()) {
        return true;
    }
    if package_type == "source" && engine_type == "Unity" && path.is_dir() {
        return [
            "library",
            "temp",
            "obj",
            "logs",
            "memorycaptures",
            "usersettings",
            "build",
            "builds",
        ]
        .contains(&name.as_str());
    }
    if package_type == "source" && engine_type == "Unreal" && path.is_dir() {
        return [
            "binaries",
            "deriveddatacache",
            "intermediate",
            "saved",
            "build",
        ]
        .contains(&name.as_str());
    }
    if package_type == "release" && path.is_dir() {
        return relative_lower.contains("saved/logs")
            || relative_lower.contains("saved/crashes")
            || name.contains("backupthisfolder")
            || name.contains("burstdebuginformation");
    }
    false
}

fn should_exclude_file(path: &Path, relative: &str, package_type: &str) -> bool {
    let relative_lower = relative.to_ascii_lowercase();
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if path.is_file() {
        if package_type == "source" {
            let excluded_archives = ["zip", "7z", "rar", "tar", "gz", "bz2", "xz"];
            if excluded_archives
                .iter()
                .any(|ext| name.ends_with(&format!(".{}", ext)))
            {
                return true;
            }
        }
        let excluded_exts = [
            "pdb", "ipch", "pch", "sdf", "obj", "tmp", "cache", "log", "dmp", "ilk", "suo",
            "opensdf", "vc.db", "csproj", "sln", "user", "mdb",
        ];
        if excluded_exts
            .iter()
            .any(|ext| name.ends_with(&format!(".{}", ext)))
        {
            return true;
        }
        if package_type == "release"
            && (relative_lower.contains("/logs/") || relative_lower.contains("/crashes/"))
        {
            return true;
        }
        return matches!(name.as_str(), "thumbs.db" | "desktop.ini");
    }
    false
}

pub(crate) fn sanitize_file_name(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|ch| {
            if ['\\', '/', ':', '*', '?', '"', '<', '>', '|'].contains(&ch) {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    if output.trim().is_empty() {
        output = "exhibit".to_string();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("himind-packaging-{}-{}", label, std::process::id()))
    }

    #[test]
    fn skips_common_vcs_and_dependency_directories() {
        let root = temp_root("common");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for name in [".git", ".svn", ".idea", "node_modules", ".vs"] {
            let dir = root.join(Path::new(name));
            fs::create_dir_all(dir.clone()).unwrap();
            assert!(
                should_skip_directory(
                    &dir,
                    name,
                    "Unity",
                    "source"
                ),
                "{} should be skipped for source package",
                name
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_unity_and_unreal_generated_directories() {
        let root = temp_root("engine");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for name in ["Library", "Temp", "Obj", "Logs", "Builds"] {
            let dir = root.join(Path::new(name));
            fs::create_dir_all(dir.clone()).unwrap();
            assert!(
                should_skip_directory(&dir, name, "Unity", "source"),
                "Unity {} should be skipped for source package",
                name
            );
        }
        for name in ["Binaries", "DerivedDataCache", "Intermediate", "Saved"] {
            let dir = root.join(Path::new(name));
            fs::create_dir_all(dir.clone()).unwrap();
            assert!(
                should_skip_directory(&dir, name, "Unreal", "source"),
                "Unreal {} should be skipped for source package",
                name
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keeps_regular_directories_for_other_engines() {
        let root = temp_root("keep");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root.join("Library")).unwrap();
        let library = root.join("Library");
        assert!(
            !should_skip_directory(&library, "Library", "Unreal", "source"),
            "Unity-only pruning must not apply to Unreal"
        );
        assert!(
            !should_skip_directory(&library, "Library", "Unity", "release"),
            "Unity pruning must not apply to release package"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_release_log_and_crash_content() {
        let root = temp_root("release");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root.join("saved/logs")).unwrap();
        let logs = root.join("saved").join("logs");
        assert!(should_skip_directory(&logs, "saved/logs", "Unity", "release"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excludes_archives_intermediates_and_system_files_from_source() {
        let root = temp_root("files");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for name in ["art.zip", "backup.7z", "bundled.tar.gz", "libs.rar", "data.tar", "x.bz2", "y.xz"] {
            let file = root.join(name);
            fs::write(&file, b"x").unwrap();
            assert!(
                should_exclude_file(&file, name, "source"),
                "{} should be excluded from source package",
                name
            );
        }
        for name in ["index.pdb", "cache.obj", "scene.log", "tmp.vc.db", "thumb.tmp", "run.dmp", "game.ilk", "proj.suo"] {
            let file = root.join(name);
            fs::write(&file, b"x").unwrap();
            assert!(
                should_exclude_file(&file, name, "source"),
                "{} should be excluded as an intermediate file",
                name
            );
        }
        let keep = ["main.cs", "scene.unity", "data.json", "texture.png", "config.asset"];
        for name in keep {
            let file = root.join(name);
            fs::write(&file, b"x").unwrap();
            assert!(
                !should_exclude_file(&file, name, "source"),
                "{} must be kept in source package",
                name
            );
        }
        let thumbs = root.join("Thumbs.db");
        fs::write(&thumbs, b"x").unwrap();
        assert!(should_exclude_file(&thumbs, "Thumbs.db", "source"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excludes_log_files_from_release_package() {
        let root = temp_root("relfiles");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let exe = root.join("game.exe");
        fs::write(&exe, b"x").unwrap();
        assert!(!should_exclude_file(&exe, "game.exe", "release"));
        let log = root.join("logs/server.log");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, b"x").unwrap();
        assert!(should_exclude_file(&log, "logs/server.log", "release"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sanitizes_forbidden_file_name_characters() {
        assert_eq!(sanitize_file_name("主屏/1:2"), "主屏_1_2");
        assert_eq!(sanitize_file_name("a*b?c"), "a_b_c");
        assert_eq!(sanitize_file_name("   "), "exhibit");
        assert_eq!(sanitize_file_name("正常名字"), "正常名字");
    }

    #[test]
    fn prefixes_multiple_input_roots_in_order() {
        let first = Path::new("D:\\proj");
        let second = Path::new("D:\\proj\\release");
        assert_eq!(package_root_name(first, 0, 1), "proj");
        assert_eq!(package_root_name(first, 0, 2), "01-proj");
        assert_eq!(package_root_name(second, 1, 2), "02-release");
    }

    #[test]
    fn snapshot_is_deterministic_and_sensitive_to_path() {
        let root = temp_root("snapshot");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/main.cs"), b"code").unwrap();
        fs::write(root.join("a/data.bin"), vec![0_u8; 128]).unwrap();
        fs::write(root.join("a/archive.zip"), b"junk").unwrap();

        let engine = "Unity";
        let first = collect_package_snapshot(&[root.join("a")], engine, "source").unwrap();
        let second = collect_package_snapshot(&[root.join("a")], engine, "source").unwrap();
        assert_eq!(first.cache_key, second.cache_key);
        assert_eq!(first.included_files, 2);
        assert_eq!(first.excluded_files, 1);
        assert_eq!(first.included_bytes, 4 + 128);

        let other = collect_package_snapshot(&[root.join("b".to_string())], engine, "source");
        assert!(other.is_err() || other.as_ref().map(|s| s.cache_key != first.cache_key).unwrap_or(true));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_directories_prunes_and_archives_expected_entries() {
        let root = temp_root("zip");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/Sub")).unwrap();
        fs::create_dir_all(root.join("src/Library")).unwrap();
        fs::write(root.join("src/main.cs"), b"hello").unwrap();
        fs::write(root.join("src/Sub/helper.cs"), b"helper").unwrap();
        fs::write(root.join("src/archive.zip"), b"z").unwrap();
        fs::write(root.join("src/Library/cache.bin"), b"cache").unwrap();

        let output = root.join("out.zip");
        let stats = zip_directories(
            &[root.join("src")],
            &output,
            "Unity",
            "source",
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(stats.included_files, 2, "main.cs and helper.cs, Library and archive.zip pruned");
        assert_eq!(stats.excluded_files, 1, "only archive.zip counts as excluded; Library is pruned before traversal");

        let mut archive = zip::ZipArchive::new(File::open(&output).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"main.cs".to_string()));
        assert!(names.contains(&"Sub/helper.cs".to_string()));
        assert!(!names.iter().any(|name| name.contains("Library") || name.contains("archive.zip")));
        let mut content = String::new();
        archive.by_name("main.cs").unwrap().read_to_string(&mut content).unwrap();
        assert_eq!(content, "hello");
        let _ = fs::remove_dir_all(&root);
    }
}
