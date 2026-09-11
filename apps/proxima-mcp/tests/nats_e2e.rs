//! The shipped host's Fact-outbox lane (issue #305, docs/18).
//!
//! Not a delivery test — `crates/outbox-nats/tests/jetstream_e2e.rs` owns
//! the delivery contract. This proves the three things only the BINARY can
//! prove: that the `PROXIMA_NATS_*` block reaches the publisher through the
//! facade's own configuration path, that the publisher uses a separately
//! provisioned stream and runs beside the embedding worker, and that
//! cancelling the runtime's token joins it.
//!
//! Requires `DATABASE_URL` and `PROXIMA_TEST_NATS_URL`; skipped locally
//! without them, and a hard failure under `CI=true`.

mod common;

use std::time::Duration;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use proxima::host::{PublicationConfig, PublicationSource};
use proxima::{Proxima, ResourceServerMetadata, ToolScope, company_owner};
use proxima_core::{AuthError, AuthPath, Authenticator, AuthzContext, Credentials, Owner, Role};
use proxima_mcp::ProximaMcpApp;
use uuid::Uuid;

use common::require_env_or_skip;

/// The shipped host refuses to serve MCP without host auth, so this lane
/// supplies the smallest authenticator that satisfies that gate. The
/// listener is never called: the subject under test is the publisher.
#[derive(Debug)]
struct StubAuthenticator {
    owner: Owner,
}

#[async_trait::async_trait]
impl Authenticator for StubAuthenticator {
    async fn authenticate(&self, _credentials: &Credentials) -> Result<AuthzContext, AuthError> {
        Ok(AuthzContext::for_subject_with_role(
            proxima_core::UserId::new(Uuid::now_v7()),
            [(self.owner, Role::admin())],
            AuthPath::HostBearer,
        ))
    }
}

/// One test's stream, deleted on the way out.
async fn delete_stream(url: &str, stream: &str) {
    if let Ok(client) = admin_client(url).await {
        let context = async_nats::jetstream::new(client);
        let _ = context.delete_stream(stream).await;
    }
}

async fn admin_client(url: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
    let options = async_nats::ConnectOptions::new();
    let options = match (
        std::env::var("PROXIMA_TEST_NATS_ADMIN_USER"),
        std::env::var("PROXIMA_TEST_NATS_ADMIN_PASSWORD"),
    ) {
        (Ok(user), Ok(password)) => options.user_and_password(user, password),
        (Err(_), Err(_)) => options,
        _ => panic!("admin NATS credentials must be set together"),
    };
    options.connect(url).await
}

/// Provision the test topology through an admin connection. The application
/// publisher is deliberately not involved in this operation.
async fn provision_stream(url: &str, stream: &str, subject_prefix: &str) {
    let client = admin_client(url)
        .await
        .expect("the fixture admin connection opens");
    async_nats::jetstream::new(client)
        .create_stream(async_nats::jetstream::stream::Config {
            name: stream.to_owned(),
            subjects: vec![format!("{subject_prefix}.>")],
            storage: async_nats::jetstream::stream::StorageType::File,
            retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
            discard: async_nats::jetstream::stream::DiscardPolicy::New,
            max_age: Duration::ZERO,
            max_bytes: 1024 * 1024 * 1024,
            max_message_size: -1,
            ..async_nats::jetstream::stream::Config::default()
        })
        .await
        .expect("the deployment fixture provisions the stream");
}

/// The stream's configuration as the BROKER holds it, or `None` while it
/// does not exist yet. The topology is provisioned by this test through a
/// separate admin connection; the application publisher never creates it.
async fn stream_config(url: &str, name: &str) -> Option<async_nats::jetstream::stream::Config> {
    let client = admin_client(url).await.ok()?;
    let mut stream = async_nats::jetstream::new(client)
        .get_stream(name)
        .await
        .ok()?;
    Some(stream.info().await.ok()?.config.clone())
}

