# AGENTS.md

Conventions and load-bearing invariants for AI agents working in
Proxima. Read [README](README.md) first; this doc covers what's
easy to break.

## State of the repo

**Implementation phase.** The design lives in `docs/`; Rust crates and the
optional REST projection have landed.

Code work is expected when the prompt asks for it. Keep edits scoped to
the existing crates/packages; do not add new binaries, crates, services,
migrations, protocol surfaces, or runtime registration paths without an
explicit request.

## Where the design lives

| Doc | Topic |
|---|---|
| `docs/universe.md` | Origin doc: ontology, the Spinning Wheel, three Realities |
| `docs/01-event-source.md` | Membrane between Reality and the agent; `Owner` scoping |
| `docs/02-memory.md` | F/A/P layering, edges, directionality |
| `docs/16-edges.md` | the edge model: two closed kinds, kind-follows-operation, rebuildability |
| `docs/03-schema-registry.md` | Payload traits (`FactPayload`, `AbstractionPayload`, `PerspectivePayload` — all required), sidecar tables, migration |
| `docs/04-consolidation.md` | F→A and A→P operators; prompt locality |
| `docs/05-actions.md` | Actions as ordinary Facts; wake/tool boundary |
| `docs/06-goals-and-self.md` | Goal entity (DAG, supersession); Self as pure query |
| `docs/07-storage.md` | IDs, identity rules, append-only, vector store independence |
| `docs/08-core-and-flavors.md` | Bare core / flavor layering; `proxima-mcp` default-on `code` packaging feature |
| `docs/09-developing-flavors.md` | Agent implementation checklist for flavor crates: typed keys, sidecars, registration, migrations, tools |
| `docs/10-configuration.md` | Env config surface (Postgres/MCP/S3), MCP auth modes, host-injected embedding client; no inference targets/tiers |
| `docs/11-citations.md` | `CitedObject` / `CitationMapping` traits; bibliographic provenance, the Fact ∪ Abstraction citation rule |
| `docs/12-tool-manifest.md` | Tool = build-time registered call surface; tool classes (core MCP / flavor MCP); no runtime tier |
| `docs/13-compliance.md` | Compliance primitives: owner deletion, source-scope deletion, pause/resume, export, suppression, audit |
| `docs/14-protocol-surface.md` | Engine's contract to clients: five verbs (Query / ChangeHistory / GoalWrite / FactIngest / Schema), owner-scoped, transport-agnostic |
| `docs/15-deployment.md` | Deploying the Code-flavor MCP server: Docker, OIDC bearer auth, network exposure, tool-surface profiles |
| `docs/17-rest-surface.md` | Current build/runtime-opt-in REST projection of the tool manifest: derived routes, HTTP status map, OpenAPI |

## Workspace layout

```
proxima/
├── apps/
│   └── proxima-mcp/         canonical MCP host (code flavor default-on)
├── crates/
│   ├── auth-oidc/           Rust OIDC JWT authenticator crate
│   ├── blob-s3/             Rust S3 cited-blob service crate
│   ├── core/                Rust lib crate `proxima-core`
│   ├── llm-openai-compat/   Rust OpenAI-compatible embedding client crate
│   ├── mcp-server/          Rust MCP HTTP listener crate (`proxima_mcp_server`)
│   ├── pg-testkit/          Rust Postgres test helper crate
│   ├── proxima/             Rust framework facade crate
│   └── storage-pg/          Rust Postgres storage crate
├── flavors/
│   └── code/                Rust code flavor crate
├── tools/
│   ├── dev-idp/             loopback OIDC issuer for local MCP
│   └── dev-migrate/         headless substrate+flavor migration runner (dev DB bootstrap)
├── docs/                    design rationale + commentary
│   └── lean/                **Lean kernel — THE source of truth** (see below)
└── Cargo.toml               Rust workspace
```

## Kernel authority

The Lean kernel at `docs/lean/Causa/` is the source of truth for
Proxima's domainless invariants (F/A/P layering, edges, operators, owner
scoping, goals, citations, compliance, composition). The prose docs under
`docs/*.md` are rationale and commentary. When code or docs disagree with the
kernel, **the kernel wins** until renegotiated in writing. Check it with
`cd docs/lean && lake build`; coverage of doc invariants is tracked in
`docs/lean/COVERAGE.md`.

## Pre-stable breaking refactor (v0.0.4 / v0.0.5 — released)

`v0.0.4` and `v0.0.5` are tagged. Both shipped breaking Rust, storage, and
MCP/API changes that removed obsolete ontology rather than preserve adapters.
Keep the detailed roadmap/matrix in ignored `.local/` planning artifacts;
tracked repo changes carry only durable, condensed rules and executable checks.

Branch policy:

1. `main` is PR-only (required CI checks + `enforce_admins`); no direct local pushes.
2. Post-v0.0.5 work continues on short reviewed branches targeting `main`, one
   slice per branch unless Heinrich explicitly stages several together.
3. Tag a new `v*` from `main` only after all required slices merge and
   post-merge CI passes (release notes are git-cliff-generated on the tag).

