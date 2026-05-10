# 10 — Configuration

Per [08 §Decision](docs/08-core-and-flavors.md): build-time
registers the *types*; runtime config picks *which* instance and
*which* credentials. This doc is the runtime side — specifically
the LLM endpoint, embedding model, and credential surfaces that
any deployment must populate before the binary can think.

## Scope

Three axes are runtime-configurable per binary:

| Axis | Default | Per-Owner override | Vector contract |
|---|---|---|---|
| LLM endpoint + model | binary-wide | yes | n/a |
| Embedding endpoint + model | binary-wide | **no** | dim assertion at boot |
| EventSource credentials (Forgejo PAT, Telegram bot, …) | per source instance | already per-source ([01 §Bootstrap](docs/01-event-source.md#bootstrap)) | n/a |

What is **not** runtime-configurable: schemas, relations,
personality prompts, registered tools, wake-filter kinds, source *types*.
Those are flavor
crates ([08](docs/08-core-and-flavors.md)). Picking different
behavior means a different binary.

## Config file

`proxima.config.toml` extends the source-instance shape from
[01 §Bootstrap](docs/01-event-source.md#bootstrap) with two new
top-level blocks. v1 ships TOML; the format is private to the
binary and Settings UI — never on the wire.

The `[[llm.models]]` and `[[embedding.models]]` arrays carry the
runtime model registrations (§Model registration). The
`[llm.default]` table picks one record per tier:

```toml
# Registered models — one [[llm.models]] entry per (vendor, model_id).
[[llm.models]]
vendor      = "anthropic"
dialect     = "anthropic"
base_url    = "https://api.anthropic.com"
model_id    = "claude-haiku-4-5"
secret_ref  = "env:ANTHROPIC_API_KEY"
caps        = { tool_use = true, json_mode = true, long_context = false, vision = false }

[[llm.models]]
vendor      = "anthropic"
dialect     = "anthropic"
base_url    = "https://api.anthropic.com"
model_id    = "claude-sonnet-4-6"
secret_ref  = "env:ANTHROPIC_API_KEY"
caps        = { tool_use = true, json_mode = true, long_context = true, vision = false }

[[llm.models]]
vendor      = "anthropic"
dialect     = "anthropic"
base_url    = "https://api.anthropic.com"
model_id    = "claude-opus-4-7"
secret_ref  = "env:ANTHROPIC_API_KEY"
caps        = { tool_use = true, json_mode = true, long_context = true, vision = true }

# Tier bindings — pick one (vendor, model_id) per tier.
# Missing tiers fall back per policy below.
[llm.default.fast]
vendor   = "anthropic"
model_id = "claude-haiku-4-5"

[llm.default.standard]
vendor   = "anthropic"
model_id = "claude-sonnet-4-6"

[llm.default.deep]
vendor   = "anthropic"
model_id = "claude-opus-4-7"

[llm]
per_owner_table = "enabled"          # | "disabled"
fallback_policy = "strict"           # strict | upgrade | downgrade — see §Model tiers

# Single global embedding (§Embedding model: one per binary).
[[embedding.models]]
vendor      = "openai"
dialect     = "openai"
base_url    = "https://api.openai.com/v1"
model_id    = "text-embedding-3-large"
secret_ref  = "env:OPENAI_API_KEY"
caps        = { dim = 3072, matryoshka = true }

[embedding.default]
vendor   = "openai"
model_id = "text-embedding-3-large"

# Source instances — unchanged from 01.
[[sources]]
id = "forgejo-aquilo"
# ...
```

`vendor` and `dialect` are independent axes. `vendor` is who runs
the inference (`anthropic`, `openrouter`, `groq`, `together`,
`ollama`); `dialect` is which HTTP API the client speaks
(`anthropic`, `openai`). Most non-Anthropic vendors expose the
OpenAI dialect, so `vendor: openrouter, dialect: openai` with
`model_id: anthropic/claude-opus-4-7` is a normal entry. The
split matters because rate limits, tool-call quirks, model
catalogs, and pricing belong to the vendor — not the dialect —
and the runtime model record (§Model registration) is keyed by
`(vendor, model_id)`.

`secret_ref` is always indirect — never the literal key. Resolved
schemes (the resolver is a build-time-registered trait, deployments
add schemes by linking a resolver crate):

| Scheme | Use |
|---|---|
| `env:NAME` | Local dev, container env vars |
| `keychain:<service>:<account>` | Embedded-engine desktop ([09](docs/09-frontend.md)) |
| `aws-sm:<arn>` / `gcp-sm:<name>` | Hosted deployments |
| `file:<path>` | On-prem with a sealed mount |

Literal keys never enter `proxima.config.toml` and never enter
the change-event stream. The resolver returns `SecretBytes` only
to the call site that needs it.

## Capability vocabulary

Build-time owns *what* model capabilities exist and *which* a
personality requires. It does **not** own which `(vendor, model_id)`
pairs the binary will use — those are runtime configuration.
Plugging in a new model never requires a flavor-crate PR.

Two types live in `proxima_core::models`:

```rust
pub struct LlmCaps {
    pub tool_use:     bool,
    pub json_mode:    bool,
    pub long_context: bool,
    pub vision:       bool,
}

pub struct EmbedCaps {
    pub dim:        u32,
    pub matryoshka: bool,
}
```

`LlmCaps` enumerates the LLM capability axes any personality can
demand; `EmbedCaps` does the same for embeddings. Expanding
either set (e.g. adding `streaming_tool_use`) is a substrate PR —
flavors cannot extend the vocabulary, only consume it.

Personalities declare a tier and a `requires: LlmCaps` at registration
(§Personality declaration). The runtime model record (§Model
registration) carries the *claimed* `LlmCaps` for that
`(vendor, model_id)`; caps validation at credential-write
(§Caps validation at credential write) checks the claim
satisfies the union of `requires` over personalities in that tier.

The contract is: *personalities say what they need; users (or a probe
step) say what their model offers; the substrate refuses
mismatches at write, not at first call.* No allowlist. No
flavor-author gating. New models plug in.

## Model registration

A registered model record carries:

```toml
[[llm.models]]
vendor      = "ollama"
dialect     = "openai"             # which HTTP API the client speaks
model_id    = "qwen3-coder-30b-a3b"
base_url    = "http://localhost:11434/v1"
secret_ref  = ""                   # empty for local; "keychain:..." for remote
caps        = { tool_use = true, json_mode = false, long_context = false, vision = false }

[[llm.models]]
vendor      = "openrouter"
dialect     = "openai"
model_id    = "anthropic/claude-opus-4-7"
base_url    = "https://openrouter.ai/api/v1"
secret_ref  = "keychain:proxima:openrouter-key"
caps        = { tool_use = true, json_mode = true, long_context = true, vision = false }

[[embedding.models]]
vendor      = "ollama"
dialect     = "openai"
model_id    = "nomic-embed-text"
base_url    = "http://localhost:11434/v1"
secret_ref  = ""
caps        = { dim = 768, matryoshka = false }
```

Records are persisted in `proxima.config.toml` (the embedded
desktop binary's app-data dir; hosted deployments lay it down at
deploy time). The Settings UI provides a registration panel that
writes these records and offers a best-effort "Probe" button that
hits the endpoint to auto-fill `model_id` and detect caps where
the provider exposes them.

`caps` is the user's (or the probe's) claim about the model. The
substrate trusts the claim for routing decisions but enforces
**caps satisfaction at credential write** — see below — so a
miss-declared model can't satisfy a personality that needs more
than the model can deliver.

`(vendor, model_id)` identifies a record uniquely. Anthropic-
direct vs. via OpenRouter are **separate records with separate
caps and separate `secret_ref`** — different rate limits,
different tool-call shapes, different reliability, different
pricing.

The embedding `dim` is checked against the storage migration's
vector column at boot. Mismatch is fatal at startup, not lazy at
first embed call.

## Model tiers

Personalities don't name a model. They name a **tier** — a coarse
quality/cost class that the deployment maps to a concrete
`(vendor, model_id)`. One model per Owner is too coarse: edge
wiring and cheap classification want a small fast model; deep
self-relevant Perspective synthesis wants a
frontier model. Tiers split that knob.

v1 ships three core tiers:

| Tier | Intent | Typical use |
|---|---|---|
| `Fast` | Cheap, low-latency, small. High-frequency mechanical work. | Edge-wiring, structural extractors, simple classification |
| `Standard` | General-purpose workhorse. The default if a flavor doesn't choose. | Most personality wake decisions |
| `Deep` | Frontier-quality, expensive. Reasoning that compounds. | Identity Perspectives, motivation analysis, cross-flavor synthesis |

The tier enum is **core**, not flavor-extensible. Cross-flavor
cognition flavors (`general-reasoning` etc.) must be able to
compose without a tier-namespace problem; expansion is a
substrate PR, not a flavor PR. If three is wrong it's wrong for
everyone at once.

### Personality declaration

Each personality declares two LLM-routing fields at registration
([08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism)):

- `tier: ModelTier` — which tier this personality belongs in. Default
  `Standard` if omitted.
- `requires: LlmCaps` — hard capability requirements (`tool_use`,
  `json_mode`, `long_context`, `vision`, …). Default empty.

```rust
impl PersonalityFlavor for CodeEngineerPersonality {
    fn tier(&self) -> ModelTier { ModelTier::Deep }
    fn requires(&self) -> LlmCaps {
        LlmCaps { json_mode: true, ..LlmCaps::none() }
    }
}
```

`requires` and `tier` are independent axes. `requires` is a hard
gate (the resolved model **must** satisfy it or the call fails);
`tier` is a routing hint mapped at deployment time.

### Caps validation at credential write

When a `(vendor, model_id)` is bound to `default[tier]` or
written into an `llm_credential` row, the binary checks that the
record's *claimed* `LlmCaps` (§Model registration) satisfies the
union of `requires` over every personality with that `tier`.
Mismatches are rejected at write — boot for `default`, INSERT for
the per-Owner table — never silently at first call.

This is the BYOK contract: an Owner whose `Standard` model lacks
`tool_use` cannot run a binary whose personality requires tool
calling. The failure is visible at credential entry, not at first
wake.

### Fallback policy

When no row exists for `(owner, tier)`:

| Policy | Effect |
|---|---|
| `strict` (default) | Fail-closed. Personality wake emits `ConfigUnavailable` Action-Fact; UI surfaces the gap. |
| `upgrade` | Walk `Fast → Standard → Deep`. Personality gets a more capable (more expensive) model than declared. |
| `downgrade` | Walk `Deep → Standard → Fast`. Caps-check still applies — a `Deep` personality that needs `long_context` won't silently fall to a `Fast` model that lacks it. |

`strict` is the default because silent quality drift on
identity-relevant work — a `Deep` self-perspective wake falling
back to `Fast` — corrupts the agent's projection of itself invisibly.
Most deployments should pin all three tiers explicitly. Fallback
is the escape hatch (local dev, single-tier free tier), not the
norm.

## Embedding model: one per binary

Embeddings are intentionally **not** per-Owner.

The vector store ([07 §Vector store](docs/07-storage.md#vector-store-independent))
keys on `(entity_kind, entity_id, embedding_version, model_id)` —
multiple `model_id`s can coexist per entity, but cross-Owner
similarity queries presuppose a canonical embedding space per
binary. Letting Owner A's content embed under one model and Owner
B's under another would either partition similarity per Owner
(kills cross-Owner discovery in multi-tenant deployments) or
force runtime re-embedding on cross-Owner queries (cost spike,
undefined precision).

Changing the embedding model is therefore a **deployment-level
event**, not a per-user knob: bump `embedding_version`, run a
re-embed sweep ([07](docs/07-storage.md)), retire the old version
when the sweep completes. The `model_id` column lets old and new
coexist during the sweep.

## Composite embedding selection

A composite binary still has a single binary-wide embedding —
that constraint is unchanged (§Embedding model: one per binary).
What changed: there is no per-flavor `embedding_models` set to
intersect. The deployment registers exactly one embedding model
record (§Model registration) at runtime, and boot validates two
things:

1. The record's claimed `dim` matches the storage migration's
   vector column type. Mismatch is fatal.
2. The configured embedding `secret_ref` resolves (where
   non-empty). Unresolvable references fail boot.

Cross-flavor authoring concerns about embedding compatibility
(does flavor A's content embed sensibly under the model flavor B
needs?) are real but live outside the substrate — they're
deployment / quality concerns, not compile-time invariants. If a
deployment switches embedding models, the storage layer's
re-embed sweep ([07](docs/07-storage.md)) handles the migration;
the substrate refuses to run cross-version queries silently.

The vector-store key `(entity_kind, entity_id, embedding_version,
model_id)` ([07](docs/07-storage.md)) admits multiple `model_id`
rows per entity, which leaves the door open for *secondary*
embeddings (specialized retrievers using a different model
alongside the primary). This is **not v1**; the rule above
governs the primary similarity surface only.

## Dispatcher concurrency

Per [04 §Execution model and isolation](docs/04-consolidation.md#execution-model-and-isolation):
personalities run inside the substrate's dispatcher, with
per-personality bounded queues and per-(Owner, personality instance)
fairness within each queue. Build-time registration fixes the
personality graph;
runtime config tunes the concurrency knobs.

```toml
[personalities.defaults]
workers     = 1            # per-personality concurrency cap
queue_depth = 1024         # bounded MPSC capacity
timeout_s   = 300          # per-invocation hard cap
fairness    = "deficit"    # round-robin | deficit

# Note: `id` strings here are flavor-shipped personality identifiers (e.g.,
# `proxima-code/engineer-v1` is a Code-flavor recipe / type identifier), not
# engine archetypes. The engine's runtime personality identity is
# `PersonalityInstanceId`. See
# `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`.

[[personalities.per_personality]]
id        = "proxima-code/commit-summary-v1"
workers   = 2
timeout_s = 180

[[personalities.per_personality]]
id          = "proxima-code/engineer-v1"
workers     = 1
queue_depth = 256

[cost_cap]
llm_concurrency       = 8        # global semaphore across all personality wakes
llm_tokens_per_minute = 200000   # rolling-window guard

[sources.defaults]
rate_limit_per_minute = 600

[[sources.per_source]]
id                    = "forgejo-aquilo"
rate_limit_per_minute = 1200
```

What each axis controls:

- `personalities.defaults.workers` — concurrent invocations of any
  one personality type. Default `1` (sequential per personality) is the safe
  starting point; bump for personalities with high LLM throughput
  budget and short prompts.
- `personalities.defaults.queue_depth` — how many invocations stack
  before the dispatcher applies backpressure to the enqueueing
  side (the `change_event` tail). Reaching the cap produces a logged event, not a
  silent drop.
- `personalities.defaults.timeout_s` — hard cap on a single
  invocation; the LLM call is cancelled and the run is recorded
  as failed (no partial persistence — see
  [04 §Output protocol](docs/04-consolidation.md#output-protocol)).
- `personalities.defaults.fairness` — how the per-personality queue
  schedules across (Owner, personality instance) keys. `deficit` gives
  weighted fairness under uneven load; `round-robin` is simpler
  and fine when load is even.
- `cost_cap.llm_concurrency` — binary-wide semaphore gating any
  personality LLM call. Caps total in-flight LLM requests
  regardless of per-personality workers; the protective ceiling for
  token spend.
- `cost_cap.llm_tokens_per_minute` — rolling-window enforcement;
  the dispatcher refuses to admit a new invocation if it would
  exceed the cap.
- `sources.defaults.rate_limit_per_minute` — Phase-1 rate limit
  per `EventSource` instance. Independent of the personality
  dispatcher; Phase 1 doesn't go through it
  ([04 §Execution model and isolation](docs/04-consolidation.md#execution-model-and-isolation)).

`per_personality` overrides match by `personality_type_id`.
Unmatched personalities inherit `defaults`. The dispatcher logs
the effective config per personality at boot.

What this **doesn't** configure:

- **Personality identity or output schemas.** Build-time facts;
  `proxima_flavor!` declares them.
- **Disjoint-output invariants.** Compile-time fact from composite
  discipline (08); the dispatcher relies on them, does not enforce
  them at runtime.
- **Cross-personality priority.** No global priority between
  personalities beyond the cost cap; personalities are independent
  by construction.

## LLM credential resolution

Per-call resolution path:

```
1. owner = call.owner
2. tier  = personality.tier                       // declared at registration; default Standard
3. cred  = llm_credential.lookup(owner, tier)     // optional table
4. if cred is None:
       cred = llm_credential.lookup(owner, fallback_tier(tier))   // per fallback_policy
5. if cred is None:
       cred = config.llm.default[tier]            // optional block, indexed by tier
6. if cred is None:
       cred = config.llm.default[fallback_tier(tier)]             // last-resort fallback
7. if cred is None or cred.secret_ref unresolvable:
       fail-closed:
         - personality wake does not run
         - emit ConfigUnavailable Action-Fact ([05](docs/05-actions.md))
         - UI surfaces the Action-Fact; user reconfigures
8. caps-check: assert entry.caps satisfies personality.requires
   (already enforced at write time per §Caps validation; this is
   a defence-in-depth assert, not a routing decision)
```

Fail-closed means **no implicit fallback** to a free Aquilo key,
no silent downgrade across tiers under `strict` policy. A revoked
BYOK key stops the pipeline that would have used it; the failure
is observable in the same stream as every other action.

Both the table and the default block are independently optional,
covering the four modes (per tier):

| `default` | per-Owner table | Mode |
|---|---|---|
| set | empty | Single-tenant or shared-key hosted |
| unset | populated | Pure BYOK (every Owner brings their own) |
| set | populated | Mixed: default for free tier, per-Owner for BYOK tier |
| unset | empty | Misconfigured for that tier — boot fails if any personality targets it and `fallback_policy = strict` |

## Per-Owner credential table

```
llm_credential(
    owner_id,           -- (principal, org_id) per 01
    tier,               -- Fast | Standard | Deep (§Model tiers)
    vendor,
    dialect,
    model_id,
    base_url,
    secret_ref,
    created_at,
    revoked_at,
    pk(owner_id, tier, created_at)
)
-- head row per (owner_id, tier) where revoked_at IS NULL is active.
```

Append-only by `created_at`; rotation is supersession ([07](docs/07-storage.md)),
not silent overwrite — credential history is the same auditable
shape as everything else. An Owner may populate any subset of
tiers; missing tiers resolve via §Fallback policy. INSERT
validates that the record's claimed `LlmCaps` satisfies the
union of `requires` over every personality registered with that
`tier` (§Caps validation at credential write). The
`(vendor, model_id)` itself is not gated against any allowlist —
new models plug in at runtime; the caps claim is the contract.

The `secret_ref` column stores the *reference*, not the secret.
The same resolver schemes from §Config file apply; for BYOK on a
hosted deployment, signup writes a `secret_ref: aws-sm:<arn>` and
the actual key lands in AWS Secrets Manager, never in Postgres.

## Price book

The dispatcher computes `cost_micro_usd` for each `LlmCallV1` /
`EmbeddingCallV1` Fact ([05 §Dispatcher-emitted call Facts](docs/05-actions.md#dispatcher-emitted-call-facts))
from a runtime price book keyed by `(vendor, model_id)`. The book
is runtime config — vendors change prices, new models appear — but
populated identically across deployment shapes.

```toml
[[prices.defaults.llm]]
vendor                   = "anthropic"
model_id                 = "claude-opus-4-7"
prompt_per_mtok_usd      = 15.00
cache_read_per_mtok_usd  = 1.50
cache_write_per_mtok_usd = 18.75    # 5-min cache; rotate for 1-hr
completion_per_mtok_usd  = 75.00

[[prices.defaults.llm]]
vendor                   = "anthropic"
model_id                 = "claude-sonnet-4-6"
prompt_per_mtok_usd      = 3.00
cache_read_per_mtok_usd  = 0.30
cache_write_per_mtok_usd = 3.75
completion_per_mtok_usd  = 15.00

[[prices.defaults.llm]]
vendor                  = "openrouter"
model_id                = "anthropic/claude-opus-4-7"
prompt_per_mtok_usd     = 15.30    # OpenRouter passthrough +2%
completion_per_mtok_usd = 76.50

[[prices.defaults.llm]]
vendor                  = "groq"
model_id                = "llama-3.3-70b-versatile"
prompt_per_mtok_usd     = 0.59
completion_per_mtok_usd = 0.79

[[prices.defaults.llm]]
vendor                  = "ollama"
model_id                = "qwen3-coder-30b-a3b"
prompt_per_mtok_usd     = 0.0
completion_per_mtok_usd = 0.0

[[prices.defaults.embedding]]
vendor       = "openai"
model_id     = "text-embedding-3-large"
per_mtok_usd = 0.13

[[prices.defaults.embedding]]
vendor       = "openai"
model_id     = "text-embedding-3-small"
per_mtok_usd = 0.02

[[prices.defaults.embedding]]
vendor       = "ollama"
model_id     = "nomic-embed-text"
per_mtok_usd = 0.0
```

Substrate ships the `defaults` block populated for the common
hosted vendors (Anthropic, OpenAI, OpenRouter, Groq, Together,
Ollama-local) sourced from publicly published per-million-token
list prices. Defaults are indicative — customers with negotiated
rates override per `(vendor, model_id)` in the runtime DB tables
below. Local-inference rows price at zero so the same
calculation runs for every deployment shape. Entries for
`(vendor, model_id)` pairs not in `defaults` produce a
once-per-pair WARN log and a `cost_micro_usd = None` on the
resulting Fact (§Cost resolution); pricing is descriptive, not
gating.

Cache pricing covers Anthropic's 5-minute tier by default; the
1-hour tier is a runtime override on the same row, not a separate
entry. Whichever rate is active at call time is what the
dispatcher records on the resulting Fact.

### Override tables

Append-only by `effective_at`, matching `llm_credential`
(§Per-Owner credential table). Historical pricing is preserved by
storing the call's computed `cost_micro_usd` on the Fact at write
time — price changes never retroactively rewrite call costs.

```
llm_price_book(
    vendor,
    model_id,
    prompt_per_mtok_usd,
    cache_read_per_mtok_usd,    -- nullable; null => no separate cache rate
    cache_write_per_mtok_usd,   -- nullable
    completion_per_mtok_usd,
    effective_at,
    pk(vendor, model_id, effective_at)
)

embedding_price_book(
    vendor,
    model_id,
    per_mtok_usd,
    effective_at,
    pk(vendor, model_id, effective_at)
)
```

Active row per `(vendor, model_id)` is the head where
`effective_at <= now()`. Pricing entries are descriptive — no
INSERT-time gate against any allowlist. Entries for
`(vendor, model_id)` pairs that no credential references are
inert; entries for missing pairs simply don't price calls (per
§Cost resolution).

Per-Owner price overrides (negotiated rates routed per tenant) are
**out of scope for v1**. The hosted cost model is "engine pays the
vendor; cost is observable to the customer"; markup, currency
conversion, and per-tenant invoicing are downstream concerns
layered on the same Fact stream.

### Cost resolution

Per LLM call:

```
1. price = lookup(vendor, model_id)
       — DB head row first, then config defaults block
2. if price is None:
       cost_micro_usd = None
       emit a once-per-(vendor, model_id) WARN log so the gap is fixable
3. else:
       cost_usd =
             prompt_tokens         * price.prompt_per_mtok_usd                / 1_000_000
           + cache_read_tokens     * price.cache_read_per_mtok_usd
                                       .unwrap_or(price.prompt_per_mtok_usd)  / 1_000_000
           + cache_write_tokens    * price.cache_write_per_mtok_usd
                                       .unwrap_or(price.prompt_per_mtok_usd)  / 1_000_000
           + completion_tokens     * price.completion_per_mtok_usd            / 1_000_000
       cost_micro_usd = Some(round(cost_usd * 1_000_000))
```

Per embedding call:

```
cost_micro_usd = Some(round(total_tokens * price.per_mtok_usd))
```

A cache field on the call with no matching cache rate in the book
falls back to the prompt rate. This over-charges, which is the
safer direction for a cost ceiling — under-charging would silently
drift `cost_cap` enforcement.

### What this section is not

- **Not a billing engine.** `cost_micro_usd` is what the engine
  pays the vendor. Customer-facing billing — markup, currency
  conversion, invoicing — reads this Fact stream and applies its
  own rules.
- **Not vendor-API-discoverable.** Anthropic, OpenAI, Groq, etc.
  return token counts but not unit prices in their API responses.
  Pricing comes from this table; the engine cannot self-update it.
- **Not on the wire.** Like credentials, the price book is admin
  surface only — the `Schema` verb ([14](docs/14-protocol-surface.md))
  advertises supported `(vendor, model_id)` pairs for credential
  editors, never their prices.

## Bootstrap

[06](docs/06-goals-and-self.md) covers each flavor's onboarding
flow eliciting founding goals. Configuration bootstrap layers on
top:

- **BYOK deployments** (memophant BYOK tier, on-prem with
  per-user keys): onboarding elicits one credential per tier the
  user wants populated and writes the corresponding `llm_credential`
  rows under that user's `Owner` *before* the first personality wake.
  Tiers the user skips fall back per `fallback_policy`; under the
  default `strict` policy any personality targeting an unpopulated
  tier raises `ConfigUnavailable` at first call.
- **Shared-key deployments** (memophant free tier, Aquilo-hosted
  Code): onboarding skips the credential step; the `default[tier]`
  blocks carry the keys.

A single binary supports both shapes simultaneously. The
resolution path returns whichever credential resolves first per
tier.

## Deployment shapes

| Shape | LLM credential source | Tier population | Embedding source |
|---|---|---|---|
| Local dev | `default[*]`: `base_url: http://localhost:11434`, no key | one tier wired (typically `Standard`); `fallback_policy: upgrade` so `Fast` and `Deep` personalities land here too | local Ollama, small dim |
| Embedded desktop ([09](docs/09-frontend.md)) | per-Owner, `keychain:` refs per tier | user fills any subset; common: `Fast`→local Ollama, `Standard`/`Deep`→hosted | local Ollama or hosted |
| BYOK on-prem | per-Owner table, populated by org admin | typically all three tiers populated explicitly | binary-wide, hosted or local |
| Aquilo-hosted (memophant free tier) | `default[*]` block, Aquilo's keys | one or more tiers; users on a single-tier free plan get `fallback_policy: upgrade` (or `strict` with degraded personalities surfaced in UI) | Aquilo's embedding model |
| Aquilo-hosted (memophant BYOK tier) | per-Owner table, populated at signup | user picks per-tier model; signup UI shows which tiers each personality targets | Aquilo's embedding model |

Same binary, same config schema, same resolution path. The
deployment shape is a populate-the-rows decision, not a build
flag.

## What this doc is not

- **Not a transport spec.** The `Schema` verb ([14](docs/14-protocol-surface.md))
  advertises *which* `(vendor, model_id)` pairs the binary
  supports so client UI can render typed credential editors;
  configuration payload itself is delivered out of band (deploy
  time or admin UI), never on the client protocol.
- **Not a tenant-management spec.** `Owner` resolution and the
  admin UX for editing `llm_credential` rows live in 09 and the
  org-admin surface (TBD).
- **Not a billing spec.** Usage metering rides on the Action-Fact
  stream — bare core's `LlmCallV1` and `EmbeddingCallV1` payloads
  ([05 §Dispatcher-emitted call Facts](docs/05-actions.md#dispatcher-emitted-call-facts))
  carry per-call token counts (including cache reads / writes),
  latency, and `cost_micro_usd` computed from §Price book.
  Billing is a downstream consumer of the same change feed every
  other client tails. The dispatcher's unconditional Fact emission
  is what makes `cost_cap` and per-Owner BYOK accounting
  trustworthy — direct vendor-SDK calls bypass both, so the
  invariant in 05 forbids them.
- **Not a model-selection-per-prompt knob.** Personalities name a
  *tier* (§Model tiers), not a model id.
  Tiers are deployment-mapped via `llm_credential` (per-Owner) or
  `default[tier]` (per-binary). A flavor that genuinely needs a
  *specific* model for a specific prompt is mistaken — the right
  move is finer-grained tiers, registered into the substrate enum
  via PR.

## Anchors

- `scope`
- `config-file`
- `capability-vocabulary`
- `model-registration`
- `model-tiers`
- `personality-declaration`
- `caps-validation-at-credential-write`
- `fallback-policy`
- `embedding-model-one-per-binary`
- `composite-embedding-selection`
- `dispatcher-concurrency`
- `llm-credential-resolution`
- `per-owner-credential-table`
- `price-book`
- `bootstrap`
- `deployment-shapes`
- `what-this-doc-is-not`
