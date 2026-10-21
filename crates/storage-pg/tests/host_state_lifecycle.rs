//! Real-Postgres acceptance coverage for the frozen host lifecycle port.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::MemoryKind;
use proxima_core::flavor::{
    CounterRule, EraseRule, ExportRule, FlavorContract, FlavorDescriptor, FlavorProvenance,
    ForgetRule, KeyShape, ProjectionDecl, Surface, TransferRule,
};
use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::publication::{
    PublicationOriginEligibility, PublicationOriginEligibilityPort,
};
use proxima_core::storage_ports::{
    FactIngestPort, HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope,
    HostStateEraseTableCount, HostStateExportReceipt, HostStateExportRequest, HostStateExportTable,
    HostStateParticipantId, HostStateReply, HostStateRequest, HostStateWritePermit,
    MemoryAuthoringPort, OwnerInversePort, OwnerTransferPort, OwnerWritePermit, StateSurfaceName,
    WriteSessionFactory,
};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, EntityId, GroupId, MemoryId, OwnerId, SchemaId, SchemaVersion, SourceId,
    StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{
    PgHostStateLifecyclePort, PgHostStateParticipant, PgPoolConfig, PgStorage,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::sync::Notify;
use uuid::Uuid;

const FLAVOR_ID: &str = "test-host-lifecycle";
const PARTICIPANT_ID: HostStateParticipantId = HostStateParticipantId::new("host_lifecycle_test");
const FACTS_TABLE: &str = "proxima_core.test_host_lifecycle_facts";
const METADATA_TABLE: &str = "proxima_core.test_host_lifecycle_metadata";
const GENERIC_COUNT_TABLE: &str = "proxima_core.test_host_lifecycle_generic_rows";
const PARTICIPANT_TABLES: &[StateSurfaceName] = &[
    StateSurfaceName::new(FACTS_TABLE),
    StateSurfaceName::new(METADATA_TABLE),
];
const FACT_EXPORT_FIELDS: &[&str] = &["fact_id", "owner_id", "source_id", "payload"];
const ERASE_CORE_DELETE_GATE_LOCK: i64 = 871_922;
const WRONG_PARTICIPANT_TABLES: &[StateSurfaceName] = &[StateSurfaceName::new(
    "proxima_core.test_host_lifecycle_facts",
)];
const UNDECLARED_PARTICIPANT_TABLES: &[StateSurfaceName] = &[
    StateSurfaceName::new(FACTS_TABLE),
    StateSurfaceName::new(METADATA_TABLE),
    StateSurfaceName::new("proxima_core.undeclared_host_state"),
];

const FACTS_SURFACE: Surface = Surface {
    table: FACTS_TABLE,
    key: KeyShape::OwnerId,
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "the host state belongs to the publishing owner",
    },
    erase: EraseRule::HostState {
        whole_owner: proxima_core::flavor::HostStateEraseDisposition::Erase,
        source: proxima_core::flavor::HostStateEraseDisposition::Erase,
        exact_fact: proxima_core::flavor::HostStateEraseDisposition::Erase,
    },
    export: ExportRule::Allowlist(FACT_EXPORT_FIELDS),
    forget: ForgetRule::Keep {
        why: "host state follows owner and source maintenance only",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("host_lifecycle_rows"),
    completeness: None,
};

const METADATA_SURFACE: Surface = Surface {
    table: METADATA_TABLE,
    key: KeyShape::OwnerId,
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "the host state belongs to the publishing owner",
    },
    erase: EraseRule::HostState {
        whole_owner: proxima_core::flavor::HostStateEraseDisposition::Erase,
        source: proxima_core::flavor::HostStateEraseDisposition::Retain,
        exact_fact: proxima_core::flavor::HostStateEraseDisposition::Retain,
    },
    // Erased by the host lifecycle, so the export must carry it or name what
    // it derives from (#478).
    export: ExportRule::DerivedFrom {
        source: "proxima_core.memory",
        why: "internal lifecycle metadata over memories the bundle carries",
    },
    forget: ForgetRule::Keep {
        why: "host state follows owner and source maintenance only",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("host_lifecycle_rows"),
    completeness: None,
};

const GENERIC_COUNT_SURFACE: Surface = Surface {
    table: GENERIC_COUNT_TABLE,
    key: KeyShape::OwnerId,
    owner_column: Some("owner_id"),
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByOwner,
    export: ExportRule::DerivedFrom {
        source: "proxima_core.memory",
        why: "the fixture row exists only to test shared erase counters",
    },
    forget: ForgetRule::Keep {
        why: "the fixture row is unrelated to memory content",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("host_lifecycle_rows"),
    completeness: None,
};

static CONTRACT: FlavorContract = FlavorContract {
    flavor_id: FLAVOR_ID,
    ordinal: 88,
    schemas: &[],
    state_surfaces: &[FACTS_SURFACE, METADATA_SURFACE, GENERIC_COUNT_SURFACE],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the lifecycle fixture does not register a search surface",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

const GENERIC_FACTS_SURFACE: Surface = Surface {
    erase: EraseRule::ByOwner,
    ..FACTS_SURFACE
};

static DUPLICATE_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "test-host-lifecycle-duplicate",
    ordinal: 89,
    state_surfaces: &[FACTS_SURFACE],
    ..CONTRACT
};

static OVERLAP_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "test-host-lifecycle-overlap",
    ordinal: 89,
    state_surfaces: &[GENERIC_FACTS_SURFACE],
    ..CONTRACT
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EraseFault {
    None,
    CallbackError,
    ForeignParticipant,
    ForeignOwner,
    ForeignScope,
    ForeignPhysicalSelection,
    ForeignOriginalOwnerSelection,
    ForeignOriginalFactSelection,
    MissingTable,
    DuplicateTable,
    ForeignTable,
    RetainedCount,
    RetainedScrubbedCount,
    ExactForeignScope,
    ExactRetainedCount,
    ExactRetainedScrubbedCount,
    ScrubFacts,
    DeferredCommitFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFault {
    None,
    ExtraField,
    PersistentWrite,
    ForeignParticipant,
    ForeignOwner,
    MissingTable,
    DuplicateTable,
    ForeignTable,
}

#[derive(Default)]
struct CallbackGate {
    entered: Notify,
    release: Notify,
}

struct Lifecycle {
    participant_id: HostStateParticipantId,
    declared_tables: &'static [StateSurfaceName],
    erase_fault: Mutex<EraseFault>,
    export_fault: Mutex<ExportFault>,
    last_request: Mutex<Option<HostStateEraseRequest>>,
    erase_gate: Mutex<Option<Arc<CallbackGate>>>,
    export_gate: Mutex<Option<Arc<CallbackGate>>>,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            participant_id: PARTICIPANT_ID,
            declared_tables: PARTICIPANT_TABLES,
            erase_fault: Mutex::new(EraseFault::None),
            export_fault: Mutex::new(ExportFault::None),
            last_request: Mutex::new(None),
            erase_gate: Mutex::new(None),
            export_gate: Mutex::new(None),
        }
    }

    fn with_participant_id(participant_id: HostStateParticipantId) -> Self {
        Self {
            participant_id,
            ..Self::new()
        }
    }

    fn with_declared_tables(declared_tables: &'static [StateSurfaceName]) -> Self {
        Self {
            declared_tables,
            ..Self::new()
        }
    }

    fn set_erase_fault(&self, fault: EraseFault) {
        *self.erase_fault.lock().expect("fault mutex") = fault;
    }

    fn set_export_fault(&self, fault: ExportFault) {
        *self.export_fault.lock().expect("fault mutex") = fault;
    }

    fn set_erase_gate(&self, gate: Option<Arc<CallbackGate>>) {
        *self.erase_gate.lock().expect("erase gate mutex") = gate;
    }

    fn set_export_gate(&self, gate: Option<Arc<CallbackGate>>) {
        *self.export_gate.lock().expect("export gate mutex") = gate;
    }
}

#[async_trait::async_trait]
impl PgHostStateLifecyclePort for Lifecycle {
    fn participant_id(&self) -> HostStateParticipantId {
        self.participant_id
    }

    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        self.declared_tables
    }

    async fn erase(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: HostStateEraseRequest,
    ) -> Result<HostStateEraseReceipt, StorageError> {
        *self.last_request.lock().expect("request mutex") = Some(request.clone());
        let fault = *self.erase_fault.lock().expect("fault mutex");
        let owner_id = request.owner().into_inner();
        let selected_ids = request
            .selected_fact_ids()
            .iter()
            .map(|id| id.into_inner())
            .collect::<Vec<_>>();
        let original_copy_owner_ids = request
            .selection()
            .original_copies()
            .iter()
            .map(|locator| locator.original_owner.into_inner())
            .collect::<Vec<_>>();
        let original_copy_fact_ids = request
            .selection()
            .original_copies()
            .iter()
            .map(|locator| locator.fact_id.into_inner())
            .collect::<Vec<_>>();
        let (facts_deleted, facts_scrubbed, metadata_deleted, metadata_scrubbed) = match request
            .scope()
        {
            HostStateEraseScope::WholeOwner => {
                let facts = sqlx::query(
                    "DELETE FROM proxima_core.test_host_lifecycle_facts f
                          WHERE f.owner_id = $1
                             OR f.fact_id = ANY($2)
                             OR EXISTS (
                                  SELECT 1
                                    FROM unnest($3::uuid[], $4::uuid[]) AS copy(owner_id, fact_id)
                                   WHERE copy.owner_id = f.owner_id
                                     AND copy.fact_id = f.fact_id
                             )",
                )
                .bind(owner_id)
                .bind(&selected_ids)
                .bind(&original_copy_owner_ids)
                .bind(&original_copy_fact_ids)
                .execute(&mut **tx)
                .await
                .map_err(|error| pg_error(&error))?
                .rows_affected();
                let metadata = sqlx::query(
                    "DELETE FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
                )
                .bind(owner_id)
                .execute(&mut **tx)
                .await
                .map_err(|error| pg_error(&error))?
                .rows_affected();
                (facts, 0, metadata, 0)
            }
            HostStateEraseScope::Source(source) => {
                let (facts, scrubbed_facts) = if fault == EraseFault::ScrubFacts {
                    let scrubbed = sqlx::query(
                        "UPDATE proxima_core.test_host_lifecycle_facts f
                            SET payload = decode('', 'hex')
                          WHERE (f.owner_id = $1 AND f.source_id = $2)
                             OR f.fact_id = ANY($3)
                             OR EXISTS (
                                  SELECT 1
                                    FROM unnest($4::uuid[], $5::uuid[]) AS copy(owner_id, fact_id)
                                   WHERE copy.owner_id = f.owner_id
                                     AND copy.fact_id = f.fact_id
                             )",
                    )
                    .bind(owner_id)
                    .bind(source.as_str())
                    .bind(&selected_ids)
                    .bind(&original_copy_owner_ids)
                    .bind(&original_copy_fact_ids)
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| pg_error(&error))?
                    .rows_affected();
                    (0, scrubbed)
                } else {
                    let deleted = sqlx::query(
                            "DELETE FROM proxima_core.test_host_lifecycle_facts f
                              WHERE (f.owner_id = $1 AND f.source_id = $2)
                                 OR f.fact_id = ANY($3)
                                 OR EXISTS (
                                      SELECT 1
                                        FROM unnest($4::uuid[], $5::uuid[]) AS copy(owner_id, fact_id)
                                       WHERE copy.owner_id = f.owner_id
                                         AND copy.fact_id = f.fact_id
                                 )",
                        )
                        .bind(owner_id)
                        .bind(source.as_str())
                        .bind(&selected_ids)
                        .bind(&original_copy_owner_ids)
                        .bind(&original_copy_fact_ids)
                        .execute(&mut **tx)
                        .await
                        .map_err(|error| pg_error(&error))?
                        .rows_affected();
                    (deleted, 0)
                };
                let metadata = if fault == EraseFault::RetainedCount {
                    sqlx::query(
                        "DELETE FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
                    )
                    .bind(owner_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| pg_error(&error))?
                    .rows_affected()
                } else {
                    0
                };
                let scrubbed_metadata = if fault == EraseFault::RetainedScrubbedCount {
                    sqlx::query(
                        "UPDATE proxima_core.test_host_lifecycle_metadata
                            SET payload = decode('', 'hex')
                          WHERE owner_id = $1 AND source_id = $2",
                    )
                    .bind(owner_id)
                    .bind(source.as_str())
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| pg_error(&error))?
                    .rows_affected()
                } else {
                    0
                };
                (facts, scrubbed_facts, metadata, scrubbed_metadata)
            }
            HostStateEraseScope::ExactFacts => {
                let facts = sqlx::query(
                    "DELETE FROM proxima_core.test_host_lifecycle_facts
                          WHERE fact_id = ANY($1)",
                )
                .bind(&selected_ids)
                .execute(&mut **tx)
                .await
                .map_err(|error| pg_error(&error))?
                .rows_affected();
                let metadata_deleted = if fault == EraseFault::ExactRetainedCount {
                    sqlx::query(
                        "DELETE FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
                    )
                    .bind(owner_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| pg_error(&error))?
                    .rows_affected()
                } else {
                    0
                };
                let metadata_scrubbed = if fault == EraseFault::ExactRetainedScrubbedCount {
                    sqlx::query(
                        "UPDATE proxima_core.test_host_lifecycle_metadata
                            SET payload = decode('', 'hex')
                          WHERE owner_id = $1",
                    )
                    .bind(owner_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| pg_error(&error))?
                    .rows_affected()
                } else {
                    0
                };
                (facts, 0, metadata_deleted, metadata_scrubbed)
            }
        };
        if fault == EraseFault::DeferredCommitFailure {
            sqlx::query(
                "INSERT INTO proxima_core.test_host_lifecycle_facts
                    (fact_id, owner_id, source_id, payload)
                 VALUES ($1, $2, 'invalid', decode('01', 'hex'))",
            )
            .bind(Uuid::now_v7())
            .bind(Uuid::now_v7())
            .execute(&mut **tx)
            .await
            .map_err(|error| pg_error(&error))?;
        }
        let gate = self.erase_gate.lock().expect("erase gate mutex").clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if fault == EraseFault::CallbackError {
            return Err(StorageError::Unavailable("test callback failure".into()));
        }

        let mut receipt = HostStateEraseReceipt {
            participant: self.participant_id,
            owner: request.owner(),
            scope: request.scope().clone(),
            selection: request.selection().clone(),
            counts: vec![
                HostStateEraseTableCount {
                    table: StateSurfaceName::new(FACTS_TABLE),
                    deleted: facts_deleted,
                    scrubbed: facts_scrubbed,
                },
                HostStateEraseTableCount {
                    table: StateSurfaceName::new(METADATA_TABLE),
                    deleted: metadata_deleted,
                    scrubbed: metadata_scrubbed,
                },
            ],
        };
        match fault {
            EraseFault::ForeignParticipant => {
                receipt.participant = HostStateParticipantId::new("wrong_participant");
            }
            EraseFault::ForeignOwner => {
                receipt.owner = OwnerId::new(Uuid::now_v7());
            }
            EraseFault::ForeignScope | EraseFault::ExactForeignScope => {
                receipt.scope = HostStateEraseScope::WholeOwner;
            }
            EraseFault::ForeignPhysicalSelection => {
                receipt.selection = proxima_core::storage_ports::HostStateEraseSelection::new(
                    vec![MemoryId::new(Uuid::now_v7())],
                    request.selection().original_copies().to_vec(),
                );
            }
            EraseFault::ForeignOriginalOwnerSelection => {
                let mut copies = request.selection().original_copies().to_vec();
                if let Some(locator) = copies.first_mut() {
                    locator.original_owner = OwnerId::new(Uuid::now_v7());
                }
                receipt.selection = proxima_core::storage_ports::HostStateEraseSelection::new(
                    request.selection().physical_facts().to_vec(),
                    copies,
                );
            }
            EraseFault::ForeignOriginalFactSelection => {
                let mut copies = request.selection().original_copies().to_vec();
                if let Some(locator) = copies.first_mut() {
                    locator.fact_id = MemoryId::new(Uuid::now_v7());
                }
                receipt.selection = proxima_core::storage_ports::HostStateEraseSelection::new(
                    request.selection().physical_facts().to_vec(),
                    copies,
                );
            }
            EraseFault::MissingTable => {
                receipt.counts.pop();
            }
            EraseFault::DuplicateTable => receipt.counts.push(receipt.counts[0].clone()),
            EraseFault::ForeignTable => {
                receipt.counts[1].table = StateSurfaceName::new("proxima_core.unknown_table");
            }
            EraseFault::RetainedCount | EraseFault::ExactRetainedCount => {
                receipt.counts[1].deleted = metadata_deleted;
            }
            EraseFault::ExactRetainedScrubbedCount => {
                receipt.counts[1].scrubbed = metadata_scrubbed;
            }
            EraseFault::None
            | EraseFault::ScrubFacts
            | EraseFault::RetainedScrubbedCount
            | EraseFault::CallbackError
            | EraseFault::DeferredCommitFailure => {}
        }
        Ok(receipt)
    }

    async fn export(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: HostStateExportRequest,
    ) -> Result<HostStateExportReceipt, StorageError> {
        let fault = *self.export_fault.lock().expect("export fault mutex");
        if fault == ExportFault::PersistentWrite {
            sqlx::query(
                "INSERT INTO proxima_core.test_host_lifecycle_facts
                    (fact_id, owner_id, source_id, payload)
                 VALUES ($1, $2, 'export-write', decode('01', 'hex'))",
            )
            .bind(Uuid::now_v7())
            .bind(request.owner().into_inner())
            .execute(&mut **tx)
            .await
            .map_err(|error| pg_error(&error))?;
        }
        let mut tables = Vec::new();
        for name in request.included_tables() {
            if name.as_str() != FACTS_TABLE {
                return Err(StorageError::Internal("unexpected included table".into()));
            }
            let gate = self.export_gate.lock().expect("export gate mutex").clone();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            let mut rows: Vec<Value> = sqlx::query_scalar(
                "SELECT jsonb_build_object(
                    'fact_id', fact_id,
                    'owner_id', owner_id,
                    'source_id', source_id,
                    'payload', payload
                 )
                   FROM proxima_core.test_host_lifecycle_facts
                  WHERE owner_id = $1
                  ORDER BY fact_id",
            )
            .bind(request.owner().into_inner())
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| pg_error(&error))?;
            if fault == ExportFault::ExtraField {
                rows.push(json!({
                    "fact_id": Uuid::now_v7(),
                    "owner_id": request.owner().into_inner(),
                    "source_id": "extra",
                    "payload": "\\x01",
                    "storage_secret": "must-not-escape"
                }));
            }
            tables.push(HostStateExportTable { table: *name, rows });
        }
        let mut receipt = HostStateExportReceipt {
            participant: self.participant_id,
            owner: request.owner(),
            tables,
        };
        match fault {
            ExportFault::ForeignParticipant => {
                receipt.participant = HostStateParticipantId::new("wrong_export_participant");
            }
            ExportFault::ForeignOwner => {
                receipt.owner = OwnerId::new(Uuid::now_v7());
            }
            ExportFault::MissingTable => {
                receipt.tables.pop();
            }
            ExportFault::DuplicateTable => {
                receipt.tables.push(receipt.tables[0].clone());
            }
            ExportFault::ForeignTable => {
                receipt.tables[0].table = StateSurfaceName::new("proxima_core.unknown_table");
            }
            ExportFault::None | ExportFault::ExtraField | ExportFault::PersistentWrite => {}
        }
        Ok(receipt)
    }
}

