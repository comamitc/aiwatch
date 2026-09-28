mod claude;
mod codex;
mod grok;

use std::{io::Write, path::Path, process::Command};

use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    config::{AccountConfig, CredentialSource},
    model::{AccountSnapshot, HealthState, Provider},
};

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("credentials unavailable: {0}")]
    Credentials(String),
    #[error("authentication required: {0}")]
    Authentication(String),
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

pub fn classify_status(
    status: StatusCode,
    login_hint: impl Into<String>,
) -> Result<(), ProviderError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(ProviderError::Authentication(login_hint.into()))
        }
        StatusCode::TOO_MANY_REQUESTS => Err(ProviderError::RateLimited),
        status if status.is_success() => Ok(()),
        status => Err(ProviderError::Http(status.as_u16())),
    }
}

pub fn login_hint(account: &AccountConfig, default_command: &str) -> String {
    match &account.credential_source {
        CredentialSource::ManagedProfile { .. } => format!(
            "run `aiwatch account login {} {}`",
            account.provider, account.name
        ),
        CredentialSource::File(_) => format!("run `{default_command}`"),
    }
}

pub fn read_secret_file(path: &Path) -> Result<Zeroizing<String>, ProviderError> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|_| ProviderError::Credentials(format!("could not read {}", path.display())))
}

/// Atomically replaces a credential file with owner-only permissions, so a crash or a concurrent
/// reader never sees a partially written token.
pub fn write_secret_file(path: &Path, body: &[u8]) -> Result<(), ProviderError> {
    let failure =
        |action: &str| ProviderError::Credentials(format!("could not {action} {}", path.display()));
    let parent = path
        .parent()
        .ok_or_else(|| failure("find the directory of"))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| failure("stage a replacement for"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| failure("protect the replacement for"))?;
    }
    temporary
        .write_all(body)
        .map_err(|_| failure("write the replacement for"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| failure("sync the replacement for"))?;
    temporary.persist(path).map_err(|_| failure("replace"))?;
    Ok(())
}

/// Managed profiles belong to aiwatch, so it may rotate their tokens. Credentials maintained by an
/// official CLI (`~/.claude`, `~/.codex`) are left for that CLI to refresh, so the two never race.
pub fn is_managed(account: &AccountConfig) -> bool {
    matches!(
        account.credential_source,
        CredentialSource::ManagedProfile { .. }
    )
}

pub fn credential_file(account: &AccountConfig) -> &Path {
    account.credential_source.credentials()
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
        credential_source: crate::config::CredentialSource::File("unused.json".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_login_hint_names_the_profile() {
        let account = AccountConfig {
            id: "grok:managed:personal".to_string(),
            name: "personal".to_string(),
            provider: Provider::Grok,
            credential_source: CredentialSource::ManagedProfile {
                profile: "profile".into(),
                credentials: "profile/auth.json".into(),
            },
        };

        assert_eq!(
            login_hint(&account, "grok login"),
            "run `aiwatch account login grok personal`"
        );
    }
}
