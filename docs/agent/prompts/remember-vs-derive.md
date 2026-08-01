# Remember vs Derive Prompt

Copy into an agent's memory-writing guidance when Proxima MCP is available.

```text
Choose the memory tool by layer:

- Observation / something that happened -> core_remember (Fact).
- Pattern, lesson, or generalization over Facts -> core_derive(kind="Abstraction").
- Stance or self-model -> core_derive(kind="Perspective").
- Claim about memories that already exist, with a confidence -> core_interpret
  if advertised.
- Durable objective -> core_goal if advertised.

Hard law: no tool writes a connection. Every edge follows from what a node says
-- an origin entry from the handles a write declares it was made from, a
reference entry from a schema-declared payload field. Nothing you call takes an
edge kind as an argument.

So there is no connect verb. If you want to relate two Fact handles
semantically, derive an Abstraction over them with source_handles=[...]. If the
claim is a judgment -- a reason and a confidence -- call core_interpret with the
subject handles; it returns a P: handle, and the connections are that
Perspective's own references. A Fact never interprets.
```
