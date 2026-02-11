# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | Yes       |

## Reporting a Vulnerability

**Do NOT open a public issue for security vulnerabilities.**

### How to Report

Email: security@pixelperfekt.dev

Include:
- Description of the vulnerability
- Steps to reproduce
- Affected component (ECS, Cortex Gateway, Dashboard, Sandbox, etc.)
- Impact assessment (if known)

### Response Timeline

| Action | Timeframe |
|--------|-----------|
| Acknowledge receipt | 48 hours |
| Initial assessment | 5 business days |
| Fix development | Depends on severity |
| Public disclosure | After fix is released |

### Severity Classification

| Severity | Examples |
|----------|---------|
| **Critical** | Sandbox escape, arbitrary code execution, credential leak |
| **High** | Agent data exfiltration, fourth-wall detection bypass, auth bypass |
| **Medium** | Information disclosure, DoS, privilege escalation within sandbox |
| **Low** | Configuration issues, minor information leaks |

## Scope

### In Scope

- Rust crates (sentinel-ecs, sentinel-bio, sentinel-sandbox, etc.)
- Cortex Gateway (Go HTTP proxy)
- Dashboard (Bun + Hono backend, Vanilla JS frontend)
- Agent sandbox isolation (bwrap, Landlock, cgroups v2)
- FlatBuffer schema validation
- Wasm tool runtime (Wasmtime + Extism)

### Out of Scope

- Third-party LLM provider APIs (Claude, Ollama)
- Upstream dependencies (report to their maintainers)
- Social engineering attacks
- Physical attacks on infrastructure

## Security Measures

### Automated

- Weekly `cargo audit` + `govulncheck` via GitHub Actions
- CodeQL scanning for Go and TypeScript (weekly + on push)
- Dependabot for dependency updates (Cargo, Go, npm, Actions)
- FlatBuffer schema validation on all inputs

### Architecture

- Agent sandbox isolation: bwrap (namespace) + Landlock (FS ACL) + cgroups v2 (resource limits)
- No `eval()`, no SQL concatenation, no `innerHTML`
- API keys never logged (redaction in Cortex Gateway)
- Wasm tools run in isolated Wasmtime sandbox
- Zero-trust between agents (each agent sees only its own data)

### Development

- Pre-commit hooks enforce `cargo fmt` + `cargo clippy`
- PR review required before merge
- Conventional commits for audit trail
