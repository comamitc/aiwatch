# aiwatch

`aiwatch` is a local-first terminal dashboard for Claude Code, OpenAI Codex, and Grok usage limits. It displays provider-reported utilization, reset times, account health, and seven-day local history without copying provider credentials into its configuration or database.

## Status

Early public release. The quota endpoints used by the official clients are not public API contracts and may change without notice. Each provider is isolated so a failure or schema change does not hide data from the others.

## Features

- All-account summary dashboard with an optional focused account view
- Claude five-hour, weekly, and model-scoped weekly windows
- Codex primary five-hour and secondary weekly windows
- Grok weekly or monthly included allowance and product breakdown
- Multiple independently authenticated Claude Code, Codex, and Grok accounts
- Multiple named accounts through explicit credential-file paths
- Independent provider errors, authentication states, and stale data
- SQLite-backed seven-day daily-peak charts
- Static text and JSON output for scripts and status lines
- No telemetry, hosted service, credential copying, or automatic token refresh

## Install

Build from source with a current Rust toolchain:

```console
cargo install --path .
```

Run the secret-free demonstration dashboard:

```console
aiwatch --demo
```

## Usage

```console
aiwatch                       # interactive dashboard
aiwatch --once                # static terminal snapshot
aiwatch --json                # one machine-readable snapshot
aiwatch --provider claude     # one provider
aiwatch --provider claude,codex
aiwatch --interval 600        # provider polling interval, minimum 300s
aiwatch --no-history          # disable local SQLite snapshots
aiwatch --demo --once         # safe static preview
```

Interactive keys:

| Key | Action |
| --- | --- |
| `q` or `Ctrl-C` | Quit |
| `r` | Refresh when the provider-safe interval is due |
| `0` | Include accounts from all providers |
| `1`, `2`, `3` | Include only Claude, Codex, or Grok accounts |
| `Tab` or `v` | Switch between the all-account summary and focused account view |
| `j` / `k` | Scroll the summary or select the next/previous focused account |
| Left/right arrows | Select the next/previous account in focused view |
| Up/down arrows or Page Up/Page Down | Scroll the current view |
| `w` | Toggle weekly-only view |

## Multiple accounts

The provider CLIs normally keep one active local login. `aiwatch` creates an isolated configuration and credential slot per provider account, then delegates authentication to the installed official CLI:

```console
aiwatch account add claude work
aiwatch account add claude personal
aiwatch account add codex work
aiwatch account add codex personal
aiwatch account add grok work
aiwatch account add grok personal
aiwatch account list
```

Each `add` command runs that provider's normal login flow: `claude auth login`, `codex login`, or `grok login`. Profile names must be 1–32 lowercase letters, digits, hyphens, or underscores and must start with a letter or digit.

Launch simultaneous sessions in their matching account slots:

```console
aiwatch account run claude work
aiwatch account run codex personal
aiwatch account run grok work
aiwatch account run claude work -- --model opus
```

Reauthenticate an existing profile when needed:

```console
aiwatch account login claude work
aiwatch account login codex personal
aiwatch account login grok work
```

Managed profiles are discovered automatically by the dashboard. They live under the operating system's local data directory: `~/Library/Application Support/aiwatch/accounts/<provider>/` on macOS and `${XDG_DATA_HOME:-~/.local/share}/aiwatch/accounts/<provider>/` on Linux.

Isolation uses each CLI's own configuration root:

| Provider | Isolated root | Credential storage |
| --- | --- | --- |
| Claude Code | `CLAUDE_CONFIG_DIR` and `CLAUDE_SECURESTORAGE_CONFIG_DIR` | Distinct macOS Keychain service; profile `.credentials.json` fallback elsewhere |
| Codex | `CODEX_HOME` | Profile `auth.json`; the managed profile sets `cli_auth_credentials_store = "file"` |
| Grok | `GROK_HOME` | Profile `auth.json` |

`CLAUDE_SECURESTORAGE_CONFIG_DIR` is an undocumented Claude Code behavior, not a supported Anthropic API contract, and may change in a future release. Current Claude Code versions derive a distinct macOS Keychain service from its exact value. `aiwatch` supplies the same stable absolute directory to both Claude variables so login, launch, and dashboard lookup address the same slot. `CODEX_HOME`, Codex's file credential mode, and `GROK_HOME` are provider-supported behavior.

