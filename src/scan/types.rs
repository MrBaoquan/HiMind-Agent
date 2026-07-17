use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathCandidate {
    pub root_type: String,
    pub path: String,
    pub name: String,
    pub engine_type: String,
    pub has_source_hint: bool,
    pub has_release_hint: bool,
}

#[derive(Debug, Deserialize)]
pub struct ScanTarget {
    pub exhibit_name: String,
    pub project_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ScanCacheEntry {
    pub root_type: String,
    pub root: String,
    pub max_depth: usize,
    pub target_key: String,
    pub root_mtime: u64,
    pub candidates: Vec<PathCandidate>,
}
