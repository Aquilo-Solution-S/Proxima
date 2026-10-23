//! The stock flavor host, end to end through the facade's environment path.
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
//! flavor host names it too.
#![cfg(feature = "auth-oidc")]

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

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
    FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator, RequestHeaders, Tool,
    ToolCtx, ToolEffect, ToolError,
};
use proxima::{
    AccessKind, AppContext, AppInfo, Authz, FlavorApp, Proxima, Role, RunningProxima,
    RuntimeBuilder,
};
use proxima_pg_testkit::SplitRoleDb;
use serde_json::{Value, json};
use split_core_db::clone_split_core_db;
use tokio::task::JoinHandle;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const KID: &str = "flavor-host-key";
const AUDIENCE: &str = "proxima-flavor-host";
const PUBLIC_URL: &str = "https://proxima.flavor.test";
const FOREIGN_HOST: &str = "foreign.flavor.test";
const FORWARDER_SUB: &str = "flavor-forwarder";
const MEMBER_SUB: &str = "flavor-member";
const TICKET_HEADER: &str = "x-flavor-ticket";
const SECRET_HEADER: &str = "x-flavor-secret";
const PROBE_TOOL: &str = "flavorhost_probe";
const SLOW_PATH: &str = "/flavor/slow";
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
    /// One application value from this request's verified token payload.
    application: Option<Value>,
    /// `x-flavor-ticket`, when the allowlist copied it.
    ticket: Option<String>,
    /// Every header name the tool can see.
    header_names: Vec<String>,
    /// The selected owner's external key.
    owner: String,
    /// The authenticated forwarding principal, distinct from the data owner.
    principal: String,
    readable_owners: Vec<String>,
    may_manage: bool,
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
    const TITLE: &'static str = "Test tool";
    const NAME: &'static str = PROBE_TOOL;
    const DESCRIPTION: &'static str =
        "Reports the request headers, owner, and role a forwarded call carries.";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);

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
                application: ctx
                    .authz()
                    .verified_claims()
                    .and_then(|claims| claims.get("application"))
                    .cloned(),
                ticket: headers
                    .as_ref()
                    .and_then(|headers| headers.get(TICKET_HEADER))
                    .map(ToOwned::to_owned),
                header_names: headers
                    .map(|headers| headers.iter().map(|(name, _)| name.to_owned()).collect())
                    .unwrap_or_default(),
                owner: owner.to_string(),
                principal: ctx.authz().principal().to_string(),
                readable_owners: ctx
                    .authz()
                    .readable_owners(AccessKind::Fact)
                    .into_iter()
                    .map(|owner| owner.to_string())
                    .collect(),
                may_manage: ctx.authz().may_manage(&owner),
                role: ctx
                    .authz()
                    .role_for_owner(&owner)
                    .map(|role| format!("{role:?}")),
                editor_write_refusal,
            })
        })
    }
}

struct FlavorHost;

impl FlavorBundle for FlavorHost {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.try_add_mcp_tool::<Probe>("flavorhost")
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for FlavorHost {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "stock-flavor-host-test",
            title: "Stock Flavor Host Test",
            version: "1",
        }
    }

    /// The tool scope has no environment variable on the facade path.
    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder.served_surface(proxima_core::ServedSurface::All)
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
        self.bearer_with_writer(sub, None)
    }

    fn bearer_for_owner(&self, sub: &str, owner: Uuid) -> TestResult<String> {
        self.bearer_for_writer(
            sub,
            owner,
            proxima_core::WriterAttribution {
                by: owner,
                via: None,
            },
        )
    }

    fn bearer_for_writer(
        &self,
        sub: &str,
        owner: Uuid,
        writer: proxima_core::WriterAttribution,
    ) -> TestResult<String> {
        self.bearer_with_writer(sub, Some((owner, writer)))
    }

    fn bearer_with_writer(
        &self,
        sub: &str,
        owner: Option<(Uuid, proxima_core::WriterAttribution)>,
    ) -> TestResult<String> {
        self.bearer_with_application(sub, owner, None)
    }

    fn bearer_with_application(
        &self,
        sub: &str,
        owner: Option<(Uuid, proxima_core::WriterAttribution)>,
        application: Option<Value>,
    ) -> TestResult<String> {
        let exp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600;
        let header = json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
        let mut claims = json!({"iss": self.issuer, "aud": AUDIENCE, "sub": sub, "exp": exp});
        if let Some(application) = application {
            claims["application"] = application;
        }
        if let Some((owner, writer)) = owner {
            claims["owner"] = json!(owner);
            claims["by"] = json!(writer.by);
            claims["via"] = json!(writer.via);
        }
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
    /// Dropped with the stack: the clone is the stack's to tear down.
    database: SplitRoleDb,
    idp: Idp,
    forwarder: Uuid,
    member: Uuid,
}

