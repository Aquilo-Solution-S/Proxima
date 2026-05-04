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

What is **not** runtime-configurable: schemas, relations, F→A and
A→P prompts, registered tools, source *types*. Those are flavor
crates ([08](docs/08-core-and-flavors.md)). Picking different
behavior means a different binary.

## Config file

`proxima.config.yaml` extends the source-instance shape from
[01 §Bootstrap](docs/01-event-source.md#bootstrap) with two new
top-level blocks:

```yaml
llm:
  default:                                 # one entry per tier; missing tiers fall back per policy below
    fast:                                  # see §Model tiers
      vendor:     anthropic
      dialect:    anthropic
      base_url:   https://api.anthropic.com
      model_id:   claude-haiku-4-5
      secret_ref: env:ANTHROPIC_API_KEY
    standard:
      vendor:     anthropic
      dialect:    anthropic
      base_url:   https://api.anthropic.com
      model_id:   claude-sonnet-4-6
      secret_ref: env:ANTHROPIC_API_KEY
    deep:
      vendor:     anthropic
      dialect:    anthropic
      base_url:   https://api.anthropic.com
      model_id:   claude-opus-4-7
      secret_ref: env:ANTHROPIC_API_KEY
  # presence enables per-Owner overrides; empty in single-tenant
  per_owner_table: enabled                 # | disabled
  fallback_policy: strict                  # strict | upgrade | downgrade — see §Model tiers

embedding:
  vendor:     openai
  dialect:    openai
  base_url:   https://api.openai.com/v1
  model_id:   text-embedding-3-large
  secret_ref: env:OPENAI_API_KEY
  # vector dim is derived from model_id, not declared here

sources:
  # unchanged from 01
  - id: forgejo-aquilo
    ...
```

`vendor` and `dialect` are independent axes. `vendor` is who runs
the inference (`anthropic`, `openrouter`, `groq`, `together`,
`ollama`); `dialect` is which HTTP API the client speaks
(`anthropic`, `openai`). Most non-Anthropic vendors expose the
OpenAI dialect, so `vendor: openrouter, dialect: openai` with
`model_id: anthropic/claude-opus-4-7` is a normal entry. The
split matters because rate limits, tool-call quirks, model
catalogs, and pricing belong to the vendor — not the dialect —
and validation (§Build-time model registry) is keyed by vendor.

`secret_ref` is always indirect — never the literal key. Resolved
schemes (the resolver is a build-time-registered trait, deployments
add schemes by linking a resolver crate):

| Scheme | Use |
|---|---|
| `env:NAME` | Local dev, container env vars |
| `keychain:<service>:<account>` | Embedded-engine desktop ([09](docs/09-frontend.md)) |
| `aws-sm:<arn>` / `gcp-sm:<name>` | Hosted deployments |
| `file:<path>` | On-prem with a sealed mount |

Literal keys never enter `proxima.config.yaml` and never enter
the change-event stream. The resolver returns `SecretBytes` only
to the call site that needs it.

## Build-time model registry

A flavor (or core) declares which `(vendor, model_id)` entries
the binary has been validated against, via fields on the existing
`proxima_flavor!` macro ([08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism)):

```rust
proxima_flavor! {
    name = "code",
    // ...schemas, sources, operators, prompts unchanged...

    llm_models = [
        LlmEntry { vendor: "anthropic",  dialect: Anthropic, model_id: "claude-opus-4-7",            caps: LlmCaps { tool_use: true,  json_mode: true  } },
        LlmEntry { vendor: "anthropic",  dialect: Anthropic, model_id: "claude-sonnet-4-6",          caps: LlmCaps { tool_use: true,  json_mode: true  } },
        LlmEntry { vendor: "openrouter", dialect: OpenAI,    model_id: "anthropic/claude-opus-4-7",  caps: LlmCaps { tool_use: true,  json_mode: true  } },
        LlmEntry { vendor: "groq",       dialect: OpenAI,    model_id: "llama-3.3-70b-versatile",    caps: LlmCaps { tool_use: true,  json_mode: true  } },
        LlmEntry { vendor: "ollama",     dialect: OpenAI,    model_id: "qwen3-coder-30b-a3b",        caps: LlmCaps { tool_use: true,  json_mode: false } },
    ],

    embedding_models = [
        EmbedEntry { vendor: "openai", dialect: OpenAI, model_id: "text-embedding-3-large", caps: EmbedCaps { dim: 3072, matryoshka: true  } },
        EmbedEntry { vendor: "openai", dialect: OpenAI, model_id: "text-embedding-3-small", caps: EmbedCaps { dim: 1536, matryoshka: true  } },
        EmbedEntry { vendor: "ollama", dialect: OpenAI, model_id: "nomic-embed-text",       caps: EmbedCaps { dim: 768,  matryoshka: false } },
    ],
}
```

`(vendor, model_id)` identifies an entry; `dialect` and `caps` are
properties of that entry. Anthropic served direct vs. via
OpenRouter are **separate entries with separate validation** —
different rate limits, different tool-call shapes, different
reliability, different pricing.

A composite binary's allow-set is the union over its flavors,
de-duplicated by `(vendor, model_id)`. Boot fails if the
configured `(vendor, model_id)` is not in the union, or if the
configured embedding's `dim` does not match the column type that
storage migrations created. The mismatch is fatal at startup, not
lazy at first call.

The registry is the contract between *"models we have JSON-mode /
tool-use / prompt discipline tested against"* and *"what users
may configure."* Adding an unsupported model is a flavor-crate PR
— same discipline as schemas. No runtime "trust the user" path.

## Model tiers

Operators don't name a model. They name a **tier** — a coarse
quality/cost class that the deployment maps to a concrete
`(vendor, model_id)`. One model per Owner is too coarse: edge
wiring and cheap classification want a small fast model; deep
self-model A→P and identity-relevant Perspective synthesis want a
frontier model. Tiers split that knob.

v1 ships three core tiers:

| Tier | Intent | Typical use |
|---|---|---|
| `Fast` | Cheap, low-latency, small. High-frequency mechanical work. | Edge-wiring operators, structural F→A extractors, simple classification |
| `Standard` | General-purpose workhorse. The default if a flavor doesn't choose. | Most F→A, tool-calling decider |
| `Deep` | Frontier-quality, expensive. Reasoning that compounds. | Self-model A→P, identity Perspectives, motivation analysis, cross-flavor synthesis |

The tier enum is **core**, not flavor-extensible. Cross-flavor
cognition flavors (`general-reasoning` etc.) must be able to
compose without a tier-namespace problem; expansion is a
substrate PR, not a flavor PR. If three is wrong it's wrong for
everyone at once.

### Operator declaration

Each operator declares two LLM-routing fields at registration
([08 §Registration mechanism](docs/08-core-and-flavors.md#registration-mechanism)):

- `tier: ModelTier` — which tier this operator belongs in. Default
  `Standard` if omitted.
- `requires: LlmCaps` — hard capability requirements (`tool_use`,
  `json_mode`, `long_context`, `vision`, …). Default empty.

```rust
f2a_operators = [
    (ForgejoCommitV3, BugFixClusterV1) => F2A {
        prompt:   prompts::COMMIT_F2A_BUGFIX,
        cadence:  BatchClose,
        requires: LlmCaps { json_mode: true, ..LlmCaps::none() },
        tier:     Standard,
    },
],

a2p_operators = [
    Cross("general/self-model") => A2P {
        inputs:   AnyAbstraction,
        output:   SelfModelV1,
        prompt:   prompts::SELF_MODEL_A2P,
        cadence:  Scheduled("nightly"),
        requires: LlmCaps { json_mode: true, long_context: true, ..LlmCaps::none() },
        tier:     Deep,
    },
],

edge_operators = [
    Cross("general/interpretive-link") => Edge {
        scope:    AbstractionAndPerspective,
        relation: "general/related-tension",
        prompt:   prompts::INTERPRETIVE_LINK,
        cadence:  AbstractionThreshold(50),
        requires: LlmCaps::none(),
        tier:     Fast,
    },
],
```

`requires` and `tier` are independent axes. `requires` is a hard
gate (the resolved model **must** satisfy it or the call fails);
`tier` is a routing hint mapped at deployment time.

### Caps validation at credential write

When a `(vendor, model_id)` lands in `default[tier]` or in an
`llm_credential` row, the binary checks that the entry's
`LlmCaps` (from §Build-time model registry) satisfies the union
of `requires` over every operator with that `tier`. Mismatches are
rejected at write — boot for `default`, INSERT for the per-Owner
table — never silently at first call.

This is the BYOK contract: an Owner whose `Standard` model lacks
`tool_use` cannot run a binary whose decider needs tool-calling.
The failure is visible at credential entry, not three F→A runs
later.

### Fallback policy

When no row exists for `(owner, tier)`:

| Policy | Effect |
|---|---|
| `strict` (default) | Fail-closed. Operator emits `ConfigUnavailable` Action-Fact; UI surfaces the gap. |
| `upgrade` | Walk `Fast → Standard → Deep`. Operator gets a more capable (more expensive) model than declared. |
| `downgrade` | Walk `Deep → Standard → Fast`. Caps-check still applies — a `Deep` operator that needs `long_context` won't silently fall to a `Fast` model that lacks it. |

`strict` is the default because silent quality drift on
identity-relevant work — a `Deep` self-model A→P falling back to
`Fast` — corrupts the agent's projection of itself invisibly.
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

Within a flavor, `embedding_models = [...]` declares the set the
flavor's content has been validated against. The composite binary
picks **one** entry, and that entry must appear in **every**
constituent flavor's set:

```rust
proxima_composite! {
    name      = "aquilo-suite",
    flavors   = [ code, learning ],
    embedding = ("openai", "text-embedding-3-large"),  // must be in every flavor's set
}
```

The macro expansion checks the intersection at compile time. If
`code` ships `[3-large, 3-small]` and `learning` ships only
`[nomic-768]`, the composite refuses to build — the author either
picks a flavor combination whose tested sets intersect, or lands
a PR validating one flavor against the other's embedding model.

This is the same authoring-cost discipline 08 names for
cross-flavor A→P ([08 §Composite discipline](docs/08-core-and-flavors.md#composite-discipline)):
composition surfaces real costs at build time — here, embedding-set
intersections; there, *some* operator (cognition flavor or composite)
must supply cross-flavor inputs. Surface the conflict at build
time, where it can be acted on, not at first query.

Single-flavor binaries inherit the flavor's only entry if its set
is a singleton; otherwise `proxima.config.yaml` names the choice
and the binary validates against the flavor's set at boot.

The vector-store key `(entity_kind, entity_id, embedding_version,
model_id)` ([07](docs/07-storage.md)) admits multiple `model_id`
rows per entity, which leaves the door open for *secondary*
embeddings (specialized retrievers using a different model
alongside the primary). This is **not v1**; the rule above
governs the primary similarity surface only.

## Operator concurrency

Per [04 §Execution model and isolation](docs/04-consolidation.md#execution-model-and-isolation):
operators run inside the substrate's dispatcher, with per-operator
bounded queues and per-(Owner, `personality_id`) fairness within
each queue. Build-time registration fixes the operator graph;
runtime config tunes the concurrency knobs.

```yaml
operators:
  defaults:
    workers:        1            # per-operator concurrency cap
    queue_depth:    1024         # bounded MPSC capacity
    timeout_s:      300          # per-invocation hard cap
    fairness:       deficit       # round-robin | deficit
  per_operator:
    - id:           "code/forgejo-commit→bug-fix-cluster"
      workers:      2
      timeout_s:    180
    - id:           "general-reasoning/self-model"
      workers:      1
      queue_depth:  256

cost_cap:
  llm_concurrency:        8       # global semaphore across all operators
  llm_tokens_per_minute:  200000  # rolling-window guard

sources:
  defaults:
    rate_limit_per_minute: 600
  per_source:
    - id:                  "forgejo-aquilo"
      rate_limit_per_minute: 1200
```

What each axis controls:

- `operators.defaults.workers` — concurrent invocations of any
  one operator. Default `1` (sequential per operator) is the safe
  starting point; bump for operators with high LLM throughput
  budget and short prompts.
- `operators.defaults.queue_depth` — how many invocations stack
  before the dispatcher applies backpressure to the enqueueing
  side (typically the change_event tail or the batch-close
  trigger). Reaching the cap produces a logged event, not a
  silent drop.
- `operators.defaults.timeout_s` — hard cap on a single
  invocation; the LLM call is cancelled and the run is recorded
  as failed (no partial persistence — see
  [04 §Output protocol](docs/04-consolidation.md#output-protocol)).
- `operators.defaults.fairness` — how the per-operator queue
  schedules across (Owner, `personality_id`) keys. `deficit` gives
  weighted fairness under uneven load; `round-robin` is simpler
  and fine when load is even.
- `cost_cap.llm_concurrency` — binary-wide semaphore gating any
  operator's LLM call. Caps total in-flight LLM requests
  regardless of per-operator workers; the protective ceiling for
  token spend.
- `cost_cap.llm_tokens_per_minute` — rolling-window enforcement;
  the dispatcher refuses to admit a new invocation if it would
  exceed the cap.
- `sources.defaults.rate_limit_per_minute` — Phase-1 rate limit
  per `EventSource` instance. Independent of the operator
  dispatcher; Phase 1 doesn't go through it
  ([04 §Execution model and isolation](docs/04-consolidation.md#execution-model-and-isolation)).

`per_operator` overrides match by operator id —
`<flavor>/<input-schema>→<output-schema>` for F→A;
`<flavor>/<operator-name>` for A→P / A→Goal / Edge. Unmatched
operators inherit `defaults`. The dispatcher logs the effective
config per operator at boot.

What this **doesn't** configure:

- **Operator identity or output schemas.** Build-time facts;
  `proxima_flavor!` declares them.
- **Disjoint-output invariants.** Compile-time fact from composite
  discipline (08); the dispatcher relies on them, does not enforce
  them at runtime.
- **Cross-operator priority.** No global priority between operators
  beyond the cost cap; operators are independent by construction.
  If F→A throughput matters more than A→P latency in a deployment,
  give F→A more workers — don't try to encode "F→A is more
  important" globally.

## LLM credential resolution

Per-call resolution path:

```
1. owner = call.owner
2. tier  = operator.tier                          // declared at registration; default Standard
3. cred  = llm_credential.lookup(owner, tier)     // optional table
4. if cred is None:
       cred = llm_credential.lookup(owner, fallback_tier(tier))   // per fallback_policy
5. if cred is None:
       cred = config.llm.default[tier]            // optional block, indexed by tier
6. if cred is None:
       cred = config.llm.default[fallback_tier(tier)]             // last-resort fallback
7. if cred is None or cred.secret_ref unresolvable:
       fail-closed:
         - operator does not run
         - emit ConfigUnavailable Action-Fact ([05](docs/05-actions.md))
         - UI surfaces the Action-Fact; user reconfigures
8. caps-check: assert entry.caps satisfies operator.requires
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
| unset | empty | Misconfigured for that tier — boot fails if any operator targets it and `fallback_policy = strict` |

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
tiers; missing tiers resolve via §Fallback policy. INSERT validates:

1. `(vendor, model_id)` is in the build-time registry — an Owner
   cannot pick a model the binary was not built to support.
2. The entry's `LlmCaps` satisfies the union of `requires` over
   every operator registered with that `tier` (§Caps validation
   at credential write).

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

```yaml
prices:
  defaults:
    llm:
      - { vendor: "anthropic",  model_id: "claude-opus-4-7",
          prompt_per_mtok_usd:       15.00,
          cache_read_per_mtok_usd:    1.50,
          cache_write_per_mtok_usd:  18.75,    # 5-min cache; rotate for 1-hr
          completion_per_mtok_usd:   75.00 }
      - { vendor: "anthropic",  model_id: "claude-sonnet-4-6",
          prompt_per_mtok_usd:        3.00,
          cache_read_per_mtok_usd:    0.30,
          cache_write_per_mtok_usd:   3.75,
          completion_per_mtok_usd:   15.00 }
      - { vendor: "openrouter", model_id: "anthropic/claude-opus-4-7",
          prompt_per_mtok_usd:       15.30,    # OpenRouter passthrough +2%
          completion_per_mtok_usd:   76.50 }
      - { vendor: "groq",       model_id: "llama-3.3-70b-versatile",
          prompt_per_mtok_usd:        0.59,
          completion_per_mtok_usd:    0.79 }
      - { vendor: "ollama",     model_id: "qwen3-coder-30b-a3b",
          prompt_per_mtok_usd:        0.0,
          completion_per_mtok_usd:    0.0 }
    embedding:
      - { vendor: "openai", model_id: "text-embedding-3-large",
          per_mtok_usd: 0.13 }
      - { vendor: "openai", model_id: "text-embedding-3-small",
          per_mtok_usd: 0.02 }
      - { vendor: "ollama", model_id: "nomic-embed-text",
          per_mtok_usd: 0.0 }
```

Substrate ships the `defaults` block populated for every entry in
§Build-time model registry, sourced from publicly published
per-million-token list prices. Defaults are indicative — customers
with negotiated rates override per `(vendor, model_id)` in the
runtime DB tables below. Local-inference rows price at zero so
the same calculation runs for every deployment shape.

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
`effective_at <= now()`. INSERT validates that `(vendor, model_id)`
is in the build-time registry — pricing entries for unregistered
models are rejected at write time, same shape as credentials.

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
  rows under that user's `Owner` *before* the first F→A or A→P run.
  Tiers the user skips fall back per `fallback_policy`; under the
  default `strict` policy any operator targeting an unpopulated
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
| Local dev | `default[*]`: `base_url: http://localhost:11434`, no key | one tier wired (typically `Standard`); `fallback_policy: upgrade` so `Fast` and `Deep` operators land here too | local Ollama, small dim |
| Embedded desktop ([09](docs/09-frontend.md)) | per-Owner, `keychain:` refs per tier | user fills any subset; common: `Fast`→local Ollama, `Standard`/`Deep`→hosted | local Ollama or hosted |
| BYOK on-prem | per-Owner table, populated by org admin | typically all three tiers populated explicitly | binary-wide, hosted or local |
| Aquilo-hosted (memophant free tier) | `default[*]` block, Aquilo's keys | one or more tiers; users on a single-tier free plan get `fallback_policy: upgrade` (or `strict` with degraded operators surfaced in UI) | Aquilo's embedding model |
| Aquilo-hosted (memophant BYOK tier) | per-Owner table, populated at signup | user picks per-tier model; signup UI shows which tiers each operator targets | Aquilo's embedding model |

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
- **Not a model-selection-per-prompt knob.** F→A, A→P, edge, and
  decider operators name a *tier* (§Model tiers), not a model id.
  Tiers are deployment-mapped via `llm_credential` (per-Owner) or
  `default[tier]` (per-binary). A flavor that genuinely needs a
  *specific* model for a specific prompt is mistaken — the right
  move is finer-grained tiers, registered into the substrate enum
  via PR.

## Anchors

- `scope`
- `config-file`
- `build-time-model-registry`
- `model-tiers`
- `operator-declaration`
- `caps-validation-at-credential-write`
- `fallback-policy`
- `embedding-model-one-per-binary`
- `composite-embedding-selection`
- `operator-concurrency`
- `llm-credential-resolution`
- `per-owner-credential-table`
- `price-book`
- `bootstrap`
- `deployment-shapes`
- `what-this-doc-is-not`
