//! External MCP session compatibility coverage for long-running tool calls.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proxima::flavor::{FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator};
use proxima::{
    AppInfo, AuthzContext, FlavorApp, McpHostTool, McpHostTools, Proxima, ServedSurface, ToolCall,
    ToolEffect, ToolReply,
};
use proxima_core::{AuthError, AuthPath, Authenticator, Credentials, McpToolError, UserId};
use proxima_pg_testkit::SplitRoleDb;
use serde_json::{Value, json};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TOKEN: &str = "host-token";
const SLEEP_TOOL: &str = "host_sleep";

struct HostApp;

impl FlavorBundle for HostApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for HostApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "mcp-host-surface",
            title: "mcp-host-surface",
            version: "0",
        }
    }
}

#[derive(Debug)]
struct SleepTools;

#[async_trait]
impl McpHostTools for SleepTools {
    async fn list(
        &self,
        _auth: &proxima::McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        Ok(vec![
            McpHostTool::new(
                SLEEP_TOOL,
                "Sleep, then answer.",
                json!({"type": "object"}),
                ToolEffect::ReadOnly,
            )
            .with_output_schema(json!({"type": "object"})),
        ])
    }

    async fn call(&self, call: ToolCall) -> Result<ToolReply, McpToolError> {
        let ms = call.args["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(ToolReply::Structured(json!({"slept_ms": ms})))
    }
}

struct StubAuth {
    subject: UserId,
}

#[async_trait]
impl Authenticator for StubAuth {
    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        match credentials {
            Credentials::Bearer(token) if token == TOKEN => Ok(AuthzContext::for_subject(
                self.subject,
                AuthPath::HostBearer,
            )),
            Credentials::Bearer(_) => Err(AuthError::InvalidCredentials),
        }
    }
}

fn mcp_post(client: &reqwest::Client, base: &str, subject: UserId) -> reqwest::RequestBuilder {
    client
        .post(format!("{base}/mcp"))
        .header("Origin", "http://localhost")
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header(
            "X-Proxima-Owner",
            proxima_mcp_server::owner_key(proxima_core::OwnerId::from(subject)),
        )
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
}

async fn sse_json(response: reqwest::Response) -> TestResult<Value> {
    let text = response.text().await?;
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find_map(|data| serde_json::from_str(data.trim()).ok())
        .ok_or_else(|| format!("missing JSON SSE data in {text:?}").into())
}

async fn sse_messages(response: reqwest::Response) -> TestResult<Vec<Value>> {
    let text = response.text().await?;
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect())
}

async fn open_session(client: &reqwest::Client, base: &str, subject: UserId) -> TestResult<String> {
    let response = mcp_post(client, base, subject)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "host-surface-test", "version": "0"}
            }
        }))
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    let session = response
        .headers()
        .get("Mcp-Session-Id")
        .ok_or("missing Mcp-Session-Id")?
        .to_str()?
        .to_owned();
    sse_json(response).await?;
    let ack = mcp_post(client, base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(ack.status().is_success(), "{}", ack.status());
    Ok(session)
}

#[tokio::test]
async fn a_tool_call_outlives_the_session_idle_timeout_on_a_heartbeat_or_without_one() -> TestResult
{
    const SLEEP_MS: u64 = 2_500;
    let db = SplitRoleDb::create("proxima_session_idle", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let serve = |idle: Option<Duration>| {
        Proxima::<HostApp>::app()
            .database_url(db.runtime_url())
            .store_number(0)
            .platform_database_url(db.platform_url())
            .mcp_bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .served_surface(ServedSurface::All)
            .host_tools(Arc::new(SleepTools))
            .mcp_transport(proxima_mcp_server::McpTransportConfig {
                session_idle_timeout: idle,
                ..proxima_mcp_server::McpTransportConfig::default()
            })
            .authenticator(Arc::new(StubAuth { subject }))
            .run()
    };
    let client = reqwest::Client::new();
    let sleep_call = |meta: Value| {
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": SLEEP_TOOL, "arguments": {"ms": SLEEP_MS}, "_meta": meta}})
    };

    let running = serve(Some(Duration::from_secs(1))).await?;
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({"progressToken": "ingest-1"})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    let beats: Vec<&Value> = messages
        .iter()
        .filter(|message| message["method"] == "notifications/progress")
        .collect();
    assert!(beats.len() >= 2, "{messages:?}");
    assert!(
        beats
            .iter()
            .all(|beat| beat["params"]["progressToken"] == "ingest-1")
    );
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(result["id"], json!(7), "{messages:?}");
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"slept_ms": SLEEP_MS})
    );

    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    assert!(
        messages
            .iter()
            .all(|message| message.get("result").is_none()),
        "{messages:?}"
    );
    let after = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": {}}))
        .send()
        .await?;
    assert!(
        !after.status().is_success(),
        "session closed: {}",
        after.status()
    );
    running.shutdown().await;

    let running = serve(None).await?;
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let session = open_session(&client, &base, subject).await?;
    let response = mcp_post(&client, &base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&sleep_call(json!({})))
        .send()
        .await?;
    let messages = sse_messages(response).await?;
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"slept_ms": SLEEP_MS}),
        "{messages:?}"
    );
    running.shutdown().await;
    Ok(())
}
