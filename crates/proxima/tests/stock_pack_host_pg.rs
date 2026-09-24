//! The stock pack host, end to end through the facade's environment path.
//!
//! Every runtime here boots as `proxima::run` does — `Proxima::app()` plus
//! one environment layer — except that the layer is an injected map
//! (`from_lookup`) rather than process env, which the workspace lints forbid
//! mutating. No authenticator, forwarder policy, header allowlist, or probe
//! switch is set in code: the OIDC authenticator is built from
//! `PROXIMA_OIDC_*` against a loopback issuer this file serves, and every
//! served-path feature comes from its `PROXIMA_*` variable.
//!
//! The one setting that cannot come from the environment is the deployment
//! tool scope: `RuntimeBuilder` reads no tool-scope variable
//! (`PROXIMA_TOOL_PROFILE` belongs to the `proxima-mcp` binary), so the test
//! flavor names it in its own `FlavorApp::configure`, which is where a stock
//! pack host names it too.
#![cfg(feature = "auth-oidc")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
use axum::Router;
use axum::http::{StatusCode, header};
use axum::routing::get;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::future::BoxFuture;
use proxima::flavor::{
    FlavorBundle, FlavorRegistry, FlavorRegistryError, McpToolAnnotations, NamedMigrator,
    RequestHeaders, Tool, ToolCtx, ToolError,
};
use proxima::{
    AccessKind, AppContext, AppInfo, Authz, FlavorApp, Proxima, Role, RunningProxima,
    RuntimeBuilder, ToolScope,
};
use proxima_pg_testkit::{create_db, drop_db, split_role_urls, unique_db_name};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const KID: &str = "pack-host-key";
const AUDIENCE: &str = "proxima-pack-host";
const PUBLIC_URL: &str = "https://proxima.pack.test";
const FOREIGN_HOST: &str = "foreign.pack.test";
const FORWARDER_SUB: &str = "pack-forwarder";
const MEMBER_SUB: &str = "pack-member";
const TICKET_HEADER: &str = "x-pack-ticket";
const SECRET_HEADER: &str = "x-pack-secret";
const PROBE_TOOL: &str = "packhost_probe";
const SLOW_PATH: &str = "/pack/slow";
const SLOW_FOR: Duration = Duration::from_millis(500);

/// Set by the slow route once its handler runs, so the drain test signals
/// only while that request is provably in flight.
static SLOW_STARTED: AtomicBool = AtomicBool::new(false);

// --- the test flavor -------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ProbeArgs {}

/// What one served call looked like from inside the tool.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ProbeOutput {
    /// `x-pack-ticket`, when the allowlist copied it.
    ticket: Option<String>,
    /// Every header name the tool can see.
    header_names: Vec<String>,
    /// The selected owner's external key.
    owner: String,
    /// `Debug` of the role the authz context holds for that owner.
    role: Option<String>,
    /// The engine's refusal of an Editor-level (Abstraction) write permit on
    /// that owner; `None` when granted.
    editor_write_refusal: Option<String>,
}

/// Declared read-only so a viewer reaches it at all: an undeclared flat tool
/// demands write access at the scope gate, which would hide the difference
/// this probe reports.
struct Probe;

impl Tool for Probe {
    const NAME: &'static str = PROBE_TOOL;
    const DESCRIPTION: &'static str =
        "Reports the request headers, owner, and role a forwarded call carries.";
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(true).open_world(false));

    type Args = ProbeArgs;
    type Output = ProbeOutput;

    fn call(ctx: ToolCtx, _args: ProbeArgs) -> BoxFuture<'static, Result<ProbeOutput, ToolError>> {
        Box::pin(async move {
            let headers = ctx.service::<RequestHeaders>();
            let owner = ctx.owner();
            let editor_write_refusal = ctx
                .owner_write_permit(AccessKind::Abstraction)
                .await
                .err()
                .map(|err| err.to_string());
            Ok(ProbeOutput {
                ticket: headers
                    .as_ref()
                    .and_then(|headers| headers.get(TICKET_HEADER))
                    .map(ToOwned::to_owned),
                header_names: headers
                    .map(|headers| headers.iter().map(|(name, _)| name.to_owned()).collect())
                    .unwrap_or_default(),
                owner: owner.external_key(),
                role: ctx
                    .authz()
                    .role_for_owner(&owner)
                    .map(|role| format!("{role:?}")),
                editor_write_refusal,
            })
        })
    }
}

