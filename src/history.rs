use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Utc};
use rusqlite::{Connection, params};

use crate::model::{DashboardSnapshot, HealthState, UsageWindow};

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
            if account.windows.is_empty()
                && matches!(
                    account.health.state,
                    HealthState::Stale | HealthState::RateLimited | HealthState::Error
                )
                && let Some((windows, last_success_at)) = self.latest_windows(&account.id)?
            {
                account.windows = windows;
                account.last_success_at = Some(last_success_at);
                account.health.state = HealthState::Stale;
            }
            for window in &mut account.windows {
                window.history = self.daily_peaks(&account.id, &window.key, 7)?;
            }
        }
        Ok(())
    }

    fn latest_windows(
        &self,
        account_id: &str,
    ) -> Result<Option<(Vec<UsageWindow>, DateTime<Utc>)>> {
        let observed_at = self.connection.query_row(
            "SELECT MAX(observed_at) FROM snapshots WHERE account_id = ?1",
            params![account_id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let Some(observed_at) = observed_at else {
            return Ok(None);
        };
        let Some(last_success_at) = DateTime::from_timestamp(observed_at, 0) else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT window_key, used_percent, resets_at
             FROM snapshots
             WHERE account_id = ?1 AND observed_at = ?2
             ORDER BY window_key",
        )?;
        let rows = statement.query_map(params![account_id, observed_at], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?;
        let mut windows = Vec::new();
        for row in rows {
            let (key, used_percent, resets_at) = row?;
            let label = cached_window_label(&key);
            windows.push(UsageWindow::new(
                key,
                label,
                used_percent,
                resets_at.and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
            ));
        }
        Ok((!windows.is_empty()).then_some((windows, last_success_at)))
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

fn cached_window_label(key: &str) -> String {
    match key {
        "five_hour" => "5H".to_string(),
        "weekly" => "WEEKLY".to_string(),
        "monthly" => "MONTHLY".to_string(),
        "primary" => "PRIMARY".to_string(),
        "secondary" => "SECONDARY".to_string(),
        scoped if scoped.starts_with("weekly_") => {
            format!(
                "{} WEEKLY",
                scoped
                    .trim_start_matches("weekly_")
                    .replace('_', " ")
                    .to_uppercase()
            )
        }
        other => other.replace('_', " ").to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::model::{AccountSnapshot, FetchHealth, Provider, UsageWindow};

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

    #[test]
    fn restores_latest_windows_as_stale_during_rate_limit() {
        let directory = tempdir().expect("temporary directory");
        let mut store = HistoryStore::open(&directory.path().join("history.sqlite3"))
            .expect("history database");
        let now = Utc::now();
        let mut successful = AccountSnapshot::empty(
            "claude:managed:personal",
            "personal",
            Provider::Claude,
            FetchHealth::ok(),
        );
        successful.fetched_at = now;
        successful.last_success_at = Some(now);
        successful
            .windows
            .push(UsageWindow::new("weekly", "WEEKLY", 42.0, None));
        store
            .record_and_hydrate(&mut DashboardSnapshot {
                generated_at: now,
                accounts: vec![successful],
            })
            .expect("record successful snapshot");

        let mut limited = DashboardSnapshot {
            generated_at: now + chrono::Duration::minutes(1),
            accounts: vec![AccountSnapshot::empty(
                "claude:managed:personal",
                "personal",
                Provider::Claude,
                FetchHealth::failure(
                    HealthState::RateLimited,
                    "provider rate limited the usage request",
                    Some(429),
                ),
            )],
        };
        store
            .record_and_hydrate(&mut limited)
            .expect("restore cached snapshot");

        let account = &limited.accounts[0];
        assert_eq!(account.health.state, HealthState::Stale);
        assert_eq!(account.health.status_code, Some(429));
        assert_eq!(account.windows.len(), 1);
        assert_eq!(account.windows[0].label, "WEEKLY");
        assert_eq!(account.windows[0].used_percent, 42.0);
        assert_eq!(
            account.last_success_at.map(|time| time.timestamp()),
            Some(now.timestamp())
        );
    }
}
