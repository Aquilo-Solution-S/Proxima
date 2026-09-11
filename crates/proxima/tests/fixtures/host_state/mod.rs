//! Synthetic embedding-host fixture: one declared state surface, typed
//! create/finalize/read commands, and an always-compiled fault seam.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

use async_trait::async_trait;
use proxima::flavor::{
    CounterRule, EraseRule, ExportRule, FlavorBundle, FlavorContract, FlavorRegistry,
    FlavorRegistryError, ForgetRule, KeyShape, NamedMigrator, ProjectionDecl, Surface,
    TransferRule,
};
use proxima::{AppInfo, FlavorApp, HostStateCommand, HostStateReply, Owner};
use proxima_core::storage_ports::{HostStateRequest, OwnerWritePermit};
use proxima_core::{MemoryId, StorageError};
use proxima_storage_pg::PgHostStateParticipant;
use sqlx::{Postgres, Row, Transaction};

pub const PARTICIPANT_ID: &str = "host_fixture";
pub const EXECUTION_TABLE: &str = "host_fixture.execution";
pub const TABLES: &[&str] = &[EXECUTION_TABLE];

const STATE_SURFACES: &[Surface] = &[Surface {
    table: EXECUTION_TABLE,
    key: KeyShape::Custom(&["invocation_id"]),
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "execution state is host-owned and does not follow transferred memories",
    },
    erase: EraseRule::ByOwner,
    export: ExportRule::Rows,
    forget: ForgetRule::Keep {
        why: "forgetting one Fact must not drop the host's execution row",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("host_fixture_execution_rows"),
    completeness: None,
}];

const CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "host_fixture",
    ordinal: 100,
    schemas: &[],
    state_surfaces: STATE_SURFACES,
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "a host-state fixture has no search surface",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxima::flavor::proxima_flavor! {
    name = "host_fixture",
    display_name = "Host fixture",
    contract = &CONTRACT,
}

pub struct HostFixtureApp;

impl FlavorBundle for HostFixtureApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        self::register(registry)
    }

    fn migrators() -> Vec<NamedMigrator> {
        let mut migrator = sqlx::migrate!("tests/fixtures/host_state/migrations");
        migrator.dangerous_set_table_name("public._sqlx_migrations_host_fixture");
        migrator.set_ignore_missing(true);
        vec![NamedMigrator::new("host_fixture", migrator)]
    }
}

impl FlavorApp for HostFixtureApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "host_fixture",
            title: "host-fixture",
            version: "0",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRow {
    pub invocation_id: MemoryId,
    pub status: String,
    pub version: i32,
}

