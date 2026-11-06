//! A complete historical path for the facade's version-two hydration fixture.

use super::{FACADE_MIGRATION_VERSION, FACADE_SCHEMAS, FacadeFact};
use proxiima::spiice::{
    DurablePayload, FactPayload, KeyField, MemoryId, PayloadDisplay, PayloadProjection,
    PgMemoryPayload, PgMemoryPayloadFuture, PgMemorySidecar, PgMigrationSchemas, PgSchemaEvolution,
    PgSidecarFuture, PgSidecarReadCtx, SidecarInsertPermit, SidecarOwnerMode, SidecarPayload,
};
use proxiima_core::spiice::{SchemaContract, SchemaEvolution, SchemaRef};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct FacadeFactV1(FacadeFact);

impl PayloadProjection for FacadeFactV1 {
    const KEY_FIELDS: &'static [KeyField<Self>] =
        &[KeyField::uuid("note_id", |payload| payload.0.note_id)];

    fn display(&self) -> PayloadDisplay {
        self.0.display()
    }

    fn durable(&self) -> Result<DurablePayload, String> {
        self.0.durable()
    }
}

impl FactPayload for FacadeFactV1 {
    const SCHEMA_ID: &'static str = "facade-test/fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = proxiima::spiice::PayloadKeyBuilder::new(Self::SCHEMA_ID, 1);
        key.field_uuid("note_id", self.0.note_id);
        key.field_str("body", &self.0.body);
        key.finish()
    }

    fn render(&self) -> String {
        self.0.render()
    }

    fn sidecar_table() -> Option<&'static str> {
        FacadeFact::sidecar_table()
    }
}

impl PgMemorySidecar for FacadeFactV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
        permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        self.0.insert_memory_sidecar(tx, memory_id, permit)
    }
}

impl PgMemoryPayload for FacadeFactV1 {
    const OWNER: SidecarOwnerMode = SidecarOwnerMode::None;
    const MEMORY_KEY_COLUMN: &'static str = "t";

    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            FacadeFact::load_memory_payload(ctx, memory_id)
                .await?
                .map(|payload| {
                    payload
                        .downcast_ref::<FacadeFact>()
                        .cloned()
                        .map(|fact| SidecarPayload::fact(Self(fact)))
                        .ok_or_else(|| {
                            proxiima::StorageError::Internal("fixture payload type".into())
                        })
                })
                .transpose()
        })
    }
}

static SOURCES: [SchemaContract; 1] = [SchemaContract {
    id: SchemaRef::new("facade-test", "fact", 1),
    natural_key_columns: proxiima::spiice::NaturalKeyDeclaration::of::<FacadeFactV1>(),
    ..FACADE_SCHEMAS[0]
}];
static TARGETS: [SchemaContract; 1] = [FACADE_SCHEMAS[0]];

pub(super) const EVOLUTION: SchemaEvolution = SchemaEvolution::new(
    "facade-test/fact-v2",
    &SOURCES,
    &TARGETS,
    FACADE_MIGRATION_VERSION,
    &[],
    "Retain the Fact fields under the version-two contract.",
    "Use the version-two payload after running the dedicated migration.",
);

pub(super) fn program() -> PgSchemaEvolution {
    let mut schemas = PgMigrationSchemas::new();
    schemas.add_fact::<FacadeFactV1>();
    schemas.add_fact::<FacadeFact>();
    PgSchemaEvolution::new(&EVOLUTION, schemas, |input, plan| {
        for source in input.memories() {
            let payload = source.payload::<FacadeFactV1>().ok_or_else(|| {
                proxiima::StorageError::ConstraintViolation("missing historical Fact".into())
            })?;
            plan.retain_fact(source, payload.0.clone())?;
        }
        Ok(())
    })
}
