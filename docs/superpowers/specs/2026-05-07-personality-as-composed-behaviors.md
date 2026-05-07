# Personality as Composed Behaviors

**Status:** Draft
**Date:** 2026-05-07
**Owner:** Heinrich
**Scope:** Substrate (`crates/core`, `crates/storage-pg`), every personality-shipping flavor (today: `flavors/code`, `flavors/goal`), wire (`proto/proxima/v1`, TS bindings), Personalities view (`packages/frontend-core/src/views/personalities`), Tauri shell, and the related numbered docs (`docs/02`, `docs/04`, `docs/06`, `docs/08`, `docs/13`, `docs/14`).
**Successor to:** [`2026-05-06-personality-wake-decide-write-design.md`](./2026-05-06-personality-wake-decide-write-design.md). Keeps that spec's wake/decide/write loop, identity model, and idempotency story. Replaces "personality is a Rust trait impl" with "personality is per-instance config composed from registered building blocks." Does not change identity = `(template_id, instance_id)`, ChangeEvent semantics, self-wake prohibition, or `wake_chain_depth` bound.

## Goal

Eliminate the conceptual coupling between *what a personality is* (identity + behavior set) and *the Rust types that ship it*. **Stop building our own LLM harness.** After this spec lands:

- Flavors ship **building blocks** — typed schemas, tool implementations (as MCP tools), mechanical emitters, **context-source kinds**, optional **templates** (default seed config + bundled recipes).
- A **Personality** is a row in storage, not a Rust trait impl. It has exactly one **WakeConfig**.
- A **WakeConfig** is a list of **WakeEntries**, one per triggering schema (or relation). The schema/relation is the entry's primary key inside the config — *each schema can be used at most once per personality*. This makes "what wakes this personality" a flat lookup, not a search.
- Each **WakeEntry** carries: a filter (when to fire), a **ContextBuilder** (deterministic data assembled before the agent runs), a **recipe reference** (path to a goose recipe YAML), a **tool palette** (MCP tool allowlist enforced at our MCP server), and **max_rounds** (overrides the recipe's `settings.max_turns`). Per-entry prompt and per-entry model selection live *inside the recipe*, not on the WakeEntry — recipe authors own those.
- A personality's **Self-Perspective** (Root Memory) carries `display_name`, `purpose`, and a `system_prompt` baseline. The dispatcher injects it into every wake as a recipe parameter; recipes that want it weave it into their prompt template via `{{ self_perspective }}`.
- **Goals** are flavor-shipped Memories (today: `proxima-goal/simple-text-v1`, `proxima-goal/task-v1`); they connect to personalities through the existing `core/inspires` edge. Two creation paths: manual (user attaches via UI, immediately Active) and self-authored (a wake calls `proxima-goal/goal_propose`, lands as `PendingApproval`, requires user approval before influencing future contexts). **Approval requires no synthetic wake event** — the next wake's ContextBuilder pulls the now-active goal into context, and behavior changes from there.
- **The agent loop is goose, period.** Wake fires → engine invokes `goose run --recipe path --params ...` with a per-wake credential in env → goose connects to the engine's MCP listener using that credential → engine's MCP server enforces tool authorization against `WakeEntry.tool_palette` → goose runs its loop until done or `max_rounds` exhausted. We do not implement a tool-call loop. We do not implement model adapters. We do not implement turn streaming. **Goose owns the harness; the engine owns the substrate (storage, change events, dispatch, authorization).**

The user-visible payoff: a Personality can be authored from scratch in the UI from existing building blocks, without recompiling. The user can add a row to the WakeConfig matrix for each new triggering schema, point it at a goose recipe (shipped or hand-rolled), pick which tools it can call. The user can audit "what wakes this personality, what does it see beforehand, what can it call" all in one table.

The architectural payoff: hardcoded `PersonalityFlavor` impls become migration sugar. The composability invariants from `2026-05-06-personality-wake-decide-write-design.md` (single dispatcher, single wake/decide/write loop, append-only memory, lineage by edge) survive intact. The harness work we'd otherwise own (Anthropic SDK adapter, tool-call retry, multi-turn state, streaming) we don't own — goose ships and maintains it. We win back months of substrate work to spend on the parts that are actually ours: ontology, memory, the spinning wheel, marketplaces.

## Non-Goals

- Tool implementations are not user-authored. Tools are flavor-shipped Rust impls exposed via the engine's MCP server. Composability is at the *selection* level (which tools a WakeEntry's palette permits), not the *implementation* level.
- Schemas are not user-authored. Sidecar tables are typed Rust payloads compiled in via `proxima_flavor!`. Composability is at the *reference* level (a WakeEntry palette references tool ids that emit registered schemas).
- We do not implement an LLM tool-call loop, model adapter, retry logic, streaming, or token accounting in v1. Those are goose's job. The engine's job is dispatch + storage + authorization.
- This spec does not change identity. `(template_id, instance_id)` survives; `template_id` shifts meaning from "Rust trait identifier" to "template reference," but the wire shape is identical.
- This spec does not introduce a separate Decider wire protocol. The Decider becomes "the goose process plus our MCP server"; clients still see only the engine's typed surfaces.

## Entity Model

```mermaid
erDiagram
    OWNER ||--o{ PERSONALITY : "scopes"
    PERSONALITY ||--|| SELF_PERSPECTIVE : "anchored by"
    PERSONALITY ||--|| WAKE_CONFIG : "has exactly one"
    PERSONALITY ||--o{ GOAL_CONNECTION : "motivated by"
    WAKE_CONFIG ||--o{ WAKE_ENTRY : "contains (unique by trigger)"
    WAKE_ENTRY ||--|| WAKE_FILTER : "fires on"
    WAKE_ENTRY ||--|| CONTEXT_BUILDER : "assembles context with"
    WAKE_ENTRY }o--|| GOOSE_RECIPE : "delegates LLM loop to"
    WAKE_ENTRY }o--o{ MCP_TOOL : "permits (allowlist)"
    CONTEXT_BUILDER ||--o{ CONTEXT_SOURCE : "queries"
    GOAL_CONNECTION ||--|| GOAL : "references"
    GOAL_CONNECTION ||--|| APPROVAL_STATE : "carries"
    SELF_PERSPECTIVE ||--|| ROOT_PAYLOAD : "carries"
    MCP_TOOL }o--o| SCHEMA : "may emit"
    WAKE_FILTER }o--o| SCHEMA : "may match (on_memory)"
    WAKE_FILTER }o--o| RELATION : "may match (on_edge)"
    CONTEXT_SOURCE }o--o| SCHEMA : "may target sidecar of"

    PERSONALITY {
      uuid instance_id PK
      string template_id "former personality_type_id"
      Owner owner
      MemoryId current_self_perspective_memory_id "Root Memory pointer"
      uint16 max_wake_chain_depth
      string status "active | needs_repair | tombstoned"
    }
    SELF_PERSPECTIVE {
      MemoryId memory_id PK "append-only; supersession evolves identity"
      SchemaId self_schema "default named-purpose-prompt-self-v1"
    }
    ROOT_PAYLOAD {
      string display_name "user-facing name"
      string purpose "what this personality exists to do"
      string system_prompt "baseline; injected as recipe param {{self_perspective}}"
    }
    WAKE_CONFIG {
      uuid personality_instance_id PK_FK "1:1 with Personality"
      timestamp updated_at
    }
    WAKE_ENTRY {
      uuid wake_entry_id PK
      uuid personality_instance_id FK
      string trigger_key "schema_id (on_memory) or relation_id (on_edge); UNIQUE within WakeConfig"
      string label "human-readable, e.g. on_commit"
      bool enabled "soft-disable without delete"
      string recipe_ref "path or id of goose recipe YAML; v1 = local file path"
      uint16 max_rounds "overrides recipe's settings.max_turns"
    }
    WAKE_FILTER {
      string kind "on_memory | on_edge | flavor-defined"
      SchemaId schema_id "if on_memory"
      RelationId relation_id "if on_edge"
      AuthoredBy authored_by "any | self_perspective | other"
      float probability "0.0 - 1.0"
    }
    CONTEXT_BUILDER {
      uuid wake_entry_id PK_FK "1:1 with WakeEntry"
    }
    CONTEXT_SOURCE {
      uuid context_source_id PK
      uuid wake_entry_id FK
      string kind "trigger_event | triggering_memory | active_goals | self_perspective | sidecar_lookup | edge_neighborhood | flavor-defined"
      jsonb config "kind-specific (e.g. {table, fields, key_from})"
      string param_name "name of the recipe parameter this source binds"
      uint16 order "stable assembly order"
    }
    GOOSE_RECIPE {
      string id "filename or registered id"
      string version "recipe schema version"
      string prompt "Jinja-templated; references {{param_name}}s"
      string extensions "always includes proxima-engine-mcp"
      jsonb settings "model, max_turns (overridden by WakeEntry.max_rounds), provider"
      jsonb parameters "name + type per parameter; rendered by engine pre-spawn"
    }
    MCP_TOOL {
      string tool_id PK "e.g. proxima-goal/goal_propose"
      string description "for UI display + MCP tool spec"
      ToolKind kind "read | write_memory | write_edge | write_goal"
      string flavor_id "owning flavor"
    }
    GOAL_CONNECTION {
      uuid edge_id PK
      MemoryId goal_memory_id FK
      MemoryId self_perspective_memory_id FK
      string relation_id "core/inspires"
    }
    APPROVAL_STATE {
      string state "active | pending_approval | rejected"
      string source "manual | self_authored"
      timestamp approved_at "null until approved"
    }
```

