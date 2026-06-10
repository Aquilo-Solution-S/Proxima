//! Boot a fresh transient DB, apply migrations, assert
//! tables exist, drop the DB. Requires admin access to a
//! local PG cluster (<postgres://postgres@localhost>).

mod common;

use common::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let row: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.tables \
             WHERE table_schema = 'proxima_core'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert!(
            row.0 >= 7,
            "expected >=7 tables in proxima_core, got {}",
            row.0
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

#[tokio::test]
async fn intervention_rename_migrates_existing_budget_state_in_place() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        seed_pre_intervention_rename_schema(pg.pool()).await?;

        sqlx::raw_sql(include_str!(
            "../migrations/20260519000030_core_intervention_rename.sql"
        ))
        .execute(pg.pool())
        .await?;

        let old_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('proxima_core.budget_decision_v1')::text")
                .fetch_one(pg.pool())
                .await?;
        assert!(old_table.is_none());
        let new_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('proxima_core.intervention_decision_v1')::text")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(
            new_table.as_deref(),
            Some("proxima_core.intervention_decision_v1")
        );

        let old_type: Option<String> =
            sqlx::query_scalar("SELECT to_regtype('proxima_core.budget_decision_kind')::text")
                .fetch_one(pg.pool())
                .await?;
        assert!(old_type.is_none());
        let new_type: Option<String> = sqlx::query_scalar(
            "SELECT to_regtype('proxima_core.intervention_decision_kind')::text",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            new_type.as_deref(),
            Some("proxima_core.intervention_decision_kind")
        );

        let old_schema_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.memories
              WHERE schema_id IN ('core/budget-review-requested-v1',
                                  'core/budget-decision-v1')",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(old_schema_rows, 0);
        let new_schema_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.memories
              WHERE schema_id IN ('core/intervention-requested-v1',
                                  'core/intervention-decision-v1')",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(new_schema_rows, 2);

        let intervention_request: Option<Uuid> = sqlx::query_scalar(
            "SELECT intervention_request_memory_id
               FROM proxima_core.intervention_decision_v1
              WHERE idempotency_key = 'decision-1'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(intervention_request, Some(FIXTURE_REQUEST_MEMORY_ID));

        let continuation_decision: Option<Uuid> = sqlx::query_scalar(
            "SELECT continuation_intervention_decision_memory_id
               FROM proxima_core.personality_wake_invocations",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(continuation_decision, Some(FIXTURE_DECISION_MEMORY_ID));

        let source_id: String =
            sqlx::query_scalar("SELECT source_id FROM proxima_core.source_batches")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(source_id, "core/intervention");
        let relation: String = sqlx::query_scalar("SELECT relation FROM proxima_core.edges")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(relation, "core/receives-intervention-request");
        let palettes: (Vec<String>, Vec<String>) = sqlx::query_as(
            "SELECT substrate_tool_palette, workspace_tool_palette
               FROM proxima_core.personality_wake_entries",
        )
        .fetch_one(pg.pool())
        .await?;
        assert!(
            palettes
                .0
                .contains(&"core/emit_intervention_decision".into())
        );
        assert!(
            palettes
                .1
                .contains(&"core/emit_intervention_decision".into())
        );
        assert!(!palettes.0.contains(&"core/emit_budget_decision".into()));
        assert!(!palettes.1.contains(&"core/emit_budget_decision".into()));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("intervention rename migration test failed");
}

const FIXTURE_REQUEST_MEMORY_ID: Uuid = Uuid::from_u128(0x018f_0000_0000_7000_8000_000000000001);
const FIXTURE_DECISION_MEMORY_ID: Uuid = Uuid::from_u128(0x018f_0000_0000_7000_8000_000000000002);

async fn seed_pre_intervention_rename_schema(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r"
        CREATE SCHEMA proxima_core;

        CREATE TYPE proxima_core.budget_decision_kind AS ENUM (
            'continue',
            'stop',
            'redirect',
            'decompose',
            'accept_terminal'
        );

        CREATE TABLE proxima_core.memories (
            memory_id uuid PRIMARY KEY,
            schema_id text NOT NULL
        );

        CREATE TABLE proxima_core.events (
            event_id bytea PRIMARY KEY,
            source_id text NOT NULL,
            schema_id text NOT NULL
        );

        CREATE TABLE proxima_core.change_event (
            seq uuid PRIMARY KEY,
            entity_schema_id text NOT NULL
        );

        CREATE TABLE proxima_core.cited_objects (
            cited_object_id uuid PRIMARY KEY,
            schema_id text NOT NULL
        );

        CREATE TABLE proxima_core.citation_mappings (
            citation_mapping_id uuid PRIMARY KEY,
            schema_id text NOT NULL
        );

        CREATE TABLE proxima_core.source_batches (
            id uuid PRIMARY KEY,
            source_id text NOT NULL
        );

        CREATE TABLE proxima_core.edges (
            edge_id uuid PRIMARY KEY,
            relation text NOT NULL
        );

        CREATE TABLE proxima_core.personality_wake_entries (
            wake_entry_id uuid PRIMARY KEY,
            substrate_tool_palette text[] NOT NULL,
            workspace_tool_palette text[] NOT NULL,
            budgeter_personality_instance_id uuid,
            budget_extension_rounds integer DEFAULT 0 NOT NULL,
            budget_hard_cap_rounds integer DEFAULT 0 NOT NULL,
            budget_progress_contract text DEFAULT ''::text NOT NULL,
            CONSTRAINT personality_wake_entries_budget_rounds_chk
                CHECK (budget_extension_rounds >= 0 AND budget_hard_cap_rounds >= 0),
            CONSTRAINT personality_wake_entries_budget_policy_chk
                CHECK (
                    (budgeter_personality_instance_id IS NULL
                     AND budget_extension_rounds = 0
                     AND budget_hard_cap_rounds = 0
                     AND budget_progress_contract = '')
                    OR
                    (budgeter_personality_instance_id IS NOT NULL
                     AND budget_extension_rounds > 0
                     AND budget_hard_cap_rounds >= budget_extension_rounds
                     AND length(budget_progress_contract) > 0)
                )
        );

        CREATE TABLE proxima_core.budget_review_requested_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            original_invocation_id uuid NOT NULL,
            original_wake_entry_id uuid NOT NULL,
            original_personality_instance_id uuid NOT NULL,
            original_change_event_seq uuid NOT NULL,
            triggering_memory_id uuid NOT NULL,
            wake_trace_memory_id uuid NOT NULL,
            target_budgeter_personality_instance_id uuid NOT NULL,
            max_rounds integer NOT NULL,
            rounds_used integer NOT NULL,
            budget_extension_rounds integer NOT NULL,
            budget_hard_cap_rounds integer NOT NULL,
            continued_rounds_used integer DEFAULT 0 NOT NULL,
            active_goal_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
            progress_contract text NOT NULL,
            requested_at timestamp with time zone DEFAULT now() NOT NULL,
            idempotency_key text NOT NULL,
            CONSTRAINT budget_review_rounds_chk
                CHECK (
                    max_rounds >= 0
                    AND rounds_used >= 0
                    AND budget_extension_rounds > 0
                    AND budget_hard_cap_rounds >= budget_extension_rounds
                    AND continued_rounds_used >= 0
                ),
            CONSTRAINT budget_review_progress_contract_chk CHECK (length(progress_contract) > 0),
            CONSTRAINT budget_review_idempotency_key_chk CHECK (length(idempotency_key) > 0)
        );

        CREATE UNIQUE INDEX budget_review_requested_invocation_uq
            ON proxima_core.budget_review_requested_v1 (original_invocation_id);
        CREATE INDEX budget_review_requested_target_idx
            ON proxima_core.budget_review_requested_v1 (target_budgeter_personality_instance_id);

        CREATE TABLE proxima_core.budget_decision_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            budget_request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
            decision proxima_core.budget_decision_kind NOT NULL,
            grant_rounds integer,
            redirect_personality_instance_id uuid,
            rationale text NOT NULL,
            decided_at timestamp with time zone DEFAULT now() NOT NULL,
            idempotency_key text NOT NULL,
            CONSTRAINT budget_decision_rounds_chk CHECK (grant_rounds IS NULL OR grant_rounds >= 0),
            CONSTRAINT budget_decision_rationale_chk CHECK (length(rationale) > 0),
            CONSTRAINT budget_decision_idempotency_key_chk CHECK (length(idempotency_key) > 0)
        );

        CREATE UNIQUE INDEX budget_decision_idempotency_uq
            ON proxima_core.budget_decision_v1 (budget_request_memory_id, idempotency_key);
        CREATE INDEX budget_decision_request_idx
            ON proxima_core.budget_decision_v1 (budget_request_memory_id);

        CREATE TABLE proxima_core.personality_wake_invocations (
            invocation_id uuid PRIMARY KEY,
            continuation_budget_decision_memory_id uuid,
            continuation_original_invocation_id uuid,
            CONSTRAINT personality_wake_invocations_continuation_decision_fkey
                FOREIGN KEY (continuation_budget_decision_memory_id)
                REFERENCES proxima_core.memories(memory_id)
        );

        CREATE UNIQUE INDEX personality_wake_invocations_continuation_decision_uq
            ON proxima_core.personality_wake_invocations (continuation_budget_decision_memory_id)
            WHERE continuation_budget_decision_memory_id IS NOT NULL;
        ",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.memories (memory_id, schema_id)
         VALUES ($1, 'core/budget-review-requested-v1'),
                ($2, 'core/budget-decision-v1')",
    )
    .bind(FIXTURE_REQUEST_MEMORY_ID)
    .bind(FIXTURE_DECISION_MEMORY_ID)
    .execute(pool)
    .await?;

    sqlx::raw_sql(
        r"
        INSERT INTO proxima_core.events (event_id, source_id, schema_id)
        VALUES ('\x01', 'core/budget-review', 'core/budget-review-requested-v1'),
               ('\x02', 'core/budget-review', 'core/budget-decision-v1');

        INSERT INTO proxima_core.change_event (seq, entity_schema_id)
        VALUES ('018f0000-0000-7000-8000-000000000011',
                'core/budget-review-requested-v1'),
               ('018f0000-0000-7000-8000-000000000012',
                'core/budget-decision-v1');

        INSERT INTO proxima_core.cited_objects (cited_object_id, schema_id)
        VALUES ('018f0000-0000-7000-8000-000000000021',
                'core/budget-review-requested-object-v1'),
               ('018f0000-0000-7000-8000-000000000022',
                'core/budget-decision-object-v1');

        INSERT INTO proxima_core.citation_mappings (citation_mapping_id, schema_id)
        VALUES ('018f0000-0000-7000-8000-000000000031',
                'core/budget-review-requested-whole-v1'),
               ('018f0000-0000-7000-8000-000000000032',
                'core/budget-decision-whole-v1');

        INSERT INTO proxima_core.source_batches (id, source_id)
        VALUES ('018f0000-0000-7000-8000-000000000041', 'core/budget-review');

        INSERT INTO proxima_core.edges (edge_id, relation)
        VALUES ('018f0000-0000-7000-8000-000000000051', 'core/receives-budget-review');

        INSERT INTO proxima_core.personality_wake_entries (
            wake_entry_id,
            substrate_tool_palette,
            workspace_tool_palette,
            budgeter_personality_instance_id,
            budget_extension_rounds,
            budget_hard_cap_rounds,
            budget_progress_contract
        )
        VALUES (
            '018f0000-0000-7000-8000-000000000061',
            ARRAY['core/emit_budget_decision'],
            ARRAY['core/emit_budget_decision'],
            '018f0000-0000-7000-8000-000000000062',
            4,
            8,
            'progress contract'
        );

        INSERT INTO proxima_core.budget_review_requested_v1 (
            memory_id,
            original_invocation_id,
            original_wake_entry_id,
            original_personality_instance_id,
            original_change_event_seq,
            triggering_memory_id,
            wake_trace_memory_id,
            target_budgeter_personality_instance_id,
            max_rounds,
            rounds_used,
            budget_extension_rounds,
            budget_hard_cap_rounds,
            progress_contract,
            idempotency_key
        )
        VALUES (
            '018f0000-0000-7000-8000-000000000001',
            '018f0000-0000-7000-8000-000000000071',
            '018f0000-0000-7000-8000-000000000061',
            '018f0000-0000-7000-8000-000000000072',
            '018f0000-0000-7000-8000-000000000011',
            '018f0000-0000-7000-8000-000000000001',
            '018f0000-0000-7000-8000-000000000001',
            '018f0000-0000-7000-8000-000000000062',
            3,
            3,
            4,
            8,
            'progress contract',
            'request-1'
        );

        INSERT INTO proxima_core.budget_decision_v1 (
            memory_id,
            budget_request_memory_id,
            decision,
            grant_rounds,
            rationale,
            idempotency_key
        )
        VALUES (
            '018f0000-0000-7000-8000-000000000002',
            '018f0000-0000-7000-8000-000000000001',
            'continue',
            4,
            'continue',
            'decision-1'
        );

        INSERT INTO proxima_core.personality_wake_invocations (
            invocation_id,
            continuation_budget_decision_memory_id,
            continuation_original_invocation_id
        )
        VALUES (
            '018f0000-0000-7000-8000-000000000081',
            '018f0000-0000-7000-8000-000000000002',
            '018f0000-0000-7000-8000-000000000071'
        );
        ",
    )
    .execute(pool)
    .await?;

    Ok(())
}
