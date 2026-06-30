# Public API Reference

## Current Consumption Mode

Workspace packages currently set `publish = false`; consume from git tags or
repo checkouts unless release notes say crates.io publishing is available.

## Main Entry Point

Use `crates/proxima` for host apps.

| Tier | Import | Use |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, Engine};` | boot and run composed binaries; call graph verbs with server-resolved authz |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, pg_sidecar};` | build-time schema/relation/tool/sidecar registration |

Flavor crates must not depend on `sqlx::PgPool` as a stable API, must not
query `proxima_core.*`, and must not call proofless storage append helpers.
Use Flavor SDK helpers and typed services instead.

## Generated Rustdoc

Build locally:

```sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked --open
```

CI treats rustdoc warnings as failures.