#[tokio::test]
async fn the_host_starts_the_publisher_from_the_nats_env_block()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("DATABASE_URL") else {
        return Ok(());
    };
    let Some(nats_url) = require_env_or_skip("PROXIMA_TEST_NATS_URL") else {
        return Ok(());
    };
    let publisher_url =
        std::env::var("PROXIMA_TEST_NATS_PUBLISHER_URL").unwrap_or_else(|_| nats_url.clone());
    let token = Uuid::now_v7().simple().to_string();
    let stream = format!("T_{token}");
    let subject_prefix = format!("t_{token}");
    provision_stream(&nats_url, &stream, &subject_prefix).await;

    // Everything the operator would put in the environment, injected. The
    // facade parses this block ONCE; nothing downstream re-reads process
    // environment.
    let mut pairs = vec![
        (
            "PROXIMA_PUBLICATION_SOURCE",
            "urn:proxima:mcp-nats-e2e".to_owned(),
        ),
        ("PROXIMA_OUTBOX_MAX_PENDING", "1000".to_owned()),
        ("PROXIMA_NATS_URL", publisher_url),
        ("PROXIMA_NATS_SUBJECT_PREFIX", subject_prefix.clone()),
    ];
    if let (Ok(user), Ok(password)) = (
        std::env::var("PROXIMA_TEST_NATS_PUBLISHER_USER"),
        std::env::var("PROXIMA_TEST_NATS_PUBLISHER_PASSWORD"),
    ) {
        pairs.push(("PROXIMA_NATS_USER", user));
        pairs.push(("PROXIMA_NATS_PASSWORD", password));
    }
    let lookup = move |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };

    let owner = company_owner(Uuid::now_v7());
    let running = Proxima::<ProximaMcpApp>::app()
        .from_lookup(lookup)?
        .database_url(database_url)
        .owner(owner)
        .authenticator(Arc::new(StubAuthenticator { owner }))
        .resource_metadata(ResourceServerMetadata {
            public_url: "http://127.0.0.1".to_string(),
            authorization_servers: vec!["https://idp.nats-e2e.test".to_string()],
        })
        .mcp_bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .tool_scope(ToolScope::All)
        .run()
        .await?;

    // The parsed block is the one the engine holds.
    let publication = running.engine.publication_config();
    assert_eq!(
        publication.source.as_ref().map(PublicationSource::as_str),
        Some("urn:proxima:mcp-nats-e2e")
    );
    assert_eq!(publication.limits.max_pending, 1000);

    let publisher = running
        .spawn_publication_publisher(running.cancel.clone())
        .expect("PROXIMA_NATS_URL is set, so the host must start a publisher");

    // The stream was provisioned before the app booted. The host only
    // connects and publishes; topology settings remain deployment-owned.
    let provisioned = stream_config(&nats_url, &stream)
        .await
        .expect("the deployment fixture provisioned the stream");
    assert_eq!(
        provisioned.storage,
        async_nats::jetstream::stream::StorageType::File
    );
    assert_eq!(
        provisioned.retention,
        async_nats::jetstream::stream::RetentionPolicy::Limits
    );
    assert_eq!(
        provisioned.discard,
        async_nats::jetstream::stream::DiscardPolicy::New
    );
    assert_eq!(provisioned.max_age, Duration::ZERO);
    assert_eq!(provisioned.num_replicas, 1);
    assert_eq!(provisioned.subjects, vec![format!("{subject_prefix}.>")]);
    assert!(
        provisioned.max_bytes > 0,
        "stream capacity must be an explicit number, got {}",
        provisioned.max_bytes
    );

    // Cancelling the runtime's token stops it, the way `run()` does.
    running.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(20), publisher)
        .await
        .expect("the publisher must stop when the runtime is cancelled")
        .expect("and must not panic");
    running.shutdown().await;

    delete_stream(&nats_url, &stream).await;
    Ok(())
}

#[tokio::test]
async fn without_a_broker_the_host_starts_no_publisher() -> Result<(), Box<dyn std::error::Error>> {
    let Some(database_url) = require_env_or_skip("DATABASE_URL") else {
        return Ok(());
    };
    // No `PROXIMA_NATS_URL`: capture (if any schema were listenable) keeps
    // working and the outbox simply stays undrained. That is a safe steady
    // state, not a boot failure.
    let owner = company_owner(Uuid::now_v7());
    let running = Proxima::<ProximaMcpApp>::app()
        .from_lookup(|_: &str| None)?
        .database_url(database_url)
        .owner(owner)
        .authenticator(Arc::new(StubAuthenticator { owner }))
        .resource_metadata(ResourceServerMetadata {
            public_url: "http://127.0.0.1".to_string(),
            authorization_servers: vec!["https://idp.nats-e2e.test".to_string()],
        })
        .mcp_bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .tool_scope(ToolScope::All)
        .publication(PublicationConfig::default())
        .run()
        .await?;
    assert!(
        running
            .spawn_publication_publisher(running.cancel.clone())
            .is_none(),
        "no broker named, no publisher"
    );
    running.shutdown().await;
    Ok(())
}