The diagram makes four structural commitments worth calling out:

1. **Personality has exactly one WakeConfig (1:1).** WakeConfig is a *table*, not a *bag of behaviors* — its rows are WakeEntries, keyed by `trigger_key`. There is no question "which WakeConfig fires" because there is only one.
2. **Each WakeEntry has a unique trigger inside its WakeConfig.** Two entries cannot both fire on `commit-summary-v1`. This means trigger-to-behavior is a flat lookup: dispatcher matches a ChangeEvent's schema/relation against `wake_entries.trigger_key`, gets at most one entry per personality, fires it (or not).
3. **ContextBuilder is per WakeEntry, with ordered ContextSources.** Each source is a kind + JSONB config + a `param_name` that names the goose recipe parameter the source binds. Built-in kinds cover the universal cases; flavors register additional kinds (e.g. `proxima-code/code-graph-neighborhood`) that are usable wherever the registered MCP tools are usable.
4. **The recipe owns the prompt and the model.** Per-WakeEntry "what to think about, how to reason, which model to use" lives in the goose recipe YAML, not on the WakeEntry row. The WakeEntry references the recipe by id/path, picks which MCP tools the goose process is allowed to call, and overrides max_turns. Multiple WakeEntries can share one recipe with different ContextBuilder bindings.

The Personality / Self-Perspective / Root Payload split (three boxes anchored by a pointer) is unchanged from the existing wake/decide/write spec — that indirection is what lets the user supersede the Root Payload without minting a new Personality.

## The Spinning Wheel — Detailed Flow

```mermaid
sequenceDiagram
    autonumber
    participant Reality
    participant Source as Mechanical Emitter
    participant Engine
    participant Stream as ChangeEvent stream
    participant Dispatcher
    participant ContextBldr as Context Builder
    participant Goose as goose subprocess
    participant MCP as Engine MCP server
    participant Storage

    Reality->>Source: file change / commit / user goal-write / goal approval
    Source->>Engine: append memory (e.g. commit-v1; or goal-connection sidecar update)
    Engine->>Storage: write memory (author = external | user)
    Engine->>Stream: emit ChangeEvent

    loop dispatcher tick (per Owner)
        Dispatcher->>Storage: SELECT wake_entries WHERE<br/>trigger_key = event.schema_id<br/>JOIN personality (status=active)
        loop per matching WakeEntry
            Dispatcher->>Dispatcher: filter.authored_by allows event.author?<br/>(self-wake excluded)
            Dispatcher->>Dispatcher: filter.probability roll
            alt fires
                Dispatcher->>Storage: idempotency check<br/>(instance, wake_entry_id, change_event_seq)
                alt not seen
                    Dispatcher->>Storage: chain depth check<br/>(event.wake_chain_depth < instance.max_wake_chain_depth)
                    alt within budget
                        Dispatcher->>ContextBldr: build(WakeEntry, ChangeEvent)
                        ContextBldr->>Storage: read current Self-Perspective Root Payload
                        loop per ContextSource in entry order
                            ContextBldr->>Storage: query (trigger / active_goals / sidecar_lookup / ...)
                            Storage-->>ContextBldr: typed rows
                        end
                        ContextBldr->>ContextBldr: assemble param map<br/>{param_name: JSON-stringified value}
                        Dispatcher->>Storage: insert invocation row<br/>(status=running, wake_token=uuid)
                        Dispatcher->>Goose: spawn `goose run --recipe RECIPE_REF<br/>--params name=value ...`<br/>env: PROXIMA_WAKE_TOKEN=uuid,<br/>PROXIMA_MCP_URL=http://127.0.0.1:.../mcp

                        loop goose internal loop (≤ max_rounds turns)
                            Goose->>MCP: tool call with PROXIMA_WAKE_TOKEN
                            MCP->>MCP: resolve token → wake_entry<br/>authorize tool ∈ wake_entry.tool_palette
                            alt allowed write tool
                                MCP->>Engine: invoke (e.g. emit_perspective)
                                Engine->>Storage: append memory<br/>(author = personality_instance_id,<br/>wake_chain_depth = trigger.depth + 1)
                                Engine->>Stream: emit ChangeEvent
                                MCP-->>Goose: tool result
                            else allowed read tool
                                MCP->>Storage: query / fetch / search
                                Storage-->>MCP: results
                                MCP-->>Goose: tool result
                            else tool not in palette
                                MCP-->>Goose: error: tool_not_authorized
                            end
                        end

                        Goose-->>Dispatcher: exit (code + structured outcome)
                        Dispatcher->>Storage: revoke wake_token; finalize invocation<br/>(succeeded | truncated | failed)
                    end
                end
            end
        end
        Dispatcher->>Storage: advance personality_wake_cursor.last_considered_seq
    end
```

Three flow notes the diagram leaves implicit:

- **Goal approval is silent on the wake stream by default.** When the user approves a self-authored Goal, the `goal_connection_v1` sidecar's `approval_state` flips to `active` and `updated_at` advances. No new ChangeEvent is required — the next time *any* personality fires for *any* reason, its ContextBuilder's `active_goals` source picks up the now-active goal. **If a flavor wants explicit reactive wakes on approval**, it can emit a `proxima-goal/goal-activated-v1` Fact from inside the approval verb; personalities that care add a WakeEntry with `trigger_key = proxima-goal/goal-activated-v1`. The substrate doesn't impose this.
- **WakeEntry trigger lookup is a flat indexed query, not a per-personality scan.** With the `(personality_instance_id, trigger_key)` UNIQUE constraint and a btree on `trigger_key`, the dispatcher's per-event work is `O(matching wake_entries)`, not `O(personalities × entries)`.
- **The wake token is short-lived and per-invocation.** Generated when the dispatcher inserts the `running` invocation row; revoked when the invocation finalizes. If goose crashes, the token expires (TTL = `max_rounds × per_round_max_seconds`). Dispatcher's GC reaper sweeps `running` rows past TTL and finalizes them as `failed`. No leaked credentials, no zombie processes calling MCP after the wake is over.

## Target Architecture (Crisp Definitions)

### Personality

A row in `proxima_core.personality` (current `personality_wake_config` is renamed and split — see Storage Shape).

```
Personality {
  instance_id:                          Uuid               // stable across self-Perspective evolution
  template_id:                          String             // pointer to a template; not a Rust type id
  owner:                                Owner
  current_self_perspective_memory_id:   MemoryId           // Root Memory pointer
  max_wake_chain_depth:                 u16                // per-instance budget
  status:                               PersonalityStatus  // active | needs_repair | tombstoned (existing)
  created_at, updated_at, tombstoned_at
}
```

A Personality has zero or more WakeConfigs. A Personality with zero enabled WakeConfigs never wakes (and the Personalities view marks it "Inert"). A Personality with at least one enabled WakeConfig but all of its filters orphaned (no producer) is marked "Stranded" — see the wake-graph-emitters-reachability-tools plan.

### Self-Perspective (Root Memory)

A typed Memory of kind Perspective whose payload is the Personality's identity:

```
Default schema: proxima-core/named-purpose-prompt-self-v1
{
  display_name:    String
  purpose:         String
  system_prompt:   String
}
```

