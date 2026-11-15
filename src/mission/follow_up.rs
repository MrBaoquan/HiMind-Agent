//! Native follow-up input only; assignment, permissions and budgets are host-owned.
use super::contract::{require_id, require_sha256, require_text, validate_task_paths, ExecutionBinding, MissionTaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum FollowUpSchemaVersion {
    #[serde(rename = "mission.follow_up.add.v1")]
    V1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionFollowUpAdd {
    pub schema_version: FollowUpSchemaVersion,
    pub mission_id: String,
    pub expected_plan_sha256: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_scope_extension: Option<ReadScopeExtension>,
    pub tasks: Vec<MissionFollowUpTask>,
}

/// Explicit human authorization, reviewed with the entire R3 request. This is
/// only a read delta for this batch, never a Mission/workspace/write grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadScopeExtension {
    pub paths: Vec<String>,
    pub authorized_by: String,
    pub reason: String,
}

impl ReadScopeExtension {
    fn validate(&self) -> Result<(), Box<dyn Error>> {
        require_text("read_scope_extension.authorized_by", &self.authorized_by, 160)?;
        require_text("read_scope_extension.reason", &self.reason, 2_000)?;
        if !(1..=128).contains(&self.paths.len()) {
            return Err("read_scope_extension.paths must contain 1..128 paths".into());
        }
        let mut seen = HashSet::new();
        for path in &self.paths {
            require_text("read_scope_extension.path", path, 1_024)?;
            // One portable spelling: no roots, traversal, redundant separators,
            // Windows devices/streams or trailing-dot/space aliases. Do not
            // normalize a caller's grant into a broader selector.
            if path != path.trim() || path.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
                || path.split('/').any(|part| {
                    let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
                    part.is_empty() || part == "." || part == ".." || part != part.trim()
                        || part.ends_with('.')
                        || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                        || ["COM", "LPT"].iter().any(|prefix|
                            device.strip_prefix(*prefix).map_or(false, |suffix|
                                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")))
                }) {
                return Err("read_scope_extension paths must be normalized workspace-relative selectors".into());
            }
            // Windows selectors are case insensitive, just like path_allowed.
            let key = if cfg!(windows) { path.to_ascii_lowercase() } else { path.clone() };
            if !seen.insert(key) { return Err("read_scope_extension paths must be unique".into()); }
        }
        Ok(())
    }
}

/// No execution field: even historical missions inherit the parent's binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MissionFollowUpTask {
    pub id: String,
    pub title: String,
    pub objective: String,
    pub repair_of: String,
    pub depends_on: Vec<String>,
    pub read_paths: Vec<String>,
    pub write_paths: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub required_checks: Vec<String>,
}

fn review_text(name: &str, value: &str, max: usize) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() || value.chars().count() > max
        || value.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err(format!("{name} must contain 1..{max} characters of review text").into());
    }
    Ok(())
}

impl MissionFollowUpTask {
    pub(crate) fn bind(&self, execution: ExecutionBinding) -> MissionTaskSpec {
        MissionTaskSpec {
            id: self.id.clone(), title: self.title.clone(), objective: self.objective.clone(),
            repair_of: self.repair_of.clone(), depends_on: self.depends_on.clone(),
            read_paths: self.read_paths.clone(), write_paths: self.write_paths.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(), required_checks: self.required_checks.clone(),
            execution,
        }
    }
}

