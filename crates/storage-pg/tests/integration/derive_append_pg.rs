use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage::Storage;
use proxima_core::{
    EntityKind, MemoryOperatorKind, Owner, PersonalityInstanceId, SchemaId, SchemaVersion,
};
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_in_tx};

fn agent_draft(
    memory_id: uuid::Uuid,
    owner: Owner,
    kind: EntityKind,
    title: &'static str,
    body: &'static str,
    author: Option<PersonalityInstanceId>,
) -> DerivedDraft<'static> {
    DerivedDraft {
        memory_id,
        owner,
        kind,
        author_personality_instance_id: author,
        schema_id: SchemaId::new("proxima-agent-memory/agent-derivation-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: body.into(),
        operator_kind: MemoryOperatorKind::ExternalAgent,
        model_id: "claude-opus-4.7",
        prompt_version: "mcp-agent-v1",
        sidecar_table: Some("proxima_agent_memory.agent_derivation_v1"),
        sidecar_payload: Some(serde_json::json!({
            "title": title,
            "body": body,
            "tags": [],
            "idempotency_key": null,
            "source_memory_ids": [],
            "model_id": "claude-opus-4.7",
            "client_name": "codex",
            "client_version": "1",
        })),
    }
}

#[tokio::test]
async fn external_agent_abstraction_persists_with_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        proxima_agent_memory::migrator().run(pg.pool()).await?;
        let owner = owner_fixture();
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-1");
        let draft = agent_draft(
            memory_id,
            owner,
            EntityKind::Abstraction,
            "x",
            "the agent view",
            None,
        );

        let mut tx = pg.pool().begin().await?;
        let outcome = append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        assert_eq!(outcome.memory_id.into_inner(), memory_id);
        assert!(!outcome.idempotent_replay);

        let mut tx = pg.pool().begin().await?;
        let replay = append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);

        let row_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_agent_memory.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn external_agent_perspective_persists() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        proxima_agent_memory::migrator().run(pg.pool()).await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-2");
        let draft = agent_draft(
            memory_id,
            owner_fixture(),
            EntityKind::Perspective,
            "p",
            "perspective body",
            None,
        );
        let mut tx = pg.pool().begin().await?;
        append_derived_in_tx(&mut tx, &draft).await?;
        tx.commit().await?;
        let kind: EntityKind =
            sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
                .bind(memory_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(kind, EntityKind::Perspective);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn external_agent_abstraction_stamps_author_without_change_event_author()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result = async {
        pg.run_migrations().await?;
        proxima_agent_memory::migrator().run(pg.pool()).await?;

        let owner = owner_fixture();
        let subject = owner.principal.clone();
        let personality = pg.ensure_subject_personality(&owner, &subject).await?;

        let authored_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-author-test-1");
        let authored = agent_draft(
            authored_id,
            owner.clone(),
            EntityKind::Abstraction,
            "authored",
            "authored abstraction",
            Some(personality.instance_id),
        );

        let mut tx = pg.pool().begin().await?;
        append_derived_in_tx(&mut tx, &authored).await?;
        tx.commit().await?;

        let stamped: uuid::Uuid = sqlx::query_scalar(
            "SELECT personality_instance_id
             FROM proxima_core.memories
             WHERE memory_id = $1",
        )
        .bind(authored_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(stamped, personality.instance_id.into_inner());
        assert_ne!(stamped, uuid::Uuid::nil());

        let authored_change_author: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT entity_personality_instance_id
             FROM proxima_core.change_event
             WHERE entity_memory_id = $1",
        )
        .bind(authored_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_change_author, None);

        let system_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-author-test-2");
        let system = agent_draft(
            system_id,
            owner,
            EntityKind::Abstraction,
            "system",
            "system abstraction",
            None,
        );

        let mut tx = pg.pool().begin().await?;
        append_derived_in_tx(&mut tx, &system).await?;
        tx.commit().await?;

        let system_stamped: uuid::Uuid = sqlx::query_scalar(
            "SELECT personality_instance_id
             FROM proxima_core.memories
             WHERE memory_id = $1",
        )
        .bind(system_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(system_stamped, uuid::Uuid::nil());

        let system_change_author: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT entity_personality_instance_id
             FROM proxima_core.change_event
             WHERE entity_memory_id = $1",
        )
        .bind(system_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(system_change_author, None);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
