use std::env;
use std::path::PathBuf;

const DEFAULT_AGENT_DIRECTORY: &str = "HiMindAgent";

/// Returns the persistent root for the current Agent profile.
///
/// `HIMIND_AGENT_HOME` is an explicit, complete root and therefore takes
/// precedence over the profile selector. The installed production Agent
/// keeps the historical default path; development profiles are nested below
/// `profiles/<name>` without changing production data.
pub(crate) fn agent_home() -> PathBuf {
    if let Some(explicit) = env::var_os("HIMIND_AGENT_HOME") {
        let path = PathBuf::from(explicit);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }

    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(DEFAULT_AGENT_DIRECTORY);
    let profile = env::var("HIMIND_AGENT_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| is_safe_profile_name(value));
    match profile {
        Some(profile) if profile != "production" && profile != "default" => {
            base.join("profiles").join(profile)
        }
        _ => base,
    }
}

pub(crate) fn profile_name() -> String {
    env::var("HIMIND_AGENT_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| is_safe_profile_name(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "production".to_string())
}

fn is_safe_profile_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::is_safe_profile_name;

    #[test]
    fn profile_names_are_path_safe() {
        assert!(is_safe_profile_name("development"));
        assert!(is_safe_profile_name("ecs-staging_01"));
        assert!(!is_safe_profile_name("../production"));
        assert!(!is_safe_profile_name("开发"));
        assert!(!is_safe_profile_name(""));
    }
}