struct HostParticipant {
    lifecycle: Arc<Lifecycle>,
    lifecycle_enabled: bool,
}

#[async_trait::async_trait]
impl PgHostStateParticipant for HostParticipant {
    fn participant_id(&self) -> HostStateParticipantId {
        PARTICIPANT_ID
    }

    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        PARTICIPANT_TABLES
    }

    fn lifecycle_port(&self) -> Option<Arc<dyn PgHostStateLifecyclePort>> {
        self.lifecycle_enabled
            .then(|| self.lifecycle.clone() as Arc<dyn PgHostStateLifecyclePort>)
    }

    async fn apply(
        &self,
        _session: &mut proxima_storage_pg::PgHostStateSession<'_, '_>,
        _permit: &HostStateWritePermit,
        _request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        Err(StorageError::Internal(
            "unused lifecycle fixture participant".into(),
        ))
    }
}

struct TableParticipant {
    lifecycle: Arc<Lifecycle>,
    tables: &'static [StateSurfaceName],
}

#[async_trait::async_trait]
impl PgHostStateParticipant for TableParticipant {
    fn participant_id(&self) -> HostStateParticipantId {
        PARTICIPANT_ID
    }

    fn declared_tables(&self) -> &'static [StateSurfaceName] {
        self.tables
    }

    fn lifecycle_port(&self) -> Option<Arc<dyn PgHostStateLifecyclePort>> {
        Some(self.lifecycle.clone())
    }

    async fn apply(
        &self,
        _session: &mut proxima_storage_pg::PgHostStateSession<'_, '_>,
        _permit: &HostStateWritePermit,
        _request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        Err(StorageError::Internal("unused test participant".into()))
    }
}

