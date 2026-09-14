use std::sync::LazyLock;

use chrono::{Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    config::AccountConfig,
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{ProviderError, classify_status, detect_cli_version, read_secret_file, unix_timestamp};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const FALLBACK_VERSION: &str = "0.142.5";
const LOGIN_HINT: &str = "run `codex login`";

static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "codex_cli_rs/{} ({}; {})",
        detect_cli_version("codex", FALLBACK_VERSION),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
});

#[derive(Deserialize)]
struct AuthFile {
    #[serde(default)]
    tokens: Option<AuthTokens>,
}

#[derive(Deserialize)]
struct AuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

struct Auth {
    token: Zeroizing<String>,
    account_id: Zeroizing<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimit>,
    #[serde(default)]
    credits: Option<Credits>,
    #[serde(default)]
    rate_limit_reset_credits: Option<ResetCredits>,
    #[serde(default)]
    spend_control: Option<SpendControl>,
}

#[derive(Deserialize)]
struct RateLimit {
    #[serde(default)]
    primary_window: Option<RawWindow>,
    #[serde(default)]
    secondary_window: Option<RawWindow>,
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_after_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct Credits {
    #[serde(default)]
    balance: Option<serde_json::Value>,
    #[serde(default)]
    unlimited: Option<bool>,
}

#[derive(Deserialize)]
struct ResetCredits {
    #[serde(default)]
    available_count: Option<i64>,
}

#[derive(Deserialize)]
struct SpendControl {
    #[serde(default)]
    individual_limit: Option<f64>,
}

pub async fn fetch(
    client: &Client,
    account: &AccountConfig,
) -> Result<AccountSnapshot, ProviderError> {
    let auth = read_auth(account)?;
    let response = client
        .get(USAGE_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header("originator", "codex_cli_rs")
        .header("ChatGPT-Account-Id", auth.account_id.as_str())
        .bearer_auth(auth.token.as_str())
        .send()
        .await
        .map_err(|_| ProviderError::Network)?;
    classify_status(response.status(), LOGIN_HINT)?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    map_usage(account, &body)
}

fn read_auth(account: &AccountConfig) -> Result<Auth, ProviderError> {
    let body = read_secret_file(&account.credentials)?;
    let auth: AuthFile = serde_json::from_str(body.as_str()).map_err(|_| {
        ProviderError::Credentials("Codex credential file is not valid JSON".into())
    })?;
    let tokens = auth
        .tokens
        .ok_or_else(|| ProviderError::Credentials("Codex OAuth credentials are missing".into()))?;
    let token = tokens
        .access_token
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or(ProviderError::Authentication(LOGIN_HINT))?;
    let account_id = tokens
        .account_id
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ProviderError::Credentials("Codex account identifier is missing".into()))?;
    Ok(Auth { token, account_id })
}

fn map_usage(account: &AccountConfig, body: &str) -> Result<AccountSnapshot, ProviderError> {
    let raw: UsageResponse = serde_json::from_str(body).map_err(|_| ProviderError::Schema)?;
    let mut windows = Vec::new();
    if let Some(rate_limit) = raw.rate_limit {
        if let Some(window) = rate_limit.primary_window {
            windows.push(map_window("primary", window, 0));
        }
        if let Some(window) = rate_limit.secondary_window {
            windows.push(map_window("secondary", window, 1));
        }
    }
    if windows.is_empty() {
        return Err(ProviderError::Schema);
    }

    let mut details = Vec::new();
    if let Some(credits) = raw.credits {
        if credits.unlimited == Some(true) {
            details.push(DetailMetric::provider("credits", "unlimited"));
        } else if let Some(balance) = credits.balance.and_then(json_scalar) {
            details.push(DetailMetric::provider("credit balance", balance));
        }
    }
    if let Some(count) = raw
        .rate_limit_reset_credits
        .and_then(|credits| credits.available_count)
    {
        details.push(DetailMetric::provider("resets", count.to_string()));
    }
    if let Some(limit) = raw
        .spend_control
        .and_then(|control| control.individual_limit)
    {
        details.push(DetailMetric::provider(
            "spend limit",
            format!("${limit:.2}"),
        ));
    }

    let now = Utc::now();
    Ok(AccountSnapshot {
        id: account.id.clone(),
        name: account.name.clone(),
        provider: account.provider,
        plan: raw.plan_type,
        windows,
        details,
        health: FetchHealth::ok(),
        fetched_at: now,
        last_success_at: Some(now),
    })
}

fn map_window(fallback_key: &str, window: RawWindow, position: usize) -> UsageWindow {
    let (key, label) = match window.limit_window_seconds {
        Some(seconds) if seconds <= 6 * 60 * 60 => ("five_hour", "5H"),
        Some(seconds) if (6 * 24 * 60 * 60..=8 * 24 * 60 * 60).contains(&seconds) => {
            ("weekly", "WEEKLY")
        }
        _ if position == 0 => (fallback_key, "PRIMARY"),
        _ => (fallback_key, "SECONDARY"),
    };
    let resets_at = unix_timestamp(window.reset_at).or_else(|| {
        window
            .reset_after_seconds
            .map(|seconds| Utc::now() + Duration::seconds(seconds.max(0)))
    });
    UsageWindow::new(key, label, window.used_percent.unwrap_or(0.0), resets_at)
}

fn json_scalar(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Provider, providers::synthetic_account};

    #[test]
    fn maps_five_hour_weekly_and_credit_details() {
        let account = synthetic_account(Provider::Codex);
        let snapshot = map_usage(
            &account,
            include_str!("../../tests/fixtures/codex_usage.json"),
        )
        .expect("Codex fixture should map");

        assert_eq!(snapshot.plan.as_deref(), Some("plus"));
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].label, "5H");
        assert_eq!(snapshot.windows[1].label, "WEEKLY");
        assert_eq!(snapshot.windows[1].used_percent, 71.4);
        assert_eq!(snapshot.details[1].value, "2");
    }
}
