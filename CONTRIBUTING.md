# Contributing to Proxima

Proxima is in **design phase** — no code yet. All artefacts live in `docs/`.
This guide covers how to contribute during this phase.

## Before You Contribute

- Read [`README.md`](README.md) for project vision and scope
- Read [`docs/universe.md`](docs/universe.md) for the ontology and philosophical commitments
- Read [`AGENTS.md`](AGENTS.md) for load-bearing invariants that must not slip
- Read [`LICENSING.md`](LICENSING.md) for license and CLA requirements

## How to Contribute

### During Design Phase (Current)

1. **Open an Issue** for:
   - Questions about the design
   - Proposals for new numbered docs (12+, or gaps in 01-11)
   - Edge cases the spec does not cover
   - Contradictions you believe you've found (flag explicitly; don't paper over)

2. **Open a Pull Request** for:
   - Fixes to existing docs (typos, broken links, inconsistencies)
   - Answers to open questions marked Q1-Qx in existing docs

3. **Join Discussions** on existing issues/PRs to:
   - Validate a proposed design decision
   - Surface tensions between docs
   - Propose concrete alternatives with tradeoffs

### When Code Lands

- Follow Rust conventions and the style of existing code
- No `unsafe` without exhaustive justification in PR description
- All new entities must include corresponding schema migrations

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes
4. Ensure all documentation cross-references are accurate
5. Open a PR with:
   - Clear title prefixed with doc number or component: `docs(02): ...` or `core: ...`
   - Description explaining the *why*, not just the *what*
   - References to any relevant issues
6. Wait for maintainer review and address feedback

## Commit Conventions

- Subject: `docs(<scope>): <summary>` for documentation changes (e.g., `docs(02): close Q3 strict layering`)
- Subject: `<component>: <summary>` for code changes (e.g., `core: add FactPayload trait`)
- Body: bulleted list of concrete changes; preserve the *why* when the change is a decision, not a fix
- Trailer: Include CLA sign-off (see below)

Example:
```
docs(04): resolve cycle detection edge case

- Add Supersedes edge buffering to consolidation operator
- Close Q7 from 04 with decision: buffer then deduplicate

Signed-off-by: Name <email>
```

## Contributor License Agreement (CLA)

External contributions require a **Contributor License Agreement** granting
Aquilo the right to relicense contributions as part of commercial offerings.

**Mechanism:**
1. CLA Assistant GitHub Action on the upstream repository
2. CLA text: standard form — you keep your copyright; you grant Aquilo a
   non-exclusive, irrevocable license to relicense
3. You must sign the CLA before your first PR can be merged

**Sign-off:**
- DCO sign-off (`Signed-off-by:` trailer) is welcome but does **not** substitute
  for the CLA
- Use `git commit -s` to add the sign-off automatically

## Code of Conduct

This project follows a standard open source code of conduct. Be respectful,
focus on technical facts, and keep discussions constructive. Personal attacks,
harassment, or trolling will not be tolerated.

## Reporting Security Issues

Do **not** open public issues for security vulnerabilities. Instead, email:

> Aquilo Solutions — `heinrich.vonhelmolt@aquilo-solutions.com`

Include:
- Steps to reproduce
- Impact assessment
- Suggested fix (if any)

We will acknowledge receipt within 24 hours and provide a timeline for resolution.

## Getting Help

- For usage questions: Open a Discussion (when enabled)
- For design questions: Open an Issue
- For commercial inquiries: `heinrich.vonhelmolt@aquilo-solutions.com`

## Recognition

All significant contributors will be acknowledged in the project's
contributors list (to be added when the first external PR is merged).