struct PackHost;

impl FlavorBundle for PackHost {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.try_add_mcp_tool::<Probe>("packhost")
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for PackHost {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "stock-pack-host-test",
            title: "Stock Pack Host Test",
            version: "1",
        }
    }

    /// The tool scope has no environment variable on the facade path.
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.tool_scope(ToolScope::All)
    }

    fn mount_http(router: Router, _ctx: AppContext) -> Router {
        router.route(SLOW_PATH, get(slow))
    }
}

/// An authenticated host route that takes long enough to still be running
/// when the shutdown signal lands.
async fn slow(Authz(_authz): Authz) -> (StatusCode, &'static str) {
    SLOW_STARTED.store(true, Ordering::SeqCst);
    tokio::time::sleep(SLOW_FOR).await;
    (StatusCode::OK, "drained")
}

// --- a loopback identity provider ------------------------------------------

/// Signs tokens and serves the matching JWKS at `{issuer}/jwks`.
struct Idp {
    signing: RsaKeyPair,
    issuer: String,
    server: JoinHandle<()>,
}

impl Idp {
    async fn start() -> TestResult<Self> {
        let signing = RsaKeyPair::generate(KeySize::Rsa2048)?;
        let public = signing.public_key();
        let jwks = json!({
            "keys": [{
                "kty": "RSA",
                "kid": KID,
                "alg": "RS256",
                "use": "sig",
                "n": URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
                "e": URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
            }]
        })
        .to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!("http://{}", listener.local_addr()?);
        let app = Router::new().route(
            "/jwks",
            get(move || {
                let body = jwks.clone();
                async move { ([(header::CONTENT_TYPE, "application/json")], body) }
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            signing,
            issuer,
            server,
        })
    }

    fn bearer(&self, sub: &str) -> TestResult<String> {
        let exp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600;
        let header = json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
        let claims = json!({"iss": self.issuer, "aud": AUDIENCE, "sub": sub, "exp": exp});
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
        );
        let mut signature = vec![0; self.signing.public_modulus_len()];
        self.signing.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )?;
        Ok(format!(
            "Bearer {signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}

// --- one database, one issuer, two mapped subjects -------------------------

struct Stack {
    db_name: String,
    runtime_url: String,
    platform_url: String,
    idp: Idp,
    forwarder: Uuid,
    member: Uuid,
}

impl Stack {
    async fn new() -> Self {
        let db_name = unique_db_name("proxima_pack_host");
        create_db(&db_name).await.expect("PG required for tests");
        let (runtime_url, platform_url) = split_role_urls(&db_name).await.expect("split roles");
        let idp = Idp::start().await.expect("loopback identity provider");
        Self {
            db_name,
            runtime_url,
            platform_url,
            idp,
            forwarder: Uuid::now_v7(),
            member: Uuid::now_v7(),
        }
    }

    /// The deployment environment, with `overrides` replacing or adding
    /// entries.
    fn env(&self, overrides: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = [
            ("DATABASE_URL", self.runtime_url.clone()),
            ("PROXIMA_PLATFORM_DATABASE_URL", self.platform_url.clone()),
            ("PROXIMA_MCP_BIND", "127.0.0.1:0".to_owned()),
            ("PROXIMA_OIDC_ISSUER", self.idp.issuer.clone()),
            ("PROXIMA_OIDC_JWKS_URI", format!("{}/jwks", self.idp.issuer)),
            ("PROXIMA_OIDC_AUDIENCE", AUDIENCE.to_owned()),
            ("PROXIMA_PUBLIC_URL", PUBLIC_URL.to_owned()),
            (
                "PROXIMA_OIDC_SUBJECT_MAP",
                format!(
                    "{FORWARDER_SUB}:{},{MEMBER_SUB}:{}",
                    self.forwarder, self.member
                ),
            ),
            ("PROXIMA_FORWARDER_SUBJECTS", self.forwarder.to_string()),
            ("PROXIMA_FORWARDER_ROLE", "editor".to_owned()),
            ("PROXIMA_REQUEST_HEADERS", TICKET_HEADER.to_owned()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
        for (key, value) in overrides {
            env.insert((*key).to_owned(), (*value).to_owned());
        }
        env
    }

    /// Boot exactly as a stock host does, with `env` as its only input.
    async fn boot(&self, overrides: &[(&str, &str)]) -> TestResult<(RunningProxima, String)> {
        let env = self.env(overrides);
        let running = Proxima::<(PackHost,)>::app()
            .from_lookup(move |key| env.get(key).cloned())?
            .run()
            .await?;
        let addr = running.mcp_addr.ok_or("MCP listener did not bind")?;
        Ok((running, format!("http://{addr}")))
    }

    async fn cleanup(self) {
        self.idp.server.abort();
        let _ = drop_db(&self.db_name).await;
    }
}

fn group_key(group: Uuid) -> String {
    format!("group:{group}")
}

// --- MCP over HTTP ---------------------------------------------------------

fn mcp_post(client: &reqwest::Client, base: &str, bearer: &str) -> reqwest::RequestBuilder {
    client
        .post(format!("{base}/mcp"))
        .header("Origin", "http://localhost")
        .header("Authorization", bearer)
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
}

async fn initialize(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    owner_key: &str,
) -> TestResult<reqwest::Response> {
    Ok(mcp_post(client, base, bearer)
        .header("X-Proxima-Owner", owner_key)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "pack-host-test", "version": "0"}
            }
        }))
        .send()
        .await?)
}

