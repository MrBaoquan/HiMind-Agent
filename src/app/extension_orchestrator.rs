use crate::Options;
use std::error::Error;

pub(crate) fn reconcile(
    options: &Options,
    agent_id: &str,
    dashboard_generation: &mut String,
) -> Result<Vec<String>, Box<dyn Error>> {
    if options.mode().dashboard_enabled() {
        crate::app::extension_reconciler::reconcile(options, agent_id, dashboard_generation)?;
    } else {
        crate::app::extension_reconciler::release_control_plane_policies()?;
    }
    crate::app::extension_source::reconcile_auto_updates()
}
