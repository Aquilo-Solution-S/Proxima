//! Real-Postgres acceptance coverage for the frozen host lifecycle port.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::flavor::{
    CounterRule, EraseRule, ExportRule, FlavorContract, FlavorDescriptor, FlavorProvenance,
    ForgetRule, KeyShape, ProjectionDecl, Surface, TransferRule,
};
use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::{
    FactIngestPort, HostStateEraseReceipt, HostStateEraseRequest, HostStateEraseScope,
    HostStateEraseTableCount, HostStateExportReceipt, HostStateExportRequest, HostStateExportTable,
    HostStateParticipantId, HostStateReply, HostStateRequest, HostStateWritePermit,
    MemoryAuthoringPort, OwnerInversePort, OwnerWritePermit, StateSurfaceName, WriteSessionFactory,
};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, GroupId, MemoryId, OwnerRef, SchemaId, SchemaVersion, SourceId, StorageError,
    UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::{
    PgHostStateLifecyclePort, PgHostStateParticipant, PgPoolConfig, PgStorage, PgTuning,
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
    },
    export: ExportRule::Excluded {
        why: "internal lifecycle metadata is not portable owner content",
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
    export: ExportRule::Excluded {
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
    ForeignOwnerKind,
    ForeignScope,
    MissingTable,
    DuplicateTable,
    ForeignTable,
    RetainedCount,
    RetainedScrubbedCount,
    ScrubFacts,
    DeferredCommitFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFault {
    None,
    ExtraField,
    PersistentWrite,
    ForeignParticipant,
    ForeignOwnerKind,
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
        let owner_id = request.owner().stored_owner_id();
        let selected_ids = request
            .selected_fact_ids()
            .iter()
            .map(|id| id.into_inner())
            .collect::<Vec<_>>();
        let (facts_deleted, facts_scrubbed, metadata_deleted, metadata_scrubbed) =
            match request.scope() {
                HostStateEraseScope::WholeOwner => {
                    let facts = sqlx::query(
                        "DELETE FROM proxima_core.test_host_lifecycle_facts WHERE owner_id = $1",
                    )
                    .bind(owner_id)
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
                            "UPDATE proxima_core.test_host_lifecycle_facts
                            SET payload = decode('', 'hex')
                          WHERE owner_id = $1 AND source_id = $2 AND fact_id = ANY($3)",
                        )
                        .bind(owner_id)
                        .bind(source.as_str())
                        .bind(&selected_ids)
                        .execute(&mut **tx)
                        .await
                        .map_err(|error| pg_error(&error))?
                        .rows_affected();
                        (0, scrubbed)
                    } else {
                        let deleted = sqlx::query(
                            "DELETE FROM proxima_core.test_host_lifecycle_facts
                          WHERE owner_id = $1 AND source_id = $2 AND fact_id = ANY($3)",
                        )
                        .bind(owner_id)
                        .bind(source.as_str())
                        .bind(&selected_ids)
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
            EraseFault::ForeignOwnerKind => {
                receipt.owner = match request.owner() {
                    OwnerRef::Personal(user) => OwnerRef::Group(GroupId::new(user.into_inner())),
                    OwnerRef::Group(group) => OwnerRef::Personal(UserId::new(group.into_inner())),
                };
            }
            EraseFault::ForeignScope => receipt.scope = HostStateEraseScope::WholeOwner,
            EraseFault::MissingTable => {
                receipt.counts.pop();
            }
            EraseFault::DuplicateTable => receipt.counts.push(receipt.counts[0].clone()),
            EraseFault::ForeignTable => {
                receipt.counts[1].table = StateSurfaceName::new("proxima_core.unknown_table");
            }
            EraseFault::RetainedCount => receipt.counts[1].deleted = metadata_deleted,
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
            .bind(request.owner().stored_owner_id())
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
            .bind(request.owner().stored_owner_id())
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| pg_error(&error))?;
            if fault == ExportFault::ExtraField {
                rows.push(json!({
                    "fact_id": Uuid::now_v7(),
                    "owner_id": request.owner().stored_owner_id(),
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
            ExportFault::ForeignOwnerKind => {
                receipt.owner = match request.owner() {
                    OwnerRef::Personal(user) => OwnerRef::Group(GroupId::new(user.into_inner())),
                    OwnerRef::Group(group) => OwnerRef::Personal(UserId::new(group.into_inner())),
                };
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
        _tx: &mut Transaction<'_, Postgres>,
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
        _tx: &mut Transaction<'_, Postgres>,
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
    create_db(&db_name)
        .await
        .expect("PG required for host lifecycle tests");
    let config = PgPoolConfig {
        max_connections,
        ..PgPoolConfig::default()
    };
    let lifecycle = Arc::new(Lifecycle::new());
    let registry = frozen_registry();
    let pg = PgStorage::connect_with_config(&db_url(&db_name), config, PgTuning::default())
        .await
        .expect("connect")
        .with_host_state_participant(Arc::new(HostParticipant {
            lifecycle: lifecycle.clone(),
            lifecycle_enabled: true,
        }))
        .try_with_flavors(&registry)
        .expect("managed descriptor and callback are exact");
    pg.run_migrations().await.expect("migrate");
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
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".to_owned(),
    }
}

async fn ingest(pg: &PgStorage, owner: OwnerRef, source: &str, key: &str) -> MemoryId {
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
    owner: OwnerRef,
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
    .bind(owner.stored_owner_id())
    .bind(source)
    .bind(payload)
    .execute(pool)
    .await
    .expect("host Fact row seeded");
}

async fn seed_metadata(pool: &PgPool, owner: OwnerRef) {
    seed_metadata_for_source(pool, owner, "source-a").await;
}

async fn seed_metadata_for_source(pool: &PgPool, owner: OwnerRef, source: &str) {
    sqlx::query(
        "INSERT INTO proxima_core.test_host_lifecycle_metadata (owner_id, source_id, metadata)
         VALUES ($1, $2, 'opaque')",
    )
    .bind(owner.stored_owner_id())
    .bind(source)
    .execute(pool)
    .await
    .expect("host metadata row seeded");
}

async fn seed_generic_counter_row(pool: &PgPool, owner: OwnerRef) {
    sqlx::query(
        "INSERT INTO proxima_core.test_host_lifecycle_generic_rows (owner_id, value)
         VALUES ($1, 'generic')",
    )
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await
    .expect("generic counter row seeded");
}

fn erase_auth(owner: OwnerRef, source: Option<SourceId>) -> EraseAuthorization {
    match (owner, source) {
        (OwnerRef::Personal(user), None) => {
            EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id: user,
                drop_event_id: "test-drop".into(),
            })
        }
        (OwnerRef::Personal(user), Some(source_id)) => {
            EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                user_id: user,
                source_id,
                drop_event_id: "test-drop".into(),
            })
        }
        (OwnerRef::Group(group), None) => {
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id: group })
        }
        (OwnerRef::Group(group), Some(source_id)) => {
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupSourceScope {
                group_id: group,
                source_id,
            })
        }
    }
}

