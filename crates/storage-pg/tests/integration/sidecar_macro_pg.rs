use crate::common::{drop_db, fresh_pg};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{FactPayload, MemoryId, PayloadKeyBuilder, SidecarPayload};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemorySidecar, PgSidecarReadCtx};
use rust_decimal::Decimal;
use serde_json::json;
use time::{Date, Month};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct MacroKindFact {
    amount: Decimal,
    booked_on: Date,
    metadata: serde_json::Value,
    optional_amount: Option<Decimal>,
    optional_booked_on: Option<Date>,
    optional_metadata: Option<serde_json::Value>,
    optional_retry_count: Option<u32>,
    optional_elapsed_ms: Option<u32>,
    optional_byte_count: Option<u64>,
}

impl FactPayload for MacroKindFact {
    const SCHEMA_ID: &'static str = "test/macro-kind-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("amount", &self.amount.to_string());
        key.field_str("booked_on", &self.booked_on.to_string());
        key.field_str("metadata", &self.metadata.to_string());
        key.finish()
    }

    fn render(&self) -> String {
        "macro kind fact".to_string()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.macro_kind_fact_v1")
    }
}

proxima_storage_pg::pg_sidecar! {
    payload: MacroKindFact,
    row: MacroKindFactRow,
    kinds: [Fact],
    table: "public.macro_kind_fact_v1",
    key: memory_id,
    fields: {
        amount => amount: (decimal),
        booked_on => booked_on: (naive_date),
        metadata => metadata: (jsonb),
        optional_amount => optional_amount: (opt_decimal),
        optional_booked_on => optional_booked_on: (opt_naive_date),
        optional_metadata => optional_metadata: (opt_jsonb),
        optional_retry_count => optional_retry_count: (opt_u32_as_i32),
        optional_elapsed_ms => optional_elapsed_ms: (opt_u32_as_i64),
        optional_byte_count => optional_byte_count: (opt_u64_as_i64),
    },
}

const CREATE_MACRO_KIND_TABLE: &str = "CREATE TABLE public.macro_kind_fact_v1 (
            memory_id uuid PRIMARY KEY,
            amount numeric NOT NULL,
            booked_on date NOT NULL,
            metadata jsonb NOT NULL,
            optional_amount numeric,
            optional_booked_on date,
            optional_metadata jsonb,
            optional_retry_count integer,
            optional_elapsed_ms bigint,
            optional_byte_count bigint
        )";

#[tokio::test]
async fn pg_sidecar_macro_round_trips_decimal_date_and_jsonb_columns()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    sqlx::query(CREATE_MACRO_KIND_TABLE)
        .execute(pg.pool_for_tests())
        .await?;

    let payload = MacroKindFact {
        amount: Decimal::new(123_456, 2),
        booked_on: Date::from_calendar_date(2026, Month::June, 23)?,
        metadata: json!({
            "currency": "EUR",
            "source": "macro-test",
            "lines": [1, 2, 3]
        }),
        optional_amount: Some(Decimal::new(99, 1)),
        optional_booked_on: Some(Date::from_calendar_date(2026, Month::July, 1)?),
        optional_metadata: Some(json!({"nullable": true})),
        optional_retry_count: Some(3),
        optional_elapsed_ms: Some(u32::MAX),
        // Above 2^53: proves the bigint lane stays exact end to end.
        optional_byte_count: Some(9_007_199_254_740_993),
    };
    let memory_id = MemoryId::new(Uuid::now_v7());

    let mut tx = pg.pool_for_tests().begin().await?;
    payload.insert_memory_sidecar(&mut tx, memory_id).await?;
    tx.commit().await?;

    let mut rows = MacroKindFact::load_batch(
        PgSidecarReadCtx::from(pg.pool_for_tests()),
        PayloadKind::Fact,
        &[memory_id],
    )
    .await?;
    assert_eq!(rows.len(), 1);
    let (loaded_memory_id, loaded_payload) = rows.pop().expect("one row loaded");
    assert_eq!(loaded_memory_id, memory_id);
    let loaded = loaded_payload
        .downcast_ref::<MacroKindFact>()
        .expect("loaded sidecar payload keeps its concrete type");
    assert_eq!(loaded, &payload);

    let protocol_json = SidecarPayload::fact(payload).to_protocol_json()?;
    assert_eq!(protocol_json["metadata"]["currency"], "EUR");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Assert the batch read rejects an already-inserted row whose unsigned column
/// holds a value outside the payload type's range, instead of wrapping it.
///
/// The INSERT stays at each call site as a string literal on purpose: routing
/// it through a parameter would turn four literal statements into one dynamic
/// SQL site for `scripts/check-sql-policy.py` to audit.
async fn assert_decode_rejected(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let err = MacroKindFact::load_batch(
        PgSidecarReadCtx::from(pool),
        PayloadKind::Fact,
        &[memory_id],
    )
    .await
    .expect_err("out-of-range value in an unsigned column is rejected on decode");
    assert!(err.to_string().contains(expected), "message: {err}");
    Ok(())
}

/// A payload whose every optional column is absent, so the NULL side of each
/// optional kind is exercised.
fn all_optionals_null() -> Result<MacroKindFact, Box<dyn std::error::Error>> {
    Ok(MacroKindFact {
        amount: Decimal::new(1, 0),
        booked_on: Date::from_calendar_date(2026, Month::June, 23)?,
        metadata: json!({}),
        optional_amount: None,
        optional_booked_on: None,
        optional_metadata: None,
        optional_retry_count: None,
        optional_elapsed_ms: None,
        optional_byte_count: None,
    })
}

