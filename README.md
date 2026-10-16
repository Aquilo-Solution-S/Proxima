<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/proxima-seal-dark.svg">
    <img alt="Proxima" src="docs/assets/proxima-seal-light.svg" width="128" height="128">
  </picture>
</p>

# Proxima

Typed, owner-scoped durable memory for agents: Facts, Abstractions,
Perspectives, Goals, citations, retrieval, and provenance. A host supplies
identity, event sources, model clients, scheduling, and effects.

| Task | Start here |
|---|---|
| Run Postgres, connect an agent, remember and search | [Get started](docs/get-started.md) |
| Understand identity, layering, pins, and Goals | [Concepts](docs/concepts.md) |
| Embed the Rust facade | [Host integration](docs/host-integration.md) |
| Build a linked vocabulary crate | [Build a flavor](docs/build-a-flavor.md) |
| Discover actions, resources, and REST | [Protocol](docs/protocol.md) |
| Deploy, back up, and maintain | [Operate](docs/operate.md) |
| Find settings | [Configuration](docs/configuration.md) |
| Contribute or build this site | [Contributing](docs/contributing.md) |

The [twelve-page documentation](docs/index.md) owns concepts and integration
instructions. Rustdoc owns types, defaults, and limits; migrations own table
shape; the running catalog owns operation schemas. The Lean kernel states
principles; Rust code and tests define runtime behavior. A principle change
starts in the kernel.

One deployment links core, storage, transports, and flavor crates at build
time. The canonical host is [proxima-mcp](apps/proxima-mcp); its Code flavor
requires `--features code`. The default host is substrate-only. Crates
are unpublished; Rust consumers pin the same git selector across the graph.

License: [Apache-2.0](LICENSE), [contribution terms](LICENSING.md).
