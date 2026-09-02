//! Engine forget/hydrate of a real `code_chunk_v1` sidecar row.

mod common;

use common::{migrated_db, owner_write_permit, seed_memory_with_sidecars_in_tx, test_owner};
use proxima_code::{CodeChunkV1, CodeExecutionPlanV1};
use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerWritePermit};
use proxima_core::{AbstractionPayload, AccessKind, ColdObjectStore, MemoryId};
use proxima_pg_testkit::drop_db;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use std::sync::Arc;
use uuid::Uuid;

fn cold_detail_offset(bytes: &[u8]) -> usize {
    fn u16_at(bytes: &[u8], offset: &mut usize) -> usize {
        let value = u16::from_be_bytes([bytes[*offset], bytes[*offset + 1]]);
        *offset += 2;
        usize::from(value)
    }
    fn skip_str(bytes: &[u8], offset: &mut usize) {
        *offset += u16_at(bytes, offset);
    }
    fn skip_opt_str(bytes: &[u8], offset: &mut usize) {
        if bytes[*offset] == 1 {
            *offset += 1;
            skip_str(bytes, offset);
        } else {
            *offset += 1;
        }
    }
    fn skip_opt_uuid(bytes: &[u8], offset: &mut usize) {
        if bytes[*offset] == 1 {
            *offset += 17;
        } else {
            *offset += 1;
        }
    }
    assert_eq!(bytes[0], 7, "the test requires the v7 cold format");
    let mut offset = 1 + 16 + 16;
    skip_str(bytes, &mut offset);
    offset += 16;
    skip_opt_str(bytes, &mut offset);
    skip_opt_str(bytes, &mut offset);
    skip_opt_uuid(bytes, &mut offset);
    for _ in 0..3 {
        let count = u16_at(bytes, &mut offset);
        offset += count * 16;
    }
    skip_str(bytes, &mut offset);
    let sidecar_count = u16_at(bytes, &mut offset);
    for _ in 0..sidecar_count {
        skip_str(bytes, &mut offset);
    }
    let dump_count = u16_at(bytes, &mut offset);
    for _ in 0..dump_count {
        skip_str(bytes, &mut offset);
        skip_str(bytes, &mut offset);
    }
    let model_count = u16_at(bytes, &mut offset);
    for _ in 0..model_count {
        skip_str(bytes, &mut offset);
    }
    skip_opt_str(bytes, &mut offset);
    offset
}

fn forge_extra_detail_declaration(bytes: &[u8]) -> Vec<u8> {
    let offset = cold_detail_offset(bytes);
    let count = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    let mut forged = bytes[..offset].to_vec();
    forged.extend_from_slice(&(count + 1).to_be_bytes());
    forged.extend_from_slice(&bytes[offset + 2..]);
    let table = b"proxima_code.forged_detail_v1";
    let table_len = u16::try_from(table.len()).expect("fixture table name fits the cold format");
    forged.extend_from_slice(&table_len.to_be_bytes());
    forged.extend_from_slice(table);
    forged.extend_from_slice(&0_u16.to_be_bytes());
    forged
}

fn forge_omitted_detail_declaration(bytes: &[u8], _memory_id: Uuid) -> Vec<u8> {
    let offset = cold_detail_offset(bytes);
    let mut forged = bytes[..offset].to_vec();
    forged.extend_from_slice(&0_u16.to_be_bytes());
    forged
}

