use std::{collections::BTreeMap, io::Write, sync::LazyLock};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

use crate::{
    config::{AccountConfig, CredentialSource},
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{
    ProviderError, classify_status, credential_file, detect_cli_version, login_hint, parse_rfc3339,
    read_secret_file,
};

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const OIDC_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const FALLBACK_VERSION: &str = "0.2.112";
const REFRESH_EARLY_SECONDS: i64 = 5 * 60;

static VERSION: LazyLock<String> = LazyLock::new(|| detect_cli_version("grok", FALLBACK_VERSION));
static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "grok-pager/{0} grok-shell/{0} ({1}; {2})",
        VERSION.as_str(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
});

#[derive(Deserialize)]
struct AuthEntry {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    oidc_issuer: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
    #[serde(default)]
    principal_type: Option<String>,
    #[serde(default)]
    principal_id: Option<String>,
}

struct Auth {
    entry_name: String,
    token: Zeroizing<String>,
    user_id: Option<Zeroizing<String>>,
    refresh_token: Option<Zeroizing<String>>,
    expires_at: Option<DateTime<Utc>>,
    oidc_issuer: Option<String>,
    oidc_client_id: Option<String>,
    principal_type: Option<String>,
    principal_id: Option<String>,
}

impl Auth {
    fn needs_refresh(&self) -> bool {
        self.expires_at
            .is_some_and(|expiry| expiry <= Utc::now() + Duration::seconds(REFRESH_EARLY_SECONDS))
    }
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct BillingResponse {
    #[serde(default)]
    config: Option<BillingConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingConfig {
    #[serde(default)]
    credit_usage_percent: Option<f64>,
    #[serde(default)]
    current_period: Option<UsagePeriod>,
    #[serde(default)]
    product_usage: Vec<ProductUsage>,
    #[serde(default)]
    on_demand_cap: Option<Money>,
    #[serde(default)]
    on_demand_used: Option<Money>,
    #[serde(default)]
    prepaid_balance: Option<Money>,
    #[serde(default)]
    billing_period_end: Option<String>,
    #[serde(default)]
    subscription_tier_display: Option<String>,
}

#[derive(Deserialize)]
struct UsagePeriod {
    #[serde(default, rename = "type")]
    period_type: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductUsage {
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    usage_percent: Option<f64>,
}

#[derive(Deserialize)]
struct Money {
    #[serde(default)]
    val: Option<f64>,
}

impl Money {
    fn dollars(&self) -> f64 {
        self.val.unwrap_or(0.0) / 100.0
    }
}

pub async fn fetch(
    client: &Client,
    account: &AccountConfig,
) -> Result<AccountSnapshot, ProviderError> {
    let mut auth = read_auth(account)?;
    if auth.needs_refresh() && refresh_managed_auth(client, account, &auth).await? {
        auth = read_auth(account)?;
    }

    let mut response = send_billing_request(client, &auth).await?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) && refresh_managed_auth(client, account, &auth).await?
    {
        auth = read_auth(account)?;
        response = send_billing_request(client, &auth).await?;
    }

    classify_status(response.status(), login_hint(account, "grok login"))?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    map_usage(account, &body)
}

async fn send_billing_request(client: &Client, auth: &Auth) -> Result<Response, ProviderError> {
    let mut request = client
        .get(BILLING_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, USER_AGENT.as_str())
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("x-grok-client-version", VERSION.as_str())
        .header("x-grok-client-mode", "interactive")
        .bearer_auth(auth.token.as_str());
    if let Some(user_id) = auth.user_id.as_ref() {
        request = request.header("x-userid", user_id.as_str());
    }
    request.send().await.map_err(|_| ProviderError::Network)
}

fn read_auth(account: &AccountConfig) -> Result<Auth, ProviderError> {
    let body = read_secret_file(credential_file(account))?;
    let entries: BTreeMap<String, AuthEntry> = serde_json::from_str(body.as_str())
        .map_err(|_| ProviderError::Credentials("Grok credential file is not valid JSON".into()))?;

    let oauth_entry = entries
        .iter()
        .filter(|(name, _)| name.as_str() != "xai::api_key")
        .find(|(name, entry)| name.starts_with("https://auth.x.ai") && entry.key.is_some())
        .or_else(|| {
            entries
                .iter()
                .filter(|(name, _)| name.as_str() != "xai::api_key")
                .find(|(_, entry)| entry.key.is_some())
        });

    let Some((entry_name, entry)) = oauth_entry else {
        if entries.contains_key("xai::api_key") {
            return Err(ProviderError::Credentials(format!(
                "Grok API keys do not expose subscription quota; {}",
                login_hint(account, "grok login")
            )));
        }
        return Err(ProviderError::Authentication(login_hint(
            account,
            "grok login",
        )));
    };

    let token = entry
        .key
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .map(Zeroizing::new)
        .ok_or_else(|| ProviderError::Authentication(login_hint(account, "grok login")))?;
    Ok(Auth {
        entry_name: entry_name.clone(),
        token,
        user_id: entry.user_id.as_ref().cloned().map(Zeroizing::new),
        refresh_token: entry.refresh_token.as_ref().cloned().map(Zeroizing::new),
        expires_at: entry.expires_at,
        oidc_issuer: entry.oidc_issuer.clone(),
        oidc_client_id: entry.oidc_client_id.clone(),
        principal_type: entry.principal_type.clone(),
        principal_id: entry.principal_id.clone(),
    })
}

async fn refresh_managed_auth(
    client: &Client,
    account: &AccountConfig,
    auth: &Auth,
) -> Result<bool, ProviderError> {
    if !matches!(
        account.credential_source,
        CredentialSource::ManagedProfile { .. }
    ) || auth.oidc_issuer.as_deref() != Some("https://auth.x.ai")
    {
        return Ok(false);
    }
    let (Some(refresh_token), Some(client_id)) =
        (auth.refresh_token.as_ref(), auth.oidc_client_id.as_deref())
    else {
        return Ok(false);
    };

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_id),
    ];
    if let Some(principal_type) = auth.principal_type.as_deref() {
        form.push(("principal_type", principal_type));
    }
    if let Some(principal_id) = auth.principal_id.as_deref() {
        form.push(("principal_id", principal_id));
    }
    let Ok(response) = client
        .post(OIDC_TOKEN_URL)
        .header("x-grok-client-version", VERSION.as_str())
        .form(&form)
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
    let mut entries: BTreeMap<String, serde_json::Value> = serde_json::from_str(body.as_str())
        .map_err(|_| ProviderError::Credentials("Grok credential file is not valid JSON".into()))?;
    let entry = entries
        .get_mut(&auth.entry_name)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            ProviderError::Credentials("Grok OAuth credential entry disappeared".into())
        })?;
    if entry
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
        != auth.refresh_token.as_ref().map(|token| token.as_str())
    {
        return Ok(());
    }
    apply_refreshed_tokens(entry, refreshed, Utc::now());

    let parent = path.parent().ok_or_else(|| {
        ProviderError::Credentials("Grok credential path has no parent directory".into())
    })?;
    let encoded = Zeroizing::new(serde_json::to_vec_pretty(&entries).map_err(|_| {
        ProviderError::Credentials("could not encode refreshed Grok credentials".into())
    })?);
    let mut temporary = NamedTempFile::new_in(parent).map_err(|_| {
        ProviderError::Credentials("could not create temporary Grok credential file".into())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| {
                ProviderError::Credentials(
                    "could not protect temporary Grok credential file".into(),
                )
            })?;
    }
    temporary.write_all(encoded.as_slice()).map_err(|_| {
        ProviderError::Credentials("could not write refreshed Grok credentials".into())
    })?;
    temporary.as_file().sync_all().map_err(|_| {
        ProviderError::Credentials("could not sync refreshed Grok credentials".into())
    })?;
    temporary.persist(path).map_err(|_| {
        ProviderError::Credentials("could not replace refreshed Grok credentials".into())
    })?;
    Ok(())
}