The v0.0.4 breaking-deletion target (now shipped) removed production
compatibility for legacy principal/read-scope APIs,
materialized Personality/Self authz, owner-reachability compatibility,
core Event/EventSource identity, legacy Goal parent tables,
public aggregate `Storage`, raw flavor `PgPool` / core-table SQL capability,
and stale MCP/wire names. Do not weaken the Lean guardrails: server-resolved
`OwnerRef`, source-owned index rows with target redaction,
optional Memory/Goal sidecars and receipts, `MemoryGraphValid`,
`OperatorInvocation` completeness for writes that declare a derivation,
abandonment-only hard deletion, build-time flavor registries,
set-based authorized reads, and atomic command-port writes.

## Agent operating discipline

- Keep LLM-internal planning, scratch roadmaps, review ledgers, prompt drafts,
  and bulky analysis under ignored `.local/`. Tracked docs should contain only
  durable decisions, condensed rules, public rationale, and executable checks.
- If you observe an error, failing check, stale contradiction, security concern,
  flaky command, or tool failure, do not dismiss it as "not mine". Either:
  1. fix it inside the accepted scope;
  2. record it in a `.local/<topic>/...` ledger with command/output and why it
     is deferred; or
  3. escalate to the user when it blocks correctness, safety, or verification.
- Never report success while known required checks fail. If a failure is
  unrelated to the requested slice, say so explicitly and ledger it; do not hide
  it or normalize it away.
- Prefer small tracked changes plus rich `.local` evidence over broad prose that
  will rot. When a durable rule changes, update Lean/COVERAGE or this file;
  keep speculative implementation plans out of tracked docs.

## Verification

Use the smallest relevant check:

| Surface | Command |
|---|---|
| Rust workspace | `cargo check --workspace` |
| Rust lint | `cargo clippy --workspace --all-targets` |

`cargo nextest run` is the fast path for the Rust suite; `cargo test`
still works as fallback. PG tests clone a pre-migrated template DB.
Single-test selection: `cargo nextest run -E 'test(<name>)'`.

`cargo nextest run --workspace` covers `apps/proxima-mcp` OIDC e2e (code
flavor is the host default). REST still needs
`cargo nextest run -p proxima-mcp --features rest --test oidc_e2e`.
Substrate-only: `--no-default-features`. Touching the flavor's `mcp_tools`
without the host e2e passes the flavor crate and fails the served tool list.

## Delegated agents (Codex / Vibe execution runs)

Rules for non-interactive agents executing a scoped brief in this repo:

- Lint gates are workspace-level and non-negotiable: `warnings = "deny"`,
  `clippy::pedantic = deny`. Code is not done until
  `cargo clippy --workspace --all-targets` is clean.
- Tests: per-package `cargo test -p <crate> --lib` plus named
  integration targets; PG-gated tests (e.g. `--test boot_pg`) need the
  local dev Postgres and may be skipped when it is absent — say so.
- Never commit; the orchestrator reviews the diff and commits.
- Never weaken, delete, or `#[ignore]` a test assertion to get green —
  restructure the implementation instead.
- In git-worktree checkouts the index lives outside your write
  boundary: no `git add`/`git mv`/`git rm`. Use plain `mv`/`cp`/`rm`;
  rename detection happens at commit time.
- Rust 2024, let-chains available; match surrounding style (see the
  Verification table above for the smallest relevant check).

## MCP — live tool surface for agents

