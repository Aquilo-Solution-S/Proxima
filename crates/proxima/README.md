# proxima

Framework facade for host applications. Most hosts should start here rather than
assembling `proxima-core` directly.

Public tiers:

1. Host entry point: `Proxima`, `RuntimeBuilder`, `RuntimeConfig`, `run`.
2. Extension API for flavor authors: selected re-exports from core/storage traits.

See `src/lib.rs` rustdoc and [`../../docs/reference/public-api.md`](../../docs/reference/public-api.md).
