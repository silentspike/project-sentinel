# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | Yes       |
| < latest | No       |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **DO NOT** create a public GitHub issue
2. Use GitHub's private vulnerability reporting:
   Settings > Security > Advisories > Report a vulnerability

### What to include
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response Timeline
- **Acknowledgment:** Within 48 hours
- **Assessment:** Within 7 days
- **Fix:** Depends on severity (Critical: 72h, High: 2 weeks)

## Security Measures
- All dependencies regularly updated via Dependabot
- CodeQL SAST scanning on every push/PR
- Cargo audit, govulncheck, npm audit in CI pipeline
- Secret scanning enabled
- Unicode/Bidi character detection in CI lint step