fn forge_detail_key(bytes: &[u8], memory_id: Uuid) -> Vec<u8> {
    let offset = cold_detail_offset(bytes);
    let count = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    assert!(
        count > 0,
        "the code chunk must declare its call detail table"
    );
    let mut cursor = offset + 2;
    let table_len = usize::from(u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]));
    cursor += 2 + table_len;
    let row_count = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
    assert!(row_count > 0, "the test must seed a detail row");
    cursor += 2;
    let row_len = usize::from(u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]));
    cursor += 2;
    let row_end = cursor + row_len;
    let old = memory_id.to_string();
    let replacement = Uuid::now_v7().to_string();
    let mut forged = bytes.to_vec();
    let position = bytes[cursor..row_end]
        .windows(old.len())
        .position(|window| window == old.as_bytes())
        .expect("the detail row carries its declared caller key")
        + cursor;
    forged[position..position + old.len()].copy_from_slice(replacement.as_bytes());
    forged
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one exact atom: seed, cool, hydrate, and physical-row proof
async fn forget_hydrate_restores_code_chunk_sidecar() {
    let (db_name, pg) = migrated_db().await;
    let cold = Arc::new(MemoryColdStore::default());
    let pg = pg.with_cold(cold.clone());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let permit: OwnerWritePermit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let pool = pg.pool_for_tests();
        // The stamp and the rows it promises land in one transaction: a
        // memory row that names a sidecar table it has no row in is refused
        // at COMMIT.
        let mut stamped = pool.begin().await?;
        let (_handle, memory_id) = seed_memory_with_sidecars_in_tx(
            &mut stamped,
            &owner,
            <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
            "abstraction",
            None,
            None,
            &[],
            &[<CodeChunkV1 as AbstractionPayload>::sidecar_table()],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                 byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             VALUES ($1, $2, 'src/lib.rs', 0, 'fn forget_me() {}', 'rust', 'fn',
                     0, 16, 1, 1, 'Present')",
        )
        .bind(memory_id)
        .bind(Uuid::now_v7())
        .execute(&mut *stamped)
        .await?;
        stamped.commit().await?;

        let callee = Uuid::now_v7();
        for (site_index, byte_start, byte_end, callee_name, is_dynamic) in [
            (0_i32, 0_i64, 4_i64, "first_call", false),
            (1_i32, 8_i64, 16_i64, "second_call", true),
        ] {
            sqlx::query(
                "INSERT INTO proxima_code.code_chunk_call_v1
                    (caller_memory_id, callee_memory_id, site_index, byte_start,
                     byte_end, callee_name, is_dynamic)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id)
            .bind(callee)
            .bind(site_index)
            .bind(byte_start)
            .bind(byte_end)
            .bind(callee_name)
            .bind(is_dynamic)
            .execute(pool)
            .await?;
        }
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text), '[]'::jsonb)
               FROM proxima_code.code_chunk_call_v1 s
              WHERE caller_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, MemoryId::new(memory_id)).await?;
        let hot: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_code.code_chunk_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(hot, 0, "forget deletes the flavor sidecar before memory");

        let hydrated =
            MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[MemoryId::new(memory_id)])
                .await?;
        assert_eq!(
            hydrated.outcomes[0].status,
            proxima_core::MemoryHydrationStatus::Hydrated
        );

        let text: String =
            sqlx::query_scalar("SELECT text FROM proxima_code.code_chunk_v1 WHERE t = $1")
                .bind(memory_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(text, "fn forget_me() {}");
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text), '[]'::jsonb)
               FROM proxima_code.code_chunk_call_v1 s
              WHERE caller_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(after, before, "all caller detail rows survive hydration");
        // Forget deleted the memory row, so the projection row went with it
        // through `ON DELETE CASCADE`; hydrate re-derives it from the
        // restored sidecar. This is the property the generated column used
        // to get for free.
        let tsv: bool = sqlx::query_scalar(
            "SELECT search_tsv <> ''::tsvector
               FROM proxima_code.projection
              WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert!(tsv, "the projection vector is rebuilt on hydrate");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forget/hydrate code_chunk_v1 failed");
}

#[test]
fn every_code_cascaded_detail_surface_is_in_the_hydration_contract() {
    let registry = proxima_code::schema_registry();
    let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);
    let details =
        surfaces.cascaded_details_for_schema(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID);
    assert_eq!(
        details
            .iter()
            .map(|detail| (detail.table, detail.key_column))
            .collect::<Vec<_>>(),
        vec![("proxima_code.code_chunk_call_v1", "caller_memory_id")]
    );
    let actual = [
        "proxima-code/acceptance-criteria-v1",
        "proxima-code/code-chunk-v1",
        "proxima-code/execution-plan-v1",
        "proxima-code/test-requested-v1",
    ]
    .into_iter()
    .flat_map(|schema_id| {
        surfaces
            .cascaded_details_for_schema(schema_id)
            .iter()
            .map(move |detail| (schema_id, detail.table, detail.key_column))
    })
    .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                "proxima-code/acceptance-criteria-v1",
                "proxima_code.acceptance_criterion_v1",
                "criteria_memory_id"
            ),
            (
                "proxima-code/code-chunk-v1",
                "proxima_code.code_chunk_call_v1",
                "caller_memory_id"
            ),
            (
                "proxima-code/execution-plan-v1",
                "proxima_code.execution_plan_item_v1",
                "plan_memory_id"
            ),
            (
                "proxima-code/test-requested-v1",
                "proxima_code.test_requested_criterion_v1",
                "test_requested_memory_id"
            ),
        ]
    );
    assert_eq!(
        registry
            .contracts()
            .iter()
            .flat_map(|contract| contract.schemas)
            .flat_map(|schema| schema.surfaces)
            .filter(|surface| {
                matches!(
                    proxima_core::flavor::ForgetLeg::derive(surface),
                    proxima_core::flavor::ForgetLeg::DumpedCascade { .. }
                )
            })
            .count(),
        actual.len()
    );
}