fn apply_refreshed_tokens(
    entry: &mut serde_json::Map<String, serde_json::Value>,
    refreshed: RefreshResponse,
    now: DateTime<Utc>,
) {
    entry.insert("key".into(), refreshed.access_token.into());
    if let Some(refresh_token) = refreshed.refresh_token {
        entry.insert("refresh_token".into(), refresh_token.into());
    }
    entry.insert(
        "create_time".into(),
        now.to_rfc3339_opts(SecondsFormat::AutoSi, true).into(),
    );
    match refreshed.expires_in {
        Some(seconds) => {
            let seconds = seconds.min(i64::MAX as u64) as i64;
            entry.insert(
                "expires_at".into(),
                (now + Duration::seconds(seconds))
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true)
                    .into(),
            );
        }
        None => {
            entry.remove("expires_at");
        }
    }
}

fn map_usage(account: &AccountConfig, body: &str) -> Result<AccountSnapshot, ProviderError> {
    let raw: BillingResponse = serde_json::from_str(body).map_err(|_| ProviderError::Schema)?;
    let config = raw.config.ok_or(ProviderError::Schema)?;
    let period = config
        .current_period
        .as_ref()
        .and_then(|period| period.period_type.as_deref())
        .and_then(period_label)
        .unwrap_or("ALLOWANCE");
    let resets_at = config
        .current_period
        .as_ref()
        .and_then(|period| period.end.as_deref())
        .or(config.billing_period_end.as_deref());
    let window = UsageWindow::new(
        period.to_ascii_lowercase(),
        period,
        config.credit_usage_percent.unwrap_or(0.0),
        parse_rfc3339(resets_at),
    );

    let mut details = config
        .product_usage
        .into_iter()
        .filter_map(|product| {
            let name = product.product?;
            let percent = product.usage_percent?;
            Some(DetailMetric::provider(
                humanize_product(&name),
                format!("{:.1}%", percent.clamp(0.0, 100.0)),
            ))
        })
        .collect::<Vec<_>>();

    let cap = config
        .on_demand_cap
        .as_ref()
        .map(Money::dollars)
        .unwrap_or(0.0);
    let used = config
        .on_demand_used
        .as_ref()
        .map(Money::dollars)
        .unwrap_or(0.0);
    if cap > 0.0 {
        details.push(DetailMetric::provider(
            "on-demand",
            format!("${used:.2} / ${cap:.2}"),
        ));
    } else if used > 0.0 {
        details.push(DetailMetric::provider("on-demand", format!("${used:.2}")));
    }
    let balance = config
        .prepaid_balance
        .as_ref()
        .map(Money::dollars)
        .unwrap_or(0.0);
    if balance > 0.0 {
        details.push(DetailMetric::provider("prepaid", format!("${balance:.2}")));
    }

    let now = Utc::now();
    Ok(AccountSnapshot {
        id: account.id.clone(),
        name: account.name.clone(),
        provider: account.provider,
        plan: config.subscription_tier_display,
        windows: vec![window],
        details,
        health: FetchHealth::ok(),
        fetched_at: now,
        last_success_at: Some(now),
    })
}

