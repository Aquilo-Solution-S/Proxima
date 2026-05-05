# AGENTS.md

Conventions and load-bearing invariants for AI agents working in
Proxima. Read [README](README.md) first; this doc covers what's
easy to break.

## State of the repo

**Implementation phase.** The design lives in `docs/`; Rust crates and
the Solid/Tauri frontend have landed. Do not treat README's old
"no code yet" wording as authoritative.

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
| `docs/03-schema-registry.md` | Payload traits (`FactPayload`, `AbstractionPayload`, `PerspectivePayload` — all required), sidecar tables, migration |
| `docs/04-consolidation.md` | F→A and A→P operators; prompt locality |
| `docs/05-actions.md` | Actions as `system`-source events; tool registry |
| `docs/06-goals-and-self.md` | Goal entity (DAG, supersession); Self as pure query |
| `docs/07-storage.md` | IDs, identity rules, append-only, vector store independence |
| `docs/08-core-and-flavors.md` | Bare core / flavor layering, no-feature-flags |
| `docs/09-frontend.md` | Tauri 2 + Solid; mobile + offline + schema-aware components |
| `docs/10-configuration.md` | Model tiers (`Fast`/`Standard`/`Deep`), build-time model registry, BYOK credential resolution, operator concurrency, deployment shapes |
| `docs/11-citations.md` | `CitedObject` / `CitationMapping` traits; bibliographic provenance, Fact-only citation rule |
| `docs/12-tool-manifest.md` | T1 (runtime, schema-consuming) vs T2 (build-time flavors) tool tiers |
| `docs/13-flavor-marketplace.md` | Substrate + reference flavors; independent authorship; composite discipline |
| `docs/14-protocol-surface.md` | Engine's contract to clients: five verbs (Query / Subscribe / GoalWrite / EventIngest / Schema), owner-scoped, transport-agnostic |
| `docs/15-compliance.md` | Compliance primitives: owner deletion, pause/resume, export, suppression, audit |

## Workspace layout

```
proxima/
├── core/                    Rust lib crate `proxima-core`
├── bin/
│   ├── proxima-engine/      Rust engine binary
│   └── proxima-code/        Rust code-flavor binary
├── flavors/
│   └── code/                Rust code flavor crate
├── storage-pg/              Rust Postgres storage crate
├── wire-grpc/               Rust gRPC wire crate
├── llm-ollama/              Rust Ollama provider crate
├── frontend-core/           npm package `@proxima/core`
├── proxima-shell/           Solid + Vite + Tauri 2 shell
│   └── src-tauri/           Tauri Rust crate
├── docs/                    design source of truth
├── Cargo.toml               Rust workspace
└── pnpm-workspace.yaml      frontend workspace
```

## Verification

Use the smallest relevant check:

| Surface | Command |
|---|---|
| Rust workspace | `cargo check --workspace` |
| Rust lint | `cargo clippy --workspace --all-targets` |
| Core frontend | `pnpm --filter @proxima/core typecheck` |
| Shell frontend | `pnpm --filter proxima-shell typecheck` |
| Shell build | `pnpm --filter proxima-shell build` |

Frontend dev server: `pnpm --filter proxima-shell dev --host 127.0.0.1`.
If port `1420` is occupied, Vite will choose another port.

## Invariants — must not violate

Each rule is a one-liner pointer to its source-of-truth section.
Detail lives in the linked doc — restating here drifts. The wheel
breaks if these slip.