/// Initialize and acknowledge; the session is bound to `owner_key`.
async fn open_session(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    owner_key: &str,
) -> TestResult<String> {
    let response = initialize(client, base, bearer, owner_key).await?;
    assert!(
        response.status().is_success(),
        "initialize as {owner_key}: {}",
        response.status()
    );
    let session = response
        .headers()
        .get("Mcp-Session-Id")
        .ok_or("missing Mcp-Session-Id")?
        .to_str()?
        .to_owned();
    sse_json(response).await?;
    let ack = mcp_post(client, base, bearer)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await?;
    assert!(ack.status().is_success(), "initialized: {}", ack.status());
    Ok(session)
}

/// Call the probe in `session`, sending `headers` with the call itself.
async fn call_probe(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    headers: &[(&str, &str)],
) -> TestResult<Value> {
    let mut request = mcp_post(client, base, bearer).header("Mcp-Session-Id", session);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": PROBE_TOOL, "arguments": {}}
        }))
        .send()
        .await?;
    assert!(
        response.status().is_success(),
        "tools/call: {}",
        response.status()
    );
    let body = sse_json(response).await?;
    assert_ne!(
        body["result"]["isError"],
        json!(true),
        "the probe itself must not fail: {body}"
    );
    if let Some(structured) = body["result"].get("structuredContent") {
        return Ok(structured.clone());
    }
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("probe returned no output: {body}"))?;
    Ok(serde_json::from_str(text)?)
}

async fn sse_json(response: reqwest::Response) -> TestResult<Value> {
    let text = response.text().await?;
    for data in text.lines().filter_map(|line| line.strip_prefix("data:")) {
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(data) {
            return Ok(value);
        }
    }
    Err(format!("missing JSON SSE data in {text:?}").into())
}

// --- 1. the forwarded call -------------------------------------------------