async fn host_fact_count(pool: &PgPool, owner: OwnerRef) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.test_host_lifecycle_facts WHERE owner_id = $1",
    )
    .bind(owner.stored_owner_id())
    .fetch_one(pool)
    .await
    .expect("host row count")
}

async fn core_fact_count(pool: &PgPool, fact_id: MemoryId) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1 AND kind = 'fact')
             + (SELECT count(*) FROM proxima_core.cooled WHERE t = $1 AND kind = 'fact')",
    )
    .bind(fact_id.into_inner())
    .fetch_one(pool)
    .await
    .expect("core Fact count")
}

#[tokio::test]
async fn source_erase_binds_exact_hot_and_cooled_fact_selection_and_owner_kind() {
    let (db_name, pg, lifecycle) = fresh_pg(5).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
            .erase_personal_source_scope(
                &auth,
                match owner {
                    OwnerRef::Personal(id) => id,
                    OwnerRef::Group(_) => unreachable!(),
                },
                &source_a,
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

        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let group_fact = ingest(&pg, group, "group-source", "group-fact").await;
        seed_host_fact(pool, group, group_fact, "group-source", &[9, 8, 7]).await;
        seed_metadata(pool, group).await;
        seed_generic_counter_row(pool, group).await;
        let group_export_auth = ExportAuthorization::new_for_tests(OwnerExportTarget::GroupOwner {
            group_id: match group {
                OwnerRef::Group(id) => id,
                OwnerRef::Personal(_) => unreachable!(),
            },
        });
        let group_bundle = pg
            .export_owner_bundle(&group_export_auth, pg.surfaces())
            .await?;
        assert_eq!(group_bundle.owner, group);
        assert_eq!(group_bundle.tables[FACTS_TABLE].len(), 1);
        assert!(!group_bundle.tables.contains_key(METADATA_TABLE));
        let auth = erase_auth(group, None);
        let OwnerEraseOutcome::Completed {
            counts,
            host_state_deleted,
            ..
        } = pg
            .erase_group_owner(
                &auth,
                match group {
                    OwnerRef::Group(id) => id,
                    OwnerRef::Personal(_) => unreachable!(),
                },
                pg.surfaces(),
            )
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
async fn invalid_callback_receipts_and_commit_failure_roll_back_core_and_host_rows() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let source = SourceId::new("rollback-source");
        let fact = ingest(&pg, owner, source.as_str(), "rollback-fact").await;
        seed_host_fact(pool, owner, fact, source.as_str(), &[42, 0, 254]).await;
        seed_metadata_for_source(pool, owner, source.as_str()).await;
        let OwnerRef::Personal(user_id) = owner else {
            unreachable!()
        };
        for fault in [
            EraseFault::CallbackError,
            EraseFault::ForeignParticipant,
            EraseFault::ForeignOwnerKind,
            EraseFault::ForeignScope,
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
            .bind(owner.stored_owner_id())
            .fetch_one(pool)
            .await?;
            assert_eq!(metadata, 1, "metadata rolled back for {fault:?}");
            let metadata_payload: Vec<u8> = sqlx::query_scalar(
                "SELECT payload FROM proxima_core.test_host_lifecycle_metadata WHERE owner_id = $1",
            )
            .bind(owner.stored_owner_id())
            .fetch_one(pool)
            .await?;
            assert_eq!(metadata_payload, vec![0xaa], "scrub rolled back for {fault:?}");
        }
        lifecycle.set_erase_fault(EraseFault::None);
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
                match owner {
                    OwnerRef::Personal(id) => id,
                    OwnerRef::Group(_) => unreachable!(),
                },
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
        .bind(owner.stored_owner_id())
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
        let missing = PgStorage::connect(&db_url(&db_name))
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

        let mismatch = PgStorage::connect(&db_url(&db_name))
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
        let wrong_id = PgStorage::connect(&db_url(&db_name))
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
        let wrong_port_tables = PgStorage::connect(&db_url(&db_name))
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

        let missing_participant = PgStorage::connect(&db_url(&db_name))
            .await?
            .try_with_flavors(&registry);
        assert!(
            missing_participant.is_err(),
            "managed policies require the actual participant"
        );

        let undeclared = PgStorage::connect(&db_url(&db_name))
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
            let invalid = PgStorage::connect(&db_url(&db_name))
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

        let direct = PgStorage::connect(&db_url(&db_name)).await?;
        let direct_surfaces =
            OwnerSurfaces::try_from_surfaces(vec![FACTS_SURFACE, METADATA_SURFACE])?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let user_id = match owner {
            OwnerRef::Personal(id) => id,
            OwnerRef::Group(_) => unreachable!(),
        };
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
async fn export_uses_one_detached_read_only_connection_and_allowlist() {
    let (db_name, pg, lifecycle) = fresh_pg(1).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let fact = ingest(&pg, owner, "export-source", "export-fact").await;
        let bytes = [0, 1, 0x80, 0xfe, 0xff];
        seed_host_fact(pool, owner, fact, "export-source", &bytes).await;
        seed_metadata(pool, owner).await;
        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: match owner {
                OwnerRef::Personal(id) => id,
                OwnerRef::Group(_) => unreachable!(),
            },
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
            ExportFault::ForeignOwnerKind,
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
    result.expect("detached max-1 export oracle failed");
}

const ADVISORY_WAITERS_SQL: &str = "\
SELECT count(*)::bigint
  FROM pg_locks
 WHERE locktype = 'advisory'
   AND NOT granted
   AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

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
async fn erase_fence_orders_write_session_and_export_snapshot_and_cancellation_closes_session() {
    let (db_name, pg, lifecycle) = fresh_pg(4).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let OwnerRef::Personal(user_id) = owner else {
            unreachable!()
        };
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

        let writer_pg = pg.clone();
        let writer =
            tokio::spawn(async move { WriteSessionFactory::begin(writer_pg.as_ref()).await });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "host-capable UoW waits at global entry fence"
        );
        assert!(!writer.is_finished(), "UoW cannot enter during erasure");

        let export_pg = pg.clone();
        let export = tokio::spawn(async move {
            let auth =
                ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
            export_pg
                .export_owner_bundle(&auth, export_pg.surfaces())
                .await
        });
        assert!(
            wait_for_advisory_waiters(pool, 2).await,
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
        let session = writer.await??;
        session.commit().await?;

        // A detached export cancelled while waiting on an owner session lock
        // must close, not return, the locked backend connection to the pool.
        let mut owner_lock = pool.acquire().await?;
        let lock_id = sqlx::query_scalar::<_, i64>(
            "SELECT hashtextextended('proxima-owner-fence:personal:' || $1::text, 0)",
        )
        .bind(user_id.into_inner())
        .fetch_one(&mut *owner_lock)
        .await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock_id)
            .execute(&mut *owner_lock)
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
            wait_for_advisory_waiters(pool, 1).await,
            "export is waiting on owner session fence"
        );
        cancel.abort();
        let _ = cancel.await;
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(lock_id)
            .execute(&mut *owner_lock)
            .await?;
        drop(owner_lock);
        let recovered =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        let bundle = pg.export_owner_bundle(&recovered, pg.surfaces()).await?;
        assert_eq!(
            bundle.counts.get(FACTS_TABLE),
            Some(&0),
            "pool and detached session recover after cancellation"
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
        let mut session = WriteSessionFactory::begin(pg.as_ref()).await?;
        session.advisory_xact_lock(55_019).await?;
        let reverse_gate = Arc::new(CallbackGate::default());
        lifecycle.set_erase_gate(Some(reverse_gate.clone()));
        let erase_pg = pg.clone();
        let erase_surfaces = pg.surfaces().clone();
        let erase_source = reverse_source.clone();
        let reverse_erase = tokio::spawn(async move {
            let auth = erase_auth(owner, Some(erase_source.clone()));
            erase_pg
                .erase_personal_source_scope(&auth, user_id, &erase_source, &erase_surfaces)
                .await
        });
        assert!(
            wait_for_advisory_waiters(pool, 1).await,
            "erase waits behind a UoW that entered the shared lifecycle fence first"
        );
        assert!(!reverse_erase.is_finished());
        session.commit().await?;
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let user_id = match owner {
            OwnerRef::Personal(id) => id,
            OwnerRef::Group(_) => unreachable!(),
        };
        let fact = ingest(&pg, owner, "snapshot-source", "snapshot-fact").await;
        seed_host_fact(pool, owner, fact, "snapshot-source", &[0x10, 0x20]).await;
        let delegation_id = Uuid::now_v7();
        let subject_id = Uuid::now_v7();
        let mut seed = pool.begin().await?;
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors
                (owner_kind, owner_id, source, cursor)
             VALUES ('personal', $1, 'snapshot-source', decode('aabb', 'hex'))",
        )
        .bind(user_id.into_inner())
        .execute(&mut *seed)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                (delegation_id, subject_user_id, owner_kind, owner_id, tool_name,
                 read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at)
             VALUES ($1, $2, 'personal', $3, 'core_remember', 'fact', 'fact',
                     TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                     TIMESTAMPTZ '2026-01-01 00:00:00Z')",
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
              WHERE owner_kind = 'personal' AND owner_id = $1 AND source = 'snapshot-source'",
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
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let OwnerRef::Personal(user_id) = owner else {
            unreachable!()
        };
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

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let OwnerRef::Personal(user_id) = owner else {
            unreachable!()
        };
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
            WriteSessionFactory::begin(pg.as_ref()),
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
async fn cancelling_max_one_export_waiting_on_owner_lock_closes_its_detached_connection() {
    let (db_name, pg, _lifecycle) = fresh_pg(1).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let user_id = match owner {
            OwnerRef::Personal(id) => id,
            OwnerRef::Group(_) => unreachable!(),
        };
        ingest(&pg, owner, "cancel-source", "cancel-fact").await;
        let barrier_pool = PgPool::connect(&db_url(&db_name)).await?;
        let mut barrier = barrier_pool.acquire().await?;
        let lock_id = sqlx::query_scalar::<_, i64>(
            "SELECT hashtextextended('proxima-owner-fence:personal:' || $1::text, 0)",
        )
        .bind(user_id.into_inner())
        .fetch_one(&mut *barrier)
        .await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock_id)
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
        assert!(wait_for_advisory_waiters(pg.pool_for_tests(), 1).await);
        export.abort();
        let _ = export.await;
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(lock_id)
            .execute(&mut *barrier)
            .await?;
        drop(barrier);
        barrier_pool.close().await;

        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id });
        let bundle = pg.export_owner_bundle(&auth, pg.surfaces()).await?;
        assert_eq!(bundle.owner, owner);
        Ok(())
    }
    .await;
    close_pg(&db_name).await;
    result.expect("cancelled one-slot export must release the detached connection");
}
