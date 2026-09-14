use std::{collections::BTreeMap, sync::LazyLock};

use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::{
    config::AccountConfig,
    model::{AccountSnapshot, DetailMetric, FetchHealth, UsageWindow},
};

use super::{
    ProviderError, classify_status, credential_file, detect_cli_version, parse_rfc3339,
    read_secret_file,
};

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const FALLBACK_VERSION: &str = "0.2.112";
const LOGIN_HINT: &str = "run `grok login`";

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
}

struct Auth {
    token: Zeroizing<String>,
    user_id: Option<Zeroizing<String>>,
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
    let auth = read_auth(account)?;
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
    let response = request.send().await.map_err(|_| ProviderError::Network)?;
    classify_status(response.status(), LOGIN_HINT)?;
    let body = response.text().await.map_err(|_| ProviderError::Network)?;
    map_usage(account, &body)
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

    let Some((_, entry)) = oauth_entry else {
        if entries.contains_key("xai::api_key") {
            return Err(ProviderError::Credentials(
                "Grok API keys do not expose subscription quota; use `grok login`".into(),
            ));
        }
        return Err(ProviderError::Authentication(LOGIN_HINT));
    };

    let token = entry
        .key
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .map(Zeroizing::new)
        .ok_or(ProviderError::Authentication(LOGIN_HINT))?;
    let user_id = entry.user_id.as_ref().cloned().map(Zeroizing::new);
    Ok(Auth { token, user_id })
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
}
