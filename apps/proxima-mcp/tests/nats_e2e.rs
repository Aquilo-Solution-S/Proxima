//! The shipped host's Fact-outbox lane (issue #305, docs/18).
//!
//! Not a delivery test — `crates/outbox-nats/tests/jetstream_e2e.rs` owns
//! the delivery contract. This proves the three things only the BINARY can
//! prove: that the `PROXIMA_NATS_*` block reaches the publisher through the
//! facade's own configuration path, that the publisher provisions its
//! stream and runs beside the embedding worker, and that cancelling the
//! runtime's token joins it.
//!
//! Requires `DATABASE_URL` and `PROXIMA_TEST_NATS_URL`; skipped locally
//! without them, and a hard failure under `CI=true`.

mod common;

use std::time::Duration;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use proxima::host::{DeliveryProfile, PublicationConfig, PublicationSource};
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
    if let Ok(client) = async_nats::connect(url).await {
        let context = async_nats::jetstream::new(client);
        let _ = context.delete_stream(stream).await;
    }
}

async fn stream_exists(url: &str, stream: &str) -> bool {
    let Ok(client) = async_nats::connect(url).await else {
        return false;
    };
    async_nats::jetstream::new(client)
        .get_stream(stream)
        .await
        .is_ok()
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
    let token = Uuid::now_v7().simple().to_string();
    let stream = format!("T_{token}");
    let subject_prefix = format!("t_{token}");

    // Everything the operator would put in the environment, injected. The
    // facade parses this block ONCE; nothing downstream re-reads process
    // environment.
    let pairs = [
        (
            "PROXIMA_PUBLICATION_SOURCE",
            "urn:proxima:mcp-nats-e2e".to_owned(),
        ),
        ("PROXIMA_OUTBOX_MAX_PENDING", "1000".to_owned()),
        ("PROXIMA_NATS_URL", nats_url.clone()),
        ("PROXIMA_NATS_STREAM", stream.clone()),
        ("PROXIMA_NATS_SUBJECT_PREFIX", subject_prefix.clone()),
        ("PROXIMA_NATS_PROFILE", "local-file".to_owned()),
    ];
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

    // The publisher provisions its own stream in the configured profile.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut provisioned = false;
    while tokio::time::Instant::now() < deadline {
        if stream_exists(&nats_url, &stream).await {
            provisioned = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        provisioned,
        "the host's publisher must create the stream it was configured with"
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
    // The profile constant is nameable from the host facade, which is what
    // makes an out-of-tree host able to configure one programmatically.
    assert_eq!(DeliveryProfile::LocalFile.to_string(), "local-file");
    running.shutdown().await;
    Ok(())
}
