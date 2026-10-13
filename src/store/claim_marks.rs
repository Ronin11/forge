//! What the claim writes on a task before its first attempt
//! (`admission`): the judgment band, the predicted size, and a workflow
//! rerouted on that size. None of these is gated on the task's state; the
//! worker that just claimed it owns it.

use super::*;

/// A workflow change at the claim (`Store::reroute_workflow`).
pub struct Reroute<'a> {
    pub workflow: &'a str,
    pub hash: &'a str,
    pub text: &'a str,
    pub tdd: bool,
    pub source: &'a str,
}

impl Store {
    /// Record the judgment band of a task (see `Task::band`).
    pub fn set_band(&self, id: i64, band: &str, p: f64) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE tasks SET band=?2, band_p=?3 WHERE id=?1",
            params![id, band, p],
        )?;
        Ok(())
    }

    /// Record the predicted size of a task (see `Task::predicted_size`).
    pub fn set_predicted_size(&self, id: i64, size: &str, p: f64) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE tasks SET predicted_size=?2, predicted_size_p=?3 WHERE id=?1",
            params![id, size, p],
        )?;
        Ok(())
    }

    /// Move a just-claimed task to another workflow (`admission::size`):
    /// the name, its pinned hash and text, whether it writes hidden
    /// tests, and the source the routing record shows. Any resolution
    /// recorded before is dropped so the run resolves the new workflow.
    pub fn reroute_workflow(&self, id: i64, r: &Reroute) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE tasks SET workflow=?2, workflow_hash=?3, workflow_text=?4, shape_tdd=?5,
                              workflow_source=?6, actions_json='' WHERE id=?1",
            params![id, r.workflow, r.hash, r.text, r.tdd as i64, r.source],
        )?;
        Ok(())
    }
}