/// A forwarder subject selects a Group it is no member of and holds the
/// configured role there; the call's allowlisted header reaches the tool and
/// nothing else does; a mapped subject that is not a forwarder cannot select
/// the same Group.
#[tokio::test]
async fn a_forwarded_call_reaches_the_tool_with_its_ticket_under_the_fixed_role() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack.boot(&[]).await?;
        let client = reqwest::Client::new();
        let group = Uuid::now_v7();
        let forwarder = stack.idp.bearer(FORWARDER_SUB)?;

        let session = open_session(&client, &base, &forwarder, &group_key(group)).await?;
        let seen = call_probe(
            &client,
            &base,
            &forwarder,
            &session,
            &[(TICKET_HEADER, "t-123"), (SECRET_HEADER, "s-456")],
        )
        .await?;
        assert_eq!(seen["ticket"], "t-123", "{seen}");
        assert_eq!(seen["owner"], group_key(group), "{seen}");
        assert_eq!(
            seen["header_names"],
            json!([TICKET_HEADER]),
            "only the allowlisted header crosses: {seen}"
        );
        assert!(
            !seen.to_string().contains("s-456"),
            "a header outside the allowlist reached the tool: {seen}"
        );
        assert_eq!(
            seen["role"],
            format!("{:?}", Role::editor()),
            "the forwarder holds exactly PROXIMA_FORWARDER_ROLE in the Group: {seen}"
        );
        assert_eq!(
            seen["editor_write_refusal"],
            Value::Null,
            "an editor forwarder may write the Group it selected: {seen}"
        );

        // The ticket belongs to the call, not to the session it rides.
        let unticketed = call_probe(&client, &base, &forwarder, &session, &[]).await?;
        assert_eq!(unticketed["ticket"], Value::Null, "{unticketed}");
        assert_eq!(unticketed["header_names"], json!([]), "{unticketed}");

        // A mapped subject that is not a forwarder: its token is good for its
        // own owner, and refused for the Group it does not belong to.
        let member = stack.idp.bearer(MEMBER_SUB)?;
        open_session(
            &client,
            &base,
            &member,
            &format!("personal:{}", stack.member),
        )
        .await?;
        let refused = initialize(&client, &base, &member, &group_key(group)).await?;
        assert_eq!(
            refused.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "a non-forwarder must not select a Group it is not a member of"
        );

        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup().await;
    result.expect("forwarded call test failed");
}

/// The role is the configured one, not a member's: with
/// `PROXIMA_FORWARDER_ROLE=viewer` the same forwarder reads the Group but
/// the engine refuses it the write an editor forwarder is granted above.
#[tokio::test]
async fn a_viewer_forwarder_is_refused_the_write_an_editor_forwarder_gets() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack.boot(&[("PROXIMA_FORWARDER_ROLE", "viewer")]).await?;
        let client = reqwest::Client::new();
        let group = Uuid::now_v7();
        let forwarder = stack.idp.bearer(FORWARDER_SUB)?;

        let session = open_session(&client, &base, &forwarder, &group_key(group)).await?;
        let seen = call_probe(
            &client,
            &base,
            &forwarder,
            &session,
            &[(TICKET_HEADER, "t-789")],
        )
        .await?;
        assert_eq!(seen["ticket"], "t-789", "{seen}");
        assert_eq!(seen["role"], format!("{:?}", Role::viewer()), "{seen}");
        assert!(
            seen["editor_write_refusal"].is_string(),
            "a viewer forwarder must be refused an Editor write: {seen}"
        );

        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup().await;
    result.expect("viewer forwarder test failed");
}

// --- 2. health probes ------------------------------------------------------

/// With `PROXIMA_HEALTH_ENDPOINTS=true` both probes answer 200 with no bearer
/// and under a Host the guard refuses everywhere else; `/mcp` still demands a
/// bearer. Without the variable there is no probe.
#[tokio::test]
async fn health_probes_answer_anonymously_across_hosts_only_when_enabled() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let client = reqwest::Client::new();

        let (running, base) = stack.boot(&[("PROXIMA_HEALTH_ENDPOINTS", "true")]).await?;
        for path in ["/healthz", "/readyz"] {
            let plain = client.get(format!("{base}{path}")).send().await?;
            assert_eq!(plain.status(), reqwest::StatusCode::OK, "{path}");
            let foreign = client
                .get(format!("{base}{path}"))
                .header("Host", FOREIGN_HOST)
                .send()
                .await?;
            assert_eq!(
                foreign.status(),
                reqwest::StatusCode::OK,
                "{path} under a non-allowlisted Host"
            );
        }
        // The same Host is refused off the probes: the guard is live.
        let guarded = client
            .get(format!("{base}/mcp"))
            .header("Host", FOREIGN_HOST)
            .send()
            .await?;
        assert_eq!(guarded.status(), reqwest::StatusCode::FORBIDDEN);
        let anonymous = client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .send()
            .await?;
        assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
        running.shutdown().await;

        let (running, base) = stack.boot(&[]).await?;
        let off = client.get(format!("{base}/healthz")).send().await?;
        assert_ne!(
            off.status(),
            reqwest::StatusCode::OK,
            "no probe without PROXIMA_HEALTH_ENDPOINTS"
        );
        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup().await;
    result.expect("health probe test failed");
}