impl MissionFollowUpAdd {
    pub(crate) fn validate(&self) -> Result<(), Box<dyn Error>> {
        require_id("mission_id", &self.mission_id)?;
        require_sha256(&self.expected_plan_sha256)?;
        review_text("reason", &self.reason, 2_000)?;
        if let Some(extension) = &self.read_scope_extension { extension.validate()?; }
        if !(1..=8).contains(&self.tasks.len()) {
            return Err("follow-up tasks must contain 1..8 tasks".into());
        }
        let mut ids = HashSet::new();
        let mut parents = HashSet::new();
        for task in &self.tasks {
            require_id("task.id", &task.id)?;
            require_id("task.repair_of", &task.repair_of)?;
            if !ids.insert(&task.id) || !parents.insert(&task.repair_of) {
                return Err("follow-up task ids and repair targets must be unique".into());
            }
            require_text("task.title", &task.title, 240)?;
            review_text("task.objective", &task.objective, 8_000)?;
            if task.depends_on.len() > 128 { return Err("depends_on exceeds 128 items".into()); }
            let mut dependencies = HashSet::new();
            for id in &task.depends_on {
                require_id("depends_on", id)?;
                if id == &task.id || !dependencies.insert(id) {
                    return Err("dependencies must be unique and cannot reference self".into());
                }
            }
            for paths in [&task.read_paths, &task.write_paths] {
                if paths.len() > 128 { return Err("task paths exceed 128 items".into()); }
                let mut seen = HashSet::new();
                for p in paths {
                    require_text("task.path", p, 1_024)?;
                    if !seen.insert(p) { return Err("task paths must be unique".into()); }
                }
            }
            for (label, values) in [("acceptance_criteria", &task.acceptance_criteria), ("required_checks", &task.required_checks)] {
                if !(1..=32).contains(&values.len()) { return Err(format!("{label} requires 1..32 items").into()); }
                for value in values { require_text(label, value, 2_000)?; }
            }
            for check in &task.required_checks { super::checks::validate_required_check(check)?; }
            // Reuse the authoritative path validator before any approval or DB open.
            let spec = task.bind(ExecutionBinding { expert: None, executor: String::new(),
                model: String::new(), role_id: String::new(), service_id: String::new() });
            validate_task_paths(&[spec], "")?;
        }
        // Same order of magnitude as the existing MCP frame gate. Bound review
        // receipts as well when this typed API is called by another adapter.
        if serde_json::to_vec(self)?.len() > 4 * 1024 * 1024 {
            return Err("follow-up compact JSON exceeds 4 MiB".into());
        }
        Ok(())
    }
}

