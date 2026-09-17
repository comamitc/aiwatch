#[cfg(target_os = "macos")]
use std::{
    collections::HashMap,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
};
use std::{
    path::Path,
    sync::{Arc, LazyLock},
};

use chrono::Utc;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
use crate::accounts::claude_keychain_service;
use crate::{
    config::{AccountConfig, CredentialSource},
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{
    ProviderError, classify_status, detect_cli_version, login_hint, parse_rfc3339, read_secret_file,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const FALLBACK_VERSION: &str = "2.1.201";
#[cfg(target_os = "macos")]
const SECURITY_COMMAND: &str = "/usr/bin/security";
#[cfg(target_os = "macos")]
const MAX_CREDENTIAL_BYTES: usize = 1024 * 1024;

#[cfg(target_os = "macos")]
static MANAGED_AUTH_CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<Auth>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

struct Auth {
    token: Zeroizing<String>,
    plan: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    five_hour: Option<RawWindow>,
    #[serde(default)]
    seven_day: Option<RawWindow>,
    #[serde(default)]
    limits: Option<Vec<RawLimit>>,
    #[serde(default)]
    spend: Option<RawSpend>,
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct RawLimit {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    scope: Option<RawScope>,
    #[serde(default)]
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct RawScope {
    #[serde(default)]
    model: Option<RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct RawSpend {
    #[serde(default)]
    used: Option<RawMoney>,
    #[serde(default)]
    balance: Option<RawMoney>,
}

#[derive(Deserialize)]
struct RawMoney {
    #[serde(default)]
    amount_minor: i64,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    exponent: i32,
}

impl RawMoney {
    fn display(&self) -> String {
        let amount = self.amount_minor as f64 / 10_f64.powi(self.exponent.max(0));
        match self.currency.as_deref() {
            None | Some("USD") => format!("${amount:.2}"),
            Some(currency) => format!("{amount:.2} {currency}"),
        }
    }
}

pub async fn fetch(
    client: &Client,
    account: &AccountConfig,
) -> Result<AccountSnapshot, ProviderError> {
    let auth = read_auth(account)?;
    let response = client
        .get(USAGE_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header("x-app", "cli")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .bearer_auth(auth.token.as_str())
        .send()
        .await
        .map_err(|_| ProviderError::Network)?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        invalidate_managed_auth(account);
    }
    classify_status(response.status(), login_hint(account, "claude auth login"))?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    map_usage(account, auth.plan.as_deref(), &body)
}

fn read_auth(account: &AccountConfig) -> Result<Arc<Auth>, ProviderError> {
    match &account.credential_source {
        CredentialSource::File(path) => {
            let body = read_secret_file(path)?;
            parse_auth(account, body.as_str()).map(Arc::new)
        }
        CredentialSource::ManagedProfile {
            profile,
            credentials,
        } => {
            #[cfg(target_os = "macos")]
            {
                read_cached_managed_auth(account, profile, credentials)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let body = read_managed_credentials(profile, credentials)?;
                parse_auth(account, body.as_str()).map(Arc::new)
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn read_cached_managed_auth(
    account: &AccountConfig,
    profile: &Path,
    credentials: &Path,
) -> Result<Arc<Auth>, ProviderError> {
    let mut cache = MANAGED_AUTH_CACHE.lock().map_err(|_| {
        ProviderError::Credentials("managed Claude credential cache is unavailable".into())
    })?;
    if let Some(auth) = cache.get(profile) {
        return Ok(Arc::clone(auth));
    }

    let body = read_managed_credentials(profile, credentials)?;
    let auth = Arc::new(parse_auth(account, body.as_str())?);
    cache.insert(profile.to_path_buf(), Arc::clone(&auth));
    Ok(auth)
}

#[cfg(target_os = "macos")]
fn invalidate_managed_auth(account: &AccountConfig) {
    let CredentialSource::ManagedProfile { profile, .. } = &account.credential_source else {
        return;
    };
    if let Ok(mut cache) = MANAGED_AUTH_CACHE.lock() {
        cache.remove(profile);
    }
}

#[cfg(not(target_os = "macos"))]
fn invalidate_managed_auth(_account: &AccountConfig) {}

fn parse_auth(account: &AccountConfig, body: &str) -> Result<Auth, ProviderError> {
    let credentials: Credentials = serde_json::from_str(body).map_err(|_| {
        ProviderError::Credentials("Claude credential store is not valid JSON".into())
    })?;
    let oauth = credentials
        .oauth
        .ok_or_else(|| ProviderError::Credentials("Claude OAuth credentials are missing".into()))?;
    let token = oauth
        .access_token
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| ProviderError::Authentication(login_hint(account, "claude auth login")))?;
    let plan = oauth
        .rate_limit_tier
        .or(oauth.subscription_type)
        .map(|value| prettify_plan(&value));
    Ok(Auth { token, plan })
}

fn read_managed_credentials(
    _profile: &Path,
    credentials: &Path,
) -> Result<Zeroizing<String>, ProviderError> {
    #[cfg(target_os = "macos")]
    if let Some(body) = read_macos_keychain(_profile)? {
        return Ok(body);
    }

    read_secret_file(credentials).map_err(|_| {
        ProviderError::Credentials(
            "managed Claude credentials are unavailable; run the account login command".into(),
        )
    })
}

#[cfg(target_os = "macos")]
fn read_macos_keychain(profile: &Path) -> Result<Option<Zeroizing<String>>, ProviderError> {
    let service = claude_keychain_service(profile);
    let Ok(account) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) else {
        return Ok(None);
    };
    let Ok(output) = Command::new(SECURITY_COMMAND)
        .args([
            "find-generic-password",
            "-s",
            service.as_str(),
            "-a",
            account.as_str(),
            "-w",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    if output.stdout.len() > MAX_CREDENTIAL_BYTES {
        return Err(ProviderError::Credentials(
            "managed Claude credential is unexpectedly large".into(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(Zeroizing::new)
        .map(Some)
        .map_err(|_| {
            ProviderError::Credentials("managed Claude credentials are not valid UTF-8".into())
        })
}

fn map_usage(
    account: &AccountConfig,
    plan: Option<&str>,
    body: &str,
) -> Result<AccountSnapshot, ProviderError> {
    let raw: UsageResponse = serde_json::from_str(body).map_err(|_| ProviderError::Schema)?;
    let mut windows = Vec::new();
    if let Some(window) = raw.five_hour {
        windows.push(map_window("five_hour", "5H", window));
    }
    if let Some(window) = raw.seven_day {
        windows.push(map_window("weekly", "WEEKLY", window));
    }

    let scoped = raw
        .limits
        .unwrap_or_default()
        .into_iter()
        .filter(|limit| limit.kind.as_deref() == Some("weekly_scoped"))
        .filter_map(|limit| {
            let label = limit.scope?.model?.display_name?;
            Some((
                limit.is_active.unwrap_or(false),
                limit.percent.unwrap_or(0.0),
                label,
                limit.resets_at,
            ))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then(left.1.total_cmp(&right.1)));
    if let Some((_, percent, label, resets_at)) = scoped {
        windows.push(UsageWindow::new(
            format!("weekly_{}", label.to_ascii_lowercase().replace(' ', "_")),
            format!("{label} WEEKLY"),
            percent,
            parse_rfc3339(resets_at.as_deref()),
        ));
    }

    if windows.is_empty() {
        return Err(ProviderError::Schema);
    }

    let mut details = Vec::new();
    if let Some(spend) = raw.spend {
        if let Some(used) = spend.used {
            details.push(DetailMetric::provider("spend", used.display()));
        }
        if let Some(balance) = spend.balance {
            details.push(DetailMetric::provider("balance", balance.display()));
        }
    }

    let now = Utc::now();
    Ok(AccountSnapshot {
        id: account.id.clone(),
        name: account.name.clone(),
        provider: account.provider,
        plan: plan.map(str::to_owned),
        windows,
        details,
        health: FetchHealth::ok(),
        fetched_at: now,
        last_success_at: Some(now),
    })
}

fn map_window(key: &str, label: &str, window: RawWindow) -> UsageWindow {
    UsageWindow::new(
        key,
        label,
        window.utilization.unwrap_or(0.0),
        parse_rfc3339(window.resets_at.as_deref()),
    )
}

fn prettify_plan(value: &str) -> String {
    value
        .trim_start_matches("default_")
        .trim_start_matches("claude_")
        .replace('_', " ")
}

static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "claude-cli/{} (external, cli)",
        detect_cli_version("claude", FALLBACK_VERSION)
    )
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Provider, providers::synthetic_account};
    #[cfg(target_os = "macos")]
    use tempfile::tempdir;

    #[test]
    fn maps_windows_scoped_limit_and_money_without_credentials() {
        let account = synthetic_account(Provider::Claude);
        let snapshot = map_usage(
            &account,
            Some("max 20x"),
            include_str!("../../tests/fixtures/claude_usage.json"),
        )
        .expect("Claude fixture should map");

        assert_eq!(snapshot.plan.as_deref(), Some("max 20x"));
        assert_eq!(snapshot.windows.len(), 3);
        assert_eq!(snapshot.windows[0].used_percent, 42.5);
        assert_eq!(snapshot.windows[2].label, "Opus WEEKLY");
        assert_eq!(snapshot.details[0].value, "$12.34");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn managed_credentials_remain_available_after_first_load() {
        let temp = tempdir().unwrap();
        let profile = temp.path().join("personal");
        std::fs::create_dir(&profile).unwrap();
        let credentials = profile.join(".credentials.json");
        std::fs::write(
            &credentials,
            r#"{"claudeAiOauth":{"accessToken":"cached-token","subscriptionType":"pro"}}"#,
        )
        .unwrap();
        let account = AccountConfig {
            id: "claude:managed:personal".into(),
            name: "personal".into(),
            provider: Provider::Claude,
            credential_source: CredentialSource::ManagedProfile {
                profile,
                credentials: credentials.clone(),
            },
        };

        let first = read_auth(&account).unwrap();
        std::fs::remove_file(credentials).unwrap();
        let second = read_auth(&account).unwrap();

        assert_eq!(first.token.as_str(), "cached-token");
        assert_eq!(second.token.as_str(), "cached-token");
        assert_eq!(second.plan.as_deref(), Some("pro"));
        invalidate_managed_auth(&account);
        assert!(read_auth(&account).is_err());
    }
}