Specialized self-schemas remain available — a flavor that needs structured Self state (e.g. a Worker tracking its current task ID) ships its own Self schema and uses the same baseline three fields plus its own.

Editing any of the three fields supersedes the Self-Perspective Memory (append-only; ChangeEvent kind `EntityMutated`) and advances `Personality.current_self_perspective_memory_id`. Wake-time prompt assembly always reads the *current* Root Payload — there is no caching at the WakeConfig level.

### WakeConfig and WakeEntry

`WakeConfig` is a thin 1:1 wrapper on Personality (just an `updated_at`-bearing row that signals "this personality has had its wake matrix configured"). The interesting structure is `WakeEntry`.

```
WakeConfig {
  personality_instance_id:  Uuid     // 1:1 with Personality
  updated_at:               Timestamp
}

WakeEntry {
  wake_entry_id:            Uuid
  personality_instance_id:  Uuid
  trigger_key:              String           // schema_id (on_memory) | relation_id (on_edge); UNIQUE per personality
  label:                    String           // human-readable: "on_commit"
  enabled:                  bool

  filter:                   WakeFilter       // existing envelope (kind/schema_or_relation/authored_by/probability)
  context_builder:          ContextBuilder   // see below
  recipe_ref:               RecipeRef        // path to a goose recipe YAML; v1 = local file path
  tool_palette:             Vec<String>      // MCP tool ids the goose process is allowed to call
  max_rounds:               u16              // overrides the recipe's settings.max_turns

  created_at, updated_at
}
```

**Trigger uniqueness is structural, not advisory.** A unique constraint on `(personality_instance_id, trigger_key)` makes "two entries on `commit-summary-v1`" rejected at the storage layer. Authoring UIs see the constraint via the storage error, not via a separate validation pass.

The dispatcher's idempotency key extends from `(instance_id, change_event_seq)` to `(instance_id, wake_entry_id, change_event_seq)` — but since at most one entry can match per personality per ChangeEvent (uniqueness above), this key never collides in practice.

A WakeEntry with an empty tool palette spawns goose in a read-only mode — no MCP tools authorized, no writes possible. The ContextBuilder still runs and assembles deterministic context, the recipe still runs the LLM. Useful for periodic introspection without write authority.

**`recipe_ref` in v1 is a local file path.** Two paths supported:
1. **Bundled recipes** ship inside the flavor crate: `flavors/code/recipes/commit_summary.yaml`. The flavor's `register()` function records the absolute path at runtime; templates point at `bundled:proxima-code/commit_summary` which resolves through the registry.
2. **User recipes** live under `~/.proxima/recipes/<owner>/<filename>.yaml`. The user manages files in their editor; the WakeEntry just stores the filename. No DB persistence of the YAML body in v1.

DB-persisted recipes (with schema validation, marketplace sharing, version pinning) are explicitly v1.1+. The user-facing payoff in v1 is "edit a YAML file, restart the wake, see the change" — no UI editor required for recipe authoring.

### ContextBuilder

Per WakeEntry. A list of `ContextSource`s that the dispatcher resolves *before* spawning goose. Each source produces a typed block of data that gets serialized to JSON and bound to a named recipe parameter via Jinja substitution.

```
ContextBuilder {
  wake_entry_id:  Uuid
  sources:        Vec<ContextSource>
}

ContextSource {
  context_source_id:  Uuid
  wake_entry_id:      Uuid
  kind:               ContextSourceKind
  config:             serde_json::Value   // kind-specific
  param_name:         String              // recipe parameter to bind; e.g. "trigger_event"
  order:              u16                 // stable assembly order (also the order in invocation logs)
}
```

The `param_name` is the contract between the ContextBuilder and the recipe. A recipe declares `parameters: [{ key: trigger_event, input_type: string, requirement: required }]`; the WakeEntry's matching ContextSource binds to it. Mismatches (recipe expects a param the ContextBuilder doesn't provide; or vice versa) are caught at WakeEntry write time when the engine validates the entry against the referenced recipe.

**Substrate-shipped ContextSourceKinds:**

| Kind | Config | Result |
|---|---|---|
| `trigger_event` | `{ include_payload: bool }` | The ChangeEvent that fired the wake (always relevant). |
| `triggering_memory` | `{ include_sidecar: bool }` | The full memory row that the ChangeEvent points at + its sidecar payload (if requested). Resolves a 90% common pattern. |
| `active_goals` | `{ schema_filter?: SchemaId, max: u16 }` | All `core/inspires`-edged Goals on this personality with `approval_state = active`, optionally filtered by schema. |
| `self_perspective` | `{ include_payload: bool }` | The current Root Payload (display_name, purpose, system_prompt). Usually included implicitly when prompt is assembled — but explicit makes "what the LLM sees of its own identity" auditable. |
| `sidecar_lookup` | `{ table: String, key_field: String, key_from: TriggerPath, fields: Vec<String>, max_rows: u16 }` | A typed read of N fields from a sidecar table, keyed by a value extracted from the trigger event via JSONPath. The pattern from item 1 of your brainstorm. |
| `edge_neighborhood` | `{ from: TriggerPath, relation_id: RelationId, depth: u8, direction: in\|out\|both }` | All memories N edge-hops away via a given relation, starting from a memory id extracted from the trigger. |

**Flavor-shipped ContextSourceKinds:** flavors register additional kinds via a new `ContextSourceKind` trait analogous to `WakeFilterKind`. Examples a Code flavor might ship: `proxima-code/code-graph-call-neighborhood` (the call graph N hops from a chunk), `proxima-code/recent-commits-touching-path` (file-history slice). The kind is just a typed query over the flavor's sidecars; the kind itself authorizes by Owner the same way Tool authorization does.

**Why the read/write split (ContextBuilder vs ToolPalette) matters.** ContextSources are *deterministic* (same trigger → same context, modulo storage state) and *cheap* (queries we can plan and bound). Tools are *non-deterministic* (the LLM decides whether and how to call) and *expensive* (each tool call adds round-trips and tokens). Pushing data the personality always needs into the ContextBuilder removes uncertainty from the prompt and keeps `max_rounds` available for the genuinely exploratory parts. Today's hardcoded personalities mash both into a system prompt that says "call `core/fetch_memory` first" — that's a wasted round-trip on something we could just inject.

### MCP Tool

Tools are MCP tools — exposed by the engine's MCP listener (`crates/mcp-server`), declared by flavors at build time via `add_mcp_tool!`:

```rust
pub trait McpTool: Send + Sync {
    const NAME: &'static str;              // e.g. "proxima-goal/goal_propose"
    const DESCRIPTION: &'static str;       // shown in MCP tool spec
    type Args: JsonSchema + DeserializeOwned;
    type Result: JsonSchema + Serialize;
    async fn invoke(ctx: &McpContext, args: Self::Args) -> Result<Self::Result, McpToolError>;
}
```

The pool is the same one external MCP clients see today. The novelty is that goose, when spawned by the dispatcher with `PROXIMA_WAKE_TOKEN` in env, becomes one more MCP client — but a constrained one: the token resolves to a WakeEntry whose `tool_palette` is the allowlist. A goose process trying to call a tool not in the allowlist hits an `Unauthorized` error from our MCP server, the same way an external MCP client without the right Owner scope would.

Substrate ships read tools (`core/fetch_memory`, `core/query`, `core/search_by_embedding`) and the bare emit tools (`core/emit_abstraction`, `core/emit_perspective`); flavors ship specialized writers (`proxima-goal/goal_propose`, future `proxima-code/...`).