/// The optional unsigned-integer kinds convert rather than pass through, so
/// they need the NULL round trip plus a write-side range guard.
#[tokio::test]
async fn pg_sidecar_macro_round_trips_null_optionals_and_guards_the_bind_direction()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    sqlx::query(CREATE_MACRO_KIND_TABLE)
        .execute(pg.pool_for_tests())
        .await?;

    let payload = all_optionals_null()?;
    let memory_id = MemoryId::new(Uuid::now_v7());

    let mut tx = pg.pool_for_tests().begin().await?;
    payload.insert_memory_sidecar(&mut tx, memory_id).await?;
    tx.commit().await?;

    let mut rows = MacroKindFact::load_batch(
        PgSidecarReadCtx::from(pg.pool_for_tests()),
        PayloadKind::Fact,
        &[memory_id],
    )
    .await?;
    assert_eq!(rows.len(), 1);
    let (_, loaded_payload) = rows.pop().expect("one row loaded");
    let loaded = loaded_payload
        .downcast_ref::<MacroKindFact>()
        .expect("loaded sidecar payload keeps its concrete type");
    assert_eq!(loaded, &payload);
    assert!(loaded.optional_retry_count.is_none());
    assert!(loaded.optional_elapsed_ms.is_none());
    assert!(loaded.optional_byte_count.is_none());

    // Bind side: a `u32` above `i32::MAX` errors instead of saturating, and
    // never reaches Postgres. (`opt_u32_as_i64` widens, so it cannot fail.)
    let too_large = MacroKindFact {
        optional_retry_count: Some(u32::try_from(i32::MAX)? + 1),
        ..payload.clone()
    };
    let mut tx = pg.pool_for_tests().begin().await?;
    let err = too_large
        .insert_memory_sidecar(&mut tx, MemoryId::new(Uuid::now_v7()))
        .await
        .expect_err("out-of-range u32 is rejected on insert");
    assert!(
        err.to_string()
            .contains("optional_retry_count out of range"),
        "message: {err}"
    );
    tx.rollback().await?;

    // Same guard one width up: a `u64` above `i64::MAX`.
    let too_large = MacroKindFact {
        optional_byte_count: Some(u64::try_from(i64::MAX)? + 1),
        ..payload.clone()
    };
    let mut tx = pg.pool_for_tests().begin().await?;
    let err = too_large
        .insert_memory_sidecar(&mut tx, MemoryId::new(Uuid::now_v7()))
        .await
        .expect_err("out-of-range u64 is rejected on insert");
    assert!(
        err.to_string().contains("optional_byte_count out of range"),
        "message: {err}"
    );
    tx.rollback().await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Read side of the same guards: values a hand-written INSERT can put in the
/// column but the payload type cannot hold must error, not wrap around.
#[tokio::test]
async fn pg_sidecar_macro_rejects_out_of_range_unsigned_columns_on_decode()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    sqlx::query(CREATE_MACRO_KIND_TABLE)
        .execute(pg.pool_for_tests())
        .await?;

    // `opt_u32_as_i32`: a negative `integer`.
    let negative_retry = MemoryId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO public.macro_kind_fact_v1
             (memory_id, amount, booked_on, metadata, optional_retry_count)
         VALUES ($1, 1, DATE '2026-06-23', '{}'::jsonb, -1)",
    )
    .bind(negative_retry.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    assert_decode_rejected(
        pg.pool_for_tests(),
        negative_retry,
        "invalid optional_retry_count",
    )
    .await?;

    // `opt_u32_as_i64`: a negative `bigint`.
    let negative_elapsed = MemoryId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO public.macro_kind_fact_v1
             (memory_id, amount, booked_on, metadata, optional_elapsed_ms)
         VALUES ($1, 1, DATE '2026-06-23', '{}'::jsonb, -1)",
    )
    .bind(negative_elapsed.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    assert_decode_rejected(
        pg.pool_for_tests(),
        negative_elapsed,
        "invalid optional_elapsed_ms",
    )
    .await?;

    // `opt_u32_as_i64` also rejects a bigint that is positive but too wide.
    let wide_elapsed = MemoryId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO public.macro_kind_fact_v1
             (memory_id, amount, booked_on, metadata, optional_elapsed_ms)
         VALUES ($1, 1, DATE '2026-06-23', '{}'::jsonb, 4294967296)",
    )
    .bind(wide_elapsed.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    assert_decode_rejected(
        pg.pool_for_tests(),
        wide_elapsed,
        "invalid optional_elapsed_ms",
    )
    .await?;

    // `opt_u64_as_i64`: a negative `bigint`.
    let negative_bytes = MemoryId::new(Uuid::now_v7());
    sqlx::query(
        "INSERT INTO public.macro_kind_fact_v1
             (memory_id, amount, booked_on, metadata, optional_byte_count)
         VALUES ($1, 1, DATE '2026-06-23', '{}'::jsonb, -1)",
    )
    .bind(negative_bytes.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    assert_decode_rejected(
        pg.pool_for_tests(),
        negative_bytes,
        "invalid optional_byte_count",
    )
    .await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