1. **Strict F/A/P layering — `layer(src) ≥ layer(tgt)`.** No upward
   edges; descriptor masks may tighten, never relax.
   See [02 §Edges](docs/02-memory.md#edges).
2. **Facts immutable at the identity layer.** Schema migration moves
   sidecar bytes, not Fact identity.
   See [03 §Schema evolution](docs/03-schema-registry.md#schema-evolution-code--migration),
   [07 §Identity rules](docs/07-storage.md#identity-rules).
3. **Append-only; the only `DELETE` path is GDPR erasure.** Supersession
   is the lifecycle mechanism for **A, P, and Goals only** — `new row +
   supersedes` carrying lineage in `personality_id` (A/P) or by
   `Core(Engine)` / `Core(User)` authorship (Goals). Facts have no
   `supersedes` link: each Fact is one observation, immutable, never
   replaced. Stateful Fact projections (file revisions, snapshot
   indexes) express "current state" via head-by-natural-key queries
   on the sidecar, not via lineage replacement.
   See [07 §Append-only](docs/07-storage.md#append-only),
   [02 §Re-derivation and supersession](docs/02-memory.md#re-derivation-and-supersession),
   [03 §Stateful Fact schemas](docs/03-schema-registry.md#stateful-fact-schemas--head-by-natural-key).
4. **Owner is per-row; `org_id` is not in the access predicate.**
   Cross-owner edges rejected.
   See [01 §Owner — scoping primitive](docs/01-event-source.md#owner--scoping-primitive).
5. **Vector store is independent — no FK from entity to embedding.**
   Re-embedding is a new row.
   See [07 §Vector store — independent](docs/07-storage.md#vector-store--independent).
6. **Edges are LLM- or operator-justified, never similarity-wired.**
   Cosine proximity is query-time only.
   See [02 §Why this layering — the trauma test](docs/02-memory.md#why-this-layering--the-trauma-test).
7. **Schemas, tools, sources, prompts, relations are build-time
   only.** No runtime registration tier; no `Registrant` enum.
   See [08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism).
8. **No feature flags for flavor inclusion.** The flavor crate is
   the unit of inclusion.
   See [08 §No feature flags](docs/08-core-and-flavors.md#no-feature-flags).
9. **Prompts live in flavor, alongside their operators.** Core ships
   the dispatcher and template interface only.
   See [04 §Prompt locality](docs/04-consolidation.md#prompt-locality).
10. **No `description` field on Memory.** Facts render on-demand; A/P
    text is operator-authored once and immutable.
    See [02 §The core entity](docs/02-memory.md#the-core-entity).
11. **Goals are an entity, not a Memory kind.** Fixed shape;
    supersession-only lifecycle.
    See [06 §Goal entity](docs/06-goals-and-self.md#goal-entity).
12. **Self has no entity — pure query `(P_active(ω), G_active(ω))`.**
    Never cache as a row.
    See [06 §Self — flavor projection](docs/06-goals-and-self.md#self--flavor-projection).
13. **Relations typed and flavor-registered.** Every `edge.relation.id`
    must resolve to a registered `RelationDescriptor`; engine rejects
    unregistered ids (Phase-1 EventSource edges included).
    See [02 §Edges](docs/02-memory.md#edges),
    [08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism).
14. **Causal chains are queries, not entities.** Materialized view
    permitted as perf cache only, never authoritative.
    See [02 §Causal chain query](docs/02-memory.md#causal-chain-query).
15. **Typed A/P payloads required, selective scaffolding.** Every A/P
    writes a typed sidecar row alongside `text`; no JSON escape hatch.
    See [03 §Sidecar tables](docs/03-schema-registry.md#sidecar-tables).
16. **Edges between sets are owned top-down; never co-owned.** F→A
    emits A→F provenance only.
    See [02 §Edges](docs/02-memory.md#edges).
17. **UUIDv7 vs ContentHash split is fixed.** `EdgeId` is a sum
    (`EventSourceAuthored` ContentHash, `OperatorAuthored` UUIDv7);
    `EventId` is ContentHash; `MemoryId` / `GoalId` /
    `CitedObjectId` / `CitationMappingId` / `SourceBatchId` are UUIDv7.
    Fact identity is **not** the content hash.
    See [07 §ID types](docs/07-storage.md#id-types),
    [07 §Identity rules](docs/07-storage.md#identity-rules).
18. **Citations are bibliographic and Fact-only.** A/P have no
    `citation_mapping_id` (provenance closes to Facts via edges);
    edges have no `citation_id`; operator-invocation reproducibility
    is inline on the memory row, not a citation.
    See [11 §Three-layer model](docs/11-citations.md#three-layer-model),
    [11 §Operator-invocation provenance lives on the Memory row](docs/11-citations.md#operator-invocation-provenance-lives-on-the-memory-row).
19. **Source-batch lifecycle is core, not flavor-typed.** Fixed
    `source_batches` shape; per-(batch, operator, personality) F→A
    tracking lives in `source_batch_f2a`. Domain metadata belongs on
    a `CitedObject`, not the batch row.
    See [04 §Source-batch lifecycle](docs/04-consolidation.md#source-batch-lifecycle),
    [07 §Core tables — abstract](docs/07-storage.md#core-tables--abstract).
20. **F→A is exclusive per `(Fact schema, Abstraction schema)` pair.**
    Multiple F→A over the same Fact schema producing distinct
    Abstractions are allowed; collision is on the pair. A→P / A→Goal
    / Edge plurality is intentional. F→A is always intra-flavor.
    See [04 §Phase 2 — Personality embedding](docs/04-consolidation.md#phase-2--personality-embedding),
    [08 §Composite discipline](docs/08-core-and-flavors.md#composite-discipline).
21. **Read-scope matrix governs cross-personality reads.** Per-Owner
    boolean adjacency; identity diagonal hardcoded; F is below the
    matrix. Hashed into `personality_state_hash` so toggles produce
    different invocation keys.
    See [02 §Read-scope matrix](docs/02-memory.md#read-scope-matrix),
    [07 §Core tables — abstract](docs/07-storage.md#core-tables--abstract).

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
  Components include `core`, `frontend-core`, `proxima-shell`,
  `storage-pg`, `wire-grpc`, `llm-ollama`, `flavors-code`.
- Body: bulleted list of concrete changes; preserve the *why* when
  the change is a decision, not a fix.
- Co-authorship trailer for AI commits matches the parent CLAUDE.md
  convention.

## Common pitfalls

One-liner each; invariant carries the rule, doc carries the detail.

- Adding a `description` field to Memory — see invariant 10.
- Wiring edges from embedding similarity — see invariant 6.
- Ad-hoc relation strings without a registered `RelationDescriptor`
  — see invariant 13.
- Adding `extra: Map<String, JsonValue>` to an `AbstractionPayload`
  / `PerspectivePayload` — see invariant 15.
- Skipping the typed sidecar for a "simple" Abstraction / Perspective
  — see invariant 15.
- Emitting an A→A edge from F→A — see invariant 16.
- Restoring `Registrant::Runtime` or any runtime registration path
  — see invariant 7.
- Adding `citation_mapping_id` to an Abstraction or Perspective —
  see invariant 18.
- Restoring `Edge.citation_id` or any per-edge citation column — see
  invariant 18.
- Adding flavor-typed columns to `source_batches` — see invariant 19.
- Registering two F→A operators on the same `(Fact, Abstraction)`
  pair — see invariant 20. Multiple F→A over the same Fact schema
  with distinct Abstractions is fine.
- Treating "dreaming" as a separate flavor kind or core component —
  see invariant 20.
- Conflating `source_batch_id` and `cited_object_id` — distinct
  concepts; see [04 §Source-batch lifecycle](docs/04-consolidation.md#source-batch-lifecycle).
- Materializing `chain(f)` as an authoritative table — see
  invariant 14.
- Building a runtime schema-registration HTTP endpoint — see
  invariants 7 and [08](docs/08-core-and-flavors.md).
- Adding `WHERE owner.org_id = ?` to access checks — see invariant 4.
- Caching Self as a row — see invariant 12.
- "Fixing" terse docs by adding explanatory paragraphs — see doc
  conventions.

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
