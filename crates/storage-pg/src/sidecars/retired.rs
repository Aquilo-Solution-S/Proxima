//! Erase-only backend metadata derived from the frozen spiice contract.

use proxiima_core::spiice::{KeyShape, RetiredFactContract, Surface};

use super::{PayloadKind, PgIdent, PgSidecarKey, StorageError};

/// A retired Fact's validated `PostgreSQL` inverse metadata.
///
/// Constructed only while freezing the PG registry against the core registry.
/// This is deliberately distinct from [`super::PgSidecarEntry`]: it carries
/// no payload inserter, loader, search projection or restore capability.
#[derive(Debug, Clone)]
pub struct PgRetiredFactSidecar {
    key: PgSidecarKey,
    sidecar_table: &'static str,
    memory_key_column: &'static str,
    owner_column: Option<&'static str>,
    surfaces: Vec<Surface>,
}

impl PgRetiredFactSidecar {
    pub(super) fn from_contract(retired: &RetiredFactContract) -> Result<Self, StorageError> {
        let mut surfaces = Vec::new();
        for surface in retired.surfaces() {
            PgIdent::table(surface.table)?;
            for column in surface.key.columns() {
                PgIdent::column(column)?;
            }
            if let Some(column) = surface.owner_column {
                PgIdent::column(column)?;
            }
            surfaces.push(surface);
        }
        let KeyShape::MemoryT { column } = retired.sidecar.key else {
            return Err(StorageError::ConstraintViolation(format!(
                "retired Fact {} does not declare a memory-keyed sidecar",
                retired.id.render()
            )));
        };
        Ok(Self {
            key: PgSidecarKey::new(
                PayloadKind::Fact,
                retired.id.schema_id(),
                retired.id.schema_version(),
            ),
            sidecar_table: retired.sidecar.table,
            memory_key_column: column,
            owner_column: retired.sidecar.owner_column,
            surfaces,
        })
    }

    #[must_use]
    pub fn key(&self) -> &PgSidecarKey {
        &self.key
    }

    #[must_use]
    pub fn sidecar_table(&self) -> &'static str {
        self.sidecar_table
    }

    #[must_use]
    pub fn memory_key_column(&self) -> &'static str {
        self.memory_key_column
    }

    #[must_use]
    pub fn owner_column(&self) -> Option<&'static str> {
        self.owner_column
    }

    /// Every inverse surface, including the primary stamped sidecar.
    #[must_use]
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }
}

#[cfg(test)]
mod tests {
    use proxiima_core::spiice::{
        CounterRule, EraseRule, ExportRule, ForgetRule, ProjectionDecl, RetiredFactContract,
        SchemaRef, SpiiceContract, TransferRule,
    };
    use proxiima_core::{SchemaId, SpiiceRegistry};

    use super::*;
    use crate::sidecars::{PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars};

    const SPIICE: &str = "test-retired";
    const TABLE: &str = "test_retired.removed_v1";
    const DETAIL: &str = "test_retired.removed_details";
    const SCHEMA: &str = "test-retired/removed-v1";

    const fn surface(table: &'static str) -> Surface {
        Surface {
            table,
            key: KeyShape::MemoryT { column: "memory_t" },
            owner_column: Some("owner_id"),
            transfer: TransferRule::Follow,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget: ForgetRule::DumpThenDelete,
            lexical_language_column: None,
            counter: CounterRule::Counted("sidecar_rows"),
            completeness: None,
        }
    }

    const fn retirement(
        sidecar: &Surface,
        inverse_surfaces: &'static [Surface],
    ) -> RetiredFactContract {
        RetiredFactContract {
            id: SchemaRef::new(SPIICE, "removed", 1),
            sidecar: *sidecar,
            inverse_surfaces,
        }
    }

    const fn contract(retired: &'static [RetiredFactContract]) -> SpiiceContract {
        SpiiceContract {
            spiice_id: SPIICE,
            ordinal: 94,
            schemas: &[],
            retired_facts: retired,
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            evolutions: &[],
            projection: ProjectionDecl::None {
                why: "retired Facts have no search surface",
            },
        }
    }

    fn freeze(contract: &'static SpiiceContract) -> Result<PgSidecarRegistryFrozen, StorageError> {
        let mut core = SpiiceRegistry::new();
        core.try_add_contract(contract).unwrap();
        let core = core.try_freeze().unwrap();
        let mut pg = PgSidecarRegistry::new();
        register_core_pg_sidecars(&mut pg);
        pg.freeze_against(&core)
    }