impl Stack {
    async fn new() -> Self {
        let database = clone_split_core_db("proxima_flavor_host")
            .await
            .expect("PG required for tests");
        let idp = Idp::start().await.expect("loopback identity provider");
        Self {
            database,
            idp,
            forwarder: Uuid::now_v7(),
            member: Uuid::now_v7(),
        }
    }

    /// The deployment environment, with `overrides` replacing or adding
    /// entries.
    fn env(&self, overrides: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = [
            ("DATABASE_URL", self.database.runtime_url().to_owned()),
            ("PROXIMA_STORE_NUMBER", "0".to_owned()),
            (
                "PROXIMA_PLATFORM_DATABASE_URL",
                self.database.platform_url().to_owned(),
            ),
            ("PROXIMA_MCP_BIND", "127.0.0.1:0".to_owned()),
            ("PROXIMA_OIDC_ISSUER", self.idp.issuer.clone()),
            ("PROXIMA_OIDC_JWKS_URI", format!("{}/jwks", self.idp.issuer)),
            ("PROXIMA_OIDC_AUDIENCE", AUDIENCE.to_owned()),
            ("PROXIMA_PUBLIC_URL", PUBLIC_URL.to_owned()),
            (
                "PROXIMA_OIDC_SUBJECT_MAP_JSON",
                serde_json::json!([
                    {"iss": self.idp.issuer.clone(), "sub": FORWARDER_SUB, "user_id": self.forwarder},
                    {"iss": self.idp.issuer.clone(), "sub": MEMBER_SUB, "user_id": self.member},
                ])
                .to_string(),
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
        let running = Proxima::<(FlavorHost,)>::app()
            .from_lookup(move |key| env.get(key).cloned())?
            .run()
            .await?;
        let addr = running.mcp_addr().ok_or("MCP listener did not bind")?;
        Ok((running, format!("http://{addr}")))
    }

    /// Register an ordinary group owner.
    async fn new_group(&self) -> TestResult<Uuid> {
        let group = Uuid::now_v7();
        let pool = sqlx::PgPool::connect(self.database.platform_url()).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group'::proxima_core.owner_kind)",
        )
        .bind(group)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        pool.close().await;
        Ok(group)
    }

    async fn new_person(&self) -> TestResult<Uuid> {
        let person = Uuid::now_v7();
        let pool = sqlx::PgPool::connect(self.database.platform_url()).await?;
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(person)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        pool.close().await;
        Ok(person)
    }

    fn cleanup(self) {
        self.idp.server.abort();
        // The clone drops with `self`.
    }
}

fn group_key(group: Uuid) -> String {
    group.to_string()
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
    owner_key: Option<&str>,
) -> TestResult<reqwest::Response> {
    let mut request = mcp_post(client, base, bearer);
    if let Some(owner_key) = owner_key {
        request = request.header("X-Proxima-Owner", owner_key);
    }
    Ok(request
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "flavor-host-test", "version": "0"}
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
    owner_key: Option<&str>,
) -> TestResult<String> {
    let response = initialize(client, base, bearer, owner_key).await?;
    assert!(
        response.status().is_success(),
        "initialize as {owner_key:?}: {}",
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
    let body = call_tool(
        client,
        base,
        bearer,
        session,
        &json!({"name": PROBE_TOOL, "arguments": {}}),
        headers,
    )
    .await?;
    assert_ne!(
        body["result"]["isError"],
        json!(true),
        "the probe itself must not fail: {body}"
    );
    tool_output(&body)
}

fn tool_output(body: &Value) -> TestResult<Value> {
    if let Some(structured) = body["result"].get("structuredContent") {
        return Ok(structured.clone());
    }
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("probe returned no output: {body}"))?;
    Ok(serde_json::from_str(text)?)
}

async fn call_tool(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    params: &Value,
    headers: &[(&str, &str)],
) -> TestResult<Value> {
    mcp_rpc(client, base, bearer, session, "tools/call", params, headers).await
}

async fn mcp_rpc(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    method: &str,
    params: &Value,
    headers: &[(&str, &str)],
) -> TestResult<Value> {
    let mut request = mcp_post(client, base, bearer).header("Mcp-Session-Id", session);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 2, "method": method, "params": params,
        }))
        .send()
        .await?;
    assert!(
        response.status().is_success(),
        "tools/call: {}",
        response.status()
    );
    sse_json(response).await
}