fn frozen_registry_with(
    extra: Option<&'static FlavorContract>,
) -> proxima_core::FlavorRegistryFrozen {
    let mut registry = proxima_core::FlavorRegistry::new();
    registry
        .try_add_flavor(FlavorDescriptor {
            flavor_id: FLAVOR_ID.to_owned(),
            display_name: "Host lifecycle test".to_owned(),
            package_version: "0.1.0".to_owned(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        })
        .expect("test descriptor accepted");
    registry
        .try_add_contract(&CONTRACT)
        .expect("test contract accepted");
    if let Some(contract) = extra {
        registry
            .try_add_flavor(FlavorDescriptor {
                flavor_id: contract.flavor_id.to_owned(),
                display_name: "Extra host lifecycle test".to_owned(),
                package_version: "0.1.0".to_owned(),
                author: None,
                provenance: FlavorProvenance::Builtin,
            })
            .expect("extra test descriptor accepted");
        registry
            .try_add_contract(contract)
            .expect("extra test contract accepted");
    }
    registry.try_freeze().expect("test registry freezes")
}

fn frozen_registry() -> proxima_core::FlavorRegistryFrozen {
    frozen_registry_with(None)
}

fn pg_error(error: &sqlx::Error) -> StorageError {
    StorageError::Internal(error.to_string())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

async fn fresh_pg(max_connections: u32) -> (String, Arc<PgStorage>, Arc<Lifecycle>) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_core_db(&db_name)
        .await
        .expect("PG required for host lifecycle tests");
    let _config = PgPoolConfig {
        max_connections,
        ..PgPoolConfig::default()
    };
    let lifecycle = Arc::new(Lifecycle::new());
    let registry = frozen_registry();
    let pg = PgStorage::connect_for_tests(db_url(&db_name))
        .await
        .expect("connect")
        .with_host_state_participant(Arc::new(HostParticipant {
            lifecycle: lifecycle.clone(),
            lifecycle_enabled: true,
        }))
        .try_with_flavors(&registry)
        .expect("managed descriptor and callback are exact");
    let pool = pg.pool_for_tests();
    sqlx::raw_sql(
        "CREATE TABLE proxima_core.test_host_lifecycle_facts (
             fact_id uuid NOT NULL,
             owner_id uuid NOT NULL,
             source_id text NOT NULL,
             payload bytea NOT NULL,
             storage_secret text NOT NULL DEFAULT 'private',
             CONSTRAINT test_host_lifecycle_owner_fk
                 FOREIGN KEY (owner_id) REFERENCES proxima_core.owners(owner_id)
                 DEFERRABLE INITIALLY DEFERRED
         );
         CREATE TABLE proxima_core.test_host_lifecycle_metadata (
             owner_id uuid NOT NULL REFERENCES proxima_core.owners(owner_id),
             source_id text NOT NULL DEFAULT 'source-a',
             metadata text NOT NULL,
             payload bytea NOT NULL DEFAULT decode('aa', 'hex')
         );
         CREATE TABLE proxima_core.test_host_lifecycle_generic_rows (
             owner_id uuid NOT NULL REFERENCES proxima_core.owners(owner_id),
             value text NOT NULL
         );",
    )
    .execute(pool)
    .await
    .expect("test host tables created");
    sqlx::raw_sql(
        "GRANT USAGE ON SCHEMA proxima_core TO proxima_test_runtime, proxima_test_platform;
         GRANT SELECT, INSERT, UPDATE, DELETE ON
           proxima_core.test_host_lifecycle_facts,
           proxima_core.test_host_lifecycle_metadata,
           proxima_core.test_host_lifecycle_generic_rows
           TO proxima_test_runtime, proxima_test_platform;",
    )
    .execute(pool)
    .await
    .expect("runtime role can inspect host lifecycle tables");
    sqlx::raw_sql(
        "ALTER TABLE proxima_core.test_host_lifecycle_facts OWNER TO proxima_test_platform;
         ALTER TABLE proxima_core.test_host_lifecycle_metadata OWNER TO proxima_test_platform;
         ALTER TABLE proxima_core.test_host_lifecycle_generic_rows OWNER TO proxima_test_platform;",
    )
    .execute(pool)
    .await
    .expect("platform role owns host lifecycle tables");
    sqlx::raw_sql(
        "ALTER TABLE proxima_core.test_host_lifecycle_facts ENABLE ROW LEVEL SECURITY;
         ALTER TABLE proxima_core.test_host_lifecycle_facts FORCE ROW LEVEL SECURITY;
         ALTER TABLE proxima_core.test_host_lifecycle_metadata ENABLE ROW LEVEL SECURITY;
         ALTER TABLE proxima_core.test_host_lifecycle_metadata FORCE ROW LEVEL SECURITY;
         ALTER TABLE proxima_core.test_host_lifecycle_generic_rows ENABLE ROW LEVEL SECURITY;
         ALTER TABLE proxima_core.test_host_lifecycle_generic_rows FORCE ROW LEVEL SECURITY;
         DO $$ DECLARE table_name text; BEGIN
           FOREACH table_name IN ARRAY ARRAY['test_host_lifecycle_facts', 'test_host_lifecycle_metadata', 'test_host_lifecycle_generic_rows'] LOOP
             EXECUTE format('CREATE POLICY proxima_owner_read ON proxima_core.%I FOR SELECT TO proxima_test_runtime USING (owner_id::text = current_setting(''app.owner'', true))', table_name);
             EXECUTE format('CREATE POLICY proxima_owner_write ON proxima_core.%I FOR ALL TO proxima_test_runtime USING (owner_id::text = current_setting(''app.owner'', true)) WITH CHECK (owner_id::text = current_setting(''app.owner'', true))', table_name);
             EXECUTE format('CREATE POLICY proxima_platform ON proxima_core.%I FOR ALL TO proxima_test_platform USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', table_name);
           END LOOP;
         END $$;",
    )
    .execute(pool)
    .await
    .expect("host lifecycle tables enforce RLS");
    (db_name, Arc::new(pg), lifecycle)
}

async fn close_pg(db_name: &str) {
    drop_db(db_name).await.expect("drop test database");
}

fn draft(source_id: &str, ingest_key: &str) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("host-lifecycle/test-fact".to_owned()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: Some(source_id.to_owned()),
        ingest_key: Some(ingest_key.to_owned()),
        payload: Vec::new(),
        rendered_text: Some(ingest_key.to_owned()),
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: MemoryKind::Fact,
        request: None,
    }
}

async fn ingest(pg: &PgStorage, owner: OwnerId, source: &str, key: &str) -> MemoryId {
    pg.ingest_fact_atomic(
        &OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
        &draft(source, key),
        None,
    )
    .await
    .expect("Fact admitted")
    .memory_id
}

async fn seed_host_fact(
    pool: &PgPool,
    owner: OwnerId,
    fact_id: MemoryId,
    source: &str,
    payload: &[u8],
) {
    sqlx::query(
        "INSERT INTO proxima_core.test_host_lifecycle_facts
            (fact_id, owner_id, source_id, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(fact_id.into_inner())
    .bind(owner.into_inner())
    .bind(source)
    .bind(payload)
    .execute(pool)
    .await
    .expect("host Fact row seeded");
}

async fn seed_metadata(pool: &PgPool, owner: OwnerId) {
    seed_metadata_for_source(pool, owner, "source-a").await;
}

async fn seed_metadata_for_source(pool: &PgPool, owner: OwnerId, source: &str) {
    sqlx::query(
        "INSERT INTO proxima_core.test_host_lifecycle_metadata (owner_id, source_id, metadata)
         VALUES ($1, $2, 'opaque')",
    )
    .bind(owner.into_inner())
    .bind(source)
    .execute(pool)
    .await
    .expect("host metadata row seeded");
}

async fn seed_generic_counter_row(pool: &PgPool, owner: OwnerId) {
    sqlx::query(
        "INSERT INTO proxima_core.test_host_lifecycle_generic_rows (owner_id, value)
         VALUES ($1, 'generic')",
    )
    .bind(owner.into_inner())
    .execute(pool)
    .await
    .expect("generic counter row seeded");
}

/// A user owner's erase authorization, carrying the verified drop proof.
fn erase_auth(owner: OwnerId, source: Option<SourceId>) -> EraseAuthorization {
    let user_id = owner.as_user();
    match source {
        None => EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "test-drop".into(),
        }),
        Some(source_id) => {
            EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                user_id,
                source_id,
                drop_event_id: "test-drop".into(),
            })
        }
    }
}

/// A group owner's erase authorization: no drop proof, only abandonment.
fn group_erase_auth(owner: OwnerId) -> EraseAuthorization {
    EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner {
        group_id: owner.as_group(),
    })
}

/// Register `owner` as a group, as group creation does in production.
async fn register_group(pool: &PgPool, owner: OwnerId) {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'group'::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner.into_inner())
    .execute(pool)
    .await
    .expect("group registers");
}

async fn host_fact_count(pool: &PgPool, owner: OwnerId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.test_host_lifecycle_facts WHERE owner_id = $1",
    )
    .bind(owner.into_inner())
    .fetch_one(pool)
    .await
    .expect("host row count")
}

async fn core_fact_count(pool: &PgPool, fact_id: MemoryId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE t = $1 AND kind = 'fact'")
        .bind(fact_id.into_inner())
        .fetch_one(pool)
        .await
        .expect("core Fact count")
}

