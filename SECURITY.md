# Security policy

`aiwatch` reads OAuth credentials maintained by local provider CLIs. Credential disclosure, unsafe token persistence, authentication sent to an unintended host, and response-body leakage are security issues.

Managed accounts are isolated with provider-specific configuration roots. On Unix, `aiwatch` creates provider and profile directories with mode `0700`; Codex's managed `config.toml` is mode `0600`. Claude credentials use distinct macOS Keychain services when available. Codex and Grok credentials remain in their protected managed profiles.

The managed login and launch commands remove known provider API-key and token overrides from child environments. The Claude secure-storage isolation variable is undocumented provider behavior; review release notes and this implementation when upgrading Claude Code.

Report vulnerabilities through a private GitHub security advisory rather than a public issue. Include the affected version, platform, reproduction steps, and impact. Never include a real credential, token, or unredacted authentication file.

The project does not accept credential fixtures copied from real accounts. Tests must use synthetic values and sanitized provider responses.
