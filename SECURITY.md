# Security policy

`limitwatch` reads OAuth credentials maintained by local provider CLIs. Credential disclosure, unsafe token persistence, authentication sent to an unintended host, and response-body leakage are security issues.

Report vulnerabilities through a private GitHub security advisory rather than a public issue. Include the affected version, platform, reproduction steps, and impact. Never include a real credential, token, or unredacted authentication file.

The project does not accept credential fixtures copied from real accounts. Tests must use synthetic values and sanitized provider responses.
