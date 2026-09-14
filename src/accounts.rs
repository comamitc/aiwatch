use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use anyhow::{Context, Result, bail};
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};

use crate::{
    config::{AccountConfig, CredentialSource},
    model::Provider,
};

pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
pub const CLAUDE_SECURE_STORAGE_DIR_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";
pub const GROK_HOME_ENV: &str = "GROK_HOME";
#[cfg(target_os = "macos")]
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const MANAGED_NAME_MAX_LEN: usize = 32;
const AUTH_OVERRIDE_ENV_VARS: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
];
const PROVIDER_HOME_ENV_VARS: [&str; 4] = [
    CLAUDE_CONFIG_DIR_ENV,
    CLAUDE_SECURE_STORAGE_DIR_ENV,
    CODEX_HOME_ENV,
    GROK_HOME_ENV,
];

#[derive(Debug, Clone)]
pub struct AccountManager {
    root: PathBuf,
    claude_binary: PathBuf,
    codex_binary: PathBuf,
    grok_binary: PathBuf,
}

impl AccountManager {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("could not determine the home directory")?;
        Ok(Self {
            root: managed_accounts_root(&home),
            claude_binary: PathBuf::from("claude"),
            codex_binary: PathBuf::from("codex"),
            grok_binary: PathBuf::from("grok"),
        })
    }

    #[cfg(test)]
    fn with_paths(root: PathBuf, binary: PathBuf) -> Self {
        Self {
            root,
            claude_binary: binary.clone(),
            codex_binary: binary.clone(),
            grok_binary: binary,
        }
    }

    pub fn add(&self, provider: Provider, name: &str) -> Result<PathBuf> {
        validate_managed_name(name)?;
        self.ensure_provider_root(provider)?;
        let profile = self.profile(provider, name);
        fs::create_dir(&profile).with_context(|| {
            format!(
                "could not create {provider} profile '{}'; use `aiwatch account login {provider} {name}` if it already exists",
                profile.display()
            )
        })?;
        set_private_directory_permissions(&profile)?;
        initialize_profile(provider, &profile)?;
        self.run_login(provider, &profile)?;
        Ok(profile)
    }

    pub fn login(&self, provider: Provider, name: &str) -> Result<PathBuf> {
        let profile = self.existing_profile(provider, name)?;
        self.run_login(provider, &profile)?;
        Ok(profile)
    }

    pub fn launch(&self, provider: Provider, name: &str, args: &[OsString]) -> Result<ExitStatus> {
        let profile = self.existing_profile(provider, name)?;
        self.run_provider(provider, &profile, args.iter().cloned())
    }

    pub fn managed_accounts(&self) -> Vec<AccountConfig> {
        discover_managed_accounts_in(&self.root)
    }

    fn profile(&self, provider: Provider, name: &str) -> PathBuf {
        self.root.join(provider.key()).join(name)
    }

    fn binary(&self, provider: Provider) -> &Path {
        match provider {
            Provider::Claude => &self.claude_binary,
            Provider::Codex => &self.codex_binary,
            Provider::Grok => &self.grok_binary,
        }
    }

    fn existing_profile(&self, provider: Provider, name: &str) -> Result<PathBuf> {
        validate_managed_name(name)?;
        let profile = self.profile(provider, name);
        let metadata = fs::symlink_metadata(&profile)
            .with_context(|| format!("managed {provider} account '{name}' does not exist"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("managed {provider} account '{name}' is not a regular directory");
        }
        if !profile.is_absolute() {
            bail!("managed account path must be absolute");
        }
        set_private_directory_permissions(&profile)?;
        Ok(profile)
    }

    fn ensure_provider_root(&self, provider: Provider) -> Result<()> {
        if !self.root.is_absolute() {
            bail!("managed account root must be absolute");
        }
        let provider_root = self.root.join(provider.key());
        fs::create_dir_all(&provider_root).with_context(|| {
            format!(
                "could not create managed account directory {}",
                provider_root.display()
            )
        })?;
        set_private_directory_permissions(&self.root)?;
        set_private_directory_permissions(&provider_root)?;
        Ok(())
    }

    fn run_login(&self, provider: Provider, profile: &Path) -> Result<ExitStatus> {
        match provider {
            Provider::Claude => self.run_provider(
                provider,
                profile,
                [OsString::from("auth"), OsString::from("login")],
            ),
            Provider::Codex | Provider::Grok => {
                self.run_provider(provider, profile, [OsString::from("login")])
            }
        }
    }

    fn run_provider<I>(&self, provider: Provider, profile: &Path, args: I) -> Result<ExitStatus>
    where
        I: IntoIterator<Item = OsString>,
    {
        let binary = self.binary(provider);
        let mut command = Command::new(binary);
        command.args(args);
        isolate_provider_process(&mut command, provider, profile);
        let status = command.status().with_context(|| {
            format!("could not run {provider} executable '{}'", binary.display())
        })?;
        if !status.success() {
            bail!("{provider} exited with status {status}");
        }
        Ok(status)
    }
}

