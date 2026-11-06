//! Real-Postgres acceptance for cited-object identity replacement.

use crate::sidecars::{PgMigrationSchemas, PgSchemaEvolution};
use crate::test_fixtures::create_core_db;
use proxiima_core::spiice::{
    EmbeddingRecipe, Provenance, SchemaContract, SchemaEvolution, SchemaRef, SearchProjectionDecl,
    TransferRule,
};
use proxiima_core::verbs::schema::PayloadKind;
use proxiima_pg_testkit::{drop_db, split_role_urls};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const SOURCE_SCHEMA: &str = "example/cited-object-v1";
const TARGET_SCHEMA: &str = "example/cited-object-v2";

static SOURCE: SchemaContract = SchemaContract {
    id: SchemaRef::new("example", "cited-object", 1),
    kind: PayloadKind::CitedObject,
    sidecar_table: None,
    search: SearchProjectionDecl::None {
        why: "blob",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never { why: "blob" },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static TARGET: SchemaContract = SchemaContract {
    id: SchemaRef::new("example", "cited-object", 2),
    ..SOURCE
};
static SECONDARY: SchemaContract = SchemaContract {
    id: SchemaRef::new("example", "cited-object-copy", 2),
    ..SOURCE
};
static EVOLUTION: SchemaEvolution = SchemaEvolution::new(
    "example/cited-object-v1-v2",
    &[SOURCE],
    &[TARGET, SECONDARY],
    2_026_110_600_010,
    &[],
    "replace cited-object identity while preserving bytes",
    "run the dedicated migration before serving",
);

fn frozen_registry() -> super::PgSidecarRegistryFrozen {
    let schemas = PgMigrationSchemas::new();
    let program = PgSchemaEvolution::new(&EVOLUTION, schemas, |input, plan| {
        for source in input.blobs() {
            let target = plan.blob(source, TARGET.id, super::MigrationAdmission::New);
            let secondary = plan.blob(source, SECONDARY.id, super::MigrationAdmission::New);
            plan.map_blob(source, target, &[secondary])?;
        }
        Ok(())
    });
    super::PgSidecarRegistryFrozen {
        active_schema_ids: Arc::from([
            (PayloadKind::CitedObject, TARGET_SCHEMA.to_owned()),
            (PayloadKind::CitedObject, SECONDARY.id.render()),
        ]),
        evolutions: Arc::from([EVOLUTION]),
        evolution_programs: Arc::new(HashMap::from([(EVOLUTION.id, program)])),
        ..Default::default()
    }
}

async fn owner(pool: &PgPool) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

#[tokio::test]
async fn cited_object_replacement_preserves_bytes_and_moves_only_primary_upload() {
    let database = format!("proxiima_test_{}", Uuid::now_v7().simple());
    create_core_db(&database)
        .await
        .expect("PG required for test");
    let result = async {
        let (_, platform_url) = split_role_urls(&database).await?;
        let platform = PgPool::connect(&platform_url).await?;
        let owner = owner(&platform).await?;
        let source = Uuid::now_v7();
        let upload = Uuid::now_v7();
        let hash = vec![0x5a_u8; 32];

        let mut tx = platform.begin().await?;
        sqlx::query("SELECT set_config('app.proxiima_scope', 'platform', true)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxiima_core.store_identity(singleton,store_number) VALUES(true,0) ON CONFLICT (singleton) DO NOTHING")
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxiima_core.blob (blob_id,owner_id,schema_id,content_hash) VALUES ($1,$2,$3,$4)")
            .bind(source).bind(owner).bind(SOURCE_SCHEMA).bind(&hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxiima_core.blob_uploads (upload_id,owner_id,bucket,object_key,filename,mime,expected_byte_len,blob_id,content_hash,object_store,expires_at) VALUES ($1,$2,'test','objects/blob','blob.bin','application/octet-stream',32,$3,$4,'s3:test/bucket',now()+interval '1 day')")
            .bind(upload)
            .bind(owner)
            .bind(source)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        let metadata: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(upload)-'blob_id' FROM proxiima_core.blob_uploads upload WHERE upload_id=$1")
            .bind(upload).fetch_one(&mut *tx).await?;
        let registry = frozen_registry();
        registry.run_schema_evolution(&mut tx, &EVOLUTION).await
            .map_err(|error| sqlx::Error::Protocol(format!("schema evolution run failed: {error}")))?;
        registry.finish_schema_evolutions(&mut tx).await
            .map_err(|error| sqlx::Error::Protocol(format!("schema evolution finish failed: {error}")))?;

        let moved: Option<Uuid> = sqlx::query_scalar("SELECT blob_id FROM proxiima_core.blob_uploads WHERE upload_id=$1")
            .bind(upload)
            .fetch_optional(&mut *tx)
            .await?;
        let surviving: Vec<(Uuid, String, Vec<u8>)> = sqlx::query_as("SELECT blob_id,schema_id,content_hash FROM proxiima_core.blob WHERE owner_id=$1 ORDER BY blob_id")
            .bind(owner)
            .fetch_all(&mut *tx)
            .await?;
        assert_eq!(surviving.len(), 2);
        assert!(surviving.iter().all(|(_, schema, bytes)| bytes == &hash && schema != SOURCE_SCHEMA));
        let target = surviving.iter().find(|(_, schema, _)| schema == TARGET_SCHEMA).unwrap().0;
        assert_ne!(source, target);
        assert_eq!(moved, Some(target), "upload follows only the primary target");
        let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(upload)-'blob_id' FROM proxiima_core.blob_uploads upload WHERE upload_id=$1")
            .bind(upload).fetch_one(&mut *tx).await?;
        assert_eq!(metadata, after, "byte identity and all upload metadata stay exact");
        let remaining_permits: i64 = sqlx::query_scalar("SELECT count(*) FROM proxiima_core.schema_evolution_authority WHERE xact=pg_current_xact_id()")
            .fetch_one(&mut *tx)
            .await?;
        assert_eq!(remaining_permits, 0);
        tx.commit().await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    drop_db(&database).await.expect("drop test database");
    result.expect("cited-object replacement acceptance");
}
