use std::{collections::BTreeSet, fmt, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowClass {
    FiveHour,
    SevenDay,
    Monthly,
    Unknown,
}

impl WindowClass {
    pub fn duration(self) -> Option<Duration> {
        match self {
            Self::FiveHour => Some(Duration::hours(5)),
            Self::SevenDay => Some(Duration::days(7)),
            Self::Monthly | Self::Unknown => None,
        }
    }
}

pub fn classify_window(key: &str, label: &str) -> WindowClass {
    let key = key.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    if haystack_has_monthly(&key) || haystack_has_monthly(&label) {
        return WindowClass::Monthly;
    }
    if haystack_has_five_hour(&key) || haystack_has_five_hour(&label) {
        return WindowClass::FiveHour;
    }
    if haystack_has_seven_day(&key) || haystack_has_seven_day(&label) {
        return WindowClass::SevenDay;
    }
    WindowClass::Unknown
}

fn haystack_has_monthly(text: &str) -> bool {
    text.contains("monthly")
}

fn haystack_has_five_hour(text: &str) -> bool {
    text.contains("five_hour")
        || text.contains("five-hour")
        || text.contains("five hour")
        || tokens(text).any(|token| token == "5h")
}

fn haystack_has_seven_day(text: &str) -> bool {
    text.contains("weekly") || tokens(text).any(|token| token == "7d")
}

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
}

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

    pub fn class(&self) -> WindowClass {
        classify_window(&self.key, &self.label)
    }

    /// Linear elapsed fraction of this window: `100 * (1 - remaining_time / duration)`.
    ///
    /// Returns `None` when duration is unknown, reset time is missing, remaining time is
    /// not positive, or remaining time is longer than the window duration.
    pub fn pace_used_percent(&self, now: DateTime<Utc>) -> Option<f64> {
        let reset = self.resets_at?;
        let duration = self.class().duration()?;
        let remaining = reset - now;
        if remaining <= Duration::zero() || remaining > duration {
            return None;
        }
        let duration_ms = duration.num_milliseconds() as f64;
        if duration_ms <= 0.0 {
            return None;
        }
        Some(100.0 * (1.0 - remaining.num_milliseconds() as f64 / duration_ms))
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

    pub fn providers(&self) -> impl Iterator<Item = Provider> + '_ {
        Provider::ALL
            .into_iter()
            .filter(|provider| self.accounts.iter().any(|a| a.provider == *provider))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .expect("fixed now")
            .with_timezone(&Utc)
    }

    fn window_with_reset(key: &str, label: &str, used: f64, remaining: Duration) -> UsageWindow {
        UsageWindow::new(key, label, used, Some(fixed_now() + remaining))
    }

    #[test]
    fn remaining_percent_is_the_unused_share() {
        let window = UsageWindow::new("5h", "5H", 91.0, None);
        assert_eq!(window.remaining_percent(), 9.0);
    }

    #[test]
    fn classifies_five_hour_weekly_model_scoped_and_monthly_windows() {
        assert_eq!(
            UsageWindow::new("five_hour", "5H", 0.0, None).class(),
            WindowClass::FiveHour
        );
        assert_eq!(
            UsageWindow::new("5h", "5H", 0.0, None).class(),
            WindowClass::FiveHour
        );
        assert_eq!(
            UsageWindow::new("weekly", "WEEKLY", 0.0, None).class(),
            WindowClass::SevenDay
        );
        assert_eq!(
            UsageWindow::new("weekly_opus", "OPUS WEEKLY", 0.0, None).class(),
            WindowClass::SevenDay
        );
        assert_eq!(
            UsageWindow::new("7d", "7D", 0.0, None).class(),
            WindowClass::SevenDay
        );
        assert_eq!(
            UsageWindow::new("monthly", "MONTHLY", 0.0, None).class(),
            WindowClass::Monthly
        );
        assert_eq!(
            UsageWindow::new("custom", "X", 0.0, None).class(),
            WindowClass::Unknown
        );
    }

    #[test]
    fn monthly_windows_have_no_duration() {
        assert_eq!(WindowClass::Monthly.duration(), None);
        assert_eq!(WindowClass::Unknown.duration(), None);
        let monthly = window_with_reset("monthly", "MONTHLY", 12.0, Duration::days(10));
        assert!(monthly.pace_used_percent(fixed_now()).is_none());
        assert_ne!(monthly.class().duration(), Some(Duration::days(30)));
    }

    #[test]
    fn pace_is_elapsed_fraction_for_five_hour_and_weekly_windows() {
        let now = fixed_now();
        let five_hour = window_with_reset(
            "five_hour",
            "5H",
            40.0,
            Duration::hours(2) + Duration::minutes(30),
        );
        let weekly = window_with_reset("weekly", "WEEKLY", 10.0, Duration::days(7));
        let scoped = window_with_reset("weekly_opus", "OPUS WEEKLY", 10.0, Duration::days(7));
        let unknown = window_with_reset("custom", "X", 10.0, Duration::hours(1));
        let no_reset = UsageWindow::new("weekly", "WEEKLY", 10.0, None);

        let five_pace = five_hour.pace_used_percent(now).expect("5h pace");
        assert!((five_pace - 50.0).abs() < 0.5, "{five_pace}");

        let weekly_pace = weekly.pace_used_percent(now).expect("weekly pace");
        assert!(weekly_pace.abs() < 0.1, "{weekly_pace}");
        let scoped_pace = scoped.pace_used_percent(now).expect("scoped weekly pace");
        assert!(scoped_pace.abs() < 0.1, "{scoped_pace}");
        assert!(unknown.pace_used_percent(now).is_none());
        assert!(no_reset.pace_used_percent(now).is_none());
    }

    #[test]
    fn pace_is_none_when_reset_is_expired_or_beyond_duration() {
        let now = fixed_now();
        let expired = UsageWindow::new("5h", "5H", 12.0, Some(now - Duration::minutes(1)));
        let too_far = window_with_reset("five_hour", "5H", 12.0, Duration::hours(6));
        assert!(expired.pace_used_percent(now).is_none());
        assert!(too_far.pace_used_percent(now).is_none());
    }

    #[test]
    fn usage_window_json_omits_pace_duration_and_cap_fields() {
        let window = UsageWindow::new("weekly", "WEEKLY", 10.0, None);
        let value = serde_json::to_value(&window).expect("window json");
        let object = value.as_object().expect("object");
        assert!(object.contains_key("key"));
        assert!(object.contains_key("label"));
        assert!(object.contains_key("used_percent"));
        assert!(object.contains_key("resets_at"));
        assert!(object.contains_key("source"));
        assert!(!object.contains_key("pace"));
        assert!(!object.contains_key("pace_used_percent"));
        assert!(!object.contains_key("duration"));
        assert!(!object.contains_key("cap"));
        assert!(!object.contains_key("history"));
    }
}
