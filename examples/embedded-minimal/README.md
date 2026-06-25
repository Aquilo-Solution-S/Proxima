# embedded-minimal

Canonical minimal host-binary embedding example.

## Run

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p embedded-minimal
```

## What It Shows

- Host app composition through the `proxima` facade.
- Minimal flavor registration.
- Typed Fact sidecar wiring.
- Query/ingest flow suitable for copying into a new host.

## Modify It

Start by editing `src/flavor.rs` to add one typed Fact schema, then add the
sidecar migration and registration described in [`../../docs/tutorials/build-first-flavor.md`](../../docs/tutorials/build-first-flavor.md).

Detailed steps:

- [Add your first Fact schema](../../docs/tutorials/add-first-fact-schema.md)
- [Add your first MCP tool](../../docs/tutorials/add-first-mcp-tool.md)
