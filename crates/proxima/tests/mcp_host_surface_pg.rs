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
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TOKEN: &str = "host-token";
const GATE_TOOL: &str = "host_gate";

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

/// A tool call that runs until the test releases it, so the call's length is
/// set by the test's barrier rather than by a sleep racing the idle timeout.
#[derive(Debug)]
struct GateTools {
    entered: Notify,
    release: Semaphore,
}

impl GateTools {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
        })
    }
}

#[async_trait]
impl McpHostTools for GateTools {
    async fn list(
        &self,
        _auth: &proxima::McpAuthContext,
    ) -> Result<Vec<McpHostTool>, McpToolError> {
        Ok(vec![
            McpHostTool::new(
                GATE_TOOL,
                "Run until released, then answer.",
                json!({"type": "object"}),
                ToolEffect::ReadOnly,
            )
            .with_output_schema(json!({"type": "object"})),
        ])
    }

    async fn call(&self, _call: ToolCall) -> Result<ToolReply, McpToolError> {
        self.entered.notify_one();
        self.release
            .acquire()
            .await
            .map_err(|err| McpToolError::Other(err.to_string()))?
            .forget();
        Ok(ToolReply::Structured(json!({"released": true})))
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

/// Read an SSE response as it arrives, handing each JSON message to
/// `on_message` before the next is read, until the stream ends.
async fn sse_stream(
    mut response: reqwest::Response,
    mut on_message: impl FnMut(&Value),
) -> TestResult<Vec<Value>> {
    let mut pending = String::new();
    let mut messages = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        pending.push_str(std::str::from_utf8(&chunk)?);
        while let Some(end) = pending.find('\n') {
            let line: String = pending.drain(..=end).collect();
            if let Some(message) = line
                .strip_prefix("data:")
                .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok())
            {
                on_message(&message);
                messages.push(message);
            }
        }
    }
    Ok(messages)
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

fn gate_call(meta: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
           "params": {"name": GATE_TOOL, "arguments": {}, "_meta": meta}})
}

async fn post_gate_call(
    client: &reqwest::Client,
    base: &str,
    subject: UserId,
    meta: &Value,
) -> TestResult<(String, reqwest::Response)> {
    let session = open_session(client, base, subject).await?;
    let response = mcp_post(client, base, subject)
        .header("Mcp-Session-Id", &session)
        .json(&gate_call(meta))
        .send()
        .await?;
    Ok((session, response))
}

#[tokio::test]
async fn a_tool_call_outlives_the_session_idle_timeout_on_a_heartbeat_or_without_one() -> TestResult
{
    // The idle timeout is this test's own choice: short enough to keep the
    // test quick, long enough that the heartbeat (idle / 2) leaves a margin
    // a loaded runner's scheduling hiccups do not eat. Beats are counted,
    // not timed: the call is held open by a barrier until three beats have
    // arrived, so it outlives the idle timeout by construction.
    const IDLE: Duration = Duration::from_secs(2);
    const BEATS: usize = 3;
    let db = SplitRoleDb::create("proxima_session_idle", &[]).await?;
    let subject = UserId::new(Uuid::now_v7());
    let serve = |idle: Option<Duration>, gate: Arc<GateTools>| {
        Proxima::<HostApp>::app()
            .database_url(db.runtime_url())
            .store_number(0)
            .platform_database_url(db.platform_url())
            .mcp_bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .served_surface(ServedSurface::All)
            .host_tools(gate)
            .mcp_transport(proxima_mcp_server::McpTransportConfig {
                session_idle_timeout: idle,
                ..proxima_mcp_server::McpTransportConfig::default()
            })
            .authenticator(Arc::new(StubAuth { subject }))
            .run()
    };
    let client = reqwest::Client::new();

    let gate = GateTools::new();
    let running = serve(Some(IDLE), Arc::clone(&gate)).await?;
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let meta = json!({"progressToken": "ingest-1"});
    let (_, response) = post_gate_call(&client, &base, subject, &meta).await?;
    let mut beats = 0;
    let messages = sse_stream(response, |message| {
        if message["method"] == "notifications/progress" {
            beats += 1;
            if beats == BEATS {
                gate.release.add_permits(1);
            }
        }
    })
    .await?;
    let beats: Vec<&Value> = messages
        .iter()
        .filter(|message| message["method"] == "notifications/progress")
        .collect();
    assert_eq!(beats.len(), BEATS, "{messages:?}");
    assert!(
        beats
            .iter()
            .all(|beat| beat["params"]["progressToken"] == "ingest-1")
    );
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(result["id"], json!(7), "{messages:?}");
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"released": true})
    );

    // Without a progress token nothing keeps the session alive: the gate is
    // never released, and the stream ends when the idle timeout closes it.
    let (session, response) = post_gate_call(&client, &base, subject, &json!({})).await?;
    let messages = sse_stream(response, |_| {}).await?;
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
    gate.release.add_permits(1);
    running.shutdown().await;

    // With no idle timeout the same call completes once released.
    let gate = GateTools::new();
    let running = serve(None, Arc::clone(&gate)).await?;
    let base = format!("http://{}", running.mcp_addr().ok_or("no MCP address")?);
    let call = async {
        let (_, response) = post_gate_call(&client, &base, subject, &json!({})).await?;
        sse_stream(response, |_| {}).await
    };
    let release = async {
        gate.entered.notified().await;
        gate.release.add_permits(1);
    };
    let (messages, ()) = tokio::join!(call, release);
    let messages = messages?;
    let result = messages.last().ok_or("no messages")?;
    assert_eq!(
        result["result"]["structuredContent"],
        json!({"released": true}),
        "{messages:?}"
    );
    running.shutdown().await;
    Ok(())
}
