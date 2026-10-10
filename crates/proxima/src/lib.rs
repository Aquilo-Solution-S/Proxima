//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::try_compose` embedding entry point
//! (`proxima_core::engine`) for host binaries. Host wiring template:
//! `apps/proxima-mcp`. Cohabitation contract: core, flavors, and the
//! host's own sqlx migrations share one database; core records into the
//! default `_sqlx_migrations` and each flavor into its own tracking table
//! (see the `migrations` module). Every migrator in that database must set
//! `ignore_missing(true)`, and host tables must stay out of the
//! `proxima_core` / per-flavor schemas.
//!
//! # Public surface tiers
//!
//! This facade exposes two intentional, supported tiers:
//!
//! - **Host entry point (most hosts use only this):** [`Proxima`], [`run`],
//!   [`RuntimeBuilder`]/[`RuntimeConfig`], and the `from_env` → migrate →
//!   compose → `build`/`run` flow. Every runtime feature starts when its
//!   config is present ([`Feature`], [`BootReport`]); the booted handle's
//!   host accessors are one [`ProximaHost`]. A host binary that just stands
//!   up the MCP server needs nothing below this line.
//! - **Flavor SDK:** import from `proxima::flavor`, not the root facade. The
//!   SDK exposes payload traits, ids, `proxima_flavor!`, `Tool`/`ToolCtx`,
//!   relation descriptors, sidecar macros/traits, and registry types. It does
//!   not expose raw `PgPool`, raw storage verbs, or proofless append helpers.
//!   Host extra-table wiring uses [`ProximaHost::clone_pool_for_host`] plus
//!   [`ProximaHost::pg_tuning_for_host`] (via [`AppContext::host`]) inside
//!   [`FlavorApp::services`] and wraps them immediately. Atomic host-state with Fact writes uses
//!   [`crate::UnitOfWork::apply_host_state`] on a startup-registered
//!   [`crate::PgHostStateParticipant`], not the extra-table pool.
//!   Flavor crates should avoid direct `proxima-core` / `proxima-storage-pg`
//!   dependencies except backend-owned adapters explicitly outside the stable
//!   SDK boundary.
//!
//! One rule governs both tiers: a type that appears in a public signature of
//! this facade is nameable through it, in the tier of the signature that
//! exposes it. A host-tier signature gets its types at the root (`host.rs`
//! groups them by the signature that needs them); a Flavor SDK signature gets
//! its types under `proxima::flavor` ([`flavor::HostStateEraseDisposition`], for
//! [`flavor::EraseRule::HostState`], is the latest). Host-only names stay out of
//! the SDK. Third-party crates stay the host's own dependencies, with one
//! exception: [`rmcp`] is re-exported at the version and feature set the
//! workspace pins, because a host implements its `ServerHandler` around
//! [`DynamicHandler`].
//!
//! The enforcer is `tests/facade_signature_names.rs`: it imports every name the
//! rule has put on the facade, through `proxima::` or `proxima::flavor::`, so a
//! dropped re-export fails to compile, and it fails when a host-only name shows
//! up in the SDK. It is a list: add the type to `host.rs` and to a group there
//! whenever a public signature names a new type. Nothing finds a signature
//! whose type is missing from the list. The SDK tier has such gaps, known and
//! open; `docs/reference/public-api.md` names them.
//!
//! The SDK names no storage handle and no owner-scoped transaction:
//!
//! ```compile_fail,E0432
//! use proxima::flavor::PgStorage;
//! ```
//!
//! ```compile_fail,E0432
//! use proxima::flavor::begin_owner_transaction;
//! ```
//!
//! The host-only capability and its permit cannot be caller-constructed:
//!
//! ```compile_fail
//! use proxima::HostStateMaintenanceAuthority;
//! let _authority = HostStateMaintenanceAuthority::new();
//! ```
//!
//! ```compile_fail
//! fn clone_authority(authority: &proxima::HostStateMaintenanceAuthority) {
//!     let _copy = Clone::clone(authority);
//! }
//! ```
//!
//! ```compile_fail
//! use proxima::{
//!     HostStateParticipantId, HostStateWriteOrigin, HostStateWritePermit, Owner, StateSurfaceName,
//! };
//! let _permit = HostStateWritePermit::new(
//!     Owner::Personal(proxima::UserId::new(uuid::Uuid::nil())),
//!     HostStateParticipantId::new("fixture"),
//!     &[StateSurfaceName::new("fixture.state")],
//!     HostStateWriteOrigin::Maintenance,
//! );
//! ```

