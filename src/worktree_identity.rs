const AGENT_LOCAL_EXCLUDES: [&str; 3] = [
    ":(exclude).himind/checkpoints/**",
    ":(exclude).himind/handoffs/**",
    ":(exclude).himind/artifacts/**",
];

pub(crate) fn status_arguments() -> Vec<&'static str> {
    let mut arguments = vec![
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--",
        ".",
    ];
    arguments.extend(AGENT_LOCAL_EXCLUDES);
    arguments
}

pub(crate) fn stage_arguments() -> Vec<&'static str> {
    let mut arguments = vec!["add", "-A", "--", "."];
    arguments.extend(AGENT_LOCAL_EXCLUDES);
    arguments
}

#[cfg(test)]
mod tests {
    use super::{stage_arguments, status_arguments};

    #[test]
    fn agent_audit_metadata_is_excluded_from_source_identity() {
        let status = status_arguments();
        let stage = stage_arguments();
        for path in [
            ":(exclude).himind/checkpoints/**",
            ":(exclude).himind/handoffs/**",
            ":(exclude).himind/artifacts/**",
        ] {
            assert!(status.contains(&path));
            assert!(stage.contains(&path));
        }
    }
}