#[derive(Debug, Clone)]
pub enum FixtureHostCommand {
    Create {
        owner: Owner,
        invocation_id: MemoryId,
    },
    Finalize {
        owner: Owner,
        invocation_id: MemoryId,
    },
    Read {
        owner: Owner,
        invocation_id: MemoryId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureHostResult {
    Created {
        invocation_id: MemoryId,
        version: i32,
    },
    Finalized {
        invocation_id: MemoryId,
        version: i32,
    },
    Row(Option<ExecutionRow>),
    Conflict {
        invocation_id: MemoryId,
        status: String,
        version: i32,
    },
    Missing,
}

impl HostStateCommand for FixtureHostCommand {
    const PARTICIPANT_ID: &'static str = PARTICIPANT_ID;
    const TABLES: &'static [&'static str] = TABLES;
    type Outcome = FixtureHostResult;

    fn owner(&self) -> Owner {
        match *self {
            Self::Create { owner, .. }
            | Self::Finalize { owner, .. }
            | Self::Read { owner, .. } => owner,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvalidBindingCommand {
    pub owner: Owner,
}

impl HostStateCommand for InvalidBindingCommand {
    const PARTICIPANT_ID: &'static str = PARTICIPANT_ID;
    const TABLES: &'static [&'static str] = &["proxima_core.memory"];
    type Outcome = ();

    fn owner(&self) -> Owner {
        self.owner
    }
}

#[derive(Debug, Clone)]
pub struct UnknownParticipantCommand {
    pub owner: Owner,
}

#[derive(Debug, Clone)]
pub struct CoreStateSurfaceCommand {
    pub owner: Owner,
}

impl HostStateCommand for CoreStateSurfaceCommand {
    const PARTICIPANT_ID: &'static str = PARTICIPANT_ID;
    const TABLES: &'static [&'static str] = &["proxima_core.goal"];
    type Outcome = ();

    fn owner(&self) -> Owner {
        self.owner
    }
}

impl HostStateCommand for UnknownParticipantCommand {
    const PARTICIPANT_ID: &'static str = "not-registered";
    const TABLES: &'static [&'static str] = TABLES;
    type Outcome = ();

    fn owner(&self) -> Owner {
        self.owner
    }
}

#[allow(clippy::struct_field_names)]
pub struct HostFixtureParticipant {
    pub fail_before_sql: AtomicBool,
    pub fail_after_sql: AtomicBool,
    pub hang_after_sql: AtomicBool,
    sql_completed: Notify,
}

impl Default for HostFixtureParticipant {
    fn default() -> Self {
        Self {
            fail_before_sql: AtomicBool::new(false),
            fail_after_sql: AtomicBool::new(false),
            hang_after_sql: AtomicBool::new(false),
            sql_completed: Notify::new(),
        }
    }
}

impl std::fmt::Debug for HostFixtureParticipant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostFixtureParticipant")
            .field("fail_before_sql", &self.fail_before_sql)
            .field("fail_after_sql", &self.fail_after_sql)
            .field("hang_after_sql", &self.hang_after_sql)
            .finish_non_exhaustive()
    }
}

impl HostFixtureParticipant {
    pub fn arm_fail_before_sql(&self) {
        self.fail_before_sql.store(true, Ordering::SeqCst);
    }

    pub fn arm_fail_after_sql(&self) {
        self.fail_after_sql.store(true, Ordering::SeqCst);
    }

    pub fn arm_hang_after_sql(&self) {
        self.hang_after_sql.store(true, Ordering::SeqCst);
    }

    /// Subscribe before `apply_host_state`. Completes after host SQL on this
    /// unit has succeeded and before the optional hang.
    pub fn sql_completed(&self) -> tokio::sync::futures::Notified<'_> {
        self.sql_completed.notified()
    }
}

#[async_trait]
impl PgHostStateParticipant for HostFixtureParticipant {
    fn participant_id(&self) -> &'static str {
        PARTICIPANT_ID
    }

    fn declared_tables(&self) -> &'static [&'static str] {
        TABLES
    }

    async fn apply(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &OwnerWritePermit,
        request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError> {
        if self.fail_before_sql.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Internal(
                "injected host-state failure before SQL".into(),
            ));
        }
        let command = request.downcast::<FixtureHostCommand>()?;
        if command.owner() != *permit.owner() {
            return Err(StorageError::ConstraintViolation(
                "host-state command owner does not match write permit".into(),
            ));
        }
        let reply = dispatch(tx, permit, command).await?;
        if self.fail_after_sql.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Internal(
                "injected host-state failure after SQL".into(),
            ));
        }
        if self.hang_after_sql.swap(false, Ordering::SeqCst) {
            self.sql_completed.notify_waiters();
            #[allow(clippy::infinite_loop)]
            loop {
                tokio::time::sleep(std::time::Duration::from_hours(1)).await;
            }
        }
        Ok(reply)
    }
}

async fn dispatch(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    command: FixtureHostCommand,
) -> Result<HostStateReply, StorageError> {
    match command {
        FixtureHostCommand::Create { invocation_id, .. } => create(tx, permit, invocation_id).await,
        FixtureHostCommand::Finalize { invocation_id, .. } => {
            finalize(tx, permit, invocation_id).await
        }
        FixtureHostCommand::Read { invocation_id, .. } => read(tx, permit, invocation_id).await,
    }
}

