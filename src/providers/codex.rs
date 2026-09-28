use std::sync::LazyLock;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    config::AccountConfig,
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{
    ProviderError, classify_status, credential_file, detect_cli_version, is_managed, login_hint,
    parse_rfc3339, read_secret_file, unix_timestamp, write_secret_file,
};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// The official Codex CLI's OAuth client and token endpoint, as used by its own refresh.
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_SCOPE: &str = "openid profile email";
/// Codex access tokens last about ten days; the CLI refreshes once the last refresh is 8 days old.
const REFRESH_AFTER_DAYS: i64 = 8;
const FALLBACK_VERSION: &str = "0.142.5";

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
    #[serde(default)]
    last_refresh: Option<String>,
}

#[derive(Deserialize)]
struct AuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

struct Auth {
    token: Zeroizing<String>,
    account_id: Zeroizing<String>,
    refresh_token: Option<Zeroizing<String>>,
    last_refresh: Option<DateTime<Utc>>,
}

impl Auth {
    fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        self.last_refresh
            .is_some_and(|last| now - last >= Duration::days(REFRESH_AFTER_DAYS))
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: &'a str,
    scope: &'static str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    email: Option<String>,
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
    let mut auth = read_auth(account)?;
    if auth.needs_refresh(Utc::now()) && refresh_managed_auth(client, account, &auth).await? {
        auth = read_auth(account)?;
    }

    let mut response = send_usage_request(client, &auth).await?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) && refresh_managed_auth(client, account, &auth).await?
    {
        auth = read_auth(account)?;
        response = send_usage_request(client, &auth).await?;
    }

    classify_status(response.status(), login_hint(account, "codex login"))?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    map_usage(account, &body)
}

async fn send_usage_request(client: &Client, auth: &Auth) -> Result<Response, ProviderError> {
    client
        .get(USAGE_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header("originator", "codex_cli_rs")
        .header("ChatGPT-Account-Id", auth.account_id.as_str())
        .bearer_auth(auth.token.as_str())
        .send()
        .await
        .map_err(|_| ProviderError::Network)
}

/// Exchanges a managed profile's refresh token for new tokens and saves them to the profile.
/// Returns whether the stored credentials changed and should be read again.
async fn refresh_managed_auth(
    client: &Client,
    account: &AccountConfig,
    auth: &Auth,
) -> Result<bool, ProviderError> {
    if !is_managed(account) {
        return Ok(false);
    }
    let Some(refresh_token) = auth.refresh_token.as_ref() else {
        return Ok(false);
    };
    let Ok(response) = client
        .post(TOKEN_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .json(&RefreshRequest {
            client_id: CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: refresh_token.as_str(),
            scope: REFRESH_SCOPE,
        })
        .send()
        .await
    else {
        return Ok(false);
    };
    if !response.status().is_success() {
        return Ok(false);
    }
    let Ok(refreshed) = response.json::<RefreshResponse>().await else {
        return Ok(false);
    };
    if refreshed.access_token.as_deref().is_none_or(str::is_empty) {
        return Ok(false);
    }
    persist_refreshed_auth(account, auth, refreshed)?;
    Ok(true)
}

fn persist_refreshed_auth(
    account: &AccountConfig,
    auth: &Auth,
    refreshed: RefreshResponse,
) -> Result<(), ProviderError> {
    let path = credential_file(account);
    let body = read_secret_file(path)?;
    let mut file: serde_json::Map<String, serde_json::Value> = serde_json::from_str(body.as_str())
        .map_err(|_| {
            ProviderError::Credentials("Codex credential file is not valid JSON".into())
        })?;
    let stored_refresh = file
        .get("tokens")
        .and_then(|tokens| tokens.get("refresh_token"))
        .and_then(serde_json::Value::as_str);
    // Another process already rotated the tokens; keep its newer copy.
    if stored_refresh != auth.refresh_token.as_ref().map(|token| token.as_str()) {
        return Ok(());
    }
    apply_refreshed_tokens(&mut file, refreshed, Utc::now())?;
    let encoded = Zeroizing::new(serde_json::to_vec_pretty(&file).map_err(|_| {
        ProviderError::Credentials("could not encode refreshed Codex credentials".into())
    })?);
    write_secret_file(path, &encoded)
}

fn apply_refreshed_tokens(
    file: &mut serde_json::Map<String, serde_json::Value>,
    refreshed: RefreshResponse,
    now: DateTime<Utc>,
) -> Result<(), ProviderError> {
    let tokens = file
        .get_mut("tokens")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| ProviderError::Credentials("Codex OAuth credentials are missing".into()))?;
    for (key, value) in [
        ("id_token", refreshed.id_token),
        ("access_token", refreshed.access_token),
        ("refresh_token", refreshed.refresh_token),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            tokens.insert(key.into(), value.into());
        }
    }
    file.insert(
        "last_refresh".into(),
        now.to_rfc3339_opts(SecondsFormat::Micros, true).into(),
    );
    Ok(())
}

