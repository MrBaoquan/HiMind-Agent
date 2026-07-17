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
