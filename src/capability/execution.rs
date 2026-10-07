use std::error::Error;

/// Shared lifecycle hooks for a Dashboard task executed through a Capability.
///
/// The capability implementation owns the operation; the task adapter only
/// supplies these hooks. This keeps progress/cancellation/approval semantics
/// consistent while legacy task payloads are migrated incrementally.
pub(crate) struct CapabilityExecutionContext<'a> {
    task_id: String,
    capability_id: String,
    workspace_scope: String,
    check_cancelled: Box<dyn FnMut() -> Result<(), Box<dyn Error>> + 'a>,
    report_progress: Box<dyn FnMut(i32, &str) -> Result<(), Box<dyn Error>> + 'a>,
    request_approval: Option<Box<dyn FnMut(&str, &str) -> Result<bool, Box<dyn Error>> + 'a>>,
}

impl<'a> CapabilityExecutionContext<'a> {
    pub(crate) fn detached(
        task_id: impl Into<String>,
        capability_id: impl Into<String>,
        workspace_scope: impl Into<String>,
    ) -> CapabilityExecutionContext<'static> {
        CapabilityExecutionContext::new(
            task_id,
            capability_id,
            workspace_scope,
            || Ok(()),
            |_, _| Ok(()),
        )
    }

    pub(crate) fn new(
        task_id: impl Into<String>,
        capability_id: impl Into<String>,
        workspace_scope: impl Into<String>,
        check_cancelled: impl FnMut() -> Result<(), Box<dyn Error>> + 'a,
        report_progress: impl FnMut(i32, &str) -> Result<(), Box<dyn Error>> + 'a,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            capability_id: capability_id.into(),
            workspace_scope: workspace_scope.into(),
            check_cancelled: Box::new(check_cancelled),
            report_progress: Box::new(report_progress),
            request_approval: None,
        }
    }

    pub(crate) fn with_approval(
        mut self,
        request_approval: impl FnMut(&str, &str) -> Result<bool, Box<dyn Error>> + 'a,
    ) -> Self {
        self.request_approval = Some(Box::new(request_approval));
        self
    }

    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    pub(crate) fn capability_id(&self) -> &str {
        &self.capability_id
    }

    pub(crate) fn workspace_scope(&self) -> &str {
        &self.workspace_scope
    }

    pub(crate) fn check_cancelled(&mut self) -> Result<(), Box<dyn Error>> {
        (self.check_cancelled)()
    }

    pub(crate) fn report_progress(
        &mut self,
        progress: i32,
        detail: &str,
    ) -> Result<(), Box<dyn Error>> {
        (self.report_progress)(progress, detail)
    }

    pub(crate) fn request_approval(
        &mut self,
        title: &str,
        description: &str,
    ) -> Result<bool, Box<dyn Error>> {
        self.request_approval
            .as_mut()
            .ok_or_else(|| Box::<dyn Error>::from("capability approval callback is unavailable"))?(
            title,
            description,
        )
    }
}
