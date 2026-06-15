//! External-agent Derived memory append verb.

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::{
    EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerPrincipalKind, PersonalityInstanceId,
    Principal, SchemaId, SchemaVersion, StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::error::map_err;
use crate::pg_ident::PgIdent;

#[derive(Debug, Clone)]
pub struct DerivedDraft<'a> {
    pub memory_id: uuid::Uuid,
    pub owner: Owner,
    pub kind: EntityKind,
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub operator_kind: MemoryOperatorKind,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub sidecar_table: Option<&'a str>,
    pub sidecar_payload: Option<serde_json::Value>,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct DerivedOutcome {
    pub memory_id: MemoryId,
    pub idempotent_replay: bool,
}

/// Append one Derived row, optional typed sidecar, and one change event.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub async fn append_derived_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
) -> Result<DerivedOutcome, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&draft.owner);
    let author_personality_instance_id = draft
        .author_personality_instance_id
        .map_or_else(uuid::Uuid::nil, PersonalityInstanceId::into_inner);

    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id,
             wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 0)
         ON CONFLICT (memory_id) DO NOTHING
         RETURNING memory_id",
    )
    .bind(draft.memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .bind(draft.kind)
    .bind(&draft.text)
    .bind(draft.operator_kind)
    .bind(draft.model_id)
    .bind(draft.prompt_version)
    .bind(author_personality_instance_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;

    if inserted.is_none() {
        return Ok(DerivedOutcome {
            memory_id: MemoryId::new(draft.memory_id),
            idempotent_replay: true,
        });
    }

    if let (Some(table), Some(payload)) = (draft.sidecar_table, &draft.sidecar_payload) {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "INSERT INTO {table}
             SELECT * FROM jsonb_populate_record(
                 NULL::{table},
                 ($1::jsonb || jsonb_build_object('memory_id', $2::uuid))
             )",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(payload)
            .bind(draft.memory_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }

    insert_embedding_in_tx(tx, draft, owner_kind, owner_principal_id, owner_org_id).await?;

    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id,
             kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version,
             wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'EntityAppend', $5, $6, $7, $8, 0)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.kind)
    .bind(draft.memory_id)
    .bind(draft.schema_id.as_str())
    .bind(i32::try_from(draft.schema_version.into_inner()).unwrap_or(1))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(DerivedOutcome {
        memory_id: MemoryId::new(draft.memory_id),
        idempotent_replay: false,
    })
}

async fn insert_embedding_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    draft: &DerivedDraft<'_>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let (Some(embedding), Some(embedding_model_id)) = (&draft.embedding, draft.embedding_model_id)
    else {
        return Ok(());
    };
    if embedding.len() != EMBEDDING_DIM {
        return Err(StorageError::ConstraintViolation(
            "embedding length must be 1024".into(),
        ));
    }
    let vec_literal = crate::pgvector::literal(embedding);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, 1, $3, $4::vector, $5, $6, $7)",
    )
    .bind(draft.kind)
    .bind(draft.memory_id)
    .bind(embedding_model_id)
    .bind(vec_literal)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