async fn publication_origin_count(pool: &PgPool, fact_id: MemoryId) -> i64 {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.publication_origin WHERE t = $1")
        .bind(fact_id.into_inner())
        .fetch_one(pool)
        .await
        .expect("publication origin count")
}

#[tokio::test]
async fn source_erase_binds_exact_hot_and_cooled_fact_selection_and_owner_kind() {
    let (db_name, pg, lifecycle) = fresh_pg(5).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let source_a = SourceId::new("source-a");
        let source_b = SourceId::new("source-b");
        let hot_a = ingest(&pg, owner, source_a.as_str(), "hot-a").await;
        let cold_a = ingest(&pg, owner, source_a.as_str(), "cold-a").await;
        let hot_b = ingest(&pg, owner, source_b.as_str(), "hot-b").await;
        let other_a = ingest(&pg, other_owner, source_a.as_str(), "other-a").await;
        MemoryAuthoringPort::forget_memory(
            pg.as_ref(),
            &OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            cold_a,
        )
        .await?;
        for (target_owner, id, source, payload) in [
            (owner, hot_a, source_a.as_str(), &[0, 1, 255][..]),
            (owner, cold_a, source_a.as_str(), &[2, 3, 254][..]),
            (owner, hot_b, source_b.as_str(), &[4, 5][..]),
            (other_owner, other_a, source_a.as_str(), &[6, 7][..]),
        ] {
            seed_host_fact(pool, target_owner, id, source, payload).await;
        }
        seed_metadata(pool, owner).await;
        seed_metadata(pool, other_owner).await;

        let auth = erase_auth(owner, Some(source_a.clone()));
        let OwnerEraseOutcome::Completed {
            counts,
            host_state_deleted,
            host_state_scrubbed,
            ..
        } = pg
            .erase_personal_source_scope(&auth, owner.as_user(), &source_a, pg.surfaces())
            .await?
        else {
            panic!("source erase should complete");
        };
        let request = lifecycle
            .last_request
            .lock()
            .expect("request mutex")
            .clone()
            .expect("callback received a request");
        let mut actual_ids = request.selected_fact_ids().to_vec();
        actual_ids.sort_by_key(|id| id.into_inner());
        let mut expected_ids = vec![hot_a, cold_a];
        expected_ids.sort_by_key(|id| id.into_inner());
        assert_eq!(request.participant(), PARTICIPANT_ID);
        assert_eq!(request.owner(), owner);
        assert_eq!(request.scope(), &HostStateEraseScope::Source(source_a));
        assert_eq!(actual_ids, expected_ids, "hot ∪ cooled Fact ids are exact");
        assert_eq!(host_state_deleted.get(FACTS_TABLE), Some(&2));
        assert_eq!(host_state_deleted.get(METADATA_TABLE), Some(&0));
        assert_eq!(host_state_scrubbed.get(FACTS_TABLE), Some(&0));
        assert_eq!(host_state_scrubbed.get(METADATA_TABLE), Some(&0));
        assert_eq!(counts.get("host_lifecycle_rows"), 2);
        assert_eq!(host_fact_count(pool, owner).await, 1, "source B survives");
        assert_eq!(
            host_fact_count(pool, other_owner).await,
            1,
            "same source under another owner survives"
        );
        assert_eq!(core_fact_count(pool, hot_a).await, 0);
        assert_eq!(core_fact_count(pool, cold_a).await, 0);
        assert_eq!(core_fact_count(pool, hot_b).await, 1);
        assert_eq!(core_fact_count(pool, other_a).await, 1);

        let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
        register_group(pool, group).await;
        let group_fact = ingest(&pg, group, "group-source", "group-fact").await;
        seed_host_fact(pool, group, group_fact, "group-source", &[9, 8, 7]).await;
        seed_metadata(pool, group).await;
        seed_generic_counter_row(pool, group).await;
        let group_export_auth = ExportAuthorization::new_for_tests(OwnerExportTarget::GroupOwner {
            group_id: group.as_group(),
        });
        let group_bundle = pg
            .export_owner_bundle(&group_export_auth, pg.surfaces())
            .await?;
        assert_eq!(group_bundle.owner, group);
        assert_eq!(group_bundle.tables[FACTS_TABLE].len(), 1);
        assert!(!group_bundle.tables.contains_key(METADATA_TABLE));
        let auth = group_erase_auth(group);
        let OwnerEraseOutcome::Completed {
            counts,
            host_state_deleted,
            ..
        } = pg
            .erase_group_owner(&auth, group.as_group(), pg.surfaces())
            .await?
        else {
            panic!("group erase should complete");
        };
        assert_eq!(host_state_deleted.get(FACTS_TABLE), Some(&1));
        assert_eq!(host_state_deleted.get(METADATA_TABLE), Some(&1));
        assert_eq!(counts.get("host_lifecycle_rows"), 3);
        assert_eq!(host_fact_count(pool, group).await, 0);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("host lifecycle source and group erase oracle failed");
}

#[tokio::test]
async fn source_erase_combines_physical_and_original_copy_selectors_once() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let original_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let transferred_owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let unrelated_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let source = SourceId::new("combined-source");
        let both = ingest(&pg, original_owner, source.as_str(), "both-selected").await;
        let original_only =
            ingest(&pg, original_owner, source.as_str(), "original-only").await;
        let unrelated = ingest(&pg, unrelated_owner, source.as_str(), "neither-selected").await;
        assert!(
            pg.transfer_to_owner(
                &OwnerWritePermit::transfer_for_tests(
                    original_owner,
                    AccessKind::Fact,
                    transferred_owner,
                ),
                EntityId::Memory(original_only),
                transferred_owner,
                pg.surfaces(),
                &[],
            )
            .await?,
            "the original-only selector is a transferred Fact"
        );
        // The lifecycle fixture's facts are ordinary Proxima facts, not
        // published admissions. Seed the payload-free origin index directly
        // to exercise both copy-selection domains; publication_outbox tests
        // cover production capture into this same index.
        sqlx::query(
            "INSERT INTO proxima_core.publication_origin
                 (t, original_owner_id, source_id)
             VALUES
                 ($1, $2, $4),
                 ($3, $2, $4)",
        )
        .bind(both.into_inner())
        .bind(original_owner.into_inner())
        .bind(original_only.into_inner())
        .bind(source.as_str())
        .execute(pool)
        .await?;

        // The current owner's `both` row is selected by both sets. The
        // transferred `original_only` row is selected only by its immutable
        // original-owner locator. A second owner's row for each Fact proves
        // the physical selector is global while the copy locator is exact.
        for (owner, fact, row_source, payload) in [
            (original_owner, both, source.as_str(), &[1][..]),
            (transferred_owner, both, "other-source", &[2][..]),
            (original_owner, original_only, source.as_str(), &[3][..]),
            (transferred_owner, original_only, source.as_str(), &[4][..]),
            (unrelated_owner, unrelated, source.as_str(), &[5][..]),
        ] {
            seed_host_fact(pool, owner, fact, row_source, payload).await;
        }

        let auth = erase_auth(original_owner, Some(source.clone()));
        let OwnerEraseOutcome::Completed {
            host_state_deleted, ..
        } = pg
            .erase_personal_source_scope(
                &auth,
                original_owner.as_user(),
                &source,
                pg.surfaces(),
            )
            .await?
        else {
            panic!("source erase should complete");
        };

        let request = lifecycle
            .last_request
            .lock()
            .expect("request mutex")
            .clone()
            .expect("callback received a request");
        assert_eq!(request.selected_fact_ids(), &[both]);
        assert_eq!(
            request.selection().original_copies(),
            &[
                proxima_core::storage_ports::HostStateFactCopyLocator {
                    original_owner,
                    fact_id: both,
                },
                proxima_core::storage_ports::HostStateFactCopyLocator {
                    original_owner,
                    fact_id: original_only,
                },
            ]
        );
        assert_eq!(
            host_state_deleted.get(FACTS_TABLE),
            Some(&3),
            "both-selected row is counted once; only two physical rows plus one original copy are removed"
        );

        let remaining: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT owner_id, fact_id
               FROM proxima_core.test_host_lifecycle_facts
              ORDER BY owner_id, fact_id",
        )
        .fetch_all(pool)
        .await?;
        let mut expected = vec![
            (transferred_owner.into_inner(), original_only.into_inner()),
            (unrelated_owner.into_inner(), unrelated.into_inner()),
        ];
        expected.sort_unstable();
        assert_eq!(remaining, expected, "the OR inverse preserves neither-selected rows");
        assert_eq!(core_fact_count(pool, both).await, 0);
        assert_eq!(core_fact_count(pool, original_only).await, 1);
        assert_eq!(core_fact_count(pool, unrelated).await, 1);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("combined physical/original-copy selector oracle failed");
}

#[tokio::test]
async fn source_erase_exclusive_fence_makes_waiting_origin_check_see_revocation() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests().clone();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let source = SourceId::new("exclusive-source-first");
        let fact = ingest(&pg, owner, source.as_str(), "exclusive-source-fact").await;
        sqlx::query(
            "INSERT INTO proxima_core.publication_origin
                 (t, original_owner_id, source_id)
             VALUES ($1, $2, $3)",
        )
        .bind(fact.into_inner())
        .bind(owner.into_inner())
        .bind(source.as_str())
        .execute(&pool)
        .await?;

        let gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_gate(Some(gate.clone()));
        let erase_pg = pg.clone();
        let erase_source = source.clone();
        let auth = erase_auth(owner, Some(source.clone()));
        let eraser = tokio::spawn(async move {
            OwnerInversePort::erase_personal_source_scope(
                erase_pg.as_ref(),
                &auth,
                user_id,
                &erase_source,
                erase_pg.surfaces(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("source callback entered under the exclusive lifecycle fence");

        let reader_pg = pg.clone();
        let reader_pool = pool.clone();
        let reader = tokio::spawn(async move {
            let mut tx = reader_pool.begin().await.expect("begin waiting UoW");
            let eligibility = PublicationOriginEligibilityPort::check_in_transaction(
                reader_pg.as_ref(),
                &mut tx,
                owner,
                fact,
            )
            .await
            .expect("revoked origin is a negative result");
            tx.commit().await.expect("eligibility UoW commits");
            eligibility
        });

        // The erase holds the origin's target fence; the late check queues on
        // it and, once the erase commits, finds the origin revoked.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (
                         SELECT 1 FROM pg_locks
                          WHERE locktype = 'advisory' AND NOT granted
                            AND mode = 'ExclusiveLock'
                            AND database = (
                                SELECT oid FROM pg_database WHERE datname = current_database()
                            )
                     )",
                )
                .fetch_one(&pool)
                .await?;
                if waiting {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok::<(), sqlx::Error>(())
        })
        .await
        .expect("late eligibility transaction waits behind erase")?;

        gate.release.notify_one();
        let outcome = tokio::time::timeout(Duration::from_secs(3), eraser)
            .await
            .expect("source erase completes within the bound")??;
        assert!(matches!(outcome, OwnerEraseOutcome::Completed { .. }));
        let eligibility = tokio::time::timeout(Duration::from_secs(3), reader)
            .await
            .expect("waiting eligibility transaction completes")?;
        assert_eq!(eligibility, PublicationOriginEligibility::Ineligible);
        assert_eq!(core_fact_count(&pool, fact).await, 0);
        assert_eq!(host_fact_count(&pool, owner).await, 0);
        assert_eq!(publication_origin_count(&pool, fact).await, 0);
        lifecycle.set_erase_gate(None);
        Ok(())
    }
    .await;
    lifecycle.set_erase_gate(None);
    close_pg(&db_name).await;
    result.expect("exclusive source erase must precede late eligibility");
}

