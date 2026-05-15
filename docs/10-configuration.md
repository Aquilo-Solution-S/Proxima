# 10 - Configuration

Current runtime configuration contract. Build-time registration owns
schemas, relations, prompts, tools, source types, wake filters, and
personality types (see [08](08-core-and-flavors.md)). Runtime config
selects concrete inference targets, tier bindings, embedding endpoint,
credentials, and deployment-level artefact storage.

<a id="scope"></a>
## Scope

| Surface | Scope | Current contract |
|---|---|---|
| Chat inference target | per Owner | `target_ref -> InferenceTargetConfig` |
| Chat tier binding | per Owner | `ModelTier -> target_ref` |
| Wake entry routing | per Owner | optional `inference_target_ref`, else `model_tier` |
| Embedding model | binary-wide | registered `(vendor, model_id)` rows |
| Active embedding model | binary-wide | singleton `(vendor, model_id)` |
| Large artefact S3 storage | binary-wide | process env + AWS SDK credential chain |
| EventSource credentials | per source instance | source-owned, not LLM-owned |

Not runtime configurable: schema ids, payload types, relation
descriptors, prompts, tool definitions, source types, wake-filter kinds,
and personality type registration.

<a id="config-file"></a>
## Config File

`AppConfig` is the Shell settings DTO for import/export/readback. The
authoritative runtime source is Postgres settings tables:

| Table | Scope |
|---|---|
| `proxima_core.inference_targets` | per Owner |
| `proxima_core.inference_tier_bindings` | per Owner |
| `proxima_core.embedding_models` | binary-wide |
| `proxima_core.embedding_active` | binary-wide singleton |

Representative TOML shape:

```toml
[inference.inference_tier_bindings]
fast = "local-fast"
standard = "standard-chat"
deep = "codex-deep"

[[inference.targets]]
target_ref = "standard-chat"

[inference.targets.config]
kind = "openai_responses"
base_url = "https://api.openai.com/v1"
model_id = "gpt-5.2"
api_key_env = "OPENAI_API_KEY"
reasoning_effort = "medium"

[[inference.targets]]
target_ref = "codex-deep"

[inference.targets.config]
kind = "chatgpt_codex"
base_url = "https://chatgpt.com/backend-api/codex"
model_id = "gpt-5.3-codex"
reasoning_effort = "high"

[[embedding.models]]
vendor = "openai"
model_id = "text-embedding-3-small"
base_url = "https://api.openai.com/v1"
secret_ref = "keychain:proxima:openai_api_key"
caps = { dim = 1536, matryoshka = true }

[embedding.active]
vendor = "openai"
model_id = "text-embedding-3-small"
```

The TOML format is private to Shell/settings. The Engine protocol does
not expose this file as a wire contract.

<a id="capability-vocabulary"></a>
## Capability Vocabulary

Core owns the vocabulary:

```rust
pub enum ModelTier { Fast, Standard, Deep }

pub struct LlmCaps {
    pub tool_use: bool,
    pub json_mode: bool,
    pub long_context: bool,
    pub vision: bool,
}

pub struct EmbedCaps {
    pub dim: u32,
    pub matryoshka: bool,
}
```

`LlmCaps` and `EmbedCaps` are core vocabulary, not flavor-extensible
runtime keys. Current chat routing stores target configs and tier
bindings; it does not yet enforce `LlmCaps` satisfaction when a target
is written.

<a id="model-registration"></a>
## Model Registration

Current chat target shape:

```rust
pub struct InferenceTargetRow {
    pub owner: Owner,
    pub target_ref: String,
    pub config: InferenceTargetConfig,
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfig {
    MistralChat(MistralChatConfig),
    OpenAIChat(OpenAIChatConfig),
    OpenAIResponses(OpenAIResponsesConfig),
    ChatGPTCodex(ChatGPTCodexConfig),
}
```

Target refs are per Owner. `register_inference_target` is idempotent
for identical config and rejects config conflicts for an existing
`target_ref`. Deletion is rejected while a target is bound to a tier or
referenced by a wake entry.

Current variants:

| Variant | Credential source |
|---|---|
| `mistral_chat` | `api_key_env` env-var name |
| `openai_chat` | `api_key_env` env-var name |
| `openai_responses` | `api_key_env` env-var name |
| `chatgpt_codex` | `~/.codex/auth.json` |

`api_key_env` is an environment variable name, not a secret value and
not a `secret_ref`.

<a id="model-tiers"></a>
## Model Tiers

`ModelTier` is core-fixed:

| Tier | Intent |
|---|---|
| `Fast` | cheap, low-latency work |
| `Standard` | default general work |
| `Deep` | expensive reasoning that compounds |

Wake entries carry `model_tier`. Dispatch resolves:

1. If `wake_entry.inference_target_ref` is set, use that target.
2. Otherwise resolve `wake_entry.model_tier` through
   `inference_tier_bindings`.
3. If the target or binding is missing, fail closed before the LLM call.

There is no automatic fallback, downgrade, or upgrade.

