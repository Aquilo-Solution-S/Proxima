# Kernel/runtime split of crates/core — decision spec

Status: DRAFT — decisions open, no implementation plan yet.
Driver: substrate-first goal (2026-06-10) — host apps (Memophant,
working-hero) must be able to adopt the epistemic graph without
compiling the agentic runtime.

## Ground facts (module import graph, verified 2026-06-10)

Within crates/core/src, `use crate::` adjacency (top-level modules):

    approval → mcp, verbs
    auth → owner
    canonical_json → (none)
    chat → approval, mcp, personality, owner, payload, ids, error, storage, models, verbs
    citations → payload, ids
    cursor → (none)
    dependency → owner, ids, storage
    embedding_settings → models
    error → (none)
    flavor → mcp, verbs, payload, ids, relation, error, models, storage, outbox,
             secrets, citations, llm, personality, wake, harness, inference,
             intervention, chat, approval, auth
    harness → personality, mcp, verbs
    ids → (none)
    inference → error, storage, models, personality
    intervention → verbs, payload, ids, storage
    llm → models
    mcp → owner, ids, payload, relation, error, storage, verbs, personality,
          wake, harness, llm, inference, intervention, chat, approval, auth
    models → (none)
    outbox → owner, ids, payload
    owner → ids
    payload → relation, ids
    payload_contract → (none)
    relation → ids
    secrets → (none)
    storage → owner, ids, approval, chat, dependency, embedding_settings,
              inference, intervention, personality, verbs, outbox, wake, mcp,
              llm, auth
    verbs → (none)
    wake → owner, ids, payload, relation, error, storage, verbs, personality,
           mcp, llm, inference, intervention, chat, approval, auth

Kernel-clean today (no runtime imports): ids, relation, payload,
payload_contract, owner, auth, canonical_json, cursor, error, models,
secrets, citations, outbox, verbs, dependency, embedding_settings, llm.

## Fusion points (the actual work)

1. **storage.rs** — the `Storage` aggregate imports runtime stores
   (ApprovalStore, ChatStore, personality, inference, intervention,
   wake-trace; storage.rs:8-41). A split needs a kernel storage
   contract vs runtime store traits, with storage-pg implementing both.
2. **flavor.rs:84-101** — core registers its own chat/approval/
   intervention/wake-trace payloads as flavor schemas. Good news: the
   runtime's data already rides the flavor mechanism, so the runtime
   can register itself like any flavor. Decision: does that
   registration move to the runtime crate's own `register()`?
3. **mcp module** — fans out to everything (tool surface over both
   kernel verbs and runtime state). Needs a cut between substrate
   tools and runtime tools.
4. **engine/** — `Engine` holds kernel state (registry, storage,
   MemoryStore) and runtime state (wake token store, target adapter,
   dispatcher, MCP listener, LLM clients). Likely splits into a graph
   engine and a personality runtime wrapping it.

## Decisions to make

D1. Crate boundary & names: extract `proxima-kernel` and keep
    `proxima-core` as the runtime? Or `proxima-core` stays kernel and
    a new `proxima-runtime` appears? (Affects every downstream dep.)
D2. Storage contract split: one trait per side vs sub-trait
    composition; what storage-pg's migration baselines look like after.
D3. Prelude strategy: core/lib.rs is a flat `pub use module::*`
    prelude. Does the runtime crate re-export the kernel to keep the
    flat surface (zero churn for shell/flavors), or do consumers
    import the kernel explicitly (honest layering, big diff)?
D4. llm/embedding placement: `llm` only depends on `models` but
    serves consolidation (kernel-adjacent) AND the personality loop.
    Which side owns the client traits?
D5. Does the runtime register its schemas via its own flavor
    `register()` (symmetric with external flavors) or stay special?
D6. Sequencing vs Memophant v0.0.x seams (Zitadel AuthResolver,
    flavors/memo) — split before or after first external adoption?

## Embedding friction observed while building examples/embedded-minimal

- Blank-database bootstrap with the sqlx CLI: `proxima-flavor-goal`
  contains live `sqlx::query!` macros validated against the DB at
  compile time, but a fresh database has no flavor tables yet, and
  `sqlx migrate run` (CLI) rejects the flavor migration with
  `VersionMissing` because both the substrate and flavor migrators
  use `set_ignore_missing(true)` against the same `_sqlx_migrations`
  table. Runtime bootstrap is fine — host binaries calling
  `PgStorage::run_migrations()` then each flavor's `migrator().run()`
  in sequence work correctly — but the CLI path for compile-time
  query validation on a blank DB requires applying flavor SQL by
  hand first. Worth a look when D2 settles the migration-baseline
  story (per-side `_sqlx_migrations` tables would dissolve it).

## Non-goals

- No behavior changes; pure reorganization.
- No gRPC/wire work (stays deferred per docs/14).
- Flavor contract (docs/08) surface stays as-is.