fn read_auth(account: &AccountConfig) -> Result<Auth, ProviderError> {
    let body = read_secret_file(credential_file(account))?;
    let auth: AuthFile = serde_json::from_str(body.as_str()).map_err(|_| {
        ProviderError::Credentials("Codex credential file is not valid JSON".into())
    })?;
    let last_refresh = parse_rfc3339(auth.last_refresh.as_deref());
    let tokens = auth
        .tokens
        .ok_or_else(|| ProviderError::Credentials("Codex OAuth credentials are missing".into()))?;
    let token = tokens
        .access_token
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ProviderError::Authentication(login_hint(account, "codex login")))?;
    let account_id = tokens
        .account_id
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ProviderError::Credentials("Codex account identifier is missing".into()))?;
    let refresh_token = tokens
        .refresh_token
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new);
    Ok(Auth {
        token,
        account_id,
        refresh_token,
        last_refresh,
    })
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
        email: raw.email.filter(|email| !email.trim().is_empty()),
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
    fn refreshes_once_the_last_refresh_is_eight_days_old() {
        let now = Utc::now();
        let auth = |last_refresh| Auth {
            token: Zeroizing::new("access".into()),
            account_id: Zeroizing::new("account".into()),
            refresh_token: Some(Zeroizing::new("refresh".into())),
            last_refresh,
        };
        assert!(!auth(Some(now - Duration::days(7))).needs_refresh(now));
        assert!(auth(Some(now - Duration::days(8))).needs_refresh(now));
        assert!(!auth(None).needs_refresh(now));
    }

    #[test]
    fn refreshed_tokens_replace_only_what_the_provider_returned() {
        let mut file = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "old-id",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "account"
            },
            "last_refresh": "2026-09-17T20:15:36Z"
        })
        .as_object()
        .unwrap()
        .clone();
        let now = DateTime::parse_from_rfc3339("2026-09-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        apply_refreshed_tokens(
            &mut file,
            RefreshResponse {
                id_token: Some("new-id".into()),
                access_token: Some("new-access".into()),
                refresh_token: None,
            },
            now,
        )
        .unwrap();

        assert_eq!(file["tokens"]["id_token"], "new-id");
        assert_eq!(file["tokens"]["access_token"], "new-access");
        assert_eq!(file["tokens"]["refresh_token"], "old-refresh");
        assert_eq!(file["tokens"]["account_id"], "account");
        assert_eq!(file["auth_mode"], "chatgpt");
        assert_eq!(file["last_refresh"], "2026-09-28T12:00:00.000000Z");
    }

    #[test]
    fn persisting_skips_tokens_another_process_already_rotated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        let rotated =
            r#"{"tokens":{"access_token":"theirs","refresh_token":"rotated","account_id":"a"}}"#;
        std::fs::write(&path, rotated).unwrap();
        let account = AccountConfig {
            credential_source: crate::config::CredentialSource::ManagedProfile {
                profile: temp.path().into(),
                credentials: path.clone(),
            },
            ..synthetic_account(Provider::Codex)
        };
        let stale = Auth {
            token: Zeroizing::new("ours".into()),
            account_id: Zeroizing::new("a".into()),
            refresh_token: Some(Zeroizing::new("original".into())),
            last_refresh: None,
        };

        persist_refreshed_auth(
            &account,
            &stale,
            RefreshResponse {
                id_token: None,
                access_token: Some("mine".into()),
                refresh_token: Some("mine".into()),
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rotated);
    }

    #[test]
    fn maps_five_hour_weekly_and_credit_details() {
        let account = synthetic_account(Provider::Codex);
        let snapshot = map_usage(
            &account,
            include_str!("../../tests/fixtures/codex_usage.json"),
        )
        .expect("Codex fixture should map");

        assert_eq!(snapshot.email.as_deref(), Some("coder@example.com"));
        assert_eq!(snapshot.plan.as_deref(), Some("plus"));
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].label, "5H");
        assert_eq!(snapshot.windows[1].label, "WEEKLY");
        assert_eq!(snapshot.windows[1].used_percent, 71.4);
        assert_eq!(snapshot.details[1].value, "2");
    }
}
