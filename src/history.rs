use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use chrono::{Days, Utc};
use rusqlite::{Connection, params};

use crate::model::DashboardSnapshot;

pub struct HistoryStore {
    connection: Connection,
}

impl HistoryStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("could not open history database at {}", path.display()))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS snapshots (
                 account_id TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 window_key TEXT NOT NULL,
                 used_percent REAL NOT NULL,
                 resets_at INTEGER,
                 observed_at INTEGER NOT NULL,
                 PRIMARY KEY (account_id, window_key, observed_at)
             );
             CREATE INDEX IF NOT EXISTS snapshots_recent
                 ON snapshots(account_id, window_key, observed_at);",
        )?;
        Ok(Self { connection })
    }

    pub fn record_and_hydrate(&mut self, snapshot: &mut DashboardSnapshot) -> Result<()> {
        let observed_at = snapshot.generated_at.timestamp();
        let transaction = self.connection.transaction()?;
        for account in &snapshot.accounts {
            if account.last_success_at != Some(account.fetched_at) {
                continue;
            }
            for window in &account.windows {
                transaction.execute(
                    "INSERT OR REPLACE INTO snapshots
                     (account_id, provider, window_key, used_percent, resets_at, observed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        account.id,
                        account.provider.key(),
                        window.key,
                        window.used_percent,
                        window.resets_at.map(|time| time.timestamp()),
                        observed_at,
                    ],
                )?;
            }
        }
        transaction.execute(
            "DELETE FROM snapshots WHERE observed_at < ?1",
            params![observed_at - 35 * 24 * 60 * 60],
        )?;
        transaction.commit()?;

        for account in &mut snapshot.accounts {
            for window in &mut account.windows {
                window.history = self.daily_peaks(&account.id, &window.key, 7)?;
            }
        }
        Ok(())
    }

    fn daily_peaks(&self, account_id: &str, window_key: &str, days: u64) -> Result<Vec<u64>> {
        let since = Utc::now()
            .date_naive()
            .checked_sub_days(Days::new(days.saturating_sub(1)))
            .expect("seven-day history range is valid");
        let mut statement = self.connection.prepare(
            "SELECT date(observed_at, 'unixepoch') AS day, MAX(used_percent)
             FROM snapshots
             WHERE account_id = ?1 AND window_key = ?2 AND observed_at >= ?3
             GROUP BY day
             ORDER BY day",
        )?;
        let rows = statement.query_map(
            params![
                account_id,
                window_key,
                since
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is valid")
                    .and_utc()
                    .timestamp()
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        )?;
        let mut peaks = BTreeMap::new();
        for row in rows {
            let (day, value) = row?;
            peaks.insert(day, value.clamp(0.0, 100.0).round() as u64);
        }

        Ok((0..days)
            .map(|offset| {
                let date = since
                    .checked_add_days(Days::new(offset))
                    .expect("seven-day history range is valid");
                peaks.get(&date.to_string()).copied().unwrap_or(0)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::model::{AccountSnapshot, DashboardSnapshot, FetchHealth, Provider, UsageWindow};

    use super::*;

    #[test]
    fn records_daily_peak_and_hydrates_seven_days() {
        let directory = tempdir().expect("temporary directory");
        let mut store = HistoryStore::open(&directory.path().join("history.sqlite3"))
            .expect("history database");
        let now = Utc::now();
        let mut account =
            AccountSnapshot::empty("claude:test", "test", Provider::Claude, FetchHealth::ok());
        account.fetched_at = now;
        account.last_success_at = Some(now);
        account
            .windows
            .push(UsageWindow::new("weekly", "WEEKLY", 42.0, None));
        let mut snapshot = DashboardSnapshot {
            generated_at: now,
            accounts: vec![account],
        };

        store
            .record_and_hydrate(&mut snapshot)
            .expect("record history");

        assert_eq!(snapshot.accounts[0].windows[0].history.len(), 7);
        assert_eq!(snapshot.accounts[0].windows[0].history[6], 42);
    }
}
