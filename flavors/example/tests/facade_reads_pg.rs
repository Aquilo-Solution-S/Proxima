//! The two facade reads a flavor was still writing by hand: a receipt lookup
//! on the caller's transaction, and per-unit embedding job diagnostics.

use proxima::company_owner;
use proxima::flavor::{
    FactWriteCommand, IngestReceipt, StorageError, ingest_receipt, lock_scope_fence_shared_tx,
};
use proxima::testkit::{SplitRoleDb, scoped_authz};
use proxima_example::NoteV1;
use proxima_example::payload::{NOTE_SCHEMA_ID, NOTEBOOK_SCOPE};
use proxima_example::testkit::{build_engine, register_notebook, remember_note};
use uuid::Uuid;

#[tokio::test]
async fn a_receipt_is_read_on_the_caller_transaction_and_a_unit_has_no_job_row() {
    let db = SplitRoleDb::create("proxima_example_reads", &[])
        .await
        .expect("PG required");
    let owner = company_owner(Uuid::now_v7());
    let built = build_engine(db.runtime_url(), db.platform_url(), owner)
        .await
        .expect("the example boots");
    let authz = scoped_authz(owner);
    let scope = authz.owner_scope().expect("owner scope");
    let runtime = sqlx::PgPool::connect(db.runtime_url()).await.unwrap();
    let notebook_id = register_notebook(&runtime, scope, owner, "inbox")
        .await
        .expect("notebook");
    let note = NoteV1 {
        notebook_id,
        key: "first".to_owned(),
        body: "a receipt under the caller's lock".to_owned(),
        written_at: time::OffsetDateTime::now_utc(),
    };
    let identity = FactWriteCommand::from_payload("proxima-example/testkit", &note);
    let source_id = identity.source_id.expect("sourced");
    let ingest_key = identity.ingest_key.expect("keyed");
    let engine = built.host().engine();
    let memory_id = remember_note(engine, &authz, owner, &note)
        .await
        .expect("admitted");

    let mut tx = proxima::begin_owner_transaction(&runtime, scope)
        .await
        .expect("caller transaction");
    lock_scope_fence_shared_tx(&mut tx, NOTEBOOK_SCOPE, &owner, notebook_id)
        .await
        .expect("shared scope lock");
    let shares_before = row_share_locks(&mut tx).await;
    let found = ingest_receipt(&mut tx, &owner, &source_id, &ingest_key)
        .await
        .expect("receipt read");
    let IngestReceipt {
        memory_id: read_id,
        handle,
        schema_id,
    } = found.expect("the admission's receipt");
    assert_eq!(read_id, memory_id);
    assert_ne!(handle, Uuid::nil());
    assert_eq!(schema_id.as_str(), NOTE_SCHEMA_ID);
    assert!(
        row_share_locks(&mut tx).await > shares_before,
        "the receipt read share-locks the admission on this transaction"
    );
    let missing = ingest_receipt(&mut tx, &owner, &source_id, "no-such-delivery")
        .await
        .expect("missing key");
    assert!(missing.is_none());
    let empty = ingest_receipt(&mut tx, &owner, "", &ingest_key)
        .await
        .expect_err("empty source");
    assert!(
        matches!(empty, StorageError::ConstraintViolation(message) if message.contains("source_id"))
    );
    tx.commit().await.expect("commit the read");

    let other = company_owner(Uuid::now_v7());
    let other_scope = proxima::testkit::owner_scope_for_tests(&[other]);
    let mut other_tx = proxima::begin_owner_transaction(&runtime, &other_scope)
        .await
        .expect("other owner transaction");
    let hidden = ingest_receipt(&mut other_tx, &owner, &source_id, &ingest_key)
        .await
        .expect("other owner's read");
    assert!(hidden.is_none(), "row security hides the receipt");
    other_tx.rollback().await.expect("rollback");

    let jobs = engine
        .embedding_job_diagnostics(&authz, &owner, &[memory_id])
        .await
        .expect("diagnostics");
    assert!(
        jobs.is_empty(),
        "a note that declines embedding has no job row"
    );
    assert!(
        engine
            .embedding_job_diagnostics(&authz, &owner, &[])
            .await
            .expect("no units")
            .is_empty()
    );
    runtime.close().await;
    built.shutdown().await;
}

async fn row_share_locks(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> i64 {
    // An uncontended `FOR SHARE` records `RowShareLock` on the relation.
    // The tuple lock stays on the row and shows up in `pg_locks` only when
    // another transaction conflicts with it.
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks
          WHERE pid = pg_backend_pid()
            AND granted
            AND locktype = 'relation'
            AND mode = 'RowShareLock'",
    )
    .fetch_one(&mut **tx)
    .await
    .expect("the caller's locks")
}
