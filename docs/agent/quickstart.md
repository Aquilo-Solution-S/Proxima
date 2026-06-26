# Agent Quickstart

## Minimal Session Ritual

1. Recall: search before architectural/debugging work.
2. Act: use normal coding tools.
3. Remember: store durable observations with `core_remember`.
4. Abstract: use `core_derive` for lessons/patterns over Facts.
5. Reflect: update Perspective only when stance changes.
6. Intend: set/refine Goals only when objective should persist.

## Do Not

- Do not write directly to the DB.
- Do not call unadvertised tools.
- Do not use `core_link` from Fact to Fact; derive an Abstraction instead.
- Do not store transcripts or git history as memory.

## Discovery First

Live server discovery is authoritative:

1. Read `initialize.instructions`.
2. Call `tools/list`.
3. Call `resources/list` and `resources/templates/list`.
4. Read `proxima://how-to`.
5. Read `proxima://tools`.

In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_search_memories`, `core_get_memory`, and `core_publish_memory`. Omitted `space` preserves the current owner behavior for single-owner deployments.

`core_publish_memory` v1 copies only `core/agent-note-v1`; flavor-specific publish is a host/flavor concern until typed replay is designed.

## Illustrative JSON-RPC Examples

Exact transport wrappers vary by MCP client; these examples show the MCP
`tools/call` shape and representative arguments:

- [core_search_memories.json](examples/core_search_memories.json)
- [core_remember.json](examples/core_remember.json)
- [core_derive.json](examples/core_derive.json)
- [invalid_fact_link.json](examples/invalid_fact_link.json)

## Prompt Snippets

- [Recall before work](prompts/recall-before-work.md)
- [Consolidate at end](prompts/consolidate-at-end.md)
- [Remember vs derive](prompts/remember-vs-derive.md)