`cargo run -p proxima-mcp` exposes a Streamable HTTP MCP server
(default `http://localhost:31415/mcp`). Client config and port override:
[README §Connecting Your Coding Agent To Proxima](README.md#connecting-your-coding-agent-to-proxima).

Treat the runtime MCP catalog as live implementation state, not kernel truth:
`proxima://tools`, MCP `resources/list`, and `resources/templates/list` are the
catalog authority for a running server. Current tool names may still contain
pre-v0.0.4 vocabulary; do not preserve stale names as architecture precedent.
The v0.0.4 refactor may break/rename MCP and wire surfaces to match the Lean
kernel.

Proxima self-ingests its own commits and chunks, so the graph holds this repo's
causal chain. Prefer MCP queries over re-grepping when investigating commit or
chunk history, but write findings back only through currently authorized tools
and only when they satisfy the Lean invariants below.

## Kernel guardrails — do not violate

Do not maintain a second long invariant ledger here. The Lean kernel and
`docs/lean/COVERAGE.md` are authoritative; this section is only the condensed
runtime checklist most likely to prevent regressions.

- **Layering:** strict F/A/P/Goal layering; no upward or similarity-authored
  edges. Similarity is query-time only.
- **Ownership/authz:** rows carry stable `OwnerRef`; the host resolves through
  `OwnerState` / `OwnerAccessPort` into roles. No org predicate, ACL/share set,
  retired read-scope API, materialized Personality/Self, or caller-supplied
  resolved owner may authorize access.
- **Facts:** Facts are admitted `Memory` rows; receipts prove admission only,
  not external truth. Fact identity is the row id, not content hash, source id,
  or receipt id.
- **Sidecars:** Memory/Goal sidecars and Fact receipts are optional kernel
  witnesses. Edges have no sidecar at all — there is no `OptionalEdgeSidecar`
  in `Causa.Flavor`. A schema/engine contract may require a typed sidecar; the
  kernel never requires a global sidecar nor permits untyped JSON escape
  hatches for typed payloads.
- **Pins (no Edge table):** `origins[]` (made-from) and `refs[]` (points-at)
  live on the Memory row and pin target `t`. Two closed kinds; the kind follows
  the operation; no verb writes a pin. Rebuildability is identity
  (`derivePins`). Target render is hot / Cold / Unavailable. There is no
  `Edge` / `FactEntity` / relation registry / follow-at-read. See Lean
  `Causa.Edges` and `.local/timeseries-core/03-signoff-uml.md`.
- **Provenance/operators:** derived rows are valid only in an admitted table
  graph (`MemoryGraphValid`). Operator outputs carry an `OperatorInvocation`
  manifest/witness proving declared-input provenance/evidence completeness — a
  write that declares NO derivation (an interpretation Perspective grounding
  through its references) declares no inputs and carries no manifest, which is
  legal and is the E4 case the kernel accommodates.
- **Goals/Self/Wake:** Goals are structural entities. Lifecycle supersession is
  row-local; topology/assignment/evidence are Goal row columns
  (`dependency_goal_ids`, `assignment_perspective_id`, `evidence_memory_ids` —
  `Goal.dependencies` / `.assignment` / `.evidence` in the kernel) from which
  the index entries are derived. Memory supersession and authorship are row
  columns too (`Memory.supersedes`, `Memory.authoring_perspective`). Self is a
  query, never a row. Wake is armed Goal behavior, not a separate kernel entity.
- **Citations/compliance/embeddings:** citations are `blob_id` 0..1 on
  Fact ∪ Abstraction (a Perspective never cites). Hard deletion is
  `wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)`; World never
  abandoned. Embeddings are independent rows and never graph authors.
- **Flavor/API/storage:** flavor composition is build-time; no runtime registry
  or plugin tier. Flavor code must use authorized helpers/private permits, not
  raw core-table SQL. Writes are atomic command-port operations or explicit
  backend-owned UnitOfWork. Closed DB vocabularies use SQL enums.

## Doc conventions

- **Lead with technical facts.** Structs, signatures, set notation,
  tables. One-line whys. No expository prose.
- **No prose padding.** If a paragraph could be deleted without
  losing information, delete it.
- **Cross-reference numbered docs** by section: "(see 06 §Bootstrap)"
  rather than restating.
- **Diagrams in ASCII.** Match the existing tree / box style in
  README and 02.

## Commit conventions

- Docs subject: `docs(<scope>): <summary>` — e.g.
  `docs(02): close Q3 strict layering`.
- Code subject: `feat(<component>): <summary>` /
  `fix(<component>): <summary>` / `chore(<component>): <summary>`.
  Components include `core`, `proxima-mcp`, `storage-pg`, `mcp-server`,
  `llm-openai-compat`, `blob-s3`, `auth-oidc`, `proxima`,
  `pg-testkit`, `flavors-code`, `tools`.
- Body: bulleted list of concrete changes; preserve the *why* when
  the change is a decision, not a fix.
- Co-authorship trailer for AI commits matches the parent CLAUDE.md
  convention.

## Common pitfalls

- Treating tracked planning prose as durable architecture. Put bulky plans,
  prompts, intermediate reviews, and ledgers in `.local/`; track only condensed
  decisions and checks.
- Seeing a failing command or contradiction and moving on silently. Fix it,
  ledger it in `.local`, or escalate.
- Preserving pre-v0.0.4 compatibility aliases for ontology the kernel removed.
  Delete stale surfaces instead of rebranding them.
- Authorizing through org, owner equality, read-scope, personality, Self, or
  caller-provided roles instead of server-resolved `OwnerRef` roles.
- Making edge targets decide edge-row visibility. Source-readable edges remain;
  targets redact independently.
- Making sidecars or Fact receipts globally mandatory, or replacing typed
  schema contracts with untyped JSON maps.
- Restoring runtime registration, raw flavor writes/reads against core tables,
  or public access to storage internals.
- Turning compliance into broad source-scope deletion. Hard delete needs
  abandonment proof.
- Using embeddings/similarity to author a connection.
- Reaching for a verb that writes an edge, or a third edge kind. A feature that
  seems to need one fails the node-home test and is missing a node.
- "Fixing" terse docs by adding explanatory paragraphs instead of technical
  facts, signatures, tables, and cross-references.

## When unsure

- Ambiguity in spec → ask before deciding. Architectural choices
  compound.
- Tension between two docs → flag it explicitly; don't paper over
  it with a third interpretation.
- A new component appears warranted → propose first, draft second.
  The numbered-doc sequence is intentional.

## Rules

- Answer always precise, not in prose. Dense short fact based
answers are preffered.
