# Security Policy

## Supported versions

Security fixes are applied to the latest release and the `main` branch.

## Reporting a vulnerability

Please do not disclose security vulnerabilities in a public issue. Use the repository's
**Security → Report a vulnerability** form to submit a private report:

https://github.com/RA1NM4KER/agent-relay/security/advisories/new

Include the affected version, reproduction steps, impact, and any suggested mitigation.

## Scope and threat model

Agent Relay never reads or stores provider credentials, and runs entirely locally. The threat
model, trust boundaries and known limitations are described in [docs/security.md](docs/security.md).

## Sensitive local data

Relay state and provider config directories can contain source code, prompts and session
transcripts. Do not attach them, or logs containing them, to public issues.