fn period_label(value: &str) -> Option<&'static str> {
    match value {
        "USAGE_PERIOD_TYPE_WEEKLY" => Some("WEEKLY"),
        "USAGE_PERIOD_TYPE_MONTHLY" => Some("MONTHLY"),
        _ => None,
    }
}

fn humanize_product(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            output.push(' ');
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Provider, providers::synthetic_account};

    #[test]
    fn maps_weekly_product_usage_and_credit_values() {
        let account = synthetic_account(Provider::Grok);
        let snapshot = map_usage(
            &account,
            include_str!("../../tests/fixtures/grok_usage.json"),
        )
        .expect("Grok fixture should map");

        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].label, "WEEKLY");
        assert_eq!(snapshot.windows[0].used_percent, 18.25);
        assert_eq!(snapshot.details[0].label, "Grok Build");
        assert_eq!(
            snapshot.details.last().expect("prepaid detail").value,
            "$10.00"
        );
    }

    #[test]
    fn rejects_error_payload_instead_of_showing_zero_usage() {
        let account = synthetic_account(Provider::Grok);
        assert!(map_usage(&account, r#"{"error":"unauthorized"}"#).is_err());
    }

    #[test]
    fn refreshed_tokens_rotate_atomically_without_dropping_metadata() {
        let mut entry = serde_json::json!({
            "key": "old-access",
            "refresh_token": "old-refresh",
            "email": "person@example.com",
            "expires_at": "2020-01-01T00:00:00Z"
        })
        .as_object()
        .unwrap()
        .clone();
        let now = DateTime::parse_from_rfc3339("2026-09-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        apply_refreshed_tokens(
            &mut entry,
            RefreshResponse {
                access_token: "new-access".into(),
                refresh_token: None,
                expires_in: Some(3600),
            },
            now,
        );
        assert_eq!(entry["key"], "new-access");
        assert_eq!(entry["refresh_token"], "old-refresh");
        assert_eq!(entry["email"], "person@example.com");
        assert_eq!(entry["expires_at"], "2026-09-17T13:00:00Z");

        apply_refreshed_tokens(
            &mut entry,
            RefreshResponse {
                access_token: "newer-access".into(),
                refresh_token: Some("rotated-refresh".into()),
                expires_in: None,
            },
            now,
        );
        assert_eq!(entry["key"], "newer-access");
        assert_eq!(entry["refresh_token"], "rotated-refresh");
        assert!(entry.get("expires_at").is_none());
    }

    #[test]
    fn refreshes_tokens_in_the_early_expiry_window() {
        let auth = Auth {
            entry_name: "issuer".into(),
            token: Zeroizing::new("access".into()),
            user_id: None,
            refresh_token: Some(Zeroizing::new("refresh".into())),
            expires_at: Some(Utc::now() + Duration::seconds(REFRESH_EARLY_SECONDS - 1)),
            oidc_issuer: Some("https://auth.x.ai".into()),
            oidc_client_id: Some("client".into()),
            principal_type: None,
            principal_id: None,
        };

        assert!(auth.needs_refresh());
    }
}
