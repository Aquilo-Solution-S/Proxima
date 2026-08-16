# Proxima Documentation

## Start Here

| Goal | Document |
|---|---|
| Run Proxima locally | [getting-started/local-dev.md](getting-started/local-dev.md) |
| Connect an LLM/coding agent | [getting-started/connect-agent.md](getting-started/connect-agent.md) |
| Give an agent compact instructions | [llms.txt](https://github.com/Aquilo-Solution-S/Proxima/blob/main/llms.txt) / [agent/quickstart.md](agent/quickstart.md) |
| Embed Proxima in a Rust host | [getting-started/local-dev.md](getting-started/local-dev.md) |
| Build a flavor | [tutorials/build-first-flavor.md](tutorials/build-first-flavor.md) |
| Add a first Fact schema | [tutorials/add-first-fact-schema.md](tutorials/add-first-fact-schema.md) |
| Add a first MCP tool | [tutorials/add-first-mcp-tool.md](tutorials/add-first-mcp-tool.md) |
| Understand architecture | [reference/architecture.md](reference/architecture.md) |
| Check invariants | [lean/README.md](lean/README.md) |

## Documentation Lanes

| Lane | Audience | Contents |
|---|---|---|
| Getting started | New users | first-run and connection guides |
| Tutorials | Builders | end-to-end learning paths |
| How-to | Operators/maintainers | focused operational recipes |
| Reference | Integrators | architecture, public APIs, env vars, MCP tools, status tables |
| Agent | LLM/coding agents | compact usage rules, root `llms.txt` files, prompt snippets, and JSON examples |
| Design | Maintainers | numbered design docs and Lean kernel |

## Docs Site

Build the local MkDocs site from the repository root:

```sh
python3 -m pip install -r requirements-docs.txt
python3 -m mkdocs build --strict
```

Publishing is intentionally not configured here. Enable deployment only after
the repository Pages/hosting policy is known.

## Authority

The Lean kernel under `lean/Causa/` is authoritative for domainless
invariants; start from [lean/README.md](lean/README.md). Numbered Markdown docs are prose reference and rationale.
Public tutorials and how-to guides must link back to the relevant reference/design
section instead of restating invariants.

## Package Entry Points

| Package | README |
|---|---|
| Headless MCP host | [apps/proxima-mcp](https://github.com/Aquilo-Solution-S/Proxima/blob/main/apps/proxima-mcp/README.md) |
| Host facade crate | [crates/proxima](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/proxima/README.md) |
| Core crate | [crates/core](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/core/README.md) |
| Postgres storage crate | [crates/storage-pg](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/storage-pg/README.md) |
| MCP server crate | [crates/mcp-server](https://github.com/Aquilo-Solution-S/Proxima/blob/main/crates/mcp-server/README.md) |
| Code flavor | [flavors/code](https://github.com/Aquilo-Solution-S/Proxima/blob/main/flavors/code/README.md) |

## Current Implementation Status

- Runtime framework, storage, MCP substrate, facade, and code flavor crates exist.
- Schema is timeseries v0.0.8: one core file `0001_v008.sql`.
- Crates are git/tag consumed unless package manifests and release notes say otherwise.
