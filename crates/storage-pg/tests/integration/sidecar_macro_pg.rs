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
    },
}

#[tokio::test]
async fn pg_sidecar_macro_round_trips_decimal_date_and_jsonb_columns()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    sqlx::query(
        "CREATE TABLE public.macro_kind_fact_v1 (
            memory_id uuid PRIMARY KEY,
            amount numeric NOT NULL,
            booked_on date NOT NULL,
            metadata jsonb NOT NULL,
            optional_amount numeric,
            optional_booked_on date,
            optional_metadata jsonb
        )",
    )
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
