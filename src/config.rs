use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{accounts, model::Provider};

pub const MIN_POLL_INTERVAL_SECS: u64 = 300;
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub poll_interval_secs: u64,
    pub history_path: PathBuf,
    pub accounts: Vec<AccountConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountConfig {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub credential_source: CredentialSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    File(PathBuf),
    ManagedProfile {
        profile: PathBuf,
        credentials: PathBuf,
    },
}

impl CredentialSource {
    pub fn credentials(&self) -> &Path {
        match self {
            Self::File(path) => path,
            Self::ManagedProfile { credentials, .. } => credentials,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    poll_interval_secs: Option<u64>,
    history_path: Option<PathBuf>,
    #[serde(default)]
    accounts: Vec<FileAccount>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAccount {
    name: String,
    provider: Provider,
    credentials: PathBuf,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

const fn enabled_by_default() -> bool {
    true
}

impl AppConfig {
    pub fn load(path: Option<&Path>, interval_override: Option<u64>) -> Result<Self> {
        let home = dirs::home_dir().context("could not determine the home directory")?;
        let config_path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".config/aiwatch/config.toml"));

        let file = if config_path.exists() {
            let body = fs::read_to_string(&config_path)
                .with_context(|| format!("could not read {}", config_path.display()))?;
            toml::from_str::<FileConfig>(&body)
                .with_context(|| format!("could not parse {}", config_path.display()))?
        } else {
            FileConfig::default()
        };

        let poll_interval_secs = interval_override
            .or(file.poll_interval_secs)
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
        if poll_interval_secs < MIN_POLL_INTERVAL_SECS {
            bail!(
                "poll interval must be at least {MIN_POLL_INTERVAL_SECS} seconds to protect provider endpoints"
            );
        }

        let history_path = file
            .history_path
            .map(|path| expand_home(&home, path))
            .unwrap_or_else(|| {
                dirs::data_local_dir()
                    .unwrap_or_else(|| home.join(".local/share"))
                    .join("aiwatch/history.sqlite3")
            });

        let mut configured_accounts = file
            .accounts
            .into_iter()
            .filter(|account| account.enabled)
            .enumerate()
            .map(|(index, account)| {
                let credentials = expand_home(&home, account.credentials);
                AccountConfig {
                    id: format!(
                        "{}:configured:{index}:{}",
                        account.provider.key(),
                        account.name
                    ),
                    name: account.name,
                    provider: account.provider,
                    credential_source: CredentialSource::File(credentials),
                }
            })
            .collect::<Vec<_>>();

        let managed_accounts = accounts::discover_managed_accounts(&home);
        if configured_accounts.is_empty() {
            configured_accounts.extend(discover_default_accounts(&home, &managed_accounts));
        }
        configured_accounts.extend(managed_accounts);
        configured_accounts.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then(left.name.cmp(&right.name))
        });
        configured_accounts.dedup_by(|left, right| left.id == right.id);

        Ok(Self {
            poll_interval_secs,
            history_path,
            accounts: configured_accounts,
        })
    }

    pub fn retain_providers(&mut self, providers: &[Provider]) {
        if !providers.is_empty() {
            self.accounts
                .retain(|account| providers.contains(&account.provider));
        }
    }
}

fn discover_default_accounts(
    home: &Path,
    managed_accounts: &[AccountConfig],
) -> Vec<AccountConfig> {
    let grok_path = env::var_os("GROK_AUTH_PATH")
        .map(PathBuf::from)
        .or_else(|| env::var_os("GROK_HOME").map(|path| PathBuf::from(path).join("auth.json")))
        .unwrap_or_else(|| home.join(".grok/auth.json"));

    [
        (Provider::Claude, home.join(".claude/.credentials.json")),
        (Provider::Codex, home.join(".codex/auth.json")),
        (Provider::Grok, grok_path),
    ]
    .into_iter()
    .filter(|(provider, path)| {
        path.is_file()
            && !managed_accounts
                .iter()
                .any(|account| account.provider == *provider)
    })
    .map(|(provider, credentials)| AccountConfig {
        id: format!("{}:default", provider.key()),
        name: "default".to_string(),
        provider,
        credential_source: CredentialSource::File(credentials),
    })
    .collect()
}

fn expand_home(home: &Path, path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home.to_path_buf();
    }
    if let Some(relative) = text.strip_prefix("~/") {
        return home.join(relative);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    #[test]
    fn expands_tilde_paths() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_home(home, PathBuf::from("~/.claude/.credentials.json")),
            PathBuf::from("/home/tester/.claude/.credentials.json")
        );
        assert_eq!(
            expand_home(home, PathBuf::from("/tmp/auth.json")),
            PathBuf::from("/tmp/auth.json")
        );
    }

    #[test]
    fn managed_profile_replaces_default_for_only_its_provider() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".claude")).unwrap();
        fs::create_dir_all(temp.path().join(".codex")).unwrap();
        fs::write(temp.path().join(".claude/.credentials.json"), "{}").unwrap();
        fs::write(temp.path().join(".codex/auth.json"), "{}").unwrap();
        let managed = vec![AccountConfig {
            id: "claude:managed:personal".to_string(),
            name: "personal".to_string(),
            provider: Provider::Claude,
            credential_source: CredentialSource::ManagedProfile {
                profile: temp.path().join("managed/claude/personal"),
                credentials: temp
                    .path()
                    .join("managed/claude/personal/.credentials.json"),
            },
        }];

        let defaults = discover_default_accounts(temp.path(), &managed);

        assert!(
            defaults
                .iter()
                .all(|account| account.provider != Provider::Claude)
        );
        assert!(
            defaults
                .iter()
                .any(|account| account.provider == Provider::Codex)
        );
    }
}
