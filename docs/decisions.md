# Decisions
> Status: current

This page records durable architectural choices. Release history belongs in
release notes. Implementation details belong in rustdoc, migrations, or the
live catalog.

## Principles and behaviour have separate owners

The Lean kernel states domainless principles. Rust code and tests are the
behaviour. A principle change starts in the kernel, then code and tests follow.
This keeps the kernel meaningful without treating it as an executable claim
about every implementation detail.

Source: [Lean principles](https://github.com/Aquilo-Solution-S/Proxima/blob/main/docs/lean/Causa/Principles.lean).

## One owner per fact

| Fact | Owner |
|---|---|
| Settings | [Configuration](configuration.md) and its completeness check |
| Types, defaults, limits | Rustdoc |
| Tool and resource schemas | Live catalog: `tools/list`, `proxima://tools`, OpenAPI |
| Tables | SQL migrations |
| Core functions, triggers, policies | `crates/storage-pg/definitions/` |
| Invariants | Lean kernel |
| Release changes | Release notes and `Upgrade:` footers |

When a description is needed elsewhere, link to its owner. Do not copy the
table, schema, or default.

## Build-time flavor composition

A flavor is a vocabulary crate linked into a host at build time. A host reads
remote stores through `OtherStores`. This makes composition visible to the
compiler and keeps storage ownership at the host boundary. See [Build a
flavor](build-a-flavor.md) for the current integration surface.

## The catalog is the contract

Each capability is one tool action or resource. MCP and REST are two views of
one catalog. Clients discover the current catalog at runtime; documentation
does not become a second schema registry.

The live catalog is described by [Protocol](protocol.md).

## Goals express intent

A Goal owns its desired end-state, owner, state, and dependencies. Scheduling,
wake policy, and execution belong to the host. See [Concepts](concepts.md) and
[Host integration](host-integration.md) for the current boundaries.

## Pins are node-owned

Only `origin` and `reference` pins exist. Their kind follows the operation, and
the node that declares a relation owns it. If a statement needs its own
meaning, it is a node rather than a new edge kind.

See [Concepts](concepts.md) for the current pin model.
