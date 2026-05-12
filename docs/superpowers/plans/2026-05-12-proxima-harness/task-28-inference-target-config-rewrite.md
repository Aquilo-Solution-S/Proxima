# Task 8.1 — Rewrite `InferenceTargetConfig`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/core/src/inference/types.rs`
- Modify: `crates/core/src/inference/mod.rs` (drop `recipe_resolve`, `recipe_validate` re-exports)
- Modify: `crates/storage-pg/src/settings/inference_targets.rs` (derive the `kind` column from the new variants)

- [ ] **Step 1: Rewrite the enum**

Replace `InferenceTargetConfig` in `crates/core/src/inference/types.rs` with adapter-selector variants:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InferenceTargetConfig {
    MistralChat(MistralChatConfig),
    #[serde(rename = "openai_chat")]
    OpenAIChat(OpenAIChatConfig),
    #[serde(rename = "openai_responses")]
    OpenAIResponses(OpenAIResponsesConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct MistralChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIChatConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct OpenAIResponsesConfig {
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: String,
    pub reasoning_effort: Option<String>,
}
```

Do not add `ChatCompletionsConfig`, `ChatCompletionsCompat`, or `MaxTokensField` to public core types. Chat Completions wire quirks are private to `crates/harness/src/providers/*`.

The explicit OpenAI renames are required. Without them, `rename_all = "snake_case"` leaves acronym casing to serde/heck behavior; migration SQL and Shell TOML need stable `kind = "openai_chat"` / `kind = "openai_responses"` strings.

- [ ] **Step 2: Delete `LocalCliConfig` and `RemoteModelConfig`**

Remove those two structs from `types.rs` entirely.

- [ ] **Step 3: Update re-exports**

`crates/core/src/inference/mod.rs` — drop these from `pub mod` and `pub use`:
- `pub mod recipe_resolve;`
- `pub mod recipe_validate;`
- `LocalCliConfig` and `RemoteModelConfig` in the `pub use types::{…};` list

Add `MistralChatConfig`, `OpenAIChatConfig`, and `OpenAIResponsesConfig` to the same `pub use`.

- [ ] **Step 4: Update storage `kind` derivation**

`crates/storage-pg/src/settings/inference_targets.rs` currently maps:

```rust
InferenceTargetConfig::LocalCli(_) => "local_cli",
InferenceTargetConfig::RemoteModel(_) => "remote_model",
```

Replace it with:

```rust
InferenceTargetConfig::MistralChat(_) => "mistral_chat",
InferenceTargetConfig::OpenAIChat(_) => "openai_chat",
InferenceTargetConfig::OpenAIResponses(_) => "openai_responses",
```

This must match Task 8.2's SQL migration and the `inference_targets_kind_chk` constraint. `inference_targets.kind` and `config->>'kind'` are the same discriminator in two storage locations; any mismatch is a migration bug.