async fn forged_detail_case(
    forge: fn(&[u8], Uuid) -> Vec<u8>,
    expected: proxima_core::MemoryHydrationStatus,
    refresh_digest: bool,
) {
    let (db_name, pg) = migrated_db().await;
    let cold = Arc::new(MemoryColdStore::default());
    let pg = pg.with_cold(cold.clone());
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let permit: OwnerWritePermit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let pool = pg.pool_for_tests();
        // The stamp and the rows it promises land in one transaction: a
        // memory row that names a sidecar table it has no row in is refused
        // at COMMIT.
        let mut stamped = pool.begin().await?;
        let (_handle, memory_id) = seed_memory_with_sidecars_in_tx(
            &mut stamped,
            &owner,
            <CodeChunkV1 as AbstractionPayload>::SCHEMA_ID,
            "abstraction",
            None,
            None,
            &[],
            &[<CodeChunkV1 as AbstractionPayload>::sidecar_table()],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                 byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             VALUES ($1, $2, 'src/forged.rs', 0, 'fn forged() {}', 'rust', 'fn',
                     0, 14, 1, 1, 'Present')",
        )
        .bind(memory_id)
        .bind(Uuid::now_v7())
        .execute(&mut *stamped)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_call_v1
                (caller_memory_id, callee_memory_id, site_index, byte_start,
                 byte_end, callee_name, is_dynamic)
             VALUES ($1, $2, 0, 0, 4, 'forged_call', false)",
        )
        .bind(memory_id)
        .bind(Uuid::now_v7())
        .execute(&mut *stamped)
        .await?;
        stamped.commit().await?;

        MemoryAuthoringPort::forget_memory(&pg, &permit, MemoryId::new(memory_id)).await?;
        let key = proxima_storage_pg::verbs::forget::cold_object_key(memory_id);
        let original = cold.get(&key).await?;
        let forged = forge(&original, memory_id);
        cold.put(&key, &forged).await?;
        if refresh_digest {
            // This models a malformed writer that supplied a self-consistent
            // object and witness. It lets the test reach the contract-derived
            // declaration/key validator; the production append-only trigger
            // still prevents this database rewrite.
            let digest = blake3::hash(&forged).as_bytes().to_vec();
            let mut tx = pool.begin().await?;
            sqlx::query("ALTER TABLE proxima_core.cooled DISABLE TRIGGER cooled_append_only")
                .execute(tx.as_mut())
                .await?;
            sqlx::query("UPDATE proxima_core.cooled SET cold_digest = $2 WHERE t = $1")
                .bind(memory_id)
                .bind(digest)
                .execute(tx.as_mut())
                .await?;
            sqlx::query("ALTER TABLE proxima_core.cooled ENABLE TRIGGER cooled_append_only")
                .execute(tx.as_mut())
                .await?;
            tx.commit().await?;
        }

        let hydrated =
            MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[MemoryId::new(memory_id)])
                .await?;
        assert_eq!(hydrated.outcomes[0].status, expected);
        let memory_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(memory_id)
                .fetch_one(pool)
                .await?;
        let cooled_count: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                .bind(memory_id)
                .fetch_one(pool)
                .await?;
        let detail_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_code.code_chunk_call_v1
              WHERE caller_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_count, 0, "forged detail must not create a parent");
        assert_eq!(cooled_count, 1, "failed hydration retains the cold locator");
        assert_eq!(
            detail_count, 0,
            "the failed atom restores no partial detail"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("forged cold detail must fail atomically");
}

