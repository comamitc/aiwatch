use std::{collections::BTreeMap, path::Path, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use futures::future::join_all;
use reqwest::Client;
use tokio::sync::{mpsc, watch};

use crate::{
    config::{AccountConfig, MIN_POLL_INTERVAL_SECS},
    history::HistoryStore,
    model::{AccountSnapshot, DashboardSnapshot, FetchHealth, HealthState},
    providers::{ProviderError, fetch_account},
};

pub struct PollCoordinator {
    client: Client,
    accounts: Vec<AccountConfig>,
    current: BTreeMap<String, AccountSnapshot>,
    history: Option<HistoryStore>,
}

impl PollCoordinator {
    pub fn new(accounts: Vec<AccountConfig>, history_path: Option<&Path>) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .context("could not initialize the HTTP client")?;
        let history = history_path.map(HistoryStore::open).transpose()?;
        Ok(Self {
            client,
            accounts,
            current: BTreeMap::new(),
            history,
        })
    }

    pub async fn poll_once(&mut self) -> DashboardSnapshot {
        let requests = self.accounts.iter().cloned().map(|account| {
            let client = self.client.clone();
            async move {
                let result = fetch_account(&client, &account).await;
                (account, result)
            }
        });

        for (account, result) in join_all(requests).await {
            let snapshot = match result {
                Ok(snapshot) => snapshot,
                Err(error) => self.failure_snapshot(&account, error),
            };
            self.current.insert(account.id.clone(), snapshot);
        }

        let mut snapshot = DashboardSnapshot {
            generated_at: Utc::now(),
            accounts: self.current.values().cloned().collect(),
        };
        snapshot.accounts.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then(left.name.cmp(&right.name))
        });
        if let Some(history) = self.history.as_mut() {
            if let Err(error) = history.record_and_hydrate(&mut snapshot) {
                for account in &mut snapshot.accounts {
                    account.details.push(crate::model::DetailMetric {
                        label: "history".into(),
                        value: format!("unavailable: {error}"),
                        source: crate::model::MetricSource::Local,
                    });
                }
            }
        }
        snapshot
    }

    pub async fn run(
        mut self,
        output: watch::Sender<DashboardSnapshot>,
        mut refresh: mpsc::Receiver<()>,
        poll_interval: Duration,
    ) {
        let poll_interval = poll_interval.max(Duration::from_secs(MIN_POLL_INTERVAL_SECS));
        let mut last_poll = None;

        loop {
            let now = tokio::time::Instant::now();
            let should_poll = last_poll
                .is_none_or(|last: tokio::time::Instant| now.duration_since(last) >= poll_interval);
            if should_poll {
                let snapshot = self.poll_once().await;
                last_poll = Some(tokio::time::Instant::now());
                if output.send(snapshot).is_err() {
                    return;
                }
            }

            let remaining = poll_interval.saturating_sub(
                last_poll
                    .map(|last| tokio::time::Instant::now().duration_since(last))
                    .unwrap_or_default(),
            );
            tokio::select! {
                _ = tokio::time::sleep(remaining) => {}
                message = refresh.recv() => {
                    if message.is_none() {
                        return;
                    }
                }
            }
        }
    }

    fn failure_snapshot(&self, account: &AccountConfig, error: ProviderError) -> AccountSnapshot {
        let health = FetchHealth::failure(
            error.health_state(),
            error.safe_message(),
            error.status_code(),
        );
        if let Some(previous) = self.current.get(&account.id) {
            let mut stale = previous.clone();
            stale.health = if matches!(health.state, HealthState::Error) {
                FetchHealth {
                    state: HealthState::Stale,
                    ..health
                }
            } else {
                health
            };
            stale.fetched_at = Utc::now();
            return stale;
        }
        AccountSnapshot::empty(
            account.id.clone(),
            account.name.clone(),
            account.provider,
            health,
        )
    }
}

pub fn loading_snapshot(accounts: &[AccountConfig]) -> DashboardSnapshot {
    DashboardSnapshot {
        generated_at: Utc::now(),
        accounts: accounts
            .iter()
            .map(|account| {
                AccountSnapshot::empty(
                    account.id.clone(),
                    account.name.clone(),
                    account.provider,
                    FetchHealth::failure(HealthState::Stale, "waiting for first poll", None),
                )
            })
            .collect(),
    }
}
