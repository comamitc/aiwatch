use std::{
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    path::Path,
    sync::{Arc, LazyLock, Mutex},
};
#[cfg(target_os = "macos")]
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use chrono::{DateTime, Duration, Utc};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
use crate::accounts::claude_keychain_service;
use crate::{
    config::{AccountConfig, CredentialSource},
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{
    ProviderError, classify_status, credential_file, detect_cli_version, is_managed, login_hint,
    parse_rfc3339, read_secret_file, write_secret_file,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// The official Claude Code OAuth client and token endpoint, as used by its own refresh.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Claude access tokens last hours, so refresh shortly before they lapse rather than after.
const REFRESH_EARLY_SECONDS: i64 = 5 * 60;
const FALLBACK_VERSION: &str = "2.1.201";
#[cfg(target_os = "macos")]
const SECURITY_COMMAND: &str = "/usr/bin/security";
#[cfg(target_os = "macos")]
const MAX_CREDENTIAL_BYTES: usize = 1024 * 1024;

#[cfg(target_os = "macos")]
static MANAGED_AUTH_CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<Auth>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Profile emails by account id, each paired with a fingerprint of the token that fetched it.
/// A different token means a different login, so the email is fetched again.
static PROFILE_EMAILS: LazyLock<Mutex<HashMap<String, (u64, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TOKEN_FINGERPRINT: LazyLock<RandomState> = LazyLock::new(RandomState::new);

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}

#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(default, rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(default, rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

struct Auth {
    token: Zeroizing<String>,
    plan: Option<String>,
    refresh_token: Option<Zeroizing<String>>,
    expires_at: Option<DateTime<Utc>>,
    scopes: Vec<String>,
    /// Whether aiwatch owns this credential file and may rotate its tokens.
    refreshable: bool,
}

impl Auth {
    fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .is_some_and(|expiry| expiry <= now + Duration::seconds(REFRESH_EARLY_SECONDS))
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'static str,
    refresh_token: &'a str,
    client_id: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct ProfileResponse {
    #[serde(default)]
    account: Option<ProfileAccount>,
}

#[derive(Deserialize)]
struct ProfileAccount {
    #[serde(default)]
    email: Option<String>,
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
    let mut auth = read_auth(account)?;
    if auth.needs_refresh(Utc::now()) && refresh_managed_auth(client, account, &auth).await? {
        auth = reload_auth(account)?;
    }

    let mut response = send_usage_request(client, &auth).await?;
    if is_rejected(&response) && refresh_managed_auth(client, account, &auth).await? {
        auth = reload_auth(account)?;
        response = send_usage_request(client, &auth).await?;
    }
    if is_rejected(&response) {
        invalidate_managed_auth(account);
    }
    classify_status(response.status(), login_hint(account, "claude auth login"))?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    let mut snapshot = map_usage(account, auth.plan.as_deref(), &body)?;
    snapshot.email = profile_email(client, account, auth.token.as_str()).await;
    Ok(snapshot)
}

async fn send_usage_request(client: &Client, auth: &Auth) -> Result<Response, ProviderError> {
    oauth_get(client, USAGE_URL, auth.token.as_str())
        .send()
        .await
        .map_err(|_| ProviderError::Network)
}

fn is_rejected(response: &Response) -> bool {
    matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    )
}

fn reload_auth(account: &AccountConfig) -> Result<Arc<Auth>, ProviderError> {
    invalidate_managed_auth(account);
    read_auth(account)
}

/// Exchanges a managed profile's refresh token for new tokens and saves them to the profile.
/// Returns whether the stored credentials changed and should be read again.
async fn refresh_managed_auth(
    client: &Client,
    account: &AccountConfig,
    auth: &Auth,
) -> Result<bool, ProviderError> {
    if !auth.refreshable {
        return Ok(false);
    }
    let Some(refresh_token) = auth.refresh_token.as_ref() else {
        return Ok(false);
    };
    let Ok(response) = client
        .post(TOKEN_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .json(&RefreshRequest {
            grant_type: "refresh_token",
            refresh_token: refresh_token.as_str(),
            client_id: CLIENT_ID,
            scope: (!auth.scopes.is_empty()).then(|| auth.scopes.join(" ")),
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
    if refreshed.access_token.is_empty() {
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
            ProviderError::Credentials("Claude credential store is not valid JSON".into())
        })?;
    let oauth = file
        .get_mut("claudeAiOauth")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| ProviderError::Credentials("Claude OAuth credentials are missing".into()))?;
    // Another process already rotated the tokens; keep its newer copy.
    if oauth
        .get("refreshToken")
        .and_then(serde_json::Value::as_str)
        != auth.refresh_token.as_ref().map(|token| token.as_str())
    {
        return Ok(());
    }
    apply_refreshed_tokens(oauth, refreshed, Utc::now());
    let encoded = Zeroizing::new(serde_json::to_vec(&file).map_err(|_| {
        ProviderError::Credentials("could not encode refreshed Claude credentials".into())
    })?);
    write_secret_file(path, &encoded)
}

fn apply_refreshed_tokens(
    oauth: &mut serde_json::Map<String, serde_json::Value>,
    refreshed: RefreshResponse,
    now: DateTime<Utc>,
) {
    oauth.insert("accessToken".into(), refreshed.access_token.into());
    if let Some(refresh_token) = refreshed.refresh_token.filter(|token| !token.is_empty()) {
        oauth.insert("refreshToken".into(), refresh_token.into());
    }
    match refreshed.expires_in {
        Some(seconds) => {
            let expiry = now + Duration::seconds(seconds.max(0));
            oauth.insert("expiresAt".into(), expiry.timestamp_millis().into());
        }
        None => {
            oauth.remove("expiresAt");
        }
    }
    if let Some(scope) = refreshed.scope.filter(|scope| !scope.trim().is_empty()) {
        let scopes = scope
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        oauth.insert("scopes".into(), scopes.into());
    }
}

fn oauth_get(client: &Client, url: &str, token: &str) -> RequestBuilder {
    client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header("x-app", "cli")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .bearer_auth(token)
}

/// Asks the provider which identity owns `token`. Returns `None` rather than an error so a
/// profile outage never hides quota that was already fetched.
async fn profile_email(client: &Client, account: &AccountConfig, token: &str) -> Option<String> {
    let fingerprint = TOKEN_FINGERPRINT.hash_one(token);
    if let Some(email) = cached_profile_email(&account.id, fingerprint) {
        return Some(email);
    }
    let response = oauth_get(client, PROFILE_URL, token).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let email = parse_profile_email(&response.text().await.ok()?)?;
    if let Ok(mut cache) = PROFILE_EMAILS.lock() {
        cache.insert(account.id.clone(), (fingerprint, email.clone()));
    }
    Some(email)
}

fn cached_profile_email(account_id: &str, fingerprint: u64) -> Option<String> {
    let cache = PROFILE_EMAILS.lock().ok()?;
    cache
        .get(account_id)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, email)| email.clone())
}

fn parse_profile_email(body: &str) -> Option<String> {
    serde_json::from_str::<ProfileResponse>(body)
        .ok()?
        .account?
        .email
        .filter(|email| !email.trim().is_empty())
}

fn read_auth(account: &AccountConfig) -> Result<Arc<Auth>, ProviderError> {
    match &account.credential_source {
        CredentialSource::File(path) => {
            let body = read_secret_file(path)?;
            parse_auth(account, body.as_str(), false).map(Arc::new)
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
                let (body, from_file) = read_managed_credentials(profile, credentials)?;
                parse_auth(account, body.as_str(), from_file).map(Arc::new)
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

    let (body, from_file) = read_managed_credentials(profile, credentials)?;
    let auth = Arc::new(parse_auth(account, body.as_str(), from_file)?);
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

/// `writable` marks a managed profile's own credential file, the only store aiwatch refreshes.
fn parse_auth(account: &AccountConfig, body: &str, writable: bool) -> Result<Auth, ProviderError> {
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
    Ok(Auth {
        token,
        plan,
        refresh_token: oauth
            .refresh_token
            .filter(|value| !value.is_empty())
            .map(Zeroizing::new),
        expires_at: oauth.expires_at.and_then(DateTime::from_timestamp_millis),
        scopes: oauth.scopes,
        refreshable: writable && is_managed(account),
    })
}

/// Returns the credential body and whether it came from the profile's file. Keychain items are
/// only read, never rewritten, so they are left for Claude Code to refresh.
fn read_managed_credentials(
    _profile: &Path,
    credentials: &Path,
) -> Result<(Zeroizing<String>, bool), ProviderError> {
    #[cfg(target_os = "macos")]
    if let Some(body) = read_macos_keychain(_profile)? {
        return Ok((body, false));
    }

    let body = read_secret_file(credentials).map_err(|_| {
        ProviderError::Credentials(
            "managed Claude credentials are unavailable; run the account login command".into(),
        )
    })?;
    Ok((body, true))
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
        email: None,
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

    fn managed_account(credentials: std::path::PathBuf) -> AccountConfig {
        AccountConfig {
            credential_source: CredentialSource::ManagedProfile {
                profile: credentials.parent().unwrap().into(),
                credentials,
            },
            ..synthetic_account(Provider::Claude)
        }
    }

    #[test]
    fn only_managed_credential_files_are_refreshable() {
        let body = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":1790638525507,"scopes":["user:inference"]}}"#;
        let managed = managed_account("profile/.credentials.json".into());
        let auth = parse_auth(&managed, body, true).unwrap();
        assert!(auth.refreshable);
        assert_eq!(auth.scopes, ["user:inference"]);
        assert_eq!(
            auth.expires_at.map(|expiry| expiry.timestamp_millis()),
            Some(1790638525507)
        );
        assert!(!parse_auth(&managed, body, false).unwrap().refreshable);
        let official = synthetic_account(Provider::Claude);
        assert!(!parse_auth(&official, body, true).unwrap().refreshable);
    }

    #[test]
    fn refreshes_shortly_before_expiry() {
        let now = Utc::now();
        let auth = |expires_at| Auth {
            token: Zeroizing::new("a".into()),
            plan: None,
            refresh_token: None,
            expires_at,
            scopes: Vec::new(),
            refreshable: true,
        };
        assert!(!auth(Some(now + Duration::minutes(30))).needs_refresh(now));
        assert!(auth(Some(now + Duration::minutes(4))).needs_refresh(now));
        assert!(auth(Some(now - Duration::hours(1))).needs_refresh(now));
        assert!(!auth(None).needs_refresh(now));
    }

    #[test]
    fn refreshed_tokens_keep_subscription_metadata() {
        let mut oauth = serde_json::json!({
            "accessToken": "old-access",
            "refreshToken": "old-refresh",
            "expiresAt": 1,
            "scopes": ["user:inference"],
            "subscriptionType": "team",
            "rateLimitTier": "default_claude_max_5x"
        })
        .as_object()
        .unwrap()
        .clone();
        let now = DateTime::parse_from_rfc3339("2026-09-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        apply_refreshed_tokens(
            &mut oauth,
            RefreshResponse {
                access_token: "new-access".into(),
                refresh_token: Some("new-refresh".into()),
                expires_in: Some(28_800),
                scope: Some("user:inference user:profile".into()),
            },
            now,
        );

        assert_eq!(oauth["accessToken"], "new-access");
        assert_eq!(oauth["refreshToken"], "new-refresh");
        assert_eq!(
            oauth["expiresAt"],
            (now + Duration::hours(8)).timestamp_millis()
        );
        assert_eq!(
            oauth["scopes"],
            serde_json::json!(["user:inference", "user:profile"])
        );
        assert_eq!(oauth["subscriptionType"], "team");
        assert_eq!(oauth["rateLimitTier"], "default_claude_max_5x");
    }

    #[test]
    fn persisting_writes_a_private_file_and_skips_tokens_rotated_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","subscriptionType":"team"},"mcpOAuth":{}}"#,
        )
        .unwrap();
        let account = managed_account(path.clone());
        let auth = parse_auth(&account, &std::fs::read_to_string(&path).unwrap(), true).unwrap();
        let refreshed = || RefreshResponse {
            access_token: "fresh".into(),
            refresh_token: Some("fresh-refresh".into()),
            expires_in: Some(3600),
            scope: None,
        };

        persist_refreshed_auth(&account, &auth, refreshed()).unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored["claudeAiOauth"]["accessToken"], "fresh");
        assert_eq!(stored["claudeAiOauth"]["subscriptionType"], "team");
        assert!(stored.get("mcpOAuth").is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // `auth` still holds the old refresh token, as if another process refreshed first.
        persist_refreshed_auth(&account, &auth, refreshed()).unwrap();
        let unchanged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(unchanged, stored);
    }

    #[test]
    fn profile_email_comes_from_the_account_object() {
        assert_eq!(
            parse_profile_email(
                r#"{"account":{"email":"person@example.com"},"organization":{"name":"Org"}}"#
            )
            .as_deref(),
            Some("person@example.com")
        );
        assert_eq!(parse_profile_email(r#"{"account":{"email":""}}"#), None);
        assert_eq!(parse_profile_email(r#"{"organization":{}}"#), None);
        assert_eq!(parse_profile_email("not json"), None);
    }

    #[test]
    fn cached_profile_email_is_dropped_when_the_token_changes() {
        let fingerprint = TOKEN_FINGERPRINT.hash_one("first-token");
        PROFILE_EMAILS.lock().unwrap().insert(
            "claude:cache-test".into(),
            (fingerprint, "first@example.com".into()),
        );

        assert_eq!(
            cached_profile_email("claude:cache-test", fingerprint).as_deref(),
            Some("first@example.com")
        );
        let switched = TOKEN_FINGERPRINT.hash_one("second-token");
        assert_eq!(cached_profile_email("claude:cache-test", switched), None);
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
