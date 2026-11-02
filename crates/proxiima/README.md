# proxiima

Framework facade for host applications. Most hosts should start here rather than
assembling `proxiima-core` directly. The compiling host template is
[`apps/proxiima-mcp`](../../apps/proxiima-mcp).

Public tiers:

1. Host entry point: `Proxiima`, `RuntimeBuilder`, `RuntimeConfig`, `run`.
2. Extension API for spiice authors: selected re-exports from core/storage ports.

`PgPoolConfig` is the programmatic pool-policy block. Set it with
`RuntimeBuilder::pg_pool_config` / `Proxiima::pg_pool_config`, or resolve the
same `PROXIIMA_PG_*` variables through `Proxiima::from_lookup`.

See `src/lib.rs` rustdoc and [`../../docs/host-integration.md`](../../docs/host-integration.md).

Membership writes use `proxiima::host` inside a host's platform-scoped
`READ COMMITTED` transaction. Members are bare `OwnerId`; the write resolves
kind under the tree locks. A group has one parent at most. Database triggers
materialize upward `Role::meet` caps and `Role::join` across paths into
`group_role_projection`; `roles_for_subject` and `group_role` read that
projection in one query; `direct_group_relations` reads the rows written
for a person, neither inherited nor folded. Fixed-snapshot `REPEATABLE READ` and `SERIALIZABLE`
writes return `StorageError::MembershipIsolation`. The host forms run no
authorization/hooks ([host integration](../../docs/host-integration.md)).