/// An owner's erase fences that owner's writes and no other owner's (#491).
///
/// The erase of A is held inside its host callback, with every lock it takes
/// still held. A write of B, through a unit of work and through the direct
/// admission path, commits meanwhile; a write of A waits for the erase.
#[tokio::test]
async fn an_erase_fences_its_own_owner_and_no_other() {
    let (db_name, pg, lifecycle) = fresh_pg(6).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let erased = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fact = ingest(&pg, erased, "source-a", "erased-fact").await;
        seed_host_fact(pool, erased, fact, "source-a", &[1]).await;
        ingest(&pg, other, "source-b", "other-first").await;

        let gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_gate(Some(gate.clone()));
        let erase_pg = pg.clone();
        let user_id = erased.as_user();
        let erase = tokio::spawn(async move {
            let auth = erase_auth(erased, None);
            erase_pg
                .erase_personal_owner(&auth, user_id, erase_pg.surfaces())
                .await
        });
        gate.entered.notified().await;

        let scope = owner_scope_for_tests(&[other]);
        let session = tokio::time::timeout(
            Duration::from_secs(2),
            WriteSessionFactory::begin(pg.as_ref(), Some(&scope)),
        )
        .await
        .expect("another owner's unit begins during the erase")?;
        tokio::time::timeout(
            Duration::from_secs(2),
            ingest(&pg, other, "source-b", "other-during-erase"),
        )
        .await
        .expect("another owner's write commits during the erase");
        session.commit().await?;

        let waiting_pg = pg.clone();
        let waiting = tokio::spawn(async move {
            ingest(&waiting_pg, erased, "source-a", "erased-owner-during-erase").await
        });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "the erased owner's write waits at its owner fence"
        );
        assert!(!waiting.is_finished());

        gate.release.notify_one();
        assert!(matches!(erase.await??, OwnerEraseOutcome::Completed { .. }));
        lifecycle.set_erase_gate(None);
        waiting.await?;
        assert_eq!(host_fact_count(pool, erased).await, 0);
        Ok(())
    }
    .await;
    lifecycle.set_erase_gate(None);
    close_pg(&db_name).await;
    result.expect("an erase fences its own owner and no other");
}

#[tokio::test]
async fn exact_physical_erase_invokes_global_copy_inverse_only_for_selected_fact() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let source = SourceId::new("exact-physical-source");
        let selected = ingest(&pg, owner, source.as_str(), "exact-selected").await;
        let unrelated = ingest(&pg, owner, source.as_str(), "exact-unrelated").await;
        let other_owner_fact =
            ingest(&pg, other_owner, "other-source", "register-other-owner").await;
        seed_host_fact(pool, owner, selected, source.as_str(), &[1]).await;
        seed_host_fact(pool, other_owner, selected, "foreign-copy", &[2]).await;
        seed_host_fact(pool, owner, unrelated, source.as_str(), &[3]).await;
        seed_metadata(pool, owner).await;

        let context = pg
            .host_state_erase_context()
            .expect("boot-frozen lifecycle callback is present");
        let mut tx = pool.begin().await?;
        proxima_storage_pg::verbs::forget::erase_memory(
            &mut tx,
            pg.sidecars(),
            &context,
            &owner,
            selected.into_inner(),
        )
        .await?;
        tx.commit().await?;

        let request = lifecycle
            .last_request
            .lock()
            .expect("request mutex")
            .clone()
            .expect("physical erase invokes callback");
        assert_eq!(request.owner(), owner);
        assert_eq!(request.scope(), &HostStateEraseScope::ExactFacts);
        assert_eq!(request.selected_fact_ids(), &[selected]);
        assert!(request.selection().original_copies().is_empty());
        assert_eq!(core_fact_count(pool, selected).await, 0);
        assert_eq!(core_fact_count(pool, unrelated).await, 1);
        assert_eq!(core_fact_count(pool, other_owner_fact).await, 1);

        let remaining: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT owner_id, fact_id
               FROM proxima_core.test_host_lifecycle_facts
              ORDER BY owner_id, fact_id",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(
            remaining,
            vec![(owner.into_inner(), unrelated.into_inner())],
            "physical selection removes every owner copy of the target and preserves other Facts"
        );
        let metadata_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
        )
        .bind(owner.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            metadata_count, 1,
            "independent authored metadata is retained"
        );
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("exact physical Fact inverse oracle failed");
}

#[tokio::test]
async fn exact_receipt_scope_and_retained_counts_fail_closed_and_wrong_owner_never_dispatches() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let wrong_owner = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let source = SourceId::new("exact-receipt-source");
        let selected = ingest(&pg, owner, source.as_str(), "exact-receipt-selected").await;
        seed_host_fact(pool, owner, selected, source.as_str(), &[1, 2, 3]).await;
        seed_metadata(pool, owner).await;
        sqlx::query(
            "INSERT INTO proxima_core.publication_origin
                 (t, original_owner_id, source_id)
             VALUES ($1, $2, $3)",
        )
        .bind(selected.into_inner())
        .bind(owner.into_inner())
        .bind(source.as_str())
        .execute(pool)
        .await?;

        // A foreign owner cannot reach callback dispatch, the core erase,
        // host inverse, or the publication-origin inverse.
        let context = pg
            .host_state_erase_context()
            .expect("boot-frozen lifecycle callback is present");
        let mut wrong_owner_tx = pool.begin().await?;
        let wrong = proxima_storage_pg::verbs::forget::erase_memory(
            &mut wrong_owner_tx,
            pg.sidecars(),
            &context,
            &wrong_owner,
            selected.into_inner(),
        )
        .await;
        assert!(matches!(wrong, Err(StorageError::NotFound)));
        wrong_owner_tx.rollback().await?;
        assert!(
            lifecycle
                .last_request
                .lock()
                .expect("request mutex")
                .is_none()
        );
        assert_eq!(core_fact_count(pool, selected).await, 1);
        assert_eq!(host_fact_count(pool, owner).await, 1);
        assert_eq!(publication_origin_count(pool, selected).await, 1);

        for fault in [
            EraseFault::ExactForeignScope,
            EraseFault::ExactRetainedCount,
            EraseFault::ExactRetainedScrubbedCount,
        ] {
            lifecycle.set_erase_fault(fault);
            let mut tx = pool.begin().await?;
            let rejected = proxima_storage_pg::verbs::forget::erase_memory(
                &mut tx,
                pg.sidecars(),
                &context,
                &owner,
                selected.into_inner(),
            )
            .await;
            assert!(
                rejected.is_err(),
                "exact receipt fault {fault:?} is rejected"
            );
            tx.rollback().await?;
            assert_eq!(
                core_fact_count(pool, selected).await,
                1,
                "core rolls back for {fault:?}"
            );
            assert_eq!(
                host_fact_count(pool, owner).await,
                1,
                "host copy rolls back for {fault:?}"
            );
            assert_eq!(
                publication_origin_count(pool, selected).await,
                1,
                "origin rolls back for {fault:?}"
            );
            let metadata_payload: Vec<u8> = sqlx::query_scalar(
                "SELECT payload FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
            )
            .bind(owner.into_inner())
            .fetch_one(pool)
            .await?;
            assert_eq!(
                metadata_payload,
                vec![0xaa],
                "retained metadata is unchanged for {fault:?}"
            );
        }
        lifecycle.set_erase_fault(EraseFault::None);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("exact receipt scope/retained count oracles failed");
}

