# Task 8.7 — Update Shell config + TOML round-trip test

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `apps/proxima-shell/src-tauri/src/config/types.rs`
- Modify: the existing config round-trip test

- [ ] **Step 1: Rewrite `InferenceTargetRecord` variants**

The Shell config mirrors `InferenceTargetConfig`. Replace the `LocalCli` / `RemoteModel` cases with `MistralChat` / `OpenAIChat` / `OpenAIResponses` following the same shape as the core enum.

Do not add `ChatCompletionsConfig`, `ChatCompletionsCompat`, or `MaxTokensField` to Shell config. Vendor-specific request quirks are adapter-private.

- [ ] **Step 2: Update the TOML round-trip test**

The current test uses `LocalCli { command: "goose", profile: Some("work") }`. Replace with three test cases:

```rust
#[test]
fn toml_round_trip_mistral_chat() {
    let r = InferenceTargetRecord {
        target_ref: "default-strategic".into(),
        config: InferenceTargetConfigRecord::MistralChat(MistralChatConfigRecord {
            base_url: "https://api.mistral.ai".into(),
            model_id: "mistral-medium-3.5".into(),
            api_key_env: "MISTRAL_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    };
    let toml = toml::to_string(&r).unwrap();
    let back: InferenceTargetRecord = toml::from_str(&toml).unwrap();
    assert_eq!(back, r);
}

#[test]
fn toml_round_trip_openai_chat() {
    let r = InferenceTargetRecord {
        target_ref: "default-chat".into(),
        config: InferenceTargetConfigRecord::OpenAIChat(OpenAIChatConfigRecord {
            base_url: "https://api.openai.com".into(),
            model_id: "gpt-5.1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        }),
    };
    let toml = toml::to_string(&r).unwrap();
    let back: InferenceTargetRecord = toml::from_str(&toml).unwrap();
    assert_eq!(back, r);
}

// + one OpenAIResponses round-trip test
```
