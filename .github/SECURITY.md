# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | Yes       |

## Reporting a Vulnerability

**Do NOT open a public issue for security vulnerabilities.**

Email: security@pixelperfekt.dev

You will receive a response within 48 hours. We will work with you to understand the scope and develop a fix before any public disclosure.

## Security Measures

- Automated dependency auditing via `cargo audit` and `govulncheck` (weekly)
- Agent sandbox isolation via bwrap + Landlock + cgroups v2
- No eval(), no SQL concatenation, no innerHTML
- API keys never logged (redaction in Cortex Gateway)
- FlatBuffer schema validation on all inputs