#[tokio::test]
async fn invalid_callback_receipts_and_commit_failure_roll_back_core_and_host_rows() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let source = SourceId::new("rollback-source");
        let fact = ingest(&pg, owner, source.as_str(), "rollback-fact").await;
        seed_host_fact(pool, owner, fact, source.as_str(), &[42, 0, 254]).await;
        seed_metadata_for_source(pool, owner, source.as_str()).await;
        sqlx::query(
            "INSERT INTO proxima_core.publication_origin
                 (t, original_owner_id, source_id)
             VALUES ($1, $2, $3)",
        )
        .bind(fact.into_inner())
        .bind(owner.into_inner())
        .bind(source.as_str())
        .execute(pool)
        .await?;
        let outbox_bytes = [0x11_u8, 0x00, 0xff];
        let outbox_digest = [0xab_u8; 32];
        sqlx::query(
            "INSERT INTO proxima_core.publication_outbox
                 (t, owner_id, schema_id, schema_version, event_type, event_id,
                  envelope, envelope_digest)
             VALUES ($1, $2, 'core.test-lifecycle-rollback', 1,
                     'com.proxima.test.lifecycle', $3, $4, $5)",
        )
        .bind(fact.into_inner())
        .bind(owner.into_inner())
        .bind(fact.into_inner().to_string())
        .bind(outbox_bytes.as_slice())
        .bind(outbox_digest.as_slice())
        .execute(pool)
        .await?;
        let user_id = owner.as_user();
        for fault in [
            EraseFault::CallbackError,
            EraseFault::ForeignParticipant,
            EraseFault::ForeignOwner,
            EraseFault::ForeignScope,
            EraseFault::ForeignPhysicalSelection,
            EraseFault::ForeignOriginalOwnerSelection,
            EraseFault::ForeignOriginalFactSelection,
            EraseFault::MissingTable,
            EraseFault::DuplicateTable,
            EraseFault::ForeignTable,
            EraseFault::RetainedCount,
            EraseFault::RetainedScrubbedCount,
            EraseFault::DeferredCommitFailure,
        ] {
            lifecycle.set_erase_fault(fault);
            let auth = erase_auth(owner, Some(source.clone()));
            let result = pg
                .erase_personal_source_scope(&auth, user_id, &source, pg.surfaces())
                .await;
            assert!(result.is_err(), "fault {fault:?} must reject the transaction");
            assert_eq!(core_fact_count(pool, fact).await, 1, "core Fact rolled back for {fault:?}");
            assert_eq!(host_fact_count(pool, owner).await, 1, "host Fact rolled back for {fault:?}");
            let metadata: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
            )
            .bind(owner.into_inner())
            .fetch_one(pool)
            .await?;
            assert_eq!(metadata, 1, "metadata rolled back for {fault:?}");
            let metadata_payload: Vec<u8> = sqlx::query_scalar(
                "SELECT payload FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
            )
            .bind(owner.into_inner())
            .fetch_one(pool)
            .await?;
            assert_eq!(metadata_payload, vec![0xaa], "scrub rolled back for {fault:?}");
            assert_eq!(publication_origin_count(pool, fact).await, 1, "origin inverse rolls back for {fault:?}");
            let retained_outbox: (Vec<u8>, Vec<u8>) = sqlx::query_as(
                "SELECT envelope, envelope_digest FROM proxima_core.publication_outbox WHERE t = $1",
            )
            .bind(fact.into_inner())
            .fetch_one(pool)
            .await?;
            assert_eq!(retained_outbox.0.as_slice(), outbox_bytes.as_slice(), "outbox bytes roll back for {fault:?}");
            assert_eq!(retained_outbox.1.as_slice(), outbox_digest.as_slice(), "outbox digest rolls back for {fault:?}");
        }
        lifecycle.set_erase_fault(EraseFault::None);
        let auth = erase_auth(owner, Some(source.clone()));
        let erased = pg
            .erase_personal_source_scope(&auth, user_id, &source, pg.surfaces())
            .await?;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        assert_eq!(core_fact_count(pool, fact).await, 0);
        assert_eq!(host_fact_count(pool, owner).await, 0);
        assert_eq!(publication_origin_count(pool, fact).await, 0);
        let remaining_outbox: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(fact.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(remaining_outbox, 0, "the success path actually selects the seeded outbox");
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("invalid receipt must roll back the shared transaction");
}

#[tokio::test]
async fn erase_receipt_separates_actual_payload_scrubbing_from_row_deletion() {
    let (db_name, pg, lifecycle) = fresh_pg(3).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let source = SourceId::new("source-a");
        let fact = ingest(&pg, owner, source.as_str(), "scrub-fact").await;
        seed_host_fact(pool, owner, fact, source.as_str(), &[0, 0x80, 0xff]).await;
        seed_metadata_for_source(pool, owner, source.as_str()).await;

        lifecycle.set_erase_fault(EraseFault::ScrubFacts);
        let auth = erase_auth(owner, Some(source.clone()));
        let OwnerEraseOutcome::Completed {
            counts,
            host_state_deleted,
            host_state_scrubbed,
            ..
        } = pg
            .erase_personal_source_scope(
                &auth,
                owner.as_user(),
                &source,
                pg.surfaces(),
            )
            .await?
        else {
            panic!("source erase should complete");
        };

        assert_eq!(host_state_deleted.get(FACTS_TABLE), Some(&0));
        assert_eq!(host_state_scrubbed.get(FACTS_TABLE), Some(&1));
        assert_eq!(host_state_deleted.get(METADATA_TABLE), Some(&0));
        assert_eq!(host_state_scrubbed.get(METADATA_TABLE), Some(&0));
        assert_eq!(counts.get("host_lifecycle_rows"), 0);
        let facts = sqlx::query(
            "SELECT fact_id, payload FROM proxima_core.test_host_lifecycle_facts WHERE owner_id = $1",
        )
        .bind(owner.into_inner())
        .fetch_all(pool)
        .await?;
        assert_eq!(facts.len(), 1, "scrubbing retains the host row");
        let persisted_id: Uuid = facts[0].try_get("fact_id")?;
        let payload: Vec<u8> = facts[0].try_get("payload")?;
        assert_eq!(persisted_id, fact.into_inner());
        assert!(payload.is_empty(), "actual payload bytes were scrubbed");
        assert_eq!(core_fact_count(pool, fact).await, 0);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("deleted and scrubbed outcome maps remain distinct");
}

#[tokio::test]
async fn lifecycle_boot_rejects_missing_callback_and_invalid_descriptor() {
    let (db_name, _pg, lifecycle) = fresh_pg(2).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let registry = frozen_registry();
        let missing = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_host_state_participant(Arc::new(HostParticipant {
                lifecycle: lifecycle.clone(),
                lifecycle_enabled: false,
            }))
            .try_with_flavors(&registry);
        assert!(
            missing.is_err(),
            "managed surfaces need an erase/export port"
        );

        let mismatch = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_host_state_participant(Arc::new(TableParticipant {
                lifecycle: lifecycle.clone(),
                tables: WRONG_PARTICIPANT_TABLES,
            }))
            .try_with_flavors(&registry);
        assert!(
            mismatch.is_err(),
            "the frozen managed set must match the captured callback"
        );

        let wrong_port = Arc::new(Lifecycle::with_participant_id(HostStateParticipantId::new(
            "other_participant",
        )));
        let wrong_id = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_host_state_participant(Arc::new(HostParticipant {
                lifecycle: wrong_port,
                lifecycle_enabled: true,
            }))
            .try_with_flavors(&registry);
        assert!(
            wrong_id.is_err(),
            "the callback ID must match the captured host participant"
        );

        let wrong_port = Arc::new(Lifecycle::with_declared_tables(WRONG_PARTICIPANT_TABLES));
        let wrong_port_tables = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_host_state_participant(Arc::new(HostParticipant {
                lifecycle: wrong_port,
                lifecycle_enabled: true,
            }))
            .try_with_flavors(&registry);
        assert!(
            wrong_port_tables.is_err(),
            "callback tables must exactly cover managed policies"
        );

        let missing_participant = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .try_with_flavors(&registry);
        assert!(
            missing_participant.is_err(),
            "managed policies require the actual participant"
        );

        let undeclared = PgStorage::connect_for_tests(db_url(&db_name))
            .await?
            .with_host_state_participant(Arc::new(TableParticipant {
                lifecycle: lifecycle.clone(),
                tables: UNDECLARED_PARTICIPANT_TABLES,
            }))
            .try_with_flavors(&registry);
        assert!(
            undeclared.is_err(),
            "participant tables must be linked state surfaces"
        );

        for extra in [&DUPLICATE_CONTRACT, &OVERLAP_CONTRACT] {
            let invalid_registry = frozen_registry_with(Some(extra));
            let invalid = PgStorage::connect_for_tests(db_url(&db_name))
                .await?
                .with_host_state_participant(Arc::new(HostParticipant {
                    lifecycle: lifecycle.clone(),
                    lifecycle_enabled: true,
                }))
                .try_with_flavors(&invalid_registry);
            assert!(
                invalid.is_err(),
                "duplicate/overlapping policy must fail boot"
            );
        }

        let direct = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        let direct_surfaces =
            OwnerSurfaces::try_from_surfaces(vec![FACTS_SURFACE, METADATA_SURFACE])?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let erase_auth = erase_auth(owner, None);
        assert!(
            direct
                .erase_personal_owner(&erase_auth, user_id, &direct_surfaces)
                .await
                .is_err()
        );
        let export_auth =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        assert!(
            direct
                .export_owner_bundle(&export_auth, &direct_surfaces)
                .await
                .is_err()
        );
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("boot lifecycle validation failed");
}

#[tokio::test]
async fn export_uses_one_read_only_snapshot_and_allowlist() {
    let (db_name, pg, lifecycle) = fresh_pg(1).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fact = ingest(&pg, owner, "export-source", "export-fact").await;
        let bytes = [0, 1, 0x80, 0xfe, 0xff];
        seed_host_fact(pool, owner, fact, "export-source", &bytes).await;
        seed_metadata(pool, owner).await;
        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: owner.as_user(),
        });
        let bundle = pg.export_owner_bundle(&auth, pg.surfaces()).await?;
        let rows = bundle.tables.get(FACTS_TABLE).expect("host Facts exported");
        assert_eq!(rows.len(), 1);
        let expected_payload = format!("\\x{}", hex(&bytes));
        assert_eq!(rows[0]["payload"], expected_payload);
        assert_eq!(
            rows[0].as_object().expect("object row").len(),
            FACT_EXPORT_FIELDS.len()
        );
        assert!(
            !bundle.tables.contains_key(METADATA_TABLE),
            "excluded policy is honored"
        );
        assert_eq!(bundle.counts.get(FACTS_TABLE), Some(&1));
        assert_eq!(bundle.counts.get(METADATA_TABLE), None);

        for fault in [
            ExportFault::ExtraField,
            ExportFault::PersistentWrite,
            ExportFault::ForeignParticipant,
            ExportFault::ForeignOwner,
            ExportFault::MissingTable,
            ExportFault::DuplicateTable,
            ExportFault::ForeignTable,
        ] {
            lifecycle.set_export_fault(fault);
            assert!(
                pg.export_owner_bundle(&auth, pg.surfaces()).await.is_err(),
                "export fault {fault:?} is rejected"
            );
        }
        lifecycle.set_export_fault(ExportFault::None);
        let still_one = host_fact_count(pool, owner).await;
        assert_eq!(
            still_one, 1,
            "read-only callback rejected the persistent write"
        );
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("max-1 export oracle failed");
}

