# proxima-llm-openai-compat

The reference `EmbeddingClient` implementation for Proxima.

`proxima-core` defines the `EmbeddingClient` trait but ships no concrete
implementation — a host injects one at boot (`Arc<dyn EmbeddingClient>`,
see [`docs/10-configuration.md`](../../docs/10-configuration.md)). This
crate is the canonical OpenAI-compatible HTTP adapter.

Hosts that bring their own embedding backend do not need this crate.

Not published to crates.io (`publish = false`); consume it via path/git
within a workspace.
