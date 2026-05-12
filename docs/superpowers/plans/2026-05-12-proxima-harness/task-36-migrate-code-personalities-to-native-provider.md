# Task 8.9 — Migrate Code's two personalities to a native provider target

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: the provisioning path located in Task 6.5

- [ ] **Step 1: Switch default `inference_target_ref`**

Today the default Engineer + Execution Worker rows point at a `target_ref` resolved by Goose's local config. After the cut they must resolve to a native `MistralChat`, `OpenAIChat`, or `OpenAIResponses` inference-target row. Prefer `MistralChat` when `MISTRAL_API_KEY` exists; otherwise use `OpenAIChat` when `OPENAI_API_KEY` exists.

```rust
let target_ref = if std::env::var("MISTRAL_API_KEY").is_ok() {
    register_default_inference_target(
        engine,
        &owner,
        "default-strategic",
        InferenceTargetConfig::MistralChat(MistralChatConfig {
            base_url: "https://api.mistral.ai".into(),
            model_id: "mistral-medium-3.5".into(),
            api_key_env: "MISTRAL_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    ).await?;
    "default-strategic"
} else if std::env::var("OPENAI_API_KEY").is_ok() {
    register_default_inference_target(
        engine,
        &owner,
        "default-chat",
        InferenceTargetConfig::OpenAIChat(OpenAIChatConfig {
            base_url: "https://api.openai.com".into(),
            model_id: "gpt-5.1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    ).await?;
    "default-chat"
} else {
    return Err("no MISTRAL_API_KEY or OPENAI_API_KEY in env; cannot provision default inference target".into());
};
```

Run on a fresh DB. Verify the Engineer personality's default `inference_target_ref` resolves correctly post-provisioning.