#[tokio::test]
async fn export_behind_a_long_erase_gives_up_with_a_retryable_error() {
    let (db_name, pg, _lifecycle) = fresh_pg(1).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: owner.as_user(),
        });
        // An erase holds the fence for its whole body.
        let mut erase = pool.begin().await?;
        sqlx::query("LOCK TABLE proxima_core.export_erase_fence IN ROW EXCLUSIVE MODE")
            .execute(&mut *erase)
            .await?;
        let started = std::time::Instant::now();
        let queued = tokio::time::timeout(
            Duration::from_secs(15),
            pg.export_owner_bundle(&auth, pg.surfaces()),
        )
        .await
        .expect("the export's wait for the fence is bounded");
        let waited = started.elapsed();
        match queued {
            Err(StorageError::Retryable(message)) => assert!(
                message.contains("erase is running"),
                "the error names the erase it waited for: {message}"
            ),
            other => panic!("expected a retryable fence timeout, got {other:?}"),
        }
        assert!(
            waited >= Duration::from_secs(4),
            "the export waited for the fence before giving up ({waited:?})"
        );
        assert!(
            wait_for_no_fence_waiters(pool).await,
            "the timed-out export left no fence lock behind"
        );
        erase.rollback().await?;
        pg.export_owner_bundle(&auth, pg.surfaces()).await?;
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("bounded export fence wait failed");
}

const ADVISORY_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'advisory'
   AND NOT granted
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

const FENCE_LOCKS_SQL: &str = "\
SELECT count(*) FILTER (WHERE NOT granted)::bigint, count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'relation'
   AND relation = 'proxima_core.export_erase_fence'::regclass
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

