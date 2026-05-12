use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use proxima_harness::conversation::{AssistantTurn, Conversation, ToolCall, ToolSpec};
use proxima_harness::providers::{ProviderClient, ProviderError, RoundResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct StubProvider {
    round: AtomicUsize,
}

#[async_trait]
impl ProviderClient for StubProvider {
    async fn tool_round(
        &self,
        _conversation: &Conversation,
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError> {
        let round = self.round.fetch_add(1, Ordering::SeqCst);
        Ok(match round {
            0 => RoundResult::ToolCalls {
                calls: vec![ToolCall {
                    call_id: "call_0".into(),
                    tool_name: "workspace_list_files".into(),
                    arguments: json!({"path": ".", "recursive": false}),
                }],
                raw_assistant: AssistantTurn::default(),
            },
            _ => RoundResult::Final {
                text: "Done.".into(),
                raw_assistant: AssistantTurn {
                    text: "Done.".into(),
                    ..Default::default()
                },
            },
        })
    }
}

#[tokio::test]
async fn stub_provider_returns_two_rounds() {
    let provider = StubProvider::default();
    let conversation = Conversation {
        system_prompt: "test".into(),
        user_seed: "go".into(),
        turns: vec![],
    };
    let tools = Vec::new();

    let first = provider
        .tool_round(&conversation, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(first, RoundResult::ToolCalls { .. }));

    let second = provider
        .tool_round(&conversation, &tools, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(second, RoundResult::Final { .. }));
}
