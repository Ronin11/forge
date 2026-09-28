//! Accounting shared by all runs of a recovered job.
use super::*;

impl Store {
    pub fn job_run(&self, id: i64) -> Result<i64> {
        Ok(self
            .lock()
            .retry_query_row("SELECT run FROM jobs WHERE id=?1", [id], |r| r.get(0))?)
    }

    pub fn job_step_cost(&self, id: i64) -> Result<f64> {
        Ok(self.lock().retry_query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM job_steps WHERE job_id=?1",
            [id],
            |r| r.get(0),
        )?)
    }
}