async fn wait_for_fence_waiters(pool: &PgPool, expected: i64) -> bool {
    for _ in 0..400 {
        let (waiting, _held): (i64, i64) = sqlx::query_as(FENCE_LOCKS_SQL)
            .fetch_one(pool)
            .await
            .expect("fence waiter probe");
        if waiting >= expected {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// No session waits for the fence.
async fn wait_for_no_fence_waiters(pool: &PgPool) -> bool {
    for _ in 0..400 {
        let (waiting, _held): (i64, i64) = sqlx::query_as(FENCE_LOCKS_SQL)
            .fetch_one(pool)
            .await
            .expect("fence waiter probe");
        if waiting == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// No session holds or waits for the fence. The probe itself takes no lock on
/// it: `regclass` resolves the name without locking the relation.
async fn wait_for_no_fence_locks(pool: &PgPool) -> bool {
    for _ in 0..400 {
        let (_waiting, held): (i64, i64) = sqlx::query_as(FENCE_LOCKS_SQL)
            .fetch_one(pool)
            .await
            .expect("fence lock probe");
        if held == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

async fn wait_for_advisory_waiters(pool: &PgPool, expected: i64) -> bool {
    for _ in 0..400 {
        let count: i64 = sqlx::query_scalar(ADVISORY_WAITERS_SQL)
            .fetch_one(pool)
            .await
            .expect("advisory waiter probe");
        if count >= expected {
            return true;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

async fn wait_for_no_advisory_waiters(pool: &PgPool) -> bool {
    for _ in 0..400 {
        let count: i64 = sqlx::query_scalar(ADVISORY_WAITERS_SQL)
            .fetch_one(pool)
            .await
            .expect("advisory waiter probe");
        if count == 0 {
            return true;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn erase_fence_orders_write_session_and_export_snapshot_and_cancellation_leaves_no_lock() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let source = SourceId::new("fence-source");
        let fact = ingest(&pg, owner, source.as_str(), "fence-fact").await;
        seed_host_fact(pool, owner, fact, source.as_str(), &[1, 3, 5]).await;
        seed_metadata(pool, owner).await;

        let gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_gate(Some(gate.clone()));
        let erase_pg = pg.clone();
        let erase_surfaces = pg.surfaces().clone();
        let erase_source = source.clone();
        let erase = tokio::spawn(async move {
            let auth = erase_auth(owner, Some(erase_source.clone()));
            erase_pg
                .erase_personal_source_scope(&auth, user_id, &erase_source, &erase_surfaces)
                .await
        });
        gate.entered.notified().await;

        // A unit takes no store-wide fence at entry: it begins during the
        // erase, and what waits is a write into the erased owner's source.
        let scope = owner_scope_for_tests(&[owner]);
        let session = tokio::time::timeout(
            Duration::from_secs(2),
            WriteSessionFactory::begin(pg.as_ref(), Some(&scope)),
        )
        .await
        .expect("a unit of work begins during an erase")?;
        let late_pg = pg.clone();
        let late_source = source.clone();
        let late_write = tokio::spawn(async move {
            ingest(
                &late_pg,
                owner,
                late_source.as_str(),
                "written-during-erase",
            )
            .await
        });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "a write into the erased source waits at its source fence"
        );
        assert!(!late_write.is_finished(), "the erase excludes that write");

        let export_pg = pg.clone();
        let export = tokio::spawn(async move {
            let auth =
                ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
            export_pg
                .export_owner_bundle(&auth, export_pg.surfaces())
                .await
        });
        assert!(
            wait_for_fence_waiters(pool, 1).await,
            "export waits behind the erasure fence"
        );
        assert!(
            !export.is_finished(),
            "export cannot begin its snapshot before the erasure commits"
        );

        gate.release.notify_one();
        let erased = erase.await??;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        lifecycle.set_erase_gate(None);
        let bundle = export.await??;
        assert_eq!(
            bundle.counts.get(FACTS_TABLE),
            Some(&0),
            "queued export sees post-erase state"
        );
        assert!(bundle.tables.get(FACTS_TABLE).is_some_and(Vec::is_empty));
        session.commit().await?;
        late_write.await?;

        // An export cancelled while it waits on the fence leaves no lock
        // behind: its transaction rolls back when its connection returns.
        let mut fence = pool.begin().await?;
        sqlx::query("LOCK TABLE proxima_core.export_erase_fence IN ROW EXCLUSIVE MODE")
            .execute(&mut *fence)
            .await?;
        let cancel_pg = pg.clone();
        let cancel = tokio::spawn(async move {
            let auth =
                ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
            cancel_pg
                .export_owner_bundle(&auth, cancel_pg.surfaces())
                .await
        });
        assert!(
            wait_for_fence_waiters(pool, 1).await,
            "export is waiting on the fence"
        );
        cancel.abort();
        let _ = cancel.await;
        fence.rollback().await?;
        let recovered =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        let bundle = pg.export_owner_bundle(&recovered, pg.surfaces()).await?;
        assert_eq!(
            bundle.counts.get(FACTS_TABLE),
            Some(&0),
            "the pool recovers after cancellation"
        );
        assert!(
            wait_for_no_fence_locks(pool).await,
            "the cancelled export left no fence lock"
        );

        let reverse_source = SourceId::new("writer-before-erase");
        let reverse_fact = ingest(&pg, owner, reverse_source.as_str(), "reverse-fact").await;
        seed_host_fact(
            pool,
            owner,
            reverse_fact,
            reverse_source.as_str(),
            &[2, 4, 6],
        )
        .await;
        // A unit holds its owner's fence shared from its first write; a
        // whole-owner erase queues behind it.
        let mut writer = pool.begin().await?;
        proxima_storage_pg::access::owner_columns::lock_owner_fence_shared_tx(&mut writer, &owner)
            .await?;
        let reverse_gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_gate(Some(reverse_gate.clone()));
        let erase_pg = pg.clone();
        let erase_surfaces = pg.surfaces().clone();
        let reverse_erase = tokio::spawn(async move {
            let auth = erase_auth(owner, None);
            erase_pg
                .erase_personal_owner(&auth, user_id, &erase_surfaces)
                .await
        });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "erase waits behind a unit that holds its owner's fence first"
        );
        assert!(!reverse_erase.is_finished());
        writer.commit().await?;
        reverse_gate.entered.notified().await;
        reverse_gate.release.notify_one();
        assert!(matches!(
            reverse_erase.await??,
            OwnerEraseOutcome::Completed { .. }
        ));
        lifecycle.set_erase_gate(None);
        assert_eq!(host_fact_count(pool, owner).await, 0);
        Ok(())
    }
    .await;
    lifecycle.set_erase_gate(None);
    close_pg(&db_name).await;
    result.expect("lifecycle fence/export oracle failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_snapshot_keeps_unfenced_cursor_and_grant_writes_coherent() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let fact = ingest(&pg, owner, "snapshot-source", "snapshot-fact").await;
        seed_host_fact(pool, owner, fact, "snapshot-source", &[0x10, 0x20]).await;
        let delegation_id = Uuid::now_v7();
        let subject_id = Uuid::now_v7();
        let mut seed = pool.begin().await?;
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors
                (owner_id, source, cursor)
             VALUES ($1, 'snapshot-source', decode('aabb', 'hex'))",
        )
        .bind(user_id.into_inner())
        .execute(&mut *seed)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                (delegation_id, subject_user_id, owner_id, tool_name,
                 read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at, \"by\", via)
             VALUES ($1, $2, $3, 'core_remember', 'fact', 'fact',
                     TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                     TIMESTAMPTZ '2026-01-01 00:00:00Z', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(delegation_id)
        .bind(subject_id)
        .bind(user_id.into_inner())
        .execute(&mut *seed)
        .await?;
        seed.commit().await?;

        let mut writer = pool.begin().await?;
        sqlx::query(
            "UPDATE proxima_core.source_cursors
                SET cursor = decode('ccdd', 'hex')
              WHERE owner_id = $1 AND source = 'snapshot-source'",
        )
        .bind(user_id.into_inner())
        .execute(&mut *writer)
        .await?;
        sqlx::query(
            "UPDATE proxima_core.delegated_authority_grants
                SET auth_epoch = 2
              WHERE delegation_id = $1",
        )
        .bind(delegation_id)
        .execute(&mut *writer)
        .await?;
        sqlx::query(
            "UPDATE proxima_core.test_host_lifecycle_facts
                SET payload = decode('3040', 'hex')
              WHERE fact_id = $1 AND owner_id = $2",
        )
        .bind(fact.into_inner())
        .bind(user_id.into_inner())
        .execute(&mut *writer)
        .await?;

        let gate = Arc::new(CallbackGate::default());
        lifecycle.set_export_gate(Some(gate.clone()));
        let export_pg = pg.clone();
        let first = tokio::spawn(async move {
            let auth =
                ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
            export_pg
                .export_owner_bundle(&auth, export_pg.surfaces())
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified()).await?;
        writer.commit().await?;
        gate.release.notify_one();
        let before_snapshot = first.await??;
        assert_eq!(
            before_snapshot.tables["proxima_core.source_cursors"][0]["cursor"].as_str(),
            Some(r"\xaabb")
        );
        assert_eq!(
            before_snapshot.tables["proxima_core.delegated_authority_grants"][0]["auth_epoch"]
                .as_i64(),
            Some(1)
        );
        assert_eq!(
            before_snapshot.tables[FACTS_TABLE][0]["payload"].as_str(),
            Some(r"\x1020")
        );

        lifecycle.set_export_gate(None);
        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        let after_commit = pg.export_owner_bundle(&auth, pg.surfaces()).await?;
        assert_eq!(
            after_commit.tables["proxima_core.source_cursors"][0]["cursor"].as_str(),
            Some(r"\xccdd")
        );
        assert_eq!(
            after_commit.tables["proxima_core.delegated_authority_grants"][0]["auth_epoch"]
                .as_i64(),
            Some(2)
        );
        assert_eq!(
            after_commit.tables[FACTS_TABLE][0]["payload"].as_str(),
            Some(r"\x3040")
        );
        Ok(())
    }
    .await;
    lifecycle.set_export_gate(None);
    close_pg(&db_name).await;
    result.expect("one owner snapshot must include both unfenced tables coherently");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_erase_after_host_callback_dml_rolls_back_and_releases_fences() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let source = SourceId::new("cancel-after-host-dml");
        let fact = ingest(&pg, owner, source.as_str(), "cancel-host-dml").await;
        let original_payload = vec![0x11, 0x22, 0x33];
        seed_host_fact(pool, owner, fact, source.as_str(), &original_payload).await;
        seed_metadata_for_source(pool, owner, source.as_str()).await;

        let gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_fault(EraseFault::ScrubFacts);
        lifecycle.set_erase_gate(Some(gate.clone()));
        let erase_pg = pg.clone();
        let erase_source = source.clone();
        let erase_surfaces = pg.surfaces().clone();
        let erase = tokio::spawn(async move {
            let auth = erase_auth(owner, Some(erase_source.clone()));
            erase_pg
                .erase_personal_source_scope(&auth, user_id, &erase_source, &erase_surfaces)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified()).await?;
        erase.abort();
        assert!(matches!(erase.await, Err(error) if error.is_cancelled()));
        lifecycle.set_erase_gate(None);
        lifecycle.set_erase_fault(EraseFault::None);

        assert!(wait_for_no_advisory_waiters(pool).await);
        assert_eq!(core_fact_count(pool, fact).await, 1);
        assert_eq!(host_fact_count(pool, owner).await, 1);
        let payload: Vec<u8> = sqlx::query_scalar(
            "SELECT payload FROM proxima_core.test_host_lifecycle_facts WHERE fact_id = $1",
        )
        .bind(fact.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(payload, original_payload, "callback scrub was rolled back");

        let followup = tokio::time::timeout(
            Duration::from_secs(5),
            ingest(&pg, owner, source.as_str(), "after-cancel-host-dml"),
        )
        .await?;
        seed_host_fact(pool, owner, followup, source.as_str(), &[0x44]).await;
        let auth = erase_auth(owner, Some(source.clone()));
        let erased = tokio::time::timeout(
            Duration::from_secs(5),
            pg.erase_personal_source_scope(&auth, user_id, &source, pg.surfaces()),
        )
        .await??;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        assert_eq!(core_fact_count(pool, fact).await, 0);
        assert_eq!(core_fact_count(pool, followup).await, 0);
        assert_eq!(host_fact_count(pool, owner).await, 0);
        Ok(())
    }
    .await;
    lifecycle.set_erase_gate(None);
    lifecycle.set_erase_fault(EraseFault::None);
    close_pg(&db_name).await;
    result.expect("cancellation after host callback SQL must roll back and release fences");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_erase_during_core_delete_after_host_callback_dml_rolls_back_all_rows() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        sqlx::raw_sql(
            "CREATE FUNCTION proxima_core.test_lifecycle_commit_gate() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM pg_advisory_xact_lock(871922);
                 RETURN OLD;
             END;
             $$;
             CREATE TRIGGER test_lifecycle_commit_gate_trigger
             AFTER DELETE ON proxima_core.memory
             FOR EACH ROW EXECUTE FUNCTION proxima_core.test_lifecycle_commit_gate();",
        )
        .execute(pool)
        .await?;

        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        let source = SourceId::new("cancel-before-commit");
        let fact = ingest(&pg, owner, source.as_str(), "cancel-commit").await;
        let original_payload = vec![0x55, 0x66, 0x77];
        seed_host_fact(pool, owner, fact, source.as_str(), &original_payload).await;
        seed_metadata_for_source(pool, owner, source.as_str()).await;

        let barrier_pool = PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(ERASE_CORE_DELETE_GATE_LOCK)
            .execute(&mut *barrier)
            .await?;

        let erase_pg = pg.clone();
        let erase_source = source.clone();
        let erase_surfaces = pg.surfaces().clone();
        let erase = tokio::spawn(async move {
            let auth = erase_auth(owner, Some(erase_source.clone()));
            erase_pg
                .erase_personal_source_scope(&auth, user_id, &erase_source, &erase_surfaces)
                .await
        });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "test trigger blocks inside core DELETE before COMMIT"
        );
        assert!(
            !erase.is_finished(),
            "core DELETE remains blocked on the fixture gate"
        );
        erase.abort();
        assert!(matches!(erase.await, Err(error) if error.is_cancelled()));

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(ERASE_CORE_DELETE_GATE_LOCK)
            .fetch_one(&mut *barrier)
            .await?;
        assert!(unlocked, "fixture lock is released");
        drop(barrier);
        barrier_pool.close().await;
        assert!(wait_for_no_advisory_waiters(pool).await);

        let session = tokio::time::timeout(
            Duration::from_secs(5),
            WriteSessionFactory::begin(pg.as_ref(), Some(&owner_scope_for_tests(&[owner]))),
        )
        .await??;
        session.commit().await?;
        assert_eq!(core_fact_count(pool, fact).await, 1);
        assert_eq!(host_fact_count(pool, owner).await, 1);
        let payload: Vec<u8> = sqlx::query_scalar(
            "SELECT payload FROM proxima_core.test_host_lifecycle_facts WHERE fact_id = $1",
        )
        .bind(fact.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(payload, original_payload, "host delete was rolled back");

        let followup = tokio::time::timeout(
            Duration::from_secs(5),
            ingest(&pg, owner, source.as_str(), "after-cancel-commit"),
        )
        .await?;
        seed_host_fact(pool, owner, followup, source.as_str(), &[0x88]).await;
        lifecycle.set_erase_fault(EraseFault::None);
        let auth = erase_auth(owner, Some(source.clone()));
        let erased = tokio::time::timeout(
            Duration::from_secs(5),
            pg.erase_personal_source_scope(&auth, user_id, &source, pg.surfaces()),
        )
        .await??;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        assert_eq!(core_fact_count(pool, fact).await, 0);
        assert_eq!(core_fact_count(pool, followup).await, 0);
        assert_eq!(host_fact_count(pool, owner).await, 0);
        Ok(())
    }
    .await;
    lifecycle.set_erase_gate(None);
    lifecycle.set_erase_fault(EraseFault::None);
    close_pg(&db_name).await;
    result.expect("cancellation during core DELETE must roll back host and core rows");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_max_one_export_waiting_on_the_fence_releases_its_connection() {
    let (db_name, pg, _lifecycle) = fresh_pg(1).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let user_id = owner.as_user();
        ingest(&pg, owner, "cancel-source", "cancel-fact").await;
        let barrier_pool = PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.begin().await?;
        sqlx::query("LOCK TABLE proxima_core.export_erase_fence IN ROW EXCLUSIVE MODE")
            .execute(&mut *barrier)
            .await?;

        let export_pg = pg.clone();
        let export = tokio::spawn(async move {
            let auth =
                ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
            export_pg
                .export_owner_bundle(&auth, export_pg.surfaces())
                .await
        });
        assert!(wait_for_fence_waiters(pg.pool_for_tests(), 1).await);
        export.abort();
        let _ = export.await;
        barrier.rollback().await?;
        barrier_pool.close().await;

        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        let bundle = pg.export_owner_bundle(&auth, pg.surfaces()).await?;
        assert_eq!(bundle.owner, owner);
        assert!(wait_for_no_fence_locks(pg.pool_for_tests()).await);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("cancelled one-slot export must release its connection and lock");
}

/// The snapshot starts after the fence is held. An export queued behind a
/// transaction holding the fence reads what that transaction committed.
/// Taking the fence with a lock function instead (`SELECT
/// pg_advisory_xact_lock(..)` as the first statement of the REPEATABLE READ
/// transaction) starts the snapshot at that statement, before its wait, and
/// this export would read no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_queued_behind_the_fence_reads_what_its_holder_committed() {
    let (db_name, pg, _lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let fact = ingest(&pg, owner, "fence-source", "fence-fact").await;
        let mut holder = pool.begin().await?;
        sqlx::query("LOCK TABLE proxima_core.export_erase_fence IN ROW EXCLUSIVE MODE")
            .execute(&mut *holder)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.test_host_lifecycle_facts
                (fact_id, owner_id, source_id, payload)
             VALUES ($1, $2, 'fence-source', decode('2a', 'hex'))",
        )
        .bind(fact.into_inner())
        .bind(owner.into_inner())
        .execute(&mut *holder)
        .await?;

        let export_pg = pg.clone();
        let export = tokio::spawn(async move {
            let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
                user_id: owner.as_user(),
            });
            export_pg
                .export_owner_bundle(&auth, export_pg.surfaces())
                .await
        });
        assert!(
            wait_for_fence_waiters(pool, 1).await,
            "export waits on the fence"
        );
        assert!(!export.is_finished());
        holder.commit().await?;
        let bundle = export.await??;
        assert_eq!(
            bundle.counts.get(FACTS_TABLE),
            Some(&1),
            "the snapshot was taken after the fence holder committed"
        );
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("queued export reads its fence holder's commit");
}
