use proxima_core::inference::{
    InferenceTargetConfig, MistralChatConfig, OpenAIChatConfig, OpenAIResponsesConfig,
};

#[test]
fn mistral_chat_variant_serializes_as_kind_mistral_chat() {
    let config = InferenceTargetConfig::MistralChat(MistralChatConfig {
        base_url: "https://api.mistral.ai".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        temperature: None,
        max_completion_tokens: None,
        reasoning_effort: Some("high".into()),
        context_window_tokens: None,
    });
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["kind"], "mistral_chat");
    assert_eq!(value["reasoning_effort"], "high");
    assert_eq!(value["context_window_tokens"], serde_json::Value::Null);
}

#[test]
fn openai_chat_kind_string() {
    let config = InferenceTargetConfig::OpenAIChat(OpenAIChatConfig {
        base_url: "x".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        temperature: None,
        max_completion_tokens: None,
        context_window_tokens: None,
    });
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["kind"], "openai_chat");
}

#[test]
fn openai_responses_kind_string() {
    let config = InferenceTargetConfig::OpenAIResponses(OpenAIResponsesConfig {
        base_url: "x".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        reasoning_effort: None,
        context_window_tokens: None,
    });
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["kind"], "openai_responses");
}

#[test]
fn openai_chat_without_context_window_deserializes_as_unknown_window() {
    let config: InferenceTargetConfig = serde_json::from_value(serde_json::json!({
        "kind": "openai_chat",
        "base_url": "x",
        "model_id": "m",
        "api_key_env": "K",
        "temperature": null,
        "max_completion_tokens": null
    }))
    .unwrap();

    match config {
        InferenceTargetConfig::OpenAIChat(config) => {
            assert_eq!(config.context_window_tokens, None);
        }
        other => panic!("expected openai_chat config, got {other:?}"),
    }
}