async fn read_memory(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    memory: &str,
) -> TestResult<Value> {
    let uri = format!("proxima://memories?ids={memory}");
    let body = mcp_rpc(
        client,
        base,
        bearer,
        session,
        "resources/read",
        &json!({"uri":uri}),
        &[],
    )
    .await?;
    let text = body["result"]["contents"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("memory resource returned no content: {body}"))?;
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
        let group = stack.new_group().await?;
        let forwarder = stack.idp.bearer_for_owner(FORWARDER_SUB, group)?;

        let session = open_session(&client, &base, &forwarder, Some(&group_key(group))).await?;
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
        let member = stack.idp.bearer_for_owner(MEMBER_SUB, group)?;
        open_session(&client, &base, &member, Some(&stack.member.to_string())).await?;
        let refused = initialize(&client, &base, &member, Some(&group_key(group))).await?;
        assert_eq!(
            refused.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "a non-forwarder must not select a Group it is not a member of"
        );

        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup();
    result.expect("forwarded call test failed");
}

/// Each call reads its own verified application payload through the facade,
/// including successive tokens on one session after forwarded-owner narrowing.
#[tokio::test]
async fn verified_application_claims_reach_each_flavor_call_on_the_same_session() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack.boot(&[]).await?;
        let client = reqwest::Client::new();
        let group = stack.new_group().await?;
        let writer = proxima_core::WriterAttribution {
            by: Uuid::now_v7(),
            via: None,
        };
        let first = json!({"request": "first", "nested": {"enabled": true}});
        let second = json!({"request": "second", "nested": {"enabled": false}});
        let first_token = stack.idp.bearer_with_application(
            FORWARDER_SUB,
            Some((group, writer)),
            Some(first.clone()),
        )?;
        let second_token = stack.idp.bearer_with_application(
            FORWARDER_SUB,
            Some((group, writer)),
            Some(second.clone()),
        )?;
        let session = open_session(&client, &base, &first_token, Some(&group_key(group))).await?;
        for (token, expected) in [
            (&first_token, &first),
            (&second_token, &second),
            (&first_token, &first),
        ] {
            let seen = call_probe(
                &client,
                &base,
                token,
                &session,
                &[("x-application", "not-a-verified-claim")],
            )
            .await?;
            assert_eq!(&seen["application"], expected, "{seen}");
            assert_eq!(seen["owner"], group_key(group), "{seen}");
            assert_eq!(seen["principal"], stack.forwarder.to_string(), "{seen}");
            assert_eq!(seen["readable_owners"], json!([group_key(group)]), "{seen}");
            assert_eq!(seen["header_names"], json!([]), "{seen}");
        }
        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup();
    result.expect("verified claims must remain local to each served request");
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
        let group = stack.new_group().await?;
        let forwarder = stack.idp.bearer_for_owner(FORWARDER_SUB, group)?;

        let session = open_session(&client, &base, &forwarder, Some(&group_key(group))).await?;
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
    stack.cleanup();
    result.expect("viewer forwarder test failed");
}

async fn remember_and_read_writer(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    session: &str,
    owner: Uuid,
    body: &str,
    writer: proxima_core::WriterAttribution,
) -> TestResult<String> {
    let arguments = json!({
        "title":"Forwarded personal memory", "body":body,
        "idempotency_key":format!("personal-{owner}")
    });
    let mut forged = arguments.clone();
    forged["by"] = json!(Uuid::nil());
    forged["via"] = json!(Uuid::nil());
    let refused = call_tool(
        client,
        base,
        token,
        session,
        &json!({"name":"core_remember","arguments":forged}),
        &[],
    )
    .await?;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let remembered = call_tool(
        client,
        base,
        token,
        session,
        &json!({"name":"core_remember","arguments":arguments}),
        &[
            ("x-proxima-by", "00000000-0000-0000-0000-000000000000"),
            ("x-proxima-via", "00000000-0000-0000-0000-000000000000"),
        ],
    )
    .await?;
    assert_ne!(remembered["result"]["isError"], true, "{remembered}");
    let output = tool_output(&remembered)?;
    assert_eq!(
        output["idempotent_replay"], false,
        "the refused call wrote nothing"
    );
    let memory = output["handle"]
        .as_str()
        .ok_or("missing remembered handle")?
        .to_owned();
    // Handles stay opaque: the existing read tool owns their parsing.
    let read = read_memory(client, base, token, session, &memory).await?;
    assert_eq!(read["missing"], json!([]), "{read}");
    assert_eq!(read["memories"].as_array().map(Vec::len), Some(1), "{read}");
    assert!(read.to_string().contains(body), "{read}");
    assert_eq!(read["memories"][0]["by"], json!(writer.by), "{read}");
    assert_eq!(
        read["memories"][0].get("via"),
        Some(&json!(writer.via)),
        "{read}"
    );
    let found = call_tool(
        client,
        base,
        token,
        session,
        &json!({"name":"core_search_memories","arguments":{
            "query":"personal private memory", "mode":"lexical", "kind":"Fact"
        }}),
        &[],
    )
    .await?;
    let found = tool_output(&found)?;
    assert_eq!(
        found["memories"].as_array().map(Vec::len),
        Some(1),
        "{found}"
    );
    assert_eq!(found["memories"][0]["by"], json!(writer.by), "{found}");
    assert_eq!(
        found["memories"][0].get("via"),
        Some(&json!(writer.via)),
        "{found}"
    );
    Ok(memory)
}

