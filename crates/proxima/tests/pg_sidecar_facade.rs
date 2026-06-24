use proxima::{FactPayload, PayloadKeyBuilder, PgMemoryPayload, PgMemorySidecar};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeMacroFact {
    note: String,
}

impl FactPayload for FacadeMacroFact {
    const SCHEMA_ID: &'static str = "test/facade-macro-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.facade_macro_fact_v1")
    }
}

proxima::pg_sidecar! {
    payload: FacadeMacroFact,
    row: FacadeMacroFactRow,
    kinds: [Fact],
    table: "public.facade_macro_fact_v1",
    key: memory_id,
    fields: {
        note => note: (text),
    },
}

fn assert_pg_sidecar_traits<T: FactPayload + PgMemorySidecar + PgMemoryPayload>() {}

#[test]
fn pg_sidecar_macro_is_reachable_from_facade() {
    assert_pg_sidecar_traits::<FacadeMacroFact>();
    let payload = FacadeMacroFact {
        note: "facade macro ok".to_string(),
    };
    assert_eq!(payload.render(), "facade macro ok");
}
