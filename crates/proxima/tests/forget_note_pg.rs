//! Engine forget/hydrate of a flavor's typed sidecar row.
//!
//! The only test of a forget that dumps a FLAVOR sidecar row to the cold
//! store and a hydrate that restores it lived in the code flavor
//! (`forget_hydrate_restores_code_chunk_sidecar`). This is its generic half,
//! on the example flavor's note sidecar; the code-specific half (call
//! details, the code projection's tsvector) went with that flavor.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima_core::{AuthPath, AuthzContext, MemoryHydrationStatus, OwnerId, UserId};
use proxima_example::NoteV1;
use proxima_example::testkit::remember_note;
use uuid::Uuid;

const NOTE_ROW_SQL: &str = "\
SELECT notebook_id, key, body, written_at
  FROM proxima_example.note_v1
 WHERE t = $1";

type NoteRow = (Uuid, String, String, time::OffsetDateTime);

#[tokio::test]
async fn forget_hydrate_restores_the_note_sidecar() {
    let harness = ExampleEngine::new("proxima_forget_note", |_| {}).await;
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let authz = proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
        user,
        AuthPath::HostBearer,
    ));
    let notebook_id = harness.notebook(&owner).await;
    let written_at = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("instant");
    let note = NoteV1 {
        notebook_id,
        key: "forget-me".to_owned(),
        body: "a note the cold store must keep".to_owned(),
        written_at,
    };
    let memory_id = remember_note(&harness.engine, &authz, owner, &note)
        .await
        .expect("the note is remembered");
    let before: NoteRow = sqlx::query_as(NOTE_ROW_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("the hot sidecar row");

    harness
        .engine
        .forget_memory(&authz, owner, memory_id)
        .await
        .expect("the note is forgotten");
    let hot: Option<NoteRow> = sqlx::query_as(NOTE_ROW_SQL)
        .bind(memory_id.into_inner())
        .fetch_optional(&harness.pool)
        .await
        .expect("sidecar probe");
    assert!(hot.is_none(), "forget deletes the flavor sidecar row");

    let hydrated = harness
        .engine
        .hydrate_memory(&authz, owner, memory_id)
        .await
        .expect("the note hydrates");
    assert_eq!(hydrated.status, MemoryHydrationStatus::Hydrated);
    let after: NoteRow = sqlx::query_as(NOTE_ROW_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("hydrate restores the sidecar row");
    assert_eq!(after, before, "every sidecar column survives the cold trip");
    assert_eq!(after.3, written_at);
}