    #[test]
    fn retired_metadata_needs_no_payload_and_grants_no_active_pg_capability() {
        static CONTRACT: SpiiceContract =
            contract(&[retirement(&surface(TABLE), &[surface(DETAIL)])]);
        let frozen = freeze(&CONTRACT).unwrap();
        let [retired] = frozen.retired_fact_sidecars() else {
            panic!("one retired Fact declaration");
        };
        assert_eq!(retired.key().schema_id, SchemaId::new(SCHEMA.to_owned()));
        assert_eq!(retired.sidecar_table(), TABLE);
        assert_eq!(retired.memory_key_column(), "memory_t");
        assert_eq!(retired.owner_column(), Some("owner_id"));
        assert_eq!(retired.surfaces().len(), 2);
        for table in [TABLE, DETAIL] {
            assert!(frozen.retired_fact_surface(table).is_some());
            assert!(!frozen.is_memory_sidecar_table(table));
            assert!(!frozen.memory_sidecar_tables().contains(&table));
        }
        assert!(!frozen.contains(retired.key()));
        assert_eq!(
            frozen.table_for_schema(
                PayloadKind::Fact,
                &retired.key().schema_id,
                retired.key().schema_version
            ),
            None
        );
        assert!(frozen.retired_fact_surface("other.unregistered").is_none());
        assert!(frozen.ensure_schema_restorable(SCHEMA).is_err());
        assert!(
            frozen
                .ensure_schema_restorable("core/agent-note-v1")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn stored_retired_schema_refuses_restore_before_mutating_the_memory() {
        use crate::verbs::fact_embeddings::RouteSpaces;
        use crate::verbs::forget::{Reach, restore_marked};
        use proxiima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
        use proxiima_core::{AccessKind, OwnerId, UserId};

        static CONTRACT: SpiiceContract = contract(&[retirement(&surface(TABLE), &[])]);
        let frozen = freeze(&CONTRACT).unwrap();
        let (pg, _database) = crate::test_fixtures::fresh_pg("retired_restore").await;
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        // This low-level fixture represents a row admitted before retirement;
        // there is intentionally no live payload type or stamped sidecar.
        let mut draft = crate::verbs::forget::forget_pg_tests::draft(None);
        draft.schema_id = SchemaId::new(SCHEMA.to_owned());
        let memory = pg.ingest_fact_atomic(&permit, &draft, None).await.unwrap();
        pg.forget_memory(&permit, memory.memory_id).await.unwrap();

        let mut tx = pg.pool_for_tests().begin().await.unwrap();
        let result = restore_marked(
            &mut tx,
            &frozen,
            memory.memory_id.into_inner(),
            &Reach::of(&permit),
            RouteSpaces {
                spaces: &[],
                non_embeddable_schemas: &[],
            },
        )
        .await;
        assert!(
            matches!(result, Err(StorageError::ConstraintViolation(ref message)) if message.contains("retired Fact"))
        );
        tx.commit().await.unwrap();
        let forgotten: bool = sqlx::query_scalar(
            "SELECT forgotten_at IS NOT NULL FROM proxiima_core.memory /* FORGET-POLICY: includes-forgotten (prove refused restore leaves the row forgotten) */ WHERE t = $1",
        )
        .bind(memory.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await.unwrap();
        assert!(forgotten);
    }

    #[test]
    fn pg_freeze_refuses_invalid_retired_table_identifier() {
        static CONTRACT: SpiiceContract =
            contract(&[retirement(&surface("test_retired.bad-name"), &[])]);
        let error = freeze(&CONTRACT).unwrap_err();
        assert!(error.to_string().contains("invalid table identifier"));
    }

    #[test]
    fn pg_freeze_refuses_invalid_retired_key_identifier() {
        static CONTRACT: SpiiceContract = contract(&[retirement(
            &Surface {
                key: KeyShape::MemoryT { column: "bad-key" },
                ..surface(TABLE)
            },
            &[],
        )]);
        let error = freeze(&CONTRACT).unwrap_err();
        assert!(error.to_string().contains("invalid column identifier"));
    }

    #[test]
    fn pg_freeze_refuses_invalid_retired_owner_identifier() {
        static CONTRACT: SpiiceContract = contract(&[retirement(
            &Surface {
                owner_column: Some("bad-owner"),
                ..surface(TABLE)
            },
            &[],
        )]);
        let error = freeze(&CONTRACT).unwrap_err();
        assert!(error.to_string().contains("invalid column identifier"));
    }

    #[test]
    fn pg_freeze_validates_additional_inverse_surfaces() {
        static CONTRACT: SpiiceContract = contract(&[retirement(
            &surface(TABLE),
            &[surface("test_retired.bad-detail")],
        )]);
        let error = freeze(&CONTRACT).unwrap_err();
        assert!(error.to_string().contains("invalid table identifier"));
    }
}
