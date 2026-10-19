//! Engine forget/restore of a flavor's typed sidecar row (#488 S14).
//!
//! A forget leaves a FLAVOR sidecar row in place, hidden with its memory, and
//! a restore brings the memory back with that row unchanged. The example
//! flavor's note sidecar stands in for every flavor.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxima_core::{AuthPath, AuthzContext, OwnerId, UserId};
use proxima_example::NoteV1;
use proxima_example::testkit::remember_note;
use uuid::Uuid;

const NOTE_ROW_SQL: &str = "\
SELECT notebook_id, key, body, written_at
  FROM proxima_example.note_v1
 WHERE t = $1";

type NoteRow = (Uuid, String, String, time::OffsetDateTime);

#[tokio::test]
async fn forget_restore_keeps_the_note_sidecar() {
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
        body: "a note the forget must keep".to_owned(),
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
    let kept: NoteRow = sqlx::query_as(NOTE_ROW_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("forget keeps the flavor sidecar row");
    assert_eq!(kept, before);

    harness
        .engine
        .restore_memory(&authz, owner, memory_id)
        .await
        .expect("the note is restored");
    let after: NoteRow = sqlx::query_as(NOTE_ROW_SQL)
        .bind(memory_id.into_inner())
        .fetch_one(&harness.pool)
        .await
        .expect("the restored sidecar row");
    assert_eq!(
        after, before,
        "every sidecar column survives forget and restore"
    );
    assert_eq!(after.3, written_at);
    let forgotten: bool =
        sqlx::query_scalar("SELECT forgotten_at IS NOT NULL FROM proxima_core.memory WHERE t = $1")
            .bind(memory_id.into_inner())
            .fetch_one(&harness.pool)
            .await
            .expect("memory row");
    assert!(!forgotten, "the note is live again");
}
