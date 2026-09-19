//! Transaction-bound inverse and snapshot callbacks for host-owned state.

use proxima_core::StorageError;
use proxima_core::storage_ports::{
    HostStateEraseReceipt, HostStateEraseRequest, HostStateExportReceipt, HostStateExportRequest,
    HostStateParticipantId, StateSurfaceName,
};
use sqlx::{Postgres, Transaction};

/// Registered on the same host participant that owns lifecycle-managed
/// surfaces. Both methods are required: a participant cannot declare a
/// managed surface while leaving one inverse unimplemented.
///
/// Callbacks run inside Proxima's existing transaction/snapshot. They may
/// execute transaction-local SQL only; they must not perform external
/// effects or wait for external operations. This boundary is not a SQL
/// sandbox: a trusted implementation can still use temporary tables,
/// functions, advisory locks, or out-of-band connections.
#[async_trait::async_trait]
pub trait PgHostStateLifecyclePort: Send + Sync {
    /// Must equal the participant ID captured from the actual registered
    /// `PgHostStateParticipant`.
    fn participant_id(&self) -> HostStateParticipantId;

    /// Exact host-owned lifecycle tables; boot checks the full set against
    /// the frozen flavor contracts before constructing an engine.
    fn declared_tables(&self) -> &'static [StateSurfaceName];

    /// Remove or scrub exactly the requested scope on the core erase
    /// transaction and return one count row for every declared lifecycle
    /// table, including zeroes and retained tables.
    async fn erase(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: HostStateEraseRequest,
    ) -> Result<HostStateEraseReceipt, StorageError>;

    /// Read every included table from the active whole-owner export snapshot.
    /// Return one row vector for each requested table, including empty ones.
    async fn export(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        request: HostStateExportRequest,
    ) -> Result<HostStateExportReceipt, StorageError>;
}
