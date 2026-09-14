use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::model::Provider;

pub const MIN_POLL_INTERVAL_SECS: u64 = 60;
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

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
    pub credentials: PathBuf,
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
            .unwrap_or_else(|| home.join(".config/limitwatch/config.toml"));

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
                    .join("limitwatch/history.sqlite3")
            });

        let accounts = if file.accounts.is_empty() {
            discover_accounts(&home)
        } else {
            file.accounts
                .into_iter()
                .filter(|account| account.enabled)
                .enumerate()
                .map(|(index, account)| {
                    let credentials = expand_home(&home, account.credentials);
                    AccountConfig {
                        id: format!("{}:{index}:{}", account.provider.key(), account.name),
                        name: account.name,
                        provider: account.provider,
                        credentials,
                    }
                })
                .collect()
        };

        Ok(Self {
            poll_interval_secs,
            history_path,
            accounts,
        })
    }

    pub fn retain_providers(&mut self, providers: &[Provider]) {
        if !providers.is_empty() {
            self.accounts
                .retain(|account| providers.contains(&account.provider));
        }
    }
}

fn discover_accounts(home: &Path) -> Vec<AccountConfig> {
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
    .filter(|(_, path)| path.is_file())
    .map(|(provider, credentials)| AccountConfig {
        id: format!("{}:default", provider.key()),
        name: "default".to_string(),
        provider,
        credentials,
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
}
