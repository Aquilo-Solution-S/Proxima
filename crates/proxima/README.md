# proxima

Framework facade for host applications. Most hosts should start here rather than
assembling `proxima-core` directly. The compiling host template is
[`apps/proxima-mcp`](../../apps/proxima-mcp).

Public tiers:

1. Host entry point: `Proxima`, `RuntimeBuilder`, `RuntimeConfig`, `run`.
2. Extension API for flavor authors: selected re-exports from core/storage ports.

`PgPoolConfig` is the programmatic pool-policy block. Set it with
`RuntimeBuilder::pg_pool_config` / `Proxima::pg_pool_config`, or resolve the
same `PROXIMA_PG_*` variables through `Proxima::from_lookup`.

See `src/lib.rs` rustdoc and [`../../docs/reference/public-api.md`](../../docs/reference/public-api.md).
