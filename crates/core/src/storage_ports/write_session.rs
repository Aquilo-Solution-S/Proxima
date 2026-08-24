//! Backend-owned write session: one transaction, several Engine writes.

use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, StorageError};
use crate::storage_ports::OwnerWritePermit;
use crate::verbs::fact_ingest::{AuthorizedFactWrite, FactIngestOutcome};
use crate::verbs::goal_write::{CreateGoalAtomicRequest, GoalWriteOutcome};
use crate::verbs::query::SidecarAtom;
use crate::{MemoryId, SchemaId, SidecarPayload};

/// Backend ceiling on one [`WriteSession::read_own_sidecar`] answer.
///
/// A precondition check reads the few rows a decision turns on. A read that
/// wants more than this is a query, and queries go through the read ports.
pub const SIDECAR_SESSION_READ_MAX_ROWS: u32 = 512;

/// One flavor-owned read of its OWN registered sidecar surface, run inside
/// the write session's transaction.
///
/// This is the read half of `read → check → append`: a flavor that must see
/// its own rows before deciding what to append needs them read under the
/// same advisory lock and the same snapshot as the append. A pool-scoped
/// read cannot give it either, which is the whole reason a raw transaction
/// used to be worth reaching for.
///
/// **Invariant — read-only by construction, not by inspection.** No caller
/// statement crosses this boundary. The backend emits the whole statement
/// from the fields below: one `SELECT` over one registered sidecar table,
/// with equality predicates on that table's own columns and every value
/// bound. There is no position in which a write, a DDL statement, a second
/// statement, or a core-table reference could be expressed, so nothing has
/// to be scanned for keywords. The session hands out no transaction, no
/// connection, and no pool.
///
/// **Invariant — the owner scope is stamped, never asked for.** The backend
/// appends `AND <owner column> = <the permit's owner>` from the resolved
/// [`OwnerWritePermit`], off the table's declared `Surface`. The predicates
/// below can only NARROW that scope; there is no predicate a caller can
/// write that widens it, and none that reaches another owner's rows. A
/// surface declaring no owner column is REFUSED rather than read unscoped.
#[derive(Debug, Clone, Copy)]
pub struct SidecarSessionRead<'a> {
    /// The sidecar table to read. Must be a registered memory-sidecar table
    /// of the frozen registry — that registration IS the declared surface
    /// this read is validated against.
    pub table: &'a str,
    /// `AND`-joined equality predicates over that table's own columns, on
    /// top of the server-stamped owner scope.
    pub predicates: &'a [(&'a str, SidecarAtom)],
    /// Row cap. `None` applies [`SIDECAR_SESSION_READ_MAX_ROWS`], which is
    /// also the ceiling on any value given.
    pub limit: Option<u32>,
}

/// Opens a backend-owned write session (one transaction).
#[async_trait::async_trait]
pub trait WriteSessionFactory: Send + Sync {
    /// Begin a transaction. Drop without [`WriteSession::commit`] rolls back.
    async fn begin(&self) -> Result<Box<dyn WriteSession>, StorageError>;
}

/// One transaction the Engine can attach several authorized writes to.
#[async_trait::async_trait]
pub trait WriteSession: Send {
    async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), StorageError>;

    /// Read the flavor's own sidecar rows INSIDE this session's transaction.
    ///
    /// Sees this session's uncommitted writes and is serialized by whatever
    /// [`Self::advisory_xact_lock`] the session already took — which is what
    /// makes a precondition check binding instead of advisory.
    ///
    /// Rows come back as JSON objects of the sidecar's own columns; the
    /// flavor deserializes them into its own row type. That is a read
    /// projection of a typed row, not an untyped payload store: the sidecar
    /// remains the typed surface, and nothing is written through this path.
    ///
    /// The rows are the PERMIT'S OWNER'S rows: the backend stamps the owner
    /// scope from `permit` onto the statement, off the table's declared
    /// `Surface`. `read`'s predicates narrow within that scope and cannot
    /// widen it — a caller never names the owner it reads as. See
    /// [`SidecarSessionRead`] for both invariants.
    ///
    /// # Errors
    ///
    /// `ConstraintViolation` when the table is not a registered memory
    /// sidecar, when its surface declares no owner column to scope by, when
    /// a table or column name is not a legal identifier, or when the
    /// predicate list is empty. Storage errors from the read.
    async fn read_own_sidecar(
        &mut self,
        permit: &OwnerWritePermit,
        read: &SidecarSessionRead<'_>,
    ) -> Result<Vec<serde_json::Value>, StorageError>;

    /// Current owned series head whose sidecar columns match, read INSIDE
    /// this session's transaction.
    ///
    /// The transaction-scoped twin of the pool-scoped natural-key lookup
    /// (`MemoryReadPort::owned_series_handle`). "Is there already a live row
    /// for this key" is the other half of a precondition check, and asking
    /// it on the pool answers about a snapshot the append will not be
    /// written against.
    ///
    /// # Errors
    ///
    /// `ConstraintViolation` when the table is not a registered memory
    /// sidecar, when a column name is not a legal identifier, or when no
    /// column is given. Storage errors from the read.
    async fn owned_series_head_memory_id(
        &mut self,
        permit: &OwnerWritePermit,
        schema_id: &SchemaId,
        sidecar_table: &str,
        columns: &[(&str, SidecarAtom)],
    ) -> Result<Option<MemoryId>, StorageError>;

    async fn ingest_fact_with_typed_sidecar(
        &mut self,
        authorized: &AuthorizedFactWrite,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError>;

    async fn author_derived(
        &mut self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<AuthorDerivedOutcome, StorageError>;

    async fn forget_memory(
        &mut self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError>;

    async fn create_goal(
        &mut self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError>;

    async fn commit(self: Box<Self>) -> Result<(), StorageError>;
}
