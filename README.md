# limitwatch

`limitwatch` is a local-first terminal dashboard for Claude Code, OpenAI Codex, and Grok usage limits. It displays provider-reported utilization, reset times, account health, and seven-day local history without storing provider credentials.

## Status

Early public release. The quota endpoints used by the official clients are not public API contracts and may change without notice. Each provider is isolated so a failure or schema change does not hide data from the others.

## Features

- Interactive terminal dashboard built with Rust and `ratatui`
- Claude five-hour, weekly, and model-scoped weekly windows
- Codex primary five-hour and secondary weekly windows
- Grok weekly or monthly included allowance and product breakdown
- Multiple named accounts through explicit credential-file paths
- Independent provider errors, authentication states, and stale data
- SQLite-backed seven-day daily-peak sparklines
- Static text and JSON output for scripts and status lines
- No telemetry, hosted service, credential copying, or token refresh

## Install

Build from source with a current Rust toolchain:

```console
cargo install --path .
```

Run the secret-free demonstration dashboard:

```console
limitwatch --demo
```

## Usage

```console
limitwatch                       # interactive dashboard
limitwatch --once                # static terminal snapshot
limitwatch --json                # one machine-readable snapshot
limitwatch --provider claude     # one provider
limitwatch --provider claude,codex
limitwatch --interval 90         # provider polling interval, minimum 60s
limitwatch --no-history          # disable local SQLite snapshots
limitwatch --demo --once         # safe static preview
```

Interactive keys:

| Key | Action |
| --- | --- |
| `q` or `Ctrl-C` | Quit |
| `r` | Refresh when the provider-safe interval is due |
| `0` | Show all providers |
| `1`, `2`, `3` | Show Claude, Codex, or Grok |
| `j` / `k` or arrows | Scroll |
| `w` | Toggle weekly-only view |

## Credentials

By default, `limitwatch` discovers the credential files maintained by the official CLIs:

| Provider | Default credential file |
| --- | --- |
| Claude Code | `~/.claude/.credentials.json` |
| Codex | `~/.codex/auth.json` |
| Grok | `~/.grok/auth.json` |

Grok also respects `GROK_AUTH_PATH` and `GROK_HOME`.

`limitwatch` reads these files in memory for authenticated quota requests. It never copies tokens into its configuration or history database. It does not refresh OAuth tokens. If a token expires, log in again using the corresponding official CLI.

API keys do not expose consumer subscription allowances. The provider CLI must be logged into the subscription account.

## Multiple accounts

Create `~/.config/limitwatch/config.toml` and point each profile at a credential file maintained for that account:

```toml
poll_interval_secs = 60
history_path = "~/.local/share/limitwatch/history.sqlite3"

[[accounts]]
name = "work"
provider = "claude"
credentials = "~/.profiles/claude-work/.credentials.json"

[[accounts]]
name = "personal"
provider = "claude"
credentials = "~/.claude/.credentials.json"

[[accounts]]
name = "work"
provider = "codex"
credentials = "~/.profiles/codex-work/auth.json"

[[accounts]]
name = "personal"
provider = "grok"
credentials = "~/.grok/auth.json"
```

The configuration contains paths and display names only. Do not put tokens or API keys in it.

## Data semantics

`limitwatch` keeps provider quota separate from locally observed activity:

- Bars and reset times come from provider account responses.
- Percentages mean consumed allowance, not percent remaining.
- Seven-day sparklines are local daily peaks recorded by `limitwatch`.
- Grok may expose only a weekly or monthly allowance. `limitwatch` does not manufacture a session limit.
- Token or message denominators are not shown unless a provider reports them authoritatively.
- Percentages from differently sized accounts are never averaged into a misleading combined quota.

The dashboard's nearest-cap value is calculated from the same normalized windows used to render account bars.

## Security model

- Credential bodies and bearer tokens are wrapped in zeroizing memory.
- Credential structures do not implement `Debug`.
- HTTP response bodies are never included in errors.
- JSON output contains usage data and profile names, never credential paths or tokens.
- Provider URLs are compiled into the binary. Configuration cannot redirect credentials to another host.
- OAuth refresh tokens are never used or modified.
- History contains account profile identifiers, percentages, reset timestamps, and observation times only.
- The project has no analytics or network service other than the three provider quota requests.

Account profile names appear in terminal and JSON output. Use non-identifying names if output may be shared.

## Provider limitations

The providers officially expose usage through their own applications, but the authenticated HTTP interfaces used by those applications are undocumented. A provider can change its endpoint, headers, authentication, or response schema at any time. `limitwatch` treats malformed responses as provider errors rather than displaying zero usage.

Polling is limited to at least 60 seconds. HTTP `429` responses preserve the last successful data and are shown explicitly.

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