<a id="personality-declaration"></a>
## Personality Declaration

Personality type registration remains build-time (see
[08 §Registration mechanism](08-core-and-flavors.md#registration-mechanism)).
Current LLM routing is wake-entry owned: each stored wake entry carries
`model_tier`, optional `inference_target_ref`, tool palettes, execution
mode, and max rounds. This keeps runtime routing mutable without making
personality types runtime-registered.

<a id="caps-validation-at-credential-write"></a>
## Caps Validation At Credential Write

Deferred. Current code persists inference target configs and validates
referential integrity. It does not compute the union of personality
requirements per tier and does not reject a target because of `LlmCaps`
at credential write.

<a id="fallback-policy"></a>
## Fallback Policy

Current policy: strict only.

| Missing state | Result |
|---|---|
| pinned `inference_target_ref` missing | dispatch error |
| tier has no binding | dispatch error |
| bound target missing | dispatch error |
| provider credentials missing | pre-run error |

No fallback target is selected implicitly.

<a id="embedding-model-one-per-binary"></a>
## Embedding Model: One Per Binary

Embeddings are binary-wide, not per Owner. The graph has one active
embedding model because vector rows are shared infrastructure, not
Owner policy. Per-Owner embedding models would make vector dimensions
and nearest-neighbor semantics tenant-dependent inside one binary.

`embedding_models` is keyed by `(vendor, model_id)`.
`embedding_active` is a singleton row pointing at one registered model.

<a id="composite-embedding-selection"></a>
## Composite Embedding Selection

Composite binaries still choose exactly one active embedding model at
runtime. Flavor crates may declare operators that consume embeddings,
but they do not own a separate embedding registry. Changing embedding
model is a binary-level operational decision and may require
re-embedding.

<a id="dispatcher-concurrency"></a>
## Dispatcher Concurrency

Deferred as runtime config. Current wake entries carry `max_rounds`; the
dispatcher resolves one provider target per fired wake and executes with
that target. No documented operator-concurrency config surface is
implemented here.

<a id="llm-credential-resolution"></a>
## LLM Credential Resolution

Resolution is variant-specific:

| Variant | Resolution |
|---|---|
| `mistral_chat` | read env var named by `api_key_env` |
| `openai_chat` | read env var named by `api_key_env` |
| `openai_responses` | read env var named by `api_key_env` |
| `chatgpt_codex` | read ChatGPT Codex auth from `~/.codex/auth.json` |

Missing credentials produce a provider-target error before execution.
Secrets are not streamed to clients.

<a id="large-artefact-s3"></a>
## Large Artefact S3

Large cited-object storage is deployment infrastructure, not per-Owner
configuration.

| Key | Required | Default |
|---|---:|---|
| `PROXIMA_S3_BUCKET` | yes | - |
| `PROXIMA_S3_REGION` | yes | - |
| `PROXIMA_S3_ENDPOINT_URL` | no | AWS region endpoint |
| `PROXIMA_S3_FORCE_PATH_STYLE` | no | `false` |
| `PROXIMA_S3_UPLOAD_TTL_SECONDS` | no | `900` |
| `PROXIMA_S3_READ_TTL_SECONDS` | no | `300` |

Credentials use the standard AWS SDK provider chain. Missing S3 config
does not fail Shell boot; cited-blob commands fail typed at call time.
Commands return presigned URLs only, never `bucket` or `object_key`.

<a id="per-owner-credential-table"></a>
## Per-Owner Inference Tables

Inference targets and tier bindings are Owner-scoped rows. This is
credential and routing ownership, not memory access control. Owner
access control remains per-row on graph data (see
[01 §Owner](01-event-source.md#owner--scoping-primitive)).

Embedding settings are explicitly excluded from this per-Owner surface.

<a id="price-book"></a>
## Price Book

Deferred. No current runtime price-book table participates in routing.
Cost policy is expressed by binding wake tiers to target refs.

<a id="bootstrap"></a>
## Bootstrap

Boot sequence:

1. Build-time registries are linked into the binary.
2. Migrations create settings tables.
3. Shell reads Owner-scoped inference settings for the sentinel Owner.
4. Shell reads binary-wide embedding settings.
5. Wake dispatch fails closed if a fired wake cannot resolve a target.
6. Embedding client wiring is skipped if no active embedding model is
   configured.

<a id="deployment-shapes"></a>
## Deployment Shapes

| Shape | Config source |
|---|---|
| Embedded desktop Shell | Tauri settings commands over local Engine/Postgres |
| Headless MCP host | Postgres settings tables plus process env/Codex auth |
| Hosted deployment | provisioned settings rows plus deployment secrets |

The same Engine contract applies: build-time types, runtime target
instances.

<a id="what-this-doc-is-not"></a>
## What This Doc Is Not

Not a protocol spec (see [14](14-protocol-surface.md)). Not a storage
schema source of truth (see migrations and [07](07-storage.md)). Not a
frontend UX spec (see [09](09-frontend.md)). Not a source-instance
contract (see [01](01-event-source.md)).