/// Real stock authentication, target-owner registry, scoped writes and RLS reads.
#[tokio::test]
async fn forwarded_owner_claim_reaches_personal_memory_and_isolates_every_request() {
    let stack = Stack::new().await;
    let result: TestResult = async {
        let (running, base) = stack.boot(&[]).await?;
        let client = reqwest::Client::new();
        let first = stack.new_person().await?;
        let second = stack.new_person().await?;
        let mut memories = Vec::new();
        for (owner, body) in [
            (first, "first personal private memory"),
            (second, "second personal private memory"),
        ] {
            let writer = proxima_core::WriterAttribution {
                by: Uuid::now_v7(),
                via: (owner == first).then(Uuid::now_v7),
            };
            assert_ne!(writer.by, owner);
            assert_ne!(writer.by, stack.forwarder);
            let token = stack.idp.bearer_for_writer(FORWARDER_SUB, owner, writer)?;
            // No owner header: the verified claim is the mandatory selection.
            let session = open_session(&client, &base, &token, None).await?;
            let probe = call_probe(&client, &base, &token, &session, &[]).await?;
            assert_eq!(probe["owner"], owner.to_string());
            assert_eq!(probe["principal"], stack.forwarder.to_string());
            assert_eq!(probe["readable_owners"], json!([owner.to_string()]));
            assert_eq!(probe["role"], format!("{:?}", Role::editor()));
            assert_eq!(probe["may_manage"], false);
            let memory =
                remember_and_read_writer(&client, &base, &token, &session, owner, body, writer)
                    .await?;
            memories.push((token, session, memory, body));
        }
        let missing = stack.idp.bearer(FORWARDER_SUB)?;
        let refused = initialize(&client, &base, &missing, Some(&first.to_string())).await?;
        assert_eq!(
            refused.status(),
            StatusCode::UNAUTHORIZED,
            "a header cannot replace a claim"
        );
        let first_token = stack.idp.bearer_for_owner(FORWARDER_SUB, first)?;
        let conflict = initialize(&client, &base, &first_token, Some(&second.to_string())).await?;
        assert_eq!(
            conflict.status(),
            StatusCode::FORBIDDEN,
            "a header cannot replace the claimed owner"
        );
        let unknown_token = stack.idp.bearer_for_owner(FORWARDER_SUB, Uuid::now_v7())?;
        let unknown = initialize(&client, &base, &unknown_token, None).await?;
        assert_eq!(
            unknown.status(),
            StatusCode::UNAUTHORIZED,
            "an unknown owner grants nothing"
        );

        for index in 0..memories.len() {
            let (token, session, _, _) = &memories[index];
            let (_, _, other_memory, other_body) = &memories[1 - index];
            let foreign = read_memory(&client, &base, token, session, other_memory).await?;
            assert_eq!(
                foreign["memories"],
                json!([]),
                "other owner's memory was readable: {foreign}"
            );
            assert_eq!(foreign["missing"], json!([other_memory]), "{foreign}");
            assert!(
                !foreign.to_string().contains(other_body),
                "another owner's body escaped"
            );
        }
        // A session cannot retain one owner's authority under another claim.
        let mismatched = mcp_post(&client, &base, &memories[1].0)
            .header("Mcp-Session-Id", &memories[0].1)
            .json(&json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
            .send()
            .await?;
        assert_eq!(mismatched.status(), StatusCode::NOT_FOUND);
        running.shutdown().await;
        Ok(())
    }
    .await;
    stack.cleanup();
    result.expect("personal forwarding must remain exactly claim-bound");
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
        // The same Host is refused off the probes, the router fallback
        // included: the guard is live.
        for path in ["/mcp", "/not-a-real-route"] {
            let guarded = client
                .get(format!("{base}{path}"))
                .header("Host", FOREIGN_HOST)
                .send()
                .await?;
            assert_eq!(guarded.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
        }
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
    stack.cleanup();
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
    stack.cleanup();
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
        let addr = running.mcp_addr().ok_or("MCP listener did not bind")?;
        let group = stack.new_group().await?;
        let bearer = stack.idp.bearer_for_owner(FORWARDER_SUB, group)?;
        let owner = group_key(group);

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
    stack.cleanup();
    result.expect("SIGTERM drain test failed");
}
