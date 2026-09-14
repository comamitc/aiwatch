mod claude;
mod codex;
mod grok;

use std::{path::Path, process::Command};

use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    config::AccountConfig,
    model::{AccountSnapshot, HealthState, Provider},
};

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("credentials unavailable: {0}")]
    Credentials(String),
    #[error("authentication required: {0}")]
    Authentication(&'static str),
    #[error("provider rate limited the usage request")]
    RateLimited,
    #[error("provider returned HTTP {0}")]
    Http(u16),
    #[error("provider response schema changed")]
    Schema,
    #[error("network request failed")]
    Network,
}

impl ProviderError {
    pub fn health_state(&self) -> HealthState {
        match self {
            Self::Authentication(_) => HealthState::AuthenticationRequired,
            Self::RateLimited => HealthState::RateLimited,
            Self::Credentials(_) | Self::Http(_) | Self::Schema | Self::Network => {
                HealthState::Error
            }
        }
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::Authentication(_) => Some(401),
            Self::RateLimited => Some(429),
            Self::Http(status) => Some(*status),
            Self::Credentials(_) | Self::Schema | Self::Network => None,
        }
    }

    pub fn safe_message(&self) -> String {
        self.to_string()
    }
}

pub async fn fetch_account(
    client: &Client,
    account: &AccountConfig,
) -> Result<AccountSnapshot, ProviderError> {
    match account.provider {
        Provider::Claude => claude::fetch(client, account).await,
        Provider::Codex => codex::fetch(client, account).await,
        Provider::Grok => grok::fetch(client, account).await,
    }
}

pub fn classify_status(status: StatusCode, login_hint: &'static str) -> Result<(), ProviderError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(ProviderError::Authentication(login_hint))
        }
        StatusCode::TOO_MANY_REQUESTS => Err(ProviderError::RateLimited),
        status if status.is_success() => Ok(()),
        status => Err(ProviderError::Http(status.as_u16())),
    }
}

pub fn read_secret_file(path: &Path) -> Result<Zeroizing<String>, ProviderError> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|_| ProviderError::Credentials(format!("could not read {}", path.display())))
}

pub fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

pub fn unix_timestamp(value: Option<i64>) -> Option<DateTime<Utc>> {
    value.and_then(|value| DateTime::from_timestamp(value, 0))
}

pub fn detect_cli_version(binary: &str, fallback: &str) -> String {
    Command::new(binary)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .split_whitespace()
                .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.'))
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
pub(crate) fn synthetic_account(provider: Provider) -> AccountConfig {
    AccountConfig {
        id: format!("{}:test", provider.key()),
        name: "test".to_string(),
        provider,
        credentials: "unused.json".into(),
    }
}
