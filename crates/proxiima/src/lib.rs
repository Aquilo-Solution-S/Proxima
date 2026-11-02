//! Embedded-engine facade: env config -> migrations -> compose -> start.
//!
//! Wraps the blessed `Engine::try_compose` embedding entry point
//! (`proxiima_core::engine`) for host binaries. Host wiring template:
//! `apps/proxiima-mcp`. Cohabitation contract: core, spiices, and the
//! host's own sqlx migrations share one database; core records into the
//! default `_sqlx_migrations` and each spiice into its own tracking table
//! (see the `migrations` module). Migration ledgers reject versions absent
//! from the composed binary unless the host explicitly opts in through
//! [`MissingMigrationPolicy`]. Host tables stay outside the core and
//! per-spiice schemas.
//!
//! # Public surface tiers
//!
//! This facade exposes two intentional, supported tiers:
//!
//! - **Host entry point (most hosts use only this):** [`Proxiima`], [`run`],
//!   [`RuntimeBuilder`]/[`RuntimeConfig`], and the `from_env` → migrate →
//!   compose → `build`/`run` flow. Every runtime feature starts when its
//!   config is present ([`Feature`], [`BootReport`]); the booted handle's
//!   host accessors are one [`ProxiimaHost`]. A host binary that just stands
//!   up the MCP server needs nothing below this line.
//! - **Spiice SDK:** import from `proxiima::spiice`, not the root facade. The
//!   SDK exposes payload traits, ids, `proxiima_spiice!`, `Tool`/`ToolCtx`,
//!   payload reference declarations, sidecar macros/traits, and registry types. It does
//!   not expose raw `PgPool`, raw storage verbs, or proofless append helpers.
//!   Host extra-table wiring uses [`ProxiimaHost::clone_pool_for_host`] plus
//!   [`ProxiimaHost::pg_tuning_for_host`] (via [`AppContext::host`]) inside
//!   [`SpiiceApp::services`] and wraps them immediately. Atomic host-state with Fact writes uses
//!   [`crate::UnitOfWork::apply_host_state`] on a startup-registered
//!   [`crate::PgHostStateParticipant`], not the extra-table pool.
//!   Spiice crates should avoid direct `proxiima-core` / `proxiima-storage-pg`
//!   dependencies except backend-owned adapters explicitly outside the stable
//!   SDK boundary.
//!
//! One rule governs both tiers: a type that appears in a public signature of
//! this facade is nameable through it, in the tier of the signature that
//! exposes it. A host-tier signature gets its types at the root (`host.rs`
//! groups them by the signature that needs them); a Spiice SDK signature gets
//! its types under `proxiima::spiice` ([`spiice::HostStateEraseDisposition`], for
//! [`spiice::EraseRule::HostState`], is the latest). Host-only names stay out of
//! the SDK. Third-party crates stay the host's own dependencies, with one
//! exception: [`rmcp`] is re-exported at the version and feature set the
//! workspace pins, because a host implements its `ServerHandler` around
//! [`DynamicHandler`].
//!
//! The enforcer is `tests/facade_signature_names.rs`: it imports every name the
//! rule has put on the facade, through `proxiima::` or `proxiima::spiice::`, so a
//! dropped re-export fails to compile, and it fails when a host-only name shows
//! up in the SDK. It is a list: add the type to `host.rs` and to a group there
//! whenever a public signature names a new type. Nothing finds a signature
//! whose type is missing from the list.
//!
//! The SDK names no storage handle and no owner-scoped transaction:
//!
//! ```compile_fail,E0432
//! use proxiima::spiice::PgStorage;
//! ```
//!
//! ```compile_fail,E0432
//! use proxiima::spiice::begin_owner_transaction;
//! ```
//!
//! The host-only capability and its permit cannot be caller-constructed:
//!
//! ```compile_fail
//! use proxiima::HostStateMaintenanceAuthority;
//! let _authority = HostStateMaintenanceAuthority::new();
//! ```
//!
//! ```compile_fail
//! fn clone_authority(authority: &proxiima::HostStateMaintenanceAuthority) {
//!     let _copy = Clone::clone(authority);
//! }
//! ```
//!
//! ```compile_fail
//! use proxiima::{
//!     HostStateParticipantId, HostStateWriteOrigin, HostStateWritePermit, OwnerId, StateSurfaceName,
//! };
//! let _permit = HostStateWritePermit::new(
//!     OwnerId::from(proxiima::UserId::new(uuid::Uuid::nil())),
//!     HostStateParticipantId::new("fixture"),
//!     &[StateSurfaceName::new("fixture.state")],
//!     HostStateWriteOrigin::Maintenance,
//! );
//! ```

//! ```compile_fail
//! use proxiima::{HostStateWriteOrigin, HostStateWritePermit};
//! fn overwrite_origin(permit: &mut HostStateWritePermit) {
//!     permit.origin = HostStateWriteOrigin::Maintenance;
//! }
//! ```
//!
//! A host-state stamp cannot be widened into an ordinary owner write permit:
//!
//! ```compile_fail
//! use proxiima::HostStateWritePermit;
//! fn widen(permit: HostStateWritePermit) -> proxiima_core::storage_ports::OwnerWritePermit {
//!     permit.into()
//! }
//! ```
//!
//! The maintenance unit intentionally has no cognitive write or read methods:
//!
//! ```compile_fail
//! fn fact(unit: &mut proxiima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.ingest_fact;
//! }
//! ```
//!
//! ```compile_fail
//! fn goal(unit: &mut proxiima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.create_goal;
//! }
//! ```
//!
//! ```compile_fail
//! fn read_or_forget(unit: &mut proxiima::HostStateUnitOfWork<'_>) {
//!     let _ = unit.owned_series_head_memory_id;
//!     let _ = unit.forget;
//! }
//! ```
//!
//! Spiice SDK imports cannot name the host-only authority:
//!
//! ```compile_fail
//! use proxiima::spiice::HostStateMaintenanceAuthority;
//! ```

mod app;
#[cfg(feature = "auth-oidc")]
pub mod auth;
mod boot;
mod bundle;
mod config;
mod core_mcp;
mod features;
mod health;
pub mod host;
mod in_process_outbox;
mod mcp_edge;
mod migrations;
mod object_maintenance;
mod outbox_http;
#[cfg(test)]
mod outbox_test_fixture;
mod owner_access;
mod proxiima_host;
mod runtime;
mod runtime_config;
mod runtime_grants;
pub mod spiice;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod vector_index_http;
mod workers;

pub use host::*;
pub use object_maintenance::{ObjectMaintenanceOutcome, object_maintenance_pass};
pub use proxiima_core::authz::SystemAuthority;

/// One Owner per embedded host: a Group principal.
///
/// This is the single place embedded hosts construct `Owner`. `Owner` is
/// `OwnerId` and carries no org scalar: tenancy is a spiice/app concern,
/// not a substrate one.
#[must_use]
pub fn company_owner(id: uuid::Uuid) -> OwnerId {
    OwnerId::from(GroupId::new(id))
}

#[cfg(test)]
mod tests {
    use proxiima_core::OwnerId;

    #[test]
    fn company_owner_is_the_bare_company_id() {
        let id = uuid::Uuid::now_v7();
        let owner = super::company_owner(id);
        assert_eq!(owner, OwnerId::new(id));
        assert_eq!(owner.as_group().into_inner(), id);
    }
}
