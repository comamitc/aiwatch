use std::{collections::BTreeSet, fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Grok,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Grok];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE",
            Self::Codex => "CODEX",
            Self::Grok => "GROK",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

impl FromStr for Provider {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "grok" => Ok(Self::Grok),
            _ => Err(format!("unknown provider '{value}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricSource {
    Provider,
    Local,
    Derived,
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub key: String,
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub source: MetricSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<u64>,
}

impl UsageWindow {
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        used_percent: f64,
        resets_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            used_percent: used_percent.clamp(0.0, 100.0),
            resets_at,
            source: MetricSource::Provider,
            history: Vec::new(),
        }
    }

    pub fn remaining_percent(&self) -> f64 {
        (100.0 - self.used_percent).clamp(0.0, 100.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailMetric {
    pub label: String,
    pub value: String,
    pub source: MetricSource,
}

impl DetailMetric {
    pub fn provider(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            source: MetricSource::Provider,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Stale,
    AuthenticationRequired,
    RateLimited,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchHealth {
    pub state: HealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

impl FetchHealth {
    pub fn ok() -> Self {
        Self {
            state: HealthState::Ok,
            message: None,
            status_code: Some(200),
        }
    }

    pub fn failure(
        state: HealthState,
        message: impl Into<String>,
        status_code: Option<u16>,
    ) -> Self {
        Self {
            state,
            message: Some(message.into()),
            status_code,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<DetailMetric>,
    pub health: FetchHealth,
    pub fetched_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<DateTime<Utc>>,
}

impl AccountSnapshot {
    pub fn empty(
        id: impl Into<String>,
        name: impl Into<String>,
        provider: Provider,
        health: FetchHealth,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            provider,
            plan: None,
            windows: Vec::new(),
            details: Vec::new(),
            health,
            fetched_at: Utc::now(),
            last_success_at: None,
        }
    }

    pub fn nearest_window(&self) -> Option<&UsageWindow> {
        self.windows
            .iter()
            .max_by(|left, right| left.used_percent.total_cmp(&right.used_percent))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub generated_at: DateTime<Utc>,
    pub accounts: Vec<AccountSnapshot>,
}

impl DashboardSnapshot {
    pub fn empty() -> Self {
        Self {
            generated_at: Utc::now(),
            accounts: Vec::new(),
        }
    }

    pub fn provider_count(&self) -> usize {
        self.accounts
            .iter()
            .map(|account| account.provider)
            .collect::<BTreeSet<_>>()
            .len()
    }

    pub fn nearest_limit(&self) -> Option<(&AccountSnapshot, &UsageWindow)> {
        self.accounts
            .iter()
            .flat_map(|account| account.windows.iter().map(move |window| (account, window)))
            .max_by(|(_, left), (_, right)| left.used_percent.total_cmp(&right.used_percent))
    }

    pub fn providers(&self) -> impl Iterator<Item = Provider> + '_ {
        Provider::ALL
            .into_iter()
            .filter(|provider| self.accounts.iter().any(|a| a.provider == *provider))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_limit_uses_the_most_consumed_window() {
        let mut first =
            AccountSnapshot::empty("claude:one", "one", Provider::Claude, FetchHealth::ok());
        first.windows.push(UsageWindow::new("5h", "5H", 91.0, None));
        let mut second =
            AccountSnapshot::empty("codex:two", "two", Provider::Codex, FetchHealth::ok());
        second
            .windows
            .push(UsageWindow::new("weekly", "WEEKLY", 73.0, None));
        let dashboard = DashboardSnapshot {
            generated_at: Utc::now(),
            accounts: vec![first, second],
        };

        let (account, window) = dashboard.nearest_limit().expect("nearest window");
        assert_eq!(account.name, "one");
        assert_eq!(window.used_percent, 91.0);
        assert_eq!(window.remaining_percent(), 9.0);
    }
}
