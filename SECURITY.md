# Security Policy

## Reporting a Vulnerability

Do **not** open public issues for security vulnerabilities. Instead, email:

> Aquilo Solutions — `heinrich.vonhelmolt@aquilo-solutions.com`

Include:

- Affected component (doc reference, file, or commit if code has landed)
- Steps to reproduce
- Impact assessment
- Suggested fix, if any

We will acknowledge receipt within 72 hours and provide a disclosure
timeline within 7 days.

## Supported Versions

| Version | Supported |
|---|---|
| `main` | best-effort development branch |
| latest `v0.0.x` tag | security fixes when practical |
| older `v0.0.x` tags | not routinely patched |

Proxima is pre-1.0. Security fixes may land on `main` first and be released as a
new tag. If a vulnerability affects a tagged release, the advisory will state the
fixed tag or commit.

## Disclosure

We follow coordinated disclosure: a fix is prepared and released before
the vulnerability is made public. Reporters are credited unless they
request otherwise.
