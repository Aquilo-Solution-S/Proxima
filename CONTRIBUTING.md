# Contributing to Proxima

Proxima is pre-release (implementation phase): Rust crates have landed under
`crates/`, `flavors/`, and `apps/`; design rationale lives in `docs/` and the
formal kernel in `docs/lean/`. This guide covers how to contribute.

## Before You Contribute

- Read [`README.md`](README.md) for project vision and scope
- Read [`docs/universe.md`](docs/universe.md) for the ontology and philosophical commitments
- Read [`AGENTS.md`](AGENTS.md) for load-bearing invariants that must not slip
- Read [`LICENSING.md`](LICENSING.md) for license and CLA requirements

## How to Contribute

### Issues & Discussions

1. **Open an Issue** for:
   - Questions about the design or the code
   - Proposals for new documentation lanes or invariant/design changes
   - Edge cases the spec does not cover
   - Contradictions you believe you've found (flag explicitly; don't paper over)

2. **Join Discussions** on existing issues/PRs to:
   - Validate a proposed design decision
   - Surface tensions between docs
   - Propose concrete alternatives with tradeoffs

### Code & Docs Contributions

- Follow Rust conventions and the style of existing code
- No `unsafe` (the workspace pins `unsafe_code = "deny"`); `warnings = "deny"`
  and `clippy::pedantic = "deny"` are enforced workspace-wide
- `cargo clippy --workspace --all-targets` and `cargo test` must be clean
- New entities ship their schema migrations; respect the load-bearing
  invariants in [`AGENTS.md`](AGENTS.md)
- Doc fixes (typos, broken links, inconsistencies) and answers to open
  `Q1-Qx` questions are welcome as PRs
- New public how-to/tutorial/reference pages belong under `docs/getting-started/`,
  `docs/tutorials/`, `docs/how-to/`, or `docs/reference/`.
- New invariant/design changes belong in the numbered docs and, when domainless,
  in `docs/lean/Foundations/` plus `docs/lean/COVERAGE.md`.
- Agent-facing usage rules belong in `docs/agent/` and the root `llms.txt` files.

Before opening a PR, run the narrowest relevant checks and always run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
git diff --check
```

Docs-only changes:

```sh
python3 scripts/check-doc-links.py
python3 scripts/check-doc-status.py
git diff --check
```

Domainless invariant changes also require:

```sh
cd docs/lean && lake build
```

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

See [`SECURITY.md`](SECURITY.md) — do **not** open public issues for
vulnerabilities; email the address listed there.

## Getting Help

- For usage questions: Open a Discussion (when enabled)
- For design questions: Open an Issue
- For commercial inquiries: `heinrich.vonhelmolt@aquilo-solutions.com`

## Recognition

All significant contributors will be acknowledged in the project's
contributors list (to be added when the first external PR is merged).
