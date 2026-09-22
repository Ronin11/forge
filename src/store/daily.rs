//! `StatsDoc.daily` (`forge stats --json`'s kernel query, web UI task 5):
//! the record's own source for `/stats`'s 30-day chart of daily landings
//! and daily spend. Split out of `stats.rs` to keep that file under the
//! 1500-line bound `no_store_file_is_over_1500_lines` checks.

use super::*;

/// One day of `StatsDoc.daily`: a UTC date, how many tasks landed that
/// day, and how much every attempt that started that day cost. See
/// `Store::daily_stats`.
#[derive(Debug)]
pub struct DailyStat {
    pub date: String,
    pub landed: i64,
    pub cost_usd: f64,
}

/// How many days `Store::daily_stats` covers, today included.
pub const DAILY_WINDOW_DAYS: i64 = 30;

impl Store {
    /// The last `DAILY_WINDOW_DAYS` UTC dates (today included), each with
    /// how many of this scope's tasks landed that day and how much every
    /// attempt on one of this scope's tasks that started that day cost —
    /// `StatsDoc.daily`, the source of `/stats`'s 30-day chart. Always
    /// exactly `DAILY_WINDOW_DAYS` rows, oldest first, zeroed on a day
    /// with no activity: a `WITH RECURSIVE` calendar generates the dates
    /// so a quiet scope still draws a flat 30-point line instead of a gap.
    pub fn daily_stats(&self, scope: &StatsFilter) -> Result<Vec<DailyStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "WITH RECURSIVE days(d) AS (
                SELECT date('now', ?3)
                UNION ALL
                SELECT date(d, '+1 day') FROM days WHERE d < date('now')
             )
             SELECT days.d AS date,
                COALESCE(l.landed, 0) AS landed,
                COALESCE(c.cost_usd, 0) AS cost_usd
             FROM days
             LEFT JOIN (
                SELECT date(landed_at, 'unixepoch') AS d, COUNT(*) AS landed
                FROM tasks
                WHERE landed_sha != '' AND landed_at IS NOT NULL
                  AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)
                GROUP BY d
             ) l ON l.d = days.d
             LEFT JOIN (
                SELECT date(a.started_at, 'unixepoch') AS d, SUM(a.cost_usd) AS cost_usd
                FROM attempts a JOIN tasks t ON t.id = a.task_id
                WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
                GROUP BY d
             ) c ON c.d = days.d
             ORDER BY days.d",
        )?;
        let offset = format!("-{} days", DAILY_WINDOW_DAYS - 1);
        let rows = stmt.query_map(params![scope.project, scope.initiative, offset], |r| {
            Ok(DailyStat {
                date: r.get("date")?,
                landed: r.get("landed")?,
                cost_usd: r.get("cost_usd")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Store::daily_stats` always returns exactly `DAILY_WINDOW_DAYS`
    /// rows, oldest date first, zero-filled on a day with no activity — a
    /// task that lands today and an attempt that costs money today land
    /// in the last row, and every earlier row this fresh store never
    /// touched stays zero.
    #[test]
    fn daily_stats_covers_the_last_30_days_zero_filled_with_todays_activity_last() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let now = crate::unix_now();
        let now_dt = chrono::DateTime::from_timestamp(now, 0).unwrap();
        let today = format!(
            "{:04}-{:02}-{:02}",
            chrono::Datelike::year(&now_dt),
            chrono::Datelike::month(&now_dt),
            chrono::Datelike::day(&now_dt)
        );

        let mut t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: now,
            started_at: Some(now),
            finished_at: Some(now),
            workflow: "direct".into(),
            landed_sha: "sha".into(),
            landed_at: Some(now),
            ..Default::default()
        };
        t.id = s.insert_task(&t).unwrap();
        s.update_task(&t).unwrap();

        let a = s
            .insert_attempt(&Attempt {
                task_id: t.id,
                attempt_no: 1,
                step: "code".into(),
                provider: "anthropic".into(),
                started_at: now,
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&FinishAttempt {
            id: a,
            state: AttemptState::Succeeded,
            reason: String::new(),
            finished_at: Some(now),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 4,
            tool_calls: 1,
            cost_usd: Some(2.5),
            agent_ms: 1000,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: "[]".into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: None,
            rl_seven_day: None,
            rl_five_hour_resets: None,
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        })
        .unwrap();

        let daily = s.daily_stats(&StatsFilter::default()).unwrap();
        assert_eq!(daily.len(), DAILY_WINDOW_DAYS as usize, "{daily:?}");
        assert!(
            daily.windows(2).all(|w| w[0].date < w[1].date),
            "oldest first: {:?}",
            daily.iter().map(|d| &d.date).collect::<Vec<_>>()
        );
        let last = daily.last().unwrap();
        assert_eq!(last.date, today);
        assert_eq!(last.landed, 1);
        assert_eq!(last.cost_usd, 2.5);
        assert!(
            daily[..daily.len() - 1]
                .iter()
                .all(|d| d.landed == 0 && d.cost_usd == 0.0),
            "{daily:?}"
        );
    }
}