`writeable_schemas` and `writeable_relations` (today's per-personality declarations) **derive** from the union of tool palettes across the personality's WakeEntries: `union over wake_entries of union over tool_palette of tool.emittable_schemas`. There is no separate declaration to drift.

### Goose Recipe Integration

The substrate spawns `goose run` per wake. Recipes are goose's standard YAML format ([reference](https://goose-docs.ai/docs/guides/recipes/recipe-reference)). Two conventions the engine relies on:

**1. The engine's MCP server is always the only extension.** Recipes that a Proxima flavor ships *must* declare exactly one extension — the engine's own MCP server, configured via env. The engine generates the extensions block at recipe-resolve time; recipe authors don't write it.

```yaml
# Recipe authors write:
version: "1.0.0"
title: "Commit Summary"
parameters:
  - key: trigger_event
    input_type: string
    requirement: required
  - key: triggering_memory
    input_type: string
    requirement: required
  - key: self_perspective
    input_type: string
    requirement: required
prompt: |
  You are {{ self_perspective.display_name }}, whose purpose is "{{ self_perspective.purpose }}".
  System guidance: {{ self_perspective.system_prompt }}

  A new commit has landed:
  {{ triggering_memory }}

  Call `core/emit_abstraction` exactly once with schema_id = "proxima-code/commit-summary-v1"
  and a payload matching the schema. Do not call any other tool.
settings:
  goose_provider: "ollama"
  goose_model: "llama3.1:8b"
  max_turns: 4

# Engine injects (or overrides) at spawn time:
extensions:
  - type: streamable_http
    name: proxima-engine-mcp
    url: ${PROXIMA_MCP_URL}
    headers:
      authorization: "Bearer ${PROXIMA_WAKE_TOKEN}"
    timeout: 300
```

**2. `WakeEntry.max_rounds` is the only WakeEntry-level override on the recipe.** It overrides `settings.max_turns` via `--max-turns` on the goose CLI. Everything else the recipe defines — prompt, model, provider, temperature, retry behavior, sub-recipes — is owned by the recipe author and not exposed as a per-WakeEntry knob. This keeps the WakeEntry shape minimal (filter / context_builder / recipe_ref / tool_palette / max_rounds) and pushes per-personality customization into recipes (where the user can fork a bundled recipe into `~/.proxima/recipes/<owner>/` and edit anything they want).

**Spawn invocation:**

```
goose run \
  --recipe /path/to/recipe.yaml \
  --params trigger_event='{"...JSON..."}' \
  --params triggering_memory='{"...JSON..."}' \
  --params self_perspective='{"...JSON..."}' \
  --params active_goals='[{"...goal1..."}, ...]' \
  --max-turns ${WAKE_ENTRY_MAX_ROUNDS} \
  --no-interactive
```

**Env injected by Proxima:** exactly two variables — `PROXIMA_WAKE_TOKEN` (per-invocation MCP credential) and `PROXIMA_MCP_URL` (where to find our MCP server). Nothing else.

**Provider credentials are NOT Proxima's concern.** Goose reads its provider config from `~/.config/goose/config.yaml` (or whichever path goose conventionally uses on the host platform), plus standard env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OLLAMA_HOST`, etc.) inherited from the parent process. The user runs `goose configure` once on their machine and Proxima never sees their keys. This means:

- A user with an OpenAI API key in their goose config gets OpenAI-backed wakes for free.
- A user running ollama locally gets local-only wakes for free.
- A user mixing providers per-recipe (one recipe pinned to `claude-sonnet-4-6`, another to `gpt-5`, another to `llama3:8b`) is supported because goose handles per-recipe `settings.goose_provider` / `goose_model` natively.
- The Proxima codebase has no credential storage, no secret encryption, no key rotation, no per-Owner credential isolation in v1. All three are deferred to the post-v1 multi-user/hosted spec where they actually matter.

**Implication for the existing `crates/llm-openai-compat` and the per-Owner model registry:** v1 retires the in-process model client. The model registry's role shrinks to "a list of registered tier names that recipe authors can target via `settings.goose_model`" — purely informational, no credentials, no client. Whether to keep the registry crate at all is a Phase 1 cleanup decision (see Open Questions).

**Acknowledged trade-offs of the goose-only choice:**

- **No per-turn telemetry inside goose.** We see invocation start, every MCP tool call (which is most of what matters), invocation end, and goose's structured outcome. We do not see internal model-retry attempts or token counts per turn. Workable; we record per-MCP-call cost (model-side estimates) and the dispatcher correlates by `wake_token`.
- **No in-process model adapter.** Models and provider credentials are entirely goose's domain (`~/.config/goose/config.yaml` plus standard env vars). Proxima holds zero LLM credentials. v1 expects the user to have run `goose configure` once on their host. Per-Owner credential isolation is deferred until we have multi-Owner deployments that actually need it.
- **Subprocess overhead.** ~50–100ms of goose process startup per wake. For our wake rates (handfuls per minute, peaking on ingest bursts) this is invisible. If a future workload needs millions of wakes per hour, we revisit then.
- **Tied to goose's release cadence.** Per Q10: v1 accepts whatever's on PATH. If goose makes a breaking recipe-schema change, the engine's `goose recipe validate` boundary catches it before a wake fires.

**Why not also keep a Native backend?** Considered; rejected as scope creep for v1. Maintaining two backends doubles testing, doubles the ContextBuilder→prompt rendering paths, and doubles the cancellation story. If we ever hit a load profile or behavior that goose can't serve, we add a second backend then. For v1: one path, one bug surface.

### Template

Data, not code. Shipped by a flavor as a registered TOML/YAML doc + bundled goose recipe(s):

```
flavors/code/
├── templates/
│   └── engineer.toml          # Personality template (this file)
└── recipes/
    └── code_review.yaml       # Goose recipe referenced by the template
```

```toml
# flavors/code/templates/engineer.toml
template_id = "proxima-code/engineer-v1"
self_schema = "proxima-core/named-purpose-prompt-self-v1"

[default_self_payload]
display_name = "Engineer"
purpose = "Develop perspectives on code changes"
system_prompt = """You are a senior development reviewer..."""

[[wake_entries]]
trigger_key = "proxima-code/commit-summary-v1"
label = "on_commit_summary"
recipe_ref = "bundled:proxima-code/code_review"
tool_palette = ["core/emit_perspective"]
max_rounds = 4

[wake_entries.filter]
kind = "on_memory"
schema_id = "proxima-code/commit-summary-v1"
authored_by = "any"
probability = 1.0

[[wake_entries.context_builder.sources]]
kind = "trigger_event"
config = { include_payload = true }
param_name = "trigger_event"
order = 0

[[wake_entries.context_builder.sources]]
kind = "triggering_memory"
config = { include_sidecar = true }
param_name = "triggering_memory"
order = 1

[[wake_entries.context_builder.sources]]
kind = "self_perspective"
config = {}
param_name = "self_perspective"
order = 2

[[wake_entries.context_builder.sources]]
kind = "active_goals"
config = { max = 8 }
param_name = "active_goals"
order = 3
```

`instantiate_personality(owner, template_id)` reads the template, mints an instance, writes the Self-Perspective Root Memory from `default_self_payload`, resolves the `bundled:` recipe refs to absolute paths via the flavor registry, and inserts one `personality_wake_entries` row per `[[wake_entries]]` block. After instantiation, the template is no longer load-bearing — the user can edit any field on the live Personality without affecting the template. (Template versioning + diff-against-template is a v1.1 nice-to-have.)

User-authored recipes go in `~/.proxima/recipes/<owner>/<filename>.yaml` and are referenced as `recipe_ref = "user:my-custom-recipe"` (resolves to `~/.proxima/recipes/<owner>/my-custom-recipe.yaml`). The Personalities view's WakeEntry editor exposes a native file picker rooted at that directory; selecting a file outside the directory copies it in (so we never store absolute paths that move when the user reorganizes their filesystem). Every selection runs `goose recipe validate` server-side before the editor enables save.

### Goal Connection

**Goals live in the `proxima-goal` flavor**, not core. The flavor today ships:

- Payload schemas: `proxima-goal/simple-text-v1`, `proxima-goal/task-v1`.
- Tools: `proxima-goal/goal_propose` (write), `proxima-goal/goal_decline` (write).
- Sidecar tables: `proxima_goal.simple_text_goal_v1`, `proxima_goal.task_goal_v1`.
- Mechanical emitter declaration (post wake-graph plan): `goal-v1` schemas mechanically emitted by the GoalWrite verb when a user submits manually.

The connection between a Goal and a Personality remains a `core/inspires` edge — the relation is in core because the relation kind itself is substrate-level, the same way `core/cites` lives in core. The new state lives on a sidecar:

```
GoalConnection {
  edge_id:                          EdgeId
  goal_memory_id:                   MemoryId
  self_perspective_memory_id:       MemoryId
  source:                           "manual" | "self_authored"
  approval_state:                   "active" | "pending_approval" | "rejected"
  approved_at:                      Option<Timestamp>
  approved_by:                      Option<Principal>
}
```

**Two creation paths, both already supported by tools today:**

| Path | How | Initial state |
|---|---|---|
| Manual | User opens the Goals view, attaches a goal to a personality. The substrate verb writes the Goal + the `core/inspires` edge + the sidecar row. | `source = manual, approval_state = active` |
| Self-authored | A WakeEntry's tool palette includes `proxima-goal/goal_propose`. The personality's wake calls it. | `source = self_authored, approval_state = pending_approval` |

**Approval is silent on the wake stream by default.** The user opens the Goals view, sees pending proposals, approves. The sidecar's `approval_state` flips to `active`. **No ChangeEvent is required to make this useful** — the next time any personality fires, its ContextBuilder's `active_goals` source pulls the now-active goal. Behavior changes through context, not through a wake.

If a flavor wants explicit *reactive* wakes when goals activate (e.g. a "Planner" personality that should immediately re-plan on new active goals), the flavor can:

1. Emit a `proxima-goal/goal-activated-v1` Fact from inside the approval verb (goal flavor extension; small change).
2. Add a WakeEntry on personalities that need it: `trigger_key = proxima-goal/goal-activated-v1`.

This keeps the substrate's wake model "filter on memory creation" pure. Mutation-driven wakes are a flavor opt-in, not a substrate concern.

## Authorization (Load-Bearing Rules)

These are the small set of rules the engine enforces. Violating any of them is a `ProtocolError`, never silent.

| Rule | Where enforced |
|---|---|
| A WakeEntry fires only if `filter.match(ChangeEvent)` AND `event.author != personality.instance_id` (no self-wake). | Dispatcher (existing invariant). |
| Within a personality's WakeConfig, `(personality_instance_id, trigger_key)` is UNIQUE. Storage rejects a second entry on the same schema. | `personality_wake_entries` table constraint. |
| Each `(instance_id, wake_entry_id, change_event_seq)` triple fires at most once. | `personality_wake_invocations` UNIQUE constraint. |
| ContextBuilder runs before goose is spawned. If any ContextSource fails (storage error, unresolved flavor kind), the invocation is marked `failed` with `kind = context_build_failed` and goose is never invoked. | Engine `run_wake`. |
| At wake time, every `tool_id` in `WakeEntry.tool_palette` must resolve to a registered MCP tool. Else: invocation marked `failed` with `kind = unresolved_tool`. (See stress test 2 for the soft-disable variant for live registry drift.) | Engine `run_wake`. |
| The `recipe_ref` must resolve to a readable goose recipe YAML, AND `goose recipe validate <path>` must succeed (exit 0). Engine shells out to goose's own validator — we do not parse recipe YAML ourselves. Failure surfaces as `kind = recipe_invalid` with goose's stderr verbatim. | WakeEntry write-time check (`add_wake_entry` / `edit_wake_entry`) + engine `run_wake` (defense in depth — file may change between save and wake). |
| Recipe parameters declared in the recipe (extracted from `goose recipe validate --json` output, or by re-parsing only the `parameters` block) must be a subset of `param_name`s the ContextBuilder produces. Missing params: `kind = recipe_param_missing` with the named missing param. Extra ContextBuilder params are dropped silently (logged at debug). | Engine recipe-validator at WakeEntry write time + at fire time. |
| Every MCP call from the goose subprocess carries `Authorization: Bearer ${PROXIMA_WAKE_TOKEN}`. The token resolves to a single in-flight WakeEntry. The MCP server checks the called `tool_id` is in the WakeEntry's `tool_palette`. Tokens are revoked when the invocation finalizes. | MCP server middleware. |
| A write tool's emitted schema must be in the tool's declared `emittable_schemas`. | Tool `invoke()` validates before calling Engine append. |
| `proxima-goal/goal_propose` always lands the Goal with the `core/inspires` edge sidecar set to `source = self_authored, approval_state = pending_approval`. There is no flag, no override, no tool variant. (Manual UI attaches use a separate substrate verb that lands as `source = manual, approval_state = active`.) | `proxima-goal/goal_propose` tool implementation + manual-attach verb. |
| The goose subprocess is invoked with `--max-turns ${WakeEntry.max_rounds}`. Goose enforces; the engine doesn't have to. If goose exits with `turn_limit`, the invocation is marked `truncated`. | `goose run --max-turns` plus exit-code mapping in the dispatcher. |
| The freeze()-time check from the wake-graph-emitters-reachability-tools plan extends: every `tool_id`, every `ContextSource.kind`, and every `recipe_ref` referenced by a persisted WakeEntry must resolve at engine boot. Hard check at `instantiate_personality_from_form` (write-time refusal); soft check at engine boot (auto-disable + warn for runtime drift). | Engine boot + `instantiate_personality_from_form`. |
| Personality cannot edit its own Self-Perspective. Self-edits are user-only via the Personalities view (which calls a substrate verb, not an MCP tool). | Engine — no `core/edit_self` tool exists. |

The last rule is deliberate: making the Personality able to rewrite its own identity creates feedback loops we have no story for. If a future spec wants this, it adds a fourth approval state — until then, identity edits are out-of-band.

## Storage Shape (Migration from Today)

| Today | Tomorrow |
|---|---|
| `proxima_core.personality_wake_config (owner, type_id, instance_id, current_self_perspective_memory_id, wake_filters JSONB[], status)` — one row per instance, JSONB array of filters. | `proxima_core.personality (owner, instance_id, template_id, current_self_perspective_memory_id, max_wake_chain_depth, status, ...)` — one row per instance. **No filters here.** |
| (none) | `proxima_core.personality_wake_entries (wake_entry_id, personality_instance_id, trigger_key, label, enabled, filter JSONB, recipe_ref TEXT, tool_palette TEXT[], context_builder JSONB, max_rounds SMALLINT, ...)` — one row per WakeEntry. UNIQUE on `(personality_instance_id, trigger_key)`. |
| `proxima_core.personality_wake_cursor (owner, type_id, instance_id, last_considered_seq)` | unchanged |
| `proxima_core.personality_wake_invocations (owner, type_id, instance_id, change_event_seq, status, ...)` | extended with `wake_entry_id uuid NOT NULL`, `recipe_sha256 text NOT NULL` (computed at fire time from the resolved recipe bytes), `wake_token uuid NOT NULL` (revoked on finalize). UNIQUE moved to `(owner, instance_id, wake_entry_id, change_event_seq)`. |
| Self-Perspective sidecars (per-flavor) carrying `display_name`, `purpose`. | New core sidecar `proxima_core.named_purpose_prompt_self_v1 (memory_id, display_name, purpose, system_prompt)`. Existing flavor sidecars kept; if a flavor's Self schema doesn't yet have `system_prompt`, the migration adds the column with the value extracted from the old hardcoded `PersonalityFlavor::system_prompt()`. |
| `core/inspires` edges to Self-Perspective for Goals. | Same edges + new sidecar `proxima_core.goal_connection_v1 (edge_id, source, approval_state, approved_at, approved_by)`. |

**Why ContextBuilder rides as JSONB on `personality_wake_entries`:** it is a typed Rust struct (`ContextBuilder { sources: Vec<ContextSource> }`) with a stable serde envelope. Same shape used by `wake_filters` JSONB today. Avoiding a child table for ContextSource keeps the dispatcher's per-event work as one indexed row read (the WakeEntry) plus N source resolutions in memory — no extra join.

**Migration strategy:**
1. Add new tables; do not drop old yet.
2. Write goose recipes for the existing personalities (CommitSummary, Engineer) from their hardcoded `PersonalityFlavor::system_prompt()` content. Bundle as `flavors/code/recipes/commit_summary.yaml` + `flavors/code/recipes/code_review.yaml`.
3. For each existing `personality_wake_config` row: write one `personality` row (move identity columns) + one `personality_wake_entries` row per element of the JSONB filter array. Generate `wake_entry_id`s. Set `trigger_key` from each filter's `schema_id` or `relation_id`. Set `recipe_ref` to the matching bundled recipe. Set `tool_palette` from the personality's hardcoded `tools()`. Default `context_builder = { sources: [trigger_event, triggering_memory{include_sidecar=true}, self_perspective, active_goals] }` (the universal pattern). Default `max_rounds = 4`.
4. Backfill `goal_connection_v1` rows for every `core/inspires` edge with `source = manual, approval_state = active`.
5. Switch dispatcher and verbs to read from the new tables; replace the in-process `run_wake` with the goose-spawn flow.
6. Drop the JSONB `wake_filters` column from `personality_wake_config` (which is now mostly empty); rename remaining identity columns; drop hardcoded `PersonalityFlavor::system_prompt`/`tools` traits — keep them as deprecated for one release for any out-of-tree flavor.

**Goose binary as a runtime dependency:** v1 expects goose on PATH. Boot self-check runs `which goose && goose --version`; any version is accepted. Missing binary refuses to start the dispatcher and surfaces the install hint (`brew install block-goose` or platform equivalent) in the UI. The Tauri shell does not bundle goose in v1 — bundling becomes a v1.1+ concern when distribution friction shows up.

**We use goose's own validator, not our own.** Whenever a `recipe_ref` is set or changed (via the Personalities view, by template instantiation, or by registry registration of bundled recipes), the engine shells out to `goose recipe validate <resolved-path>`. Goose tells us whether the YAML is structurally valid, whether `extensions` and `parameters` are well-formed, and whether referenced templates resolve. We then do *one* additional check on top — that the recipe's declared `parameters` are covered by the WakeEntry's ContextBuilder `param_name`s. We never hand-parse recipe YAML for validation purposes; that's a maintenance trap. Goose's recipe schema can evolve and our validator stays current automatically.

## Stress Tests

### 1. User tries to add a second WakeEntry on the same schema

**Setup:** Engineer already has a WakeEntry with `trigger_key = proxima-code/commit-summary-v1`. User opens the editor and adds another row with the same trigger.

**Expected:** Storage rejects the insert via the UNIQUE constraint on `(personality_instance_id, trigger_key)`. UI surfaces "An entry for `proxima-code/commit-summary-v1` already exists — edit it instead." There is no "merge" semantics, no "first-wins" rule, no race. **One trigger → one entry → one decision** is structural. (If the user genuinely wants two parallel behaviors on the same trigger, they create a *second personality* — possibly from the same template — and give it the alternate behavior. Personality multiplicity, not entry multiplicity, is how parallelism is expressed.)

### 2. WakeEntry palette references a deleted tool

**Setup:** Flavor publishes a new version that removes `proxima-code/old_thing`. Engine restarts; the WakeEntry row still references it.

**Expected:** Engine startup self-check logs a warning, marks the affected WakeEntry as `enabled = false` with `disabled_reason = "tool proxima-code/old_thing not registered"`, surfaces in the Personalities view. **Why soft-disable, not refuse-to-boot:** flavor downgrade should not be a P0 outage; a misconfigured WakeEntry is a recoverable state the user fixes by editing the palette or reinstalling the flavor. The same soft-disable handles deleted ContextSource kinds.

### 3. Personality writes a goal without `proxima-goal/goal_propose` in palette

**Setup:** Engineer's WakeEntry palette has `[core/emit_perspective]` only. The LLM hallucinates a `proxima-goal/goal_propose` call.

**Expected:** Authorization rejects at the engine's MCP boundary (same boundary native and goose backends share). Tool call returns an error; the LLM sees it and may retry. The wake invocation eventually completes (the LLM gives up or `max_rounds` exhausts). Invocation row is `succeeded` if the LLM finished without writing, `truncated` if `max_rounds` hit. No Goal is written.

### 4. User edits Self-Perspective system_prompt mid-flight

**Setup:** Engineer wake A is in progress (LLM call out). User opens Personalities view, edits system_prompt, saves. New Self-Perspective Memory is appended; `current_self_perspective_memory_id` advances. Wake A is still using the old prompt.

**Expected:** Wake A finishes with the old prompt. The next wake (B) reads the new `current_self_perspective_memory_id` (and the ContextBuilder's `self_perspective` source pulls fresh) and uses the new prompt. The transition is per-wake, not per-instance. **Subtle:** the old Self-Perspective Memory remains in storage forever (append-only) and is visible through Atlas — the audit trail of identity evolution is intact.

### 5. Personality has zero enabled WakeEntries

**Setup:** User disables every WakeEntry on Engineer (or never adds any).

**Expected:** Engineer never wakes. Personalities view marks it "Inert" (vs "Reachable" / "Stranded"). The Self-Perspective is unchanged; Goals stay attached; cognitive history is intact. Re-enabling any WakeEntry immediately makes it eligible for the next dispatcher tick. **No special tombstoning** — an inert personality is a deliberate, recoverable state.

### 6. Cross-personality goal flow (silent approval path)

**Setup:** Personality A (Visionary) has a WakeEntry with `proxima-goal/goal_propose` in palette. Personality B (Engineer) has a WakeEntry whose ContextBuilder includes `kind = active_goals`.

1. A fires, calls `proxima-goal/goal_propose(payload)`. Goal Memory is appended; `core/inspires` edge created from Goal → A's Self-Perspective; sidecar row `approval_state = pending_approval, source = self_authored`. ChangeEvent emits but dispatcher's wake-filter evaluator does not match — B does not wake.
2. User opens Goals view, sees pending proposal, approves. Sidecar updates to `approval_state = active, approved_at = now`. **No ChangeEvent is emitted on the wake stream.** B does not wake.
3. Some independent ChangeEvent (a new commit, a periodic tick, anything) eventually fires B's WakeEntry. B's ContextBuilder's `active_goals` source now includes the newly-active Goal. B's prompt sees it. Behavior changes.

**Expected:** This is the *default* path. The user's mental model: approval changes what's in context, not what fires. **For flavors that need explicit reactive wakes on approval** (e.g. Planner: must re-plan immediately on new goals), the proxima-goal flavor extension emits a `proxima-goal/goal-activated-v1` Fact from the approval verb; Planner adds a WakeEntry with `trigger_key = proxima-goal/goal-activated-v1`. Substrate stays out of mutation-driven wakes.

### 7. ContextBuilder pulls sidecar fields based on trigger

**Setup:** Engineer's WakeEntry on `proxima-code/commit-summary-v1` has a `sidecar_lookup` ContextSource:
```json
{
  "kind": "sidecar_lookup",
  "config": {
    "table": "proxima_code.commit_v1",
    "key_field": "memory_id",
    "key_from": "$.trigger.payload.commit_memory_id",
    "fields": ["sha", "message", "author_name", "committer_time"],
    "max_rows": 1
  }
}
```
A commit-summary-v1 ChangeEvent triggers the wake.

**Expected:** ContextBuilder runs the JSONPath against the trigger event, extracts the `commit_memory_id` UUID, queries `proxima_code.commit_v1 WHERE memory_id = $1`, gets the four fields. Result is serialized into the assembled context as a typed block. The LLM sees the commit details *without* needing a `core/fetch_memory` tool round-trip. **Saved cost:** one LLM turn. **Determinism gained:** the LLM cannot "forget" to fetch the commit.

### 8. MaxRounds exhaustion

**Setup:** Engineer's WakeEntry has `max_rounds = 3`. The recipe's prompt encourages exploration; the LLM enters a tool-call loop calling `core/search_by_embedding` repeatedly.

**Expected:** Goose enforces `--max-turns 3` and exits with the `turn_limit` outcome after the third turn. Any partial writes that happened during turns 1-3 are kept (append-only memory; we don't roll back). The dispatcher reads goose's exit and marks the invocation as `truncated`. UI surfaces "Engineer's wake on commit X was truncated at the round budget — consider raising max_rounds, refining the prompt, or scoping the tool palette."

### 9. Migration: existing engineer-v1 instance

**Setup:** DB has the active engineer-v1 instance from today's run.

**Expected:** Migration script reads the running flavor registry's `CodeEngineerPersonality::system_prompt()`, `default_wake_filters()`, and `tools()`. Generates one `personality` row + one `personality_wake_entries` row per filter (today: two filters → two WakeEntries with distinct `trigger_key`s). The Self-Perspective Memory's typed payload is augmented with the system_prompt text via a sidecar column add. Default ContextBuilder is `[trigger_event, triggering_memory{include_sidecar=true}, active_goals]`. Default `max_rounds = 4`. Default backend = Native. Post-migration: re-running ingest produces equivalent behavior — slightly *better* than before because the ContextBuilder injects the triggering memory rather than forcing a `fetch_memory` round-trip.

### 10. Composing a new personality from scratch

**Setup:** No template. User opens "New Personality" in the UI.

**Expected:** Form fields:
- Name (display_name)
- Purpose
- System prompt (multiline)
- Self schema dropdown (default: named-purpose-prompt-self-v1)
- A WakeConfig table the user adds rows to. Each row (= WakeEntry):
  - Label
  - Trigger (kind dropdown, schema/relation autocomplete from registry; **the form refuses a duplicate trigger inline before submission**)
  - Filter modifiers (authored_by, probability)
  - ContextBuilder (a sub-form: pick built-in sources + flavor-defined sources; configure each, set the `param_name` each binds to)
  - **Recipe picker** (two-mode):
    - **Bundled:** dropdown listing flavor-registered recipes (e.g. `bundled:proxima-code/code_review` shows as "Code Review (proxima-code)"). On selection, the engine runs `goose recipe validate` server-side and shows ✓ / error inline. The recipe's declared `parameters` are listed read-only beneath the picker; the form auto-suggests matching `param_name` values for ContextSources.
    - **User file:** native file picker rooted at `~/.proxima/recipes/<owner>/`. On selection, same `goose recipe validate` run, same parameter introspection. If the user picks a file outside the recipes directory, the picker copies it into the directory under its original name and references it as `user:filename`.
  - Tool palette (multi-select from registered tools, grouped by flavor + kind; flagged inline if a tool's emittable_schemas conflict with the WakeEntry's expected outputs)
  - Max rounds (number; defaults from `recipe.settings.max_turns` when a recipe is selected)

On submit: substrate verb `instantiate_personality_from_form(form)` mints `instance_id`, writes Self-Perspective Memory, inserts personality row + N wake_entries rows. The reachability check from the wake-graph plan runs synchronously and the form refuses to submit if any WakeEntry filter has no producer or any recipe fails validation.

### 11. Two personalities both write `commit-summary-v1`

**Setup:** Two Personalities each have a WakeEntry on `proxima-code/commit-v1` whose palette includes `core/emit_commit_summary`. Same commit triggers both.

**Expected:** Both fire (different instance_ids → both pass self-wake check). Two separate `commit-summary-v1` Abstractions written, with different `personality_instance_id` author columns. Downstream Engineer's WakeEntry on `commit-summary-v1` fires twice — once per summary. This is by design: parallel summarizers are a feature, not a bug. If the user wants only one summary per commit, they configure only one summarizer personality.

### 12. Cycle: A wakes B, B wakes A

**Setup:** A has WakeEntry on B's output schema; B has WakeEntry on A's output schema.

**Expected:** Bounded by `wake_chain_depth` (per-personality, default 10). The chain runs A → B → A → B → … until depth 10, at which point dispatcher refuses to fire. Invocation row carries `kind = chain_depth_exhausted`. UI surfaces a notification. **No deadlock**, **no infinite loop** — the existing wake_chain_depth invariant from `2026-05-06-personality-wake-decide-write-design.md` carries forward unchanged.

### 13. Wake token isolation

**Setup:** Two wakes fire concurrently — Engineer-Alice on commit X, Engineer-Bob on commit Y. Two goose subprocesses spawn in parallel, each with its own `PROXIMA_WAKE_TOKEN`.

**Expected:** Each subprocess's MCP calls resolve to its own WakeEntry; tool-palette enforcement is per-token, not per-process. If Alice's recipe somehow extracted Bob's token from a shared file (it can't — tokens are env-only) and tried to call a tool that's only in Bob's palette, our MCP server's token resolution would route the call to Bob's WakeEntry context — which is exactly what an MCP request authenticated with Bob's token *should* do. **The token IS the identity; there's no cross-personality bleed.** Tokens are revoked on invocation finalize and have a TTL fallback for crashed processes.

### 14. Recipe validation at write time (two checks)

**Setup:** User edits a WakeEntry, picks `recipe_ref = "user:my-recipe"` from the file picker. The Tauri command resolves the ref to `~/.proxima/recipes/<owner>/my-recipe.yaml` and runs validation before allowing save.

**Expected — sequence:**
1. **Structural check via goose:** engine runs `goose recipe validate /path/to/my-recipe.yaml`.
   - **Exit non-zero:** save rejected with `kind = recipe_invalid`. UI surfaces goose's stderr verbatim ("line 14: parameter `foo` declared without `key`"). Editor stays open.
   - **Exit zero:** continue.
2. **Param-coverage check via engine:** engine reads the recipe's `parameters` block, extracts the set of declared param keys, and compares against the WakeEntry's ContextBuilder `param_name`s.
   - **Recipe declares `key: foo, requirement: required` but no ContextSource has `param_name = "foo"`:** save rejected with `kind = recipe_param_missing: foo`. UI surfaces "Recipe `my-recipe` requires parameter `foo` but no ContextSource binds it. Add a ContextSource with `param_name = foo` or use a different recipe."
   - **All required params covered:** save succeeds.

The reverse case (ContextBuilder produces a `param_name` the recipe doesn't declare) is allowed — extra context is dropped silently with a debug log, since recipes legitimately may not need every source the WakeEntry assembles.

Both checks repeat at fire time too (defense in depth) — the YAML file may have been edited between save and wake. A fire-time failure marks the invocation `failed` instead of saving the WakeEntry.

### 15. Goose binary missing on PATH

**Setup:** Engine boots; `which goose` returns nothing.

**Expected:** Boot self-check refuses to start the dispatcher (the rest of the engine — storage, MCP listener, ingest — comes up; only the wake/dispatch loop is gated). UI shows "Goose binary not found on PATH — install via `brew install block-goose` (or your platform's equivalent), then restart Proxima." Other engine surfaces stay functional so the user isn't locked out of the app while they install. v1 accepts whatever version `goose --version` reports — no minimum check, no pinning. If a recipe fails because of a missing CLI flag (e.g. user has goose ≤ x.y which lacks `--max-turns`), the failed invocation surfaces the goose stderr verbatim so the user can read the actual incompatibility and upgrade.

## Open Questions (need decisions before implementation)

1. ~~**Approval-state transitions: ChangeEvent or synthetic memory?**~~ **RESOLVED.** Silent default. Approval flips the sidecar; the next ContextBuilder run picks up the now-active goal via the `active_goals` source. Flavors that need reactive approval wakes emit a `proxima-goal/goal-activated-v1` Fact from their approval verb.

2. **Per-WakeEntry cost budget.** `max_wake_chain_depth` stays per personality (it's a chain bound). Cost budget (token / $ ceiling) — open. Goose has no native per-invocation $-cap. We could enforce by killing the subprocess if MCP-call estimates exceed a threshold, but that's heuristic. **Recommendation:** v1 ships `max_rounds` only; add cost ceilings in v1.1 once we see real spend patterns. The goose `--max-turns` knob is a coarse but adequate v1 budget.

3. **Templates: shipped by flavor only, or also user-authored?** v1: flavor-shipped only (TOML files compiled in via `proxima_flavor!`). v1.1: user-authored templates persisted to `personality_templates`. Decision affects whether we invest in a template editor in v1 or only an instance editor.

4. ~~**Where does `core/create_goal` live: substrate or flavor?**~~ **RESOLVED.** Goal flavor, as `proxima-goal/goal_propose`.

5. ~~**Per-WakeEntry model tier.**~~ **RESOLVED.** Lives in the goose recipe's `settings.goose_model`. The engine doesn't manage per-WakeEntry model selection; recipes do.

6. **What does `system_prompt` mean for non-LLM-driven Self schemas?** A future Self schema for a periodic-batch Worker that doesn't run a recipe at all — what does `system_prompt` mean? **Recommendation:** `system_prompt` is `Option<String>` on the Self payload. If the personality has no recipe-running WakeEntries (every entry is a future non-LLM variant), it can be null. With at least one recipe-using entry, it must be non-null and the recipe must declare a `self_perspective` parameter. Validated at `instantiate_personality_from_form` time.

7. ~~**Goose backend: load-bearing or experimental in v1?**~~ **RESOLVED.** Load-bearing. Goose is the only LLM loop runner in v1. No native backend ships; if we ever need one, it's a separate spec.

8. **ContextBuilder DSL: typed Rust enum or arbitrary JSONB?** `ContextSource { kind: String, config: JsonValue }` — kind is a string, config is open. Rust-side, each kind has a typed parser (similar to today's WakeFilter). Open: do we ship a text-DSL for users to author JSONPath expressions, or rely on a UI picker? **Recommendation:** v1 ships raw typed JSON in the editor + a small UI picker for the most common patterns ("the memory id of the trigger event", "a field of the trigger payload"). DSL deferred until we see what users compose.

9. ~~**Recipe content checksum on invocation row.**~~ **RESOLVED: yes.** Add `recipe_sha256: String` to `personality_wake_invocations`, computed at fire time from the resolved recipe content. Makes "what recipe actually ran" forensically answerable when a user edits a YAML file between wakes — the invocation row records exactly which bytes goose saw.

10. ~~**Goose binary version pinning.**~~ **RESOLVED for v1: whatever's on PATH.** Boot self-check runs `which goose` and `goose --version`; if the binary is missing, dispatcher refuses to start with a clear install message. If present, accept any version. No bundled binary in the Tauri shell for v1; user is expected to have goose installed (`brew install block-goose` or platform equivalent). Pinning becomes a v1.1+ concern once we have user-support pain that justifies it — the recipe schema version field (`version: "1.0.0"` in the recipe YAML) is a finer-grained compat signal than the goose binary version anyway.

11. **Model registry crate fate (`crates/llm-openai-compat`, `docs/10-configuration.md` model tiers).** With provider credentials moving entirely to goose, the in-process model client has no callers in v1. Three options: (a) **delete** the crate and any tier infrastructure that doesn't have non-LLM purposes; (b) **keep** the crate as a (now-orphaned) dependency in case we re-add a Native backend post-v1; (c) **shrink** to a tier-name list (no client, no credentials) so recipe-author UI can autocomplete model names against a known set. **Recommendation:** (a) — delete in Phase 1. The provider list is goose's concern; if recipe authors want autocomplete, that's a frontend feature against goose's own provider list (which it surfaces via `goose models list` or similar). Keeping a half-dead crate is dead weight that drifts.

## Rejected Alternatives

- **Many WakeConfigs per Personality.** Considered; rejected at user feedback. One WakeConfig with N entries (unique per trigger) is structurally simpler — dispatcher does an indexed lookup, not a per-personality scan; UI shows a single matrix not a list of behavior collections; the "two parallel behaviors on the same trigger" use case is better served by two personalities than two WakeEntries.
- **Make `PersonalityFlavor::tools()` per-wake by passing `&WakeContext`.** Considered; rejected because it keeps the Rust trait as the unit of definition, which leaves us unable to compose new personalities without recompiling. Half-step that buys nothing.
- **Native (in-process) LLM loop alongside goose.** Considered; rejected as scope creep for v1. Maintaining two loop runners doubles testing, doubles cancellation paths, doubles the prompt-rendering path. If we hit a workload goose can't serve, we add a Native backend then. For v1: one path, one bug surface.
- **Build our own model adapter / Anthropic SDK integration.** Considered (per the original wake/decide/write spec); rejected because goose ships and maintains all major-provider integrations and we have no advantage from owning that code.
- **Single personality with conditional logic in the prompt.** ("If trigger is X do A, else do B.") Considered; rejected because tool palettes are not conditional — once `emit_perspective` is in the palette, the LLM can call it regardless of trigger. WakeEntry is the only way to vary tools per trigger.
- **EntityMutated ChangeEvents on sidecar updates as the goal-approval signal.** Considered; rejected because the silent-context-update path is simpler and serves the common case. EntityMutated as a stream surface is a bigger commitment than this spec needs.
- **Move tool implementations into config too** (templated Bash scripts, WASM blobs). Considered; rejected because tool execution touches storage and the engine's transaction boundaries. Sandboxing user-authored tool code is a separate, much larger problem (think: marketplace permission model). Out of scope.
- **Personality can edit its own Self-Perspective via a `core/edit_self` tool.** Considered; rejected because identity drift creates feedback loops with no obvious bound. Identity edits are user-only via substrate verbs. If a future spec wants this, add a fourth approval state.
- **Recipes embedded inline as a JSONB column on WakeEntry.** Considered; rejected for v1 because file-based recipes let users edit YAML in their editor, which is a much better authoring UX than an in-app textarea. v1.1+ may add a "snapshot recipe content into the row" path for portability.

## Migration Plan (Phasing)

**Phase 0 (prereq, separate plan):** [wake-graph-emitters-reachability-tools](../../../.plans/2026-05-07T12-04-44+0200-wake-graph-emitters-reachability-tools.md) lands first. Declared mechanical emitters + freeze-time reachability give us the validation surface that recipe authoring relies on.

**Phase 1 (replace the harness with goose, persistence change):** Add new tables (`personality`, `personality_wake_entries`, `goal_connection_v1`, `named_purpose_prompt_self_v1`). Extend `personality_wake_invocations` with `wake_entry_id`, `recipe_sha256`, `wake_token`. Write the bundled goose recipes for CommitSummary and Engineer (extracted from current hardcoded `system_prompt()` strings). Add the dispatcher's goose-spawn path with wake-token issuance and the boot self-check that verifies `goose` is on PATH (any version). Migrate existing `personality_wake_config` rows: split into `personality` + `personality_wake_entries` rows; set `recipe_ref` to the bundled recipe; populate `tool_palette` from current `tools()`; default ContextBuilder = `[trigger_event, triggering_memory{include_sidecar=true}, self_perspective, active_goals]`. Switch the dispatcher to read from the new tables and spawn goose. Verify behavior parity against the pre-migration ingest run (number of abstractions written, schema correctness, write authorship). Drop the in-process `run_wake` code path.

Note: v1 does NOT bundle the goose binary in the Tauri shell — user installs from their platform's package manager. Bundling is a v1.1+ distribution concern.

**Phase 2 (composability surface):** Personalities view exposes "Edit WakeEntries" — user can add/remove/edit per-instance WakeEntries (with the unique-trigger constraint enforced inline) and pick `recipe_ref` from a list of bundled + user recipes. ContextBuilder editor (typed JSON + UI picker for common JSONPath cases) lands.

**Phase 3 (compose from scratch):** "New Personality from scratch" UI flow. Templates become TOML files in flavor crates (registered via `proxima_flavor!`). Hardcoded `PersonalityFlavor::system_prompt`/`tools` traits removed; flavor crates ship their personality bodies as TOML templates + bundled goose recipes loaded at registry build time. User can author their own goose recipes in `~/.proxima/recipes/<owner>/`.

**Phase 4 (post-v1, optional):** DB-persisted recipes for marketplace sharing; template editor UI; template versioning + diff-against-template; ContextSource DSL; cost ceilings beyond `max_rounds`.

## References

- [`2026-05-06-personality-wake-decide-write-design.md`](./2026-05-06-personality-wake-decide-write-design.md) — wake/decide/write loop, identity, idempotency, chain depth. **Note:** that spec assumed the engine would own the LLM loop (Anthropic SDK adapter, multi-turn, tool-call retry). This spec replaces that assumption: goose owns the loop, the engine owns dispatch + storage + authorization. The identity model, idempotency rows, and chain-depth bound carry forward unchanged.
- [`docs/04-consolidation.md`](../../04-consolidation.md) — F→A and A→P semantics.
- [`docs/06-goals-and-self.md`](../../06-goals-and-self.md) — Self as anchored Memory; Goals via `core/inspires`.
- [`docs/08-core-and-flavors.md`](../../08-core-and-flavors.md) — what flavors declare; this spec adds "ContextSourceKind", "templates", and "bundled recipes" as additional declaration kinds.
- [`docs/13-flavor-marketplace.md`](../../13-flavor-marketplace.md) — composition discipline; templates and goose recipes extend the marketplace surface beyond schemas/tools.
- [`docs/14-protocol-surface.md`](../../14-protocol-surface.md) — protocol verbs; this spec adds `instantiate_personality_from_form`, `add_wake_entry`, `remove_wake_entry`, `edit_wake_entry`, `attach_goal_manual`, `approve_goal`, `reject_goal`.
- [`.plans/2026-05-07T12-04-44+0200-wake-graph-emitters-reachability-tools.md`](../../../.plans/2026-05-07T12-04-44+0200-wake-graph-emitters-reachability-tools.md) — Phase 0 prerequisite.
- [Goose recipe reference](https://goose-docs.ai/docs/guides/recipes/recipe-reference) — recipe YAML schema; `parameters` (Jinja-substituted from ContextBuilder), `extensions` (always our MCP server), `settings.max_turns` (overridden by `WakeEntry.max_rounds`).
- [Goose CLI](https://github.com/block/goose) — pinned binary version is what the Tauri shell ships; `goose run --recipe ... --params ... --max-turns ... --no-interactive` is the v1 invocation shape.