pub(crate) fn input_schema() -> Value {
    let id = json!({"type":"string","minLength":1,"maxLength":160,"pattern":"^[A-Za-z0-9_.-]+$"});
    let paths = json!({"type":"array","maxItems":128,"uniqueItems":true,
        "items":{"type":"string","minLength":1,"maxLength":1024}});
    let criteria = json!({"type":"array","minItems":1,"maxItems":32,
        "items":{"type":"string","minLength":1,"maxLength":2000}});
    json!({"type":"object","additionalProperties":false,
        "description":"Complete request: compact UTF-8 JSON <= 4 MiB. R3 approval binds every field, including explicit human read_scope_extension authorization, inline or via a digest-bound full review JSON. Extension is read-only, only while waiting without running/awaiting_verification work; writes, execution, original checks and confirmation stay authoritative.",
        "x-maxCompactUtf8Bytes":4194304,
        "required":["schema_version","mission_id","expected_plan_sha256","reason","tasks"],
        "properties":{
            "schema_version":{"type":"string","enum":["mission.follow_up.add.v1"]},
            "mission_id":id,
            "expected_plan_sha256":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"},
            "reason":{"type":"string","minLength":1,"maxLength":2000},
            "read_scope_extension":{"type":["object","null"],"additionalProperties":false,
                "description":"Optional explicit user authorization. paths must equal the union of this batch's read_paths outside each repair parent's read scope: no unused grants or already granted selectors. Existing file/directory selectors only, every ancestor free of symlinks/reparse points and canonicalized inside the real workspace root. No write/execution/check override.",
                "required":["paths","authorized_by","reason"],
                "properties":{
                    "paths":{"type":"array","minItems":1,"maxItems":128,"uniqueItems":true,
                        "items":{"type":"string","minLength":1,"maxLength":1024,
                            "description":"Normalized workspace-relative selector using /; no dot/root/absolute/parent segments, empty segments, backslashes, controls, Windows devices/streams or trailing dot/space aliases. Exact selector matching, no implicit descendant grants."}},
                    "authorized_by":{"type":"string","minLength":1,"maxLength":160,
                        "description":"Nonblank human authority from existing explicit user permission; text alone does not bypass R3 approval. No controls."},
                    "reason":{"type":"string","minLength":1,"maxLength":2000,
                        "description":"Nonblank bounded explanation of the read-only authorization; no controls."}
                }},
            "tasks":{"type":"array","minItems":1,"maxItems":8,"items":{
                "type":"object","additionalProperties":false,
                "required":["id","title","objective","repair_of","depends_on","read_paths","write_paths","acceptance_criteria","required_checks"],
                "properties":{
                    "id":id,"repair_of":id,
                    "title":{"type":"string","minLength":1,"maxLength":240},
                    "objective":{"type":"string","minLength":1,"maxLength":8000},
                    "depends_on":{"type":"array","maxItems":128,"uniqueItems":true,"items":id},
                    "read_paths":paths,"write_paths":paths,
                    "acceptance_criteria":criteria,"required_checks":criteria
                }
            }}
        }})
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn request_value() -> Value {
        json!({"schema_version":"mission.follow_up.add.v1","mission_id":"mission_test",
            "expected_plan_sha256":"a".repeat(64),"reason":"Explicit user correction",
            "tasks":[{"id":"repair-6","title":"Correct delivery","objective":"Use the authorized runtime resources\nand retain real verification",
                "repair_of":"repair-5","depends_on":[],"read_paths":["src"],"write_paths":["src"],
                "acceptance_criteria":["Real delivery passes"],"required_checks":["cargo test"]}]})
    }

    #[test]
    fn mission_follow_up_contract_rejects_unknown_fields_versions_and_execution() {
        for field in ["outer", "task", "execution", "version", "missing"] {
            let mut value = request_value();
            match field {
                "outer" => value["approved_paths"] = json!(["."]),
                "task" => value["tasks"][0]["allow_all"] = json!(true),
                "execution" => value["tasks"][0]["execution"] = json!({"executor":"personal.codex"}),
                "version" => value["schema_version"] = json!("mission.follow_up.add.v2"),
                _ => { value["tasks"][0].as_object_mut().unwrap().remove("required_checks"); }
            }
            assert!(serde_json::from_value::<MissionFollowUpAdd>(value).is_err(), "{field}");
        }
        serde_json::from_value::<MissionFollowUpAdd>(request_value()).unwrap().validate().unwrap();
    }

    #[test]
    fn mission_follow_up_contract_accepts_relative_paths_with_empty_workspace_validator_argument() {
        for relative in [".", "src", "src/nested/file.rs", "src\\nested\\file.rs", "资料/任务.json"] {
            let mut value = request_value();
            value["tasks"][0]["read_paths"] = json!([relative]);
            value["tasks"][0]["write_paths"] = json!([relative]);
            serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().unwrap();
        }
    }

    #[test]
    fn mission_follow_up_contract_bounds_and_paths_are_authoritative() {
        for field in ["empty", "count", "id", "hash", "reason", "objective", "control", "paths", "path_len", "escape", "unc", "device", "root", "criteria", "check", "deps", "duplicate", "parent"] {
            let mut value = request_value();
            match field {
                "empty" => value["tasks"] = json!([]),
                "count" => value["tasks"] = json!(vec![value["tasks"][0].clone();9]),
                "id" => value["tasks"][0]["id"] = json!("invalid/task"),
                "hash" => value["expected_plan_sha256"] = json!("z".repeat(64)),
                "reason" => value["reason"] = json!("界".repeat(2001)),
                "objective" => value["tasks"][0]["objective"] = json!("界".repeat(8001)),
                "control" => value["tasks"][0]["objective"] = json!("bad\0input"),
                "paths" => value["tasks"][0]["read_paths"] = json!(vec!["src";129]),
                "path_len" => value["tasks"][0]["read_paths"] = json!(["x".repeat(1025)]),
                "escape" => value["tasks"][0]["read_paths"] = json!(["src/../secret"]),
                "unc" => value["tasks"][0]["read_paths"] = json!(["\\\\server\\share\\secret"]),
                "device" => value["tasks"][0]["read_paths"] = json!(["\\\\.\\C:\\secret"]),
                "root" => value["tasks"][0]["read_paths"] = json!(["C:/"]),
                "criteria" => value["tasks"][0]["acceptance_criteria"] = json!(vec!["x";33]),
                "check" => value["tasks"][0]["required_checks"] = json!(["cargo test | echo pass"]),
                "deps" => value["tasks"][0]["depends_on"] = json!(["same","same"]),
                "duplicate" => { let t=value["tasks"][0].clone(); value["tasks"].as_array_mut().unwrap().push(t); }
                _ => { let mut t=value["tasks"][0].clone(); t["id"]=json!("repair-other"); value["tasks"].as_array_mut().unwrap().push(t); }
            }
            assert!(serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().is_err(), "{field}");
        }
        let mut maximum = request_value();
        maximum["tasks"][0]["objective"] = json!("界".repeat(8000));
        maximum["reason"] = json!("界".repeat(2000));
        serde_json::from_value::<MissionFollowUpAdd>(maximum).unwrap().validate().unwrap();
    }
    #[test]
    fn mission_follow_up_read_extension_is_optional_and_does_not_change_old_receipts() {
        let original = request_value();
        let request: MissionFollowUpAdd = serde_json::from_value(original.clone()).unwrap();
        assert!(request.read_scope_extension.is_none());
        assert_eq!(serde_json::to_value(&request).unwrap(), original);
        let mut null = original.clone(); null["read_scope_extension"] = Value::Null;
        assert_eq!(serde_json::from_value::<MissionFollowUpAdd>(null).unwrap(), request);
        let mut explicit = original;
        explicit["read_scope_extension"] = json!({"paths":["runtime-profiles/config.json","contracts"],
            "authorized_by":"user:existing-explicit-permission", "reason":"Compile inputs for independent QA3"});
        let request: MissionFollowUpAdd = serde_json::from_value(explicit.clone()).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), explicit);
        for field in ["execution", "write_paths", "required_checks", "approved_paths"] {
            let mut invalid = explicit.clone(); invalid["read_scope_extension"][field] = json!(true);
            assert!(serde_json::from_value::<MissionFollowUpAdd>(invalid).is_err(), "{field}");
        }
    }

    #[test]
    fn mission_follow_up_read_extension_rejects_nonportable_or_unnormalized_selectors() {
        for path in ["", ".", "..", "/", "/etc/hosts", "C:/data", "C:data", "//server/share",
            "\\server\\share", "src\\input", "src//input", "src/./input", "src/../input", "src/",
            " src", "src ", "src/ hidden", "src/hidden ", "src/hidden.", "NUL", "src/COM1.txt",
            "src:stream", "src/*", "src/\0secret", "src/\nsecret", "src/\u{7f}secret"] {
            let mut value = request_value();
            value["read_scope_extension"] = json!({"paths":[path],"authorized_by":"user", "reason":"Read inputs"});
            assert!(serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().is_err(), "{path:?}");
        }
        let mut value = request_value();
        value["read_scope_extension"] = json!({"paths":["contracts","contracts"],"authorized_by":"user", "reason":"Read inputs"});
        assert!(serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().is_err());
        #[cfg(windows)] {
            let mut value = request_value();
            value["read_scope_extension"] = json!({"paths":["contracts","Contracts"],"authorized_by":"user", "reason":"Read inputs"});
            assert!(serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().is_err());
        }
    }

    #[test]
    fn mission_follow_up_read_extension_bounds_all_authorization_fields() {
        for field in ["empty", "count", "long_path", "by_blank", "by_long", "by_control", "reason_blank", "reason_long", "reason_control"] {
            let mut value = request_value();
            value["read_scope_extension"] = json!({"paths":["contracts"],"authorized_by":"user", "reason":"Read inputs"});
            let extension = &mut value["read_scope_extension"];
            match field {
                "empty" => extension["paths"] = json!([]),
                "count" => extension["paths"] = json!((0..129).map(|i| format!("inputs/{i}")).collect::<Vec<_>>()),
                "long_path" => extension["paths"] = json!(["x".repeat(1025)]),
                "by_blank" => extension["authorized_by"] = json!(" "),
                "by_long" => extension["authorized_by"] = json!("界".repeat(161)),
                "by_control" => extension["authorized_by"] = json!("user\nspoof"),
                "reason_blank" => extension["reason"] = json!(" "),
                "reason_long" => extension["reason"] = json!("界".repeat(2001)),
                _ => extension["reason"] = json!("reason\0spoof"),
            }
            assert!(serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().is_err(), "{field}");
        }
        let mut value = request_value();
        value["read_scope_extension"] = json!({"paths":(0..128).map(|i| format!("inputs/{i}")).collect::<Vec<_>>(),
            "authorized_by":"界".repeat(160),"reason":"界".repeat(2000)});
        serde_json::from_value::<MissionFollowUpAdd>(value).unwrap().validate().unwrap();
    }

}