pub fn managed_accounts_root(home: &Path) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| home.join(".local/share"))
        .join("aiwatch/accounts")
}

pub fn discover_managed_accounts(home: &Path) -> Vec<AccountConfig> {
    discover_managed_accounts_in(&managed_accounts_root(home))
}

fn discover_managed_accounts_in(root: &Path) -> Vec<AccountConfig> {
    let mut accounts = Provider::ALL
        .into_iter()
        .flat_map(|provider| discover_provider_accounts(root, provider))
        .collect::<Vec<_>>();
    accounts.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then(left.name.cmp(&right.name))
    });
    accounts
}

fn discover_provider_accounts(root: &Path, provider: Provider) -> Vec<AccountConfig> {
    let provider_root = root.join(provider.key());
    let Ok(entries) = fs::read_dir(&provider_root) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if validate_managed_name(&name).is_err() {
                return None;
            }
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return None;
            }
            let profile = entry.path();
            let credentials = managed_credential_path(provider, &profile);
            Some(AccountConfig {
                id: format!("{}:managed:{name}", provider.key()),
                name,
                provider,
                credential_source: CredentialSource::ManagedProfile {
                    profile,
                    credentials,
                },
            })
        })
        .collect()
}

pub fn validate_managed_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let valid_first = bytes.first().is_some_and(u8::is_ascii_lowercase)
        || bytes.first().is_some_and(u8::is_ascii_digit);
    let valid_rest = bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    });
    if name.len() > MANAGED_NAME_MAX_LEN || !valid_first || !valid_rest {
        bail!(
            "account name must be 1-{MANAGED_NAME_MAX_LEN} lowercase ASCII letters, digits, hyphens, or underscores, and start with a letter or digit"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn claude_keychain_service(profile: &Path) -> String {
    let digest = Sha256::digest(profile.as_os_str().as_encoded_bytes());
    let suffix = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{CLAUDE_KEYCHAIN_SERVICE}-{suffix}")
}

fn managed_credential_path(provider: Provider, profile: &Path) -> PathBuf {
    match provider {
        Provider::Claude => profile.join(".credentials.json"),
        Provider::Codex | Provider::Grok => profile.join("auth.json"),
    }
}

fn initialize_profile(provider: Provider, profile: &Path) -> Result<()> {
    if provider != Provider::Codex {
        return Ok(());
    }
    let path = profile.join("config.toml");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(b"cli_auth_credentials_store = \"file\"\n")
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

fn isolate_provider_process(command: &mut Command, provider: Provider, profile: &Path) {
    for name in PROVIDER_HOME_ENV_VARS {
        command.env_remove(name);
    }
    match provider {
        Provider::Claude => {
            command
                .env(CLAUDE_CONFIG_DIR_ENV, profile)
                .env(CLAUDE_SECURE_STORAGE_DIR_ENV, profile);
        }
        Provider::Codex => {
            command.env(CODEX_HOME_ENV, profile);
        }
        Provider::Grok => {
            command.env(GROK_HOME_ENV, profile);
        }
    }
    for name in AUTH_OVERRIDE_ENV_VARS {
        command.env_remove(name);
    }
}

fn set_private_directory_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "could not protect managed account directory {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn validates_safe_profile_names() {
        for valid in ["work", "account-2", "team_ops"] {
            validate_managed_name(valid).unwrap();
        }
        for invalid in ["", "Work", "../work", "two words", "-work"] {
            assert!(validate_managed_name(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn discovers_regular_valid_profiles_for_every_provider() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("accounts");
        for provider in Provider::ALL {
            fs::create_dir_all(root.join(provider.key()).join("work")).unwrap();
        }
        fs::create_dir_all(root.join("claude/Work")).unwrap();
        fs::write(root.join("codex/personal"), "not a directory").unwrap();

        let accounts = discover_managed_accounts_in(&root);
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].id, "claude:managed:work");
        assert_eq!(accounts[1].id, "codex:managed:work");
        assert_eq!(accounts[2].id, "grok:managed:work");
    }

    #[cfg(unix)]
    #[test]
    fn login_isolates_every_provider_and_removes_token_overrides() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let fake_binary = temp.path().join("provider-cli");
        fs::write(
            &fake_binary,
            r#"#!/bin/sh
{
  printf '%s\n' "$*"
  printf '%s\n' "$CLAUDE_CONFIG_DIR"
  printf '%s\n' "$CLAUDE_SECURESTORAGE_CONFIG_DIR"
  printf '%s\n' "$CODEX_HOME"
  printf '%s\n' "$GROK_HOME"
  printf '%s|%s|%s|%s|%s|%s|%s\n' "$ANTHROPIC_API_KEY" "$ANTHROPIC_AUTH_TOKEN" "$CLAUDE_CODE_OAUTH_TOKEN" "$OPENAI_API_KEY" "$CODEX_API_KEY" "$XAI_API_KEY" "$GROK_API_KEY"
} > "${CLAUDE_CONFIG_DIR:-${CODEX_HOME:-$GROK_HOME}}/invocation"
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_binary, fs::Permissions::from_mode(0o700)).unwrap();

        let root = temp.path().join("accounts");
        let manager = AccountManager::with_paths(root, fake_binary);
        for provider in Provider::ALL {
            let profile = manager.add(provider, "work").unwrap();
            let invocation = fs::read_to_string(profile.join("invocation")).unwrap();
            let lines = invocation.lines().collect::<Vec<_>>();
            let expected_args = if provider == Provider::Claude {
                "auth login"
            } else {
                "login"
            };
            assert_eq!(lines[0], expected_args);
            match provider {
                Provider::Claude => {
                    assert_eq!(lines[1], profile.to_string_lossy());
                    assert_eq!(lines[2], profile.to_string_lossy());
                    assert_eq!(lines[3], "");
                    assert_eq!(lines[4], "");
                }
                Provider::Codex => {
                    assert_eq!(lines[1], "");
                    assert_eq!(lines[2], "");
                    assert_eq!(lines[3], profile.to_string_lossy());
                    assert_eq!(lines[4], "");
                    assert_eq!(
                        fs::read_to_string(profile.join("config.toml")).unwrap(),
                        "cli_auth_credentials_store = \"file\"\n"
                    );
                }
                Provider::Grok => {
                    assert_eq!(lines[1], "");
                    assert_eq!(lines[2], "");
                    assert_eq!(lines[3], "");
                    assert_eq!(lines[4], profile.to_string_lossy());
                }
            }
            assert_eq!(lines[5], "||||||");
            assert_eq!(
                fs::metadata(profile).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