// --- 3. the body limit -----------------------------------------------------

#[tokio::test]
async fn a_body_over_the_configured_limit_is_refused_413() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack
            .boot(&[("PROXIMA_MAX_REQUEST_BODY_BYTES", "1024")])
            .await?;
        let client = reqwest::Client::new();
        let bearer = stack.idp.bearer(FORWARDER_SUB)?;

        let over = mcp_post(&client, &base, &bearer)
            .body(vec![b' '; 2048])
            .send()
            .await?;
        assert_eq!(over.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

        // Under the cap the request passes the guard and meets auth.
        let under = client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .body(vec![b' '; 512])
            .send()
            .await?;
        assert_eq!(under.status(), reqwest::StatusCode::UNAUTHORIZED);

        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup().await;
    result.expect("body limit test failed");
}

// --- 4. SIGTERM drains -----------------------------------------------------

/// `until_shutdown_signal` returns `Ok(())` on SIGTERM — what a stock `main`
/// maps to exit 0 — after the request in flight completes, and the listener
/// accepts nothing afterwards.
///
/// This test's own SIGTERM listener is installed before anything else, so
/// the signal replaces the default (terminate) disposition for the whole
/// test process before the first one is sent.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_drains_the_request_in_flight_and_exits_ok() {
    use tokio::signal::unix::{SignalKind, signal};
    use tokio::time::{Instant, sleep, timeout};

    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM listener");
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack.boot(&[]).await?;
        let addr = running.mcp_addr.ok_or("MCP listener did not bind")?;
        let bearer = stack.idp.bearer(FORWARDER_SUB)?;
        let owner = group_key(Uuid::now_v7());

        let in_flight = tokio::spawn({
            let url = format!("{base}{SLOW_PATH}");
            async move {
                let response = reqwest::Client::new()
                    .get(url)
                    .header("Authorization", bearer)
                    .header("X-Proxima-Owner", owner)
                    .send()
                    .await?;
                let status = response.status();
                Ok::<_, reqwest::Error>((status, response.text().await?))
            }
        });
        let started = Instant::now() + Duration::from_secs(10);
        while !SLOW_STARTED.load(Ordering::SeqCst) {
            assert!(
                !in_flight.is_finished(),
                "the slow request ended before its handler ran"
            );
            assert!(Instant::now() < started, "the slow handler never started");
            sleep(Duration::from_millis(5)).await;
        }

        let drain = tokio::spawn(running.until_shutdown_signal());
        // The handler inside `until_shutdown_signal` registers on the task's
        // first poll, which may follow the first signal: repeat until it
        // lands.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut in_flight_when_signalled = None;
        while !drain.is_finished() {
            assert!(
                Instant::now() < deadline,
                "SIGTERM never drained the runtime"
            );
            in_flight_when_signalled.get_or_insert(!in_flight.is_finished());
            let kill = std::process::Command::new("kill")
                .args(["-TERM", &std::process::id().to_string()])
                .status()?;
            assert!(kill.success(), "kill -TERM failed: {kill}");
            sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            in_flight_when_signalled,
            Some(true),
            "the first SIGTERM must land while the request is in flight"
        );
        timeout(Duration::from_secs(1), terminate.recv())
            .await
            .map_err(|_| "the process never received SIGTERM")?;

        let outcome = drain.await?;
        assert!(
            outcome.is_ok(),
            "SIGTERM is a clean shutdown (exit 0): {outcome:?}"
        );
        let (status, body) = in_flight.await??;
        assert_eq!(status, reqwest::StatusCode::OK, "in-flight request: {body}");
        assert_eq!(body, "drained");
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "the listener still accepts after the drain"
        );
        Ok(())
    }
    .await;
    stack.cleanup().await;
    result.expect("SIGTERM drain test failed");
}