Provider API-key and token environment variables are removed from managed child processes so they cannot silently replace the selected subscription login. `aiwatch` does not infer or print account email addresses.

## Default credentials

When a provider has no managed profile and no explicit account configuration is present, `aiwatch` discovers credentials maintained by the official CLI:

| Provider | Default credential file |
| --- | --- |
| Claude Code | `~/.claude/.credentials.json` when present |
| Codex | `~/.codex/auth.json` |
| Grok | `~/.grok/auth.json` |

A managed profile suppresses automatic discovery of that provider's `default` profile, preventing stale or duplicate credentials from appearing beside named accounts. Add the default credential path explicitly under a unique name if both profiles are intentional.

Grok also respects `GROK_AUTH_PATH` and `GROK_HOME`. On macOS, use a managed Claude account because the default Claude login is normally stored in Keychain rather than `~/.claude/.credentials.json`.

`aiwatch` reads credentials in memory for authenticated quota requests. It never copies tokens into its configuration or history database. It does not refresh OAuth tokens itself. Re-run the matching `aiwatch account login` command when a dashboard profile requires authentication; running the provider through `aiwatch account run` preserves that provider's normal refresh behavior.

API keys do not expose consumer subscription allowances. The provider CLI must be logged into the subscription account.

## Explicit account paths

Create `~/.config/aiwatch/config.toml` to add credential files maintained outside `aiwatch`:

```toml
poll_interval_secs = 60
history_path = "~/.local/share/aiwatch/history.sqlite3"

[[accounts]]
name = "work"
provider = "claude"
credentials = "~/.profiles/claude-work/.credentials.json"

[[accounts]]
name = "personal"
provider = "codex"
credentials = "~/.codex/auth.json"

[[accounts]]
name = "personal"
provider = "grok"
credentials = "~/.grok/auth.json"
```

Managed profiles are included in addition to explicitly configured accounts. The configuration contains paths and display names only. Do not put tokens or API keys in it.

## Data semantics

`aiwatch` keeps provider quota separate from locally observed activity:

- Bars and reset times come from provider account responses.
- Percentages mean consumed allowance, not percent remaining.
- Seven-day sparklines are local daily peaks recorded by `aiwatch`.
- Grok may expose only a weekly or monthly allowance. `aiwatch` does not manufacture a session limit.
- Token or message denominators are not shown unless a provider reports them authoritatively.
- Percentages from differently sized accounts are never averaged into a misleading combined quota.

The dashboard's nearest-cap value is calculated from the same normalized windows used to render account bars.

## Security model

- Managed profile directories are restricted to the current user on Unix.
- Claude managed credentials remain in separate macOS Keychain entries when available; other managed credentials remain in owner-protected provider profiles.
- Credential bodies and bearer tokens are wrapped in zeroizing memory.
- Credential structures do not implement `Debug`.
- HTTP response bodies are never included in errors.
- JSON output contains usage data and profile names, never credential paths or tokens.
- Provider URLs are compiled into the binary. Configuration cannot redirect credentials to another host.
- OAuth refresh tokens are never used or modified by `aiwatch`.
- History contains account profile identifiers, percentages, reset timestamps, and observation times only.
- The project has no analytics or network service other than the three provider quota requests and the official provider login flows it launches.

Account profile names appear in terminal and JSON output. Use non-identifying names if output may be shared.

## Provider limitations

The providers officially expose usage through their own applications, but the authenticated HTTP interfaces used by those applications are undocumented. A provider can change its endpoint, headers, authentication, or response schema at any time. `aiwatch` treats malformed responses as provider errors rather than displaying zero usage.

Polling is limited to at least five minutes, and requests to multiple accounts on the same provider are staggered. A transient HTTP `429` preserves the last successful data as stale while the next safe poll recovers.

## Development

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run -- --demo --once
```

All committed fixtures are synthetic and contain no real account identifiers or credentials.

## License

MIT