async fn create(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    invocation_id: MemoryId,
) -> Result<HostStateReply, StorageError> {
    let (kind, owner_id) = permit.owner().columns();
    // Same-transaction probe: a second connection cannot see the Fact
    // ingested earlier in this unit. A participant that uses another
    // pooled connection fails this SELECT and cannot create the row.
    let fact_visible: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE t = $1 AND owner_id = $2")
            .bind(invocation_id.into_inner())
            .bind(owner_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
    if fact_visible.is_none() {
        return Ok(HostStateReply::refused(FixtureHostResult::Missing));
    }
    let inserted = sqlx::query(
        "INSERT INTO host_fixture.execution (invocation_id, owner_kind, owner_id, status, version)
         VALUES ($1, $2, $3, 'created', 1)
         ON CONFLICT (invocation_id) DO NOTHING
         RETURNING version",
    )
    .bind(invocation_id.into_inner())
    .bind(kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;
    if inserted.is_some() {
        return Ok(HostStateReply::permitted(FixtureHostResult::Created {
            invocation_id,
            version: 1,
        }));
    }
    conflict_or_applied(tx, permit, invocation_id, true).await
}

async fn finalize(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    invocation_id: MemoryId,
) -> Result<HostStateReply, StorageError> {
    let (kind, owner_id) = permit.owner().columns();
    let updated: Option<i32> = sqlx::query_scalar(
        "UPDATE host_fixture.execution
         SET status = 'finalized', version = version + 1
         WHERE invocation_id = $1 AND owner_kind = $2 AND owner_id = $3 AND status = 'created'
         RETURNING version",
    )
    .bind(invocation_id.into_inner())
    .bind(kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;
    if let Some(version) = updated {
        return Ok(HostStateReply::permitted(FixtureHostResult::Finalized {
            invocation_id,
            version,
        }));
    }
    conflict_or_applied(tx, permit, invocation_id, false).await
}

async fn read(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    invocation_id: MemoryId,
) -> Result<HostStateReply, StorageError> {
    let (kind, owner_id) = permit.owner().columns();
    let row = sqlx::query(
        "SELECT invocation_id, status, version FROM host_fixture.execution
         WHERE invocation_id = $1 AND owner_kind = $2 AND owner_id = $3",
    )
    .bind(invocation_id.into_inner())
    .bind(kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;
    let result = FixtureHostResult::Row(row.map(|row| ExecutionRow {
        invocation_id: MemoryId::new(row.get("invocation_id")),
        status: row.get("status"),
        version: row.get("version"),
    }));
    Ok(HostStateReply::permitted(result))
}

async fn conflict_or_applied(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    invocation_id: MemoryId,
    creating: bool,
) -> Result<HostStateReply, StorageError> {
    let (kind, owner_id) = permit.owner().columns();
    let row = sqlx::query(
        "SELECT status, version FROM host_fixture.execution
         WHERE invocation_id = $1 AND owner_kind = $2 AND owner_id = $3",
    )
    .bind(invocation_id.into_inner())
    .bind(kind)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| StorageError::Internal(err.to_string()))?;
    let Some(row) = row else {
        return Ok(HostStateReply::refused(FixtureHostResult::Missing));
    };
    let status: String = row.get("status");
    let version: i32 = row.get("version");
    if creating {
        let result = FixtureHostResult::Created {
            invocation_id,
            version,
        };
        return Ok(if status == "created" {
            HostStateReply::already_applied(result)
        } else {
            HostStateReply::already_applied(FixtureHostResult::Finalized {
                invocation_id,
                version,
            })
        });
    }
    if status == "finalized" {
        return Ok(HostStateReply::already_applied(
            FixtureHostResult::Finalized {
                invocation_id,
                version,
            },
        ));
    }
    Ok(HostStateReply::refused(FixtureHostResult::Conflict {
        invocation_id,
        status,
        version,
    }))
}

#[allow(clippy::cast_possible_truncation)]
pub fn invocation_lock_key(id: MemoryId) -> i64 {
    let value = id.into_inner().as_u128();
    (value as i64) ^ ((value >> 64) as i64)
}
