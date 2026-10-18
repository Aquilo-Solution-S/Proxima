# AGENTS.md

Read [README](README.md) first. The [twelve-page guide](docs/index.md) owns
concepts and integration; avoid a second invariant or configuration ledger.

## Scope and authority

- The Lean kernel in `docs/lean/Causa/` states principles. Rust code and tests
  define runtime behavior. Change a principle in Lean first; `lake build` and
  `scripts/check-lean-axioms.py` check the kernel, not a Rust refinement proof.
- Types/defaults/limits: rustdoc. Tables: migrations; core functions, triggers
  and policies: `crates/storage-pg/definitions/`. Settings:
  [configuration](docs/configuration.md), checked bidirectionally by
  `scripts/check-settings.py`. Action/resource schemas: the live catalog.
- Keep work within existing crates/packages. Adding a binary, crate, service,
  migration, protocol surface, or registration path requires an explicit task;
  the pre-push review enforces this boundary. Example: a retrieval fix does
  not authorize a new inference service.
- Scratch plans and bulky review evidence belong in ignored `.local/`. This is
  a review judgment: tracked docs should survive the task; a failed-command
  transcript belongs in a ledger, a lasting decision in `docs/decisions.md`.

## Workspace

| Path | Responsibility |
|---|---|
| `apps/proxima-mcp` | Canonical MCP host; core only, links no flavor |
| `crates/core` | Domain types, engine, typed ports, registry |
| `crates/proxima` | Host facade and flavor SDK |
| `crates/storage-pg`, `crates/blob-s3` | PostgreSQL and object backends |
| `crates/mcp-server`, `crates/auth-oidc` | Transports and bearer authentication |
| `crates/llm-openai-compat` | Host-injected embedding adapter |
| `crates/pg-testkit` | Owned-server test databases/templates |
| `flavors/example` | Test and tutorial flavor (`publish = false`) |
| `tools/dev-idp`, `tools/dev-migrate` | Local issuer and database bootstrap |

## Branch and release policy

1. Push to `main` directly. History stays linear: rebase, never merge or
   force-push. Before pushing, run the complete local gates and an independent
   review of `origin/main..HEAD` by a vendor other than the one the commits'
   `Co-Authored-By` trailers name. Every rule in this file that says "review"
   means this review; nothing else reads a change before it lands.
2. `verified` is the last `main` commit that passed CI; only `verify.yml`
   moves it. Start work from `origin/verified`, not `origin/main`: commits on
   `main` past `verified` are untested and are not ground truth.
3. `verify.yml` runs CI on the newest `main` commit and batches pushes that
   land meanwhile. Green fast-forwards `verified`. Red re-runs CI on commits in
   the batch, reverts the first red one on `main`, or opens an issue naming the
   range. Only verify runs save Rust caches. PRs remain for outside
   contributors.
4. A commit that changes `proxima_core::RELEASE_VERSION` cuts `v${VERSION}`
   through `release.yml` once `verified` reaches it; an unchanged constant cuts
   no release, and no release is cut from an unverified commit. The version
   checker validates the next published number.
5. Released migrations are immutable; `scripts/check-migrations.py` pins every
   released file from `RELEASE_EPOCH`. New migration versions must be above all
   released versions in their directory. Definitions are edited in place; see
   [contributing](docs/contributing.md#migrations).

## Boundaries checked by code and review

| Boundary | Enforcer |
|---|---|
| F/A/P layering, closed pin roles, declared provenance | Typed write/operator ports; Lean principles; graph tests |
| Server-resolved owner roles and source-owned pins | `AuthzContext`, owner command ports, mandatory owner RLS |
| Optional kernel witnesses, typed schema contracts | Payload traits, registry freeze, authorized sidecar helpers |
| Build-time flavors; no raw flavor core SQL | Facade SDK, architecture/SQL guardrails |
| Atomic writes and scoped host state | `UnitOfWork`, private permits, PG rollback tests |
| Hard deletion through authorized inverses | `wipeable` principle, erase ports, inverse-completeness tests |
| Independent embeddings; similarity authors no pins | Retrieval/write separation and graph tests |
| Trusted composed SQL and closed database vocabularies | SQL-policy gate; migration catalog tests |

Judgment: if a proposed edge needs its own reason/confidence, find the node
that owns the statement before adding code. Example: an interpretation belongs
in a Perspective payload, not a third edge kind.

## Delegation

Prefer native read-only Codex workers for non-trivial orientation and dependency
tracing. After intent and invariants are clear, prefer bounded native Codex
implementation workers with disjoint file ownership and executable gates.
This is a judgment call to preserve the engineer's critical path: skip a worker
for one known `rg` lookup. Non-Codex workers need explicit user selection.

The calling engineer owns scope, architecture, actual diff review, load-bearing
verification, commits, pushes, and publication. Workers never commit or stage;
in a worktree the index can be outside their write boundary. Do not revert
other workers' edits or delegate secrets, customer data, or sensitive fixtures.
These boundaries are enforced by worker briefs and orchestrator diff review.

## Verification and failures

The complete local gates are listed once, in
[contributing](docs/contributing.md#local-verification); CI runs the same
commands. Workspace lints deny warnings and Clippy pedantic findings. Use
`cargo check` and selected nextest tests during iteration; per-package success
does not verify a flavor's served catalog. Changes to `mcp_tools` require the
host e2e path. Verify both the all-features lane and the default substrate-only
host.

PG tests require `PROXIMA_TEST_PG_URL`, an admin URL for a server you own.
Testkit sweep/GC touches every matching test database behind that URL.
`admin_url_or_skip()` skips locally and fails in CI; report every skip.

If any check fails, fix it, ledger exact command/output and a concrete deferral
reason under `.local`, or escalate a blocker. Review must account for every
observed failure; never report required checks green while one still fails.
Never weaken, delete, or ignore a surviving behavioral assertion to pass.

## Artifacts and commits

- English repository artifacts. Documentation review favors technical facts,
  signatures, tables, and links over repeated explanations: a second defaults
  table would drift from rustdoc. `check-doc-status.py` enforces twelve pages;
  link and settings gates enforce their own boundaries.
- Mermaid diagrams are inline and rendered by `pymdownx.superfences` in the
  strict MkDocs build. Keep diagrams conceptual; schemas remain generated.
- Subjects use Conventional Commits: `docs(<scope>): …`,
  `feat(<component>): …`, `fix(<component>): …`, `chore(<component>): …`.
  Use existing crate/component names. Git-cliff groups these subjects.
- Breaking changes carry an `Upgrade:` footer naming the concrete client or
  operator action. Review judgment: readers need one usable upgrade note;
  e.g. `Upgrade: Replace the retired resource with the host outbox intake.`
  `cliff.toml` renders the footer; its fixture gate verifies rendering.
- AI commits carry a `Co-Authored-By` trailer naming the model that wrote
  them, e.g. `Codex <noreply@openai.com>` or
  `Claude Opus 5.5 <noreply@anthropic.com>`. The independent review reads it
  to exclude the authoring vendor, so a borrowed trailer defeats the review.
  Add `Tesla-Valve:` trailers for decisions made under that skill.

If a requirement is ambiguous or conflicts with a principle, ask before making
an architectural choice. Judgment: such choices compound; an unfamiliar helper
name needs source inspection, a new authorization model needs agreement.
