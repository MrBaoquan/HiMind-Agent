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
    Ok(Vec::new())
}

/// Reconcile user-managed GitHub extension sources independently of Dashboard.
///
/// A connected Agent may temporarily lose its Dashboard Worker credential while
/// local plugins, Skills, and DSH presets remain fully usable. Keeping this
/// path separate prevents a control-plane failure from blocking local source
/// refresh and preset installation.
pub(crate) fn reconcile_local_sources() -> Result<Vec<String>, Box<dyn Error>> {
    let mut updated = crate::app::extension_source::reconcile_auto_updates()?;
    match crate::runtime::builtin::interactive_home_path() {
        Ok(home) => updated.extend(crate::app::extension_source::reconcile_dsh_presets(&home)?),
        Err(error) => eprintln!("DSH preset reconcile skipped: {error}"),
    }
    Ok(updated)
}
