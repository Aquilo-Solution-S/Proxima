# Agent Quickstart

## Minimal Session Ritual

1. Recall: search before architectural/debugging work.
2. Act: use normal coding tools.
3. Remember: store durable observations with `core_remember`.
4. Abstract: use `core_derive` for lessons/patterns over Facts.
5. Interpret: use `core_interpret` when the claim is a judgment about memories
   that already exist.
6. Reflect: update Perspective only when stance changes.
7. Intend: set/refine Goals only when objective should persist.

## Do Not

- Do not write directly to the DB.
- Do not call unadvertised tools.
- Do not look for a connect verb: no tool writes an edge. To relate Facts,
  derive an Abstraction over them; to claim what existing memories mean, use
  `core_interpret`.
- Do not store transcripts or git history as memory.

## Discovery First

Live server discovery is authoritative:

1. Read `initialize.instructions`.
2. Call `tools/list`.
3. Call `resources/list` and `resources/templates/list`.
4. Read `proxima://how-to`.
5. Read `proxima://tools`.

In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_interpret`; hydrate a memory through `proxima://memory/{id}`. Omitted `space` preserves the current bound owner. A cross-space derivation or interpretation may ground in readable handles outside the selected write space.


## Illustrative JSON-RPC Examples

Exact transport wrappers vary by MCP client; these examples show the MCP
`tools/call` shape and representative arguments:

- [core_search_memories.json](examples/core_search_memories.json)
- [core_remember.json](examples/core_remember.json)
- [core_derive.json](examples/core_derive.json)
- [core_interpret.json](examples/core_interpret.json)

## Prompt Snippets

- [Recall before work](prompts/recall-before-work.md)
- [Consolidate at end](prompts/consolidate-at-end.md)
- [Remember vs derive](prompts/remember-vs-derive.md)
