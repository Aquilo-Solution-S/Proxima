//! A write between result selection and event-cursor sampling must remain
//! visible to the following poll. Real relation locks expose that interval.

use std::sync::Arc;
use std::time::Duration;

use proxima_core::verbs::change_history::ChangeHistoryRequest;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    AgentNoteV1, AuthPath, AuthzContext, Engine, EntityKind, FactIngestOutcome, FactPayload,
    FactWriteCommand, FlavorRegistry, OwnerRef, Relation, SidecarPayload, Speaker, UserId,
    UtteranceV1,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[path = "change_event_watermark_pg/lifecycle.rs"]
mod lifecycle;

struct Fixture {
    db_name: String,
    pg: PgStorage,
    engine: Arc<Engine>,
    owner: OwnerRef,
    authz: AuthzContext,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        create_core_db(&db_name).await?;
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        pg.run_before_owner_rls_migrations().await?;
        let engine = Arc::new(
            Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
                .with_storage_ports(Arc::new(pg.clone()).storage_ports()),
        );
        let user = UserId::new(Uuid::now_v7());
        Ok(Self {
            db_name,
            pg,
            engine,
            owner: OwnerRef::Personal(user),
            authz: AuthzContext::for_subject(user, AuthPath::HostBearer),
        })
    }

    async fn close(self) {
        self.pg.pool_for_tests().close().await;
        drop_db(&self.db_name)
            .await
            .expect("drop isolated database");
    }

    async fn ingest<P: FactPayload>(&self, payload: P) -> TestResult<FactIngestOutcome> {
        self.ingest_as(&self.authz, payload).await
    }

    async fn ingest_as<P: FactPayload>(
        &self,
        authz: &AuthzContext,
        payload: P,
    ) -> TestResult<FactIngestOutcome> {
        let mut draft = FactWriteCommand::from_payload(
            "test/event-watermark",
            &payload,
            time::OffsetDateTime::now_utc(),
        );
        draft.lexical_language =
            Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
        let sidecars = [SidecarPayload::fact(payload)];
        let authorized = self
            .engine
            .authorize_fact_ingest(authz, Relation::Ingest, draft, &sidecars)
            .await?;
        Ok(self
            .engine
            .ingest_fact_with_typed_sidecar(&authorized)
            .await?)
    }

    async fn initial_note(&self) -> TestResult<FactIngestOutcome> {
        self.ingest(AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: "before result selection".into(),
            body: "the sidecar read supplies a deterministic barrier".into(),
            tags: Vec::new(),
            idempotency_key: None,
        })
        .await
    }

    async fn later_utterance(&self) -> TestResult<FactIngestOutcome> {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.ingest(UtteranceV1 {
                speaker: Speaker::User,
                conversation_id: "event-watermark".into(),
                text: "committed while the selected result is being hydrated".into(),
            }),
        )
        .await?
    }
}

#[tokio::test]
async fn watermarks_preserve_authorized_owners_and_page_boundaries() {
    let fixture = Fixture::new().await.expect("isolated PG fixture");
    let result: TestResult<()> = async {
        for entity_kind in [None, Some(EntityKind::Fact), Some(EntityKind::Goal)] {
            let empty = fixture
                .engine
                .query(
                    &fixture.authz,
                    &QueryRequest {
                        entity_kind,
                        ..QueryRequest::readable()
                    },
                )
                .await?;
            assert_eq!(empty.seq_high_water, None);
            assert!(empty.memories.is_empty() && empty.goals.is_empty());
        }
        let mut history_request = ChangeHistoryRequest {
            owner: fixture.owner,
            before: None,
            limit: 1,
        };
        let empty_history = fixture
            .engine
            .change_history(&fixture.authz, &history_request)
            .await?;
        assert!(empty_history.events.is_empty());
        assert_eq!(empty_history.seq_high_water, None);

        let first = fixture.initial_note().await?;
        let later = fixture.later_utterance().await?;
        let foreign_user = UserId::new(Uuid::now_v7());
        let foreign_authz = AuthzContext::for_subject(foreign_user, AuthPath::HostBearer);
        let foreign = fixture
            .ingest_as(
                &foreign_authz,
                UtteranceV1 {
                    speaker: Speaker::User,
                    conversation_id: "foreign-owner".into(),
                    text: "a larger event cursor outside the reader's authorization".into(),
                },
            )
            .await?;
        assert!(foreign.change_event_seq > later.change_event_seq);
        let foreign_owner = OwnerRef::Personal(foreign_user);
        // The reader's authorization, not the request, bounds rows and watermark.
        let mut request = QueryRequest {
            entity_kind: Some(EntityKind::Fact),
            limit: 1,
            ..QueryRequest::readable()
        };
        let newest = fixture.engine.query(&fixture.authz, &request).await?;
        assert_eq!(newest.memories.len(), 1);
        assert_eq!(newest.memories[0].id, later.memory_id);
        assert_eq!(newest.seq_high_water, Some(later.change_event_seq));
        request.page.after = Some(newest.next_cursor.expect("older authorized Fact remains"));
        let oldest = fixture.engine.query(&fixture.authz, &request).await?;
        assert_eq!(oldest.memories.len(), 1);
        assert_eq!(oldest.memories[0].id, first.memory_id);
        assert!(oldest.next_cursor.is_none());
        assert_eq!(oldest.seq_high_water, Some(later.change_event_seq));

        for entity_kind in [None, Some(EntityKind::Goal)] {
            let response = fixture
                .engine
                .query(
                    &fixture.authz,
                    &QueryRequest {
                        entity_kind,
                        ..QueryRequest::readable()
                    },
                )
                .await?;
            assert_eq!(response.seq_high_water, Some(later.change_event_seq));
            assert!(
                response
                    .memories
                    .iter()
                    .all(|row| row.owner == fixture.owner)
            );
            assert!(response.goals.is_empty());
        }
        history_request.owner = foreign_owner;
        let newest_history = fixture
            .engine
            .change_history(&fixture.authz, &history_request)
            .await?;
        assert_eq!(newest_history.events.len(), 1);
        assert_eq!(newest_history.events[0].seq, later.change_event_seq);
        history_request.before = Some(later.change_event_seq);
        let oldest_history = fixture
            .engine
            .change_history(&fixture.authz, &history_request)
            .await?;
        assert_eq!(oldest_history.events.len(), 1);
        assert_eq!(oldest_history.events[0].seq, first.change_event_seq);
        assert_eq!(oldest_history.events[0].owner, fixture.owner);
        assert_eq!(oldest_history.seq_high_water, Some(later.change_event_seq));
        Ok(())
    }
    .await;
    fixture.close().await;
    result.expect("empty, authorized-owner, and page-boundary controls");
}
