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
    });
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["kind"], "mistral_chat");
}

#[test]
fn openai_chat_kind_string() {
    let config = InferenceTargetConfig::OpenAIChat(OpenAIChatConfig {
        base_url: "x".into(),
        model_id: "m".into(),
        api_key_env: "K".into(),
        temperature: None,
        max_completion_tokens: None,
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
    });
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["kind"], "openai_responses");
}