//! ```compile_fail
//! use proxima::{HostStateWriteOrigin, HostStateWritePermit};
//! fn overwrite_origin(permit: &mut HostStateWritePermit) {
//!     permit.origin = HostStateWriteOrigin::Maintenance;
//! }
//! ```
//!
//! A host-state stamp cannot be widened into an ordinary owner write permit:
//!
//! ```compile_fail
//! use proxima::HostStateWritePermit;
//! fn widen(permit: HostStateWritePermit) -> proxima_core::storage_ports::OwnerWritePermit {
//!     permit.into()
//! }
//! ```
//!
//! The maintenance unit intentionally has no cognitive write or read methods:
//!
//! ```compile_fail
//! fn fact(unit: &mut proxima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.ingest_fact;
//! }
//! ```
//!
//! ```compile_fail
//! fn goal(unit: &mut proxima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.create_goal;
//! }
//! ```
//!
//! ```compile_fail
//! fn read_or_forget(unit: &mut proxima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.owned_series_head_memory_id;
//!     let _ = unit.forget;
//! }
//! ```
//!
//! Flavor SDK imports cannot name the host-only authority:
//!
//! ```compile_fail
//! use proxima::flavor::HostStateMaintenanceAuthority;
//! ```

mod app;
#[cfg(feature = "auth-oidc")]
pub mod auth;
mod boot;
mod bundle;
mod config;
mod core_mcp;
mod features;
pub mod flavor;
mod health;
pub mod host;
mod mcp_edge;
mod migrations;
mod owner_access;
mod proxima_host;
mod runtime;
mod runtime_config;
mod runtime_grants;
#[cfg(feature = "testkit")]
pub mod testkit;
mod workers;

pub use host::*;
pub use proxima_core::authz::SystemAuthority;

/// One Owner per embedded host: a Group principal.
///
/// This is the single place embedded hosts construct `Owner`. `Owner` is
/// `OwnerRef` and carries no org scalar: tenancy is a flavor/app concern,
/// not a substrate one.
#[must_use]
pub fn company_owner(id: uuid::Uuid) -> Owner {
    OwnerRef::Group(GroupId::new(id))
}

/// Persist one host-observed MCP tool call through an embedded engine.
///
/// `authz` is the authenticated context of the served MCP call (the
/// host already holds it from dispatch); the engine authorizes the log
/// Owner against it rather than trusting a caller-supplied Owner.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot access the log Owner or lacks an
/// `Ingest`/write-capable owner role for that Owner; or `Internal` on storage
/// failure.
pub async fn log_mcp_call(
    engine: &Engine,
    authz: &AuthzContext,
    input: McpCallLogInput,
) -> Result<McpCallLogOutcome, ProtocolError> {
    engine.persist_mcp_call(authz, input).await
}

/// Read one Owner's MCP-call activity log through an embedded engine.
/// Owner-scoped, `GraphRead`-gated; `req.actor_oid = Some` narrows to one actor.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot access `req.owner` or lacks
/// graph-read, or `Internal` on storage failure / `limit == 0`.
pub async fn read_mcp_call_history(
    engine: &Engine,
    authz: &AuthzContext,
    req: &McpCallHistoryRequest,
) -> Result<McpCallHistoryResponse, ProtocolError> {
    engine.read_mcp_call_history(authz, req).await
}

/// Load one owner-scoped opaque source cursor through an embedded engine.
///
/// Cursor state is projector write-state; the engine gates this read through
/// owner `Ingest` authorization before touching storage.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot write `owner` with `Ingest`, or
/// `Internal` on storage failure.
pub async fn load_source_cursor(
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    source: &str,
) -> Result<Option<Cursor>, ProtocolError> {
    engine.load_source_cursor(authz, owner, source).await
}

/// Store one owner-scoped opaque source cursor through an embedded engine.
///
/// # Errors
///
/// Returns `Forbidden` when `authz` cannot write `owner` with `Ingest`, or
/// `Internal` on storage failure.
pub async fn store_source_cursor(
    engine: &Engine,
    authz: &AuthzContext,
    owner: &Owner,
    source: &str,
    cursor: &Cursor,
) -> Result<(), ProtocolError> {
    engine
        .store_source_cursor(authz, owner, source, cursor)
        .await
}

#[cfg(test)]
mod tests {
    use proxima_core::OwnerRef;

    #[test]
    fn company_owner_is_group_scoped() {
        let id = uuid::Uuid::now_v7();
        let owner = super::company_owner(id);
        assert!(matches!(owner, OwnerRef::Group(_)));
    }
}
