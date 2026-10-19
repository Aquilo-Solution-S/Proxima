use proxima::flavor::{FactPayload, PayloadKeyBuilder, PgMemoryPayload, PgMemorySidecar};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeMacroFact {
    note: String,
}

impl FactPayload for FacadeMacroFact {
    const SCHEMA_ID: &'static str = "test/facade-macro-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
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

proxima::flavor::pg_sidecar! {
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FacadeFollowsFact {
    mark: String,
}

impl FactPayload for FacadeFollowsFact {
    const SCHEMA_ID: &'static str = "test/facade-follows-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("mark", &self.mark);
        key.finish()
    }

    fn render(&self) -> String {
        self.mark.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.facade_follows_fact_v1")
    }
}

proxima::flavor::pg_sidecar! {
    payload: FacadeFollowsFact,
    row: FacadeFollowsFactRow,
    kinds: [Fact],
    table: "public.facade_follows_fact_v1",
    key: t,
    owner: follows,
    fields: {
        mark => mark: (text),
    },
}

#[test]
fn pg_sidecar_owner_follows_token_is_accepted() {
    use proxima::flavor::SidecarOwnerMode;
    assert_pg_sidecar_traits::<FacadeFollowsFact>();
    assert_eq!(
        <FacadeFollowsFact as PgMemoryPayload>::OWNER,
        SidecarOwnerMode::Follows
    );
    assert!(<FacadeFollowsFact as PgMemoryPayload>::OWNER.stamps_owner_at_insert());
    assert!(!<FacadeFollowsFact as PgMemoryPayload>::OWNER.is_pinned());
}
