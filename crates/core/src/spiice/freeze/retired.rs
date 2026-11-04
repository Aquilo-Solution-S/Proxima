use super::{SpiiceRegistry, SpiiceRegistryError};
use crate::spiice::{EraseRule, KeyShape};

impl SpiiceRegistry {
    pub(super) fn validate_retired_facts(&self) -> Result<(), SpiiceRegistryError> {
        let mut schemas = std::collections::HashSet::new();
        let mut tables = std::collections::HashSet::new();
        for contract in &self.contracts {
            for retired in contract.retired_facts {
                let schema_id = retired.id.schema_id();
                if contract.is_core()
                    || retired.id.spiice == "core"
                    || retired.id.spiice != contract.spiice_id
                {
                    return Err(SpiiceRegistryError::RetiredFactOwnership {
                        spiice_id: contract.spiice_id,
                        schema_id,
                        why: "only the owning non-core spiice may retire a Fact",
                    });
                }
                // Memory rows select by schema id, without a separate version.
                if !schemas.insert(schema_id.clone()) {
                    return Err(SpiiceRegistryError::DuplicateRetiredFact { schema_id });
                }
                if self
                    .schemas
                    .iter()
                    .any(|schema| schema.schema_id == schema_id)
                    || self.contracts.iter().any(|active| {
                        active
                            .schemas
                            .iter()
                            .any(|schema| schema.schema_id() == schema_id)
                    })
                {
                    return Err(SpiiceRegistryError::ActiveRetiredFactOverlap { schema_id });
                }
                let namespace = contract.spiice_id.replace('-', "_");
                for surface in retired.surfaces() {
                    if !surface
                        .table
                        .split_once('.')
                        .is_some_and(|(owner, _)| owner == namespace && owner != "proxiima_core")
                    {
                        return Err(SpiiceRegistryError::RetiredFactOwnership {
                            spiice_id: contract.spiice_id,
                            schema_id,
                            why: "inverse tables must be qualified by the owning spiice namespace",
                        });
                    }
                    if self.contracts.iter().any(|active| {
                        active
                            .all_surfaces()
                            .any(|declared| declared.table == surface.table)
                            || active
                                .schemas
                                .iter()
                                .any(|schema| schema.sidecar_table == Some(surface.table))
                    }) {
                        return Err(SpiiceRegistryError::ActiveRetiredFactOverlap { schema_id });
                    }
                    if !tables.insert(surface.table) {
                        return Err(SpiiceRegistryError::RetiredFactInverse {
                            schema_id,
                            table: surface.table,
                            why: "an inverse table must be declared exactly once",
                        });
                    }
                    if !matches!(surface.key, KeyShape::MemoryT { column } if !column.is_empty())
                        || surface.erase != EraseRule::ByKey
                        || surface.owner_column.is_some_and(str::is_empty)
                    {
                        return Err(SpiiceRegistryError::RetiredFactInverse {
                            schema_id,
                            table: surface.table,
                            why: "retirement requires a by-key memory inverse with usable key and owner columns",
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spiice::contract::retired_fact_fixture;
    use crate::spiice::freeze::tests::fixtures::contract;
    use crate::spiice::{RetiredFactContract, SchemaRef, Surface};
    use crate::verbs::schema::PayloadKind;
    use crate::{SchemaVersion, SpiiceRegistryFrozen};

    fn registry(retired: &RetiredFactContract) -> SpiiceRegistry {
        let mut registry = SpiiceRegistry::new();
        let mut declaration = contract(7, &[], &[], &[]);
        declaration.retired_facts = Box::leak(vec![*retired].into_boxed_slice());
        registry
            .try_add_contract(Box::leak(Box::new(declaration)))
            .unwrap();
        registry
    }

    fn frozen() -> SpiiceRegistryFrozen {
        registry(&retired_fact_fixture()).try_freeze().unwrap()
    }

    #[test]
    fn inverse_only_fact_freezes_without_a_payload_or_active_capability() {
        let frozen = frozen();
        let retired = retired_fact_fixture();
        let id = retired.id.schema_id();
        assert_eq!(
            frozen.retired_facts().copied().collect::<Vec<_>>(),
            vec![retired]
        );
        assert!(frozen.lookup(&id, SchemaVersion::new(1)).is_none());
        assert!(frozen.schemas().iter().all(|schema| schema.schema_id != id));
        assert!(
            !frozen
                .listenable_schema_ids()
                .any(|schema| schema == id.as_str())
        );
        assert!(
            frozen
                .search_projections()
                .iter()
                .all(|schema| schema.schema_id != id)
        );
        assert!(
            frozen
                .embed_units()
                .iter()
                .all(|schema| schema.schema_id != id)
        );
        assert!(
            frozen
                .ingest_protocol_payload(
                    &id,
                    SchemaVersion::new(1),
                    PayloadKind::Fact,
                    &serde_json::json!({})
                )
                .is_err()
        );
        assert!(
            frozen
                .contracts()
                .iter()
                .flat_map(|contract| contract.all_surfaces())
                .all(|surface| surface.table != retired.sidecar.table)
        );
        assert!(crate::composed_schema_names(&frozen).contains(&"test_spiice".to_owned()));
    }

    #[test]
    fn duplicate_and_active_retirement_are_refused() {
        let retired = retired_fact_fixture();
        let mut duplicate = registry(&retired);
        let mut second = contract(8, &[], &[], &[]);
        second.retired_facts = Box::leak(vec![retired].into_boxed_slice());
        duplicate
            .try_add_contract(Box::leak(Box::new(second)))
            .unwrap();
        assert!(matches!(
            duplicate.try_freeze(),
            Err(SpiiceRegistryError::DuplicateRetiredFact { .. })
        ));

        let mut overlap = registry(&retired);
        let mut active = crate::spiice::freeze::tests::fixtures::schema(
            retired.id,
            crate::spiice::TransferRule::StaysOnKey,
        );
        active.kind = PayloadKind::Fact;
        let active = contract(8, Box::leak(vec![active].into_boxed_slice()), &[], &[]);
        overlap
            .try_add_contract(Box::leak(Box::new(active)))
            .unwrap();
        assert!(matches!(
            overlap.try_freeze(),
            Err(SpiiceRegistryError::ActiveRetiredFactOverlap { .. })
        ));
    }

    #[test]
    fn foreign_schema_and_foreign_or_core_tables_are_refused() {
        let mut foreign = retired_fact_fixture();
        foreign.id = SchemaRef::new("another-spiice", "retired", 1);
        assert!(matches!(
            registry(&foreign).try_freeze(),
            Err(SpiiceRegistryError::RetiredFactOwnership { .. })
        ));
        for table in [
            "another_spiice.retired_v1",
            "proxiima_core.memory",
            "retired_v1",
        ] {
            let mut retired = retired_fact_fixture();
            retired.sidecar.table = table;
            assert!(matches!(
                registry(&retired).try_freeze(),
                Err(SpiiceRegistryError::RetiredFactOwnership { .. })
            ));
        }
        let mut core = retired_fact_fixture();
        core.id = SchemaRef::new("core", "retired", 1);
        let mut contract = crate::SPIICE_0;
        contract.retired_facts = Box::leak(vec![core].into_boxed_slice());
        let mut core_registry = SpiiceRegistry::new();
        core_registry.contracts[0] = Box::leak(Box::new(contract));
        assert!(matches!(
            core_registry.try_freeze(),
            Err(SpiiceRegistryError::RetiredFactOwnership { .. })
        ));
    }

    #[test]
    fn unusable_or_duplicate_inverse_metadata_is_refused() {
        for sidecar in [
            Surface {
                key: KeyShape::Custom(&[]),
                ..retired_fact_fixture().sidecar
            },
            Surface {
                key: KeyShape::MemoryT { column: "" },
                ..retired_fact_fixture().sidecar
            },
            Surface {
                owner_column: Some(""),
                ..retired_fact_fixture().sidecar
            },
            Surface {
                erase: EraseRule::Never {
                    why: "cannot erase",
                },
                ..retired_fact_fixture().sidecar
            },
        ] {
            let mut retired = retired_fact_fixture();
            retired.sidecar = sidecar;
            assert!(matches!(
                registry(&retired).try_freeze(),
                Err(SpiiceRegistryError::RetiredFactInverse { .. })
            ));
        }
        let mut retired = retired_fact_fixture();
        retired.inverse_surfaces = Box::leak(vec![retired.sidecar].into_boxed_slice());
        assert!(matches!(
            registry(&retired).try_freeze(),
            Err(SpiiceRegistryError::RetiredFactInverse { .. })
        ));
    }
}