#[tokio::test]
async fn forged_cascaded_detail_declaration_is_rejected_atomically() {
    forged_detail_case(
        |bytes, memory_id| {
            let mut forged = forge_detail_key(bytes, memory_id);
            forged = forge_extra_detail_declaration(&forged);
            forged
        },
        proxima_core::MemoryHydrationStatus::UnsupportedColdObject,
        true,
    )
    .await;
}

#[tokio::test]
async fn omitted_cascaded_detail_declaration_is_rejected_atomically() {
    forged_detail_case(
        forge_omitted_detail_declaration,
        proxima_core::MemoryHydrationStatus::UnsupportedColdObject,
        true,
    )
    .await;
}

#[tokio::test]
async fn forged_cascaded_detail_key_is_rejected_atomically() {
    forged_detail_case(
        forge_detail_key,
        proxima_core::MemoryHydrationStatus::InvalidColdObject,
        true,
    )
    .await;
}

#[tokio::test]
async fn forged_cascaded_detail_fails_the_database_digest_gate() {
    forged_detail_case(
        forge_detail_key,
        proxima_core::MemoryHydrationStatus::InvalidColdObject,
        false,
    )
    .await;
}

#[tokio::test]
async fn forget_hydrate_restores_execution_plan_details() {
    let (db_name, pg) = migrated_db().await;
    let cold = Arc::new(MemoryColdStore::default());
    let pg = pg.with_cold(cold);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let permit: OwnerWritePermit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let pool = pg.pool_for_tests();
        // The stamp and the row it promises land in one transaction: a memory
        // row that names a sidecar table it has no row in is refused at COMMIT.
        let mut stamped = pool.begin().await?;
        let (_handle, memory_id) = seed_memory_with_sidecars_in_tx(
            &mut stamped,
            &owner,
            <CodeExecutionPlanV1 as AbstractionPayload>::SCHEMA_ID,
            "abstraction",
            None,
            None,
            &[],
            &[<CodeExecutionPlanV1 as AbstractionPayload>::sidecar_table()],
        )
        .await?;
        let repo_id = Uuid::now_v7();
        let activation = memory_id;
        sqlx::query(
            "INSERT INTO proxima_code.execution_plan_v1
                (t, repo_id, plan_key, goal_activated_memory_id, summary,
                 item_count, evidence_memory_ids)
             VALUES ($1, $2, 'roundtrip-plan', $3, 'plan summary', 2, ARRAY[]::uuid[])",
        )
        .bind(memory_id)
        .bind(repo_id)
        .bind(activation)
        .execute(&mut *stamped)
        .await?;
        stamped.commit().await?;
        for (item_index, item_key, title, depends_on) in [
            (0_i32, "compile", "Compile the crate", Vec::<String>::new()),
            (1_i32, "test", "Run the tests", vec!["compile".to_owned()]),
        ] {
            sqlx::query(
                "INSERT INTO proxima_code.execution_plan_item_v1
                    (plan_memory_id, item_index, item_key, kind, title,
                     depends_on, request_key, request_memory_id)
                 VALUES ($1, $2, $3, 'work', $4, $5, $6, $7)",
            )
            .bind(memory_id)
            .bind(item_index)
            .bind(item_key)
            .bind(title)
            .bind(depends_on)
            .bind(format!("request-{item_index}"))
            .bind(Uuid::now_v7())
            .execute(pool)
            .await?;
        }
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text), '[]'::jsonb)
               FROM proxima_code.execution_plan_item_v1 s
              WHERE plan_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, MemoryId::new(memory_id)).await?;
        let hydrated =
            MemoryAuthoringPort::hydrate_memories(&pg, &permit, &[MemoryId::new(memory_id)])
                .await?;
        assert_eq!(
            hydrated.outcomes[0].status,
            proxima_core::MemoryHydrationStatus::Hydrated
        );
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text), '[]'::jsonb)
               FROM proxima_code.execution_plan_item_v1 s
              WHERE plan_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(after, before, "enum and text[] detail rows survive hydrate");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("execution plan details must survive forget/hydrate");
}
