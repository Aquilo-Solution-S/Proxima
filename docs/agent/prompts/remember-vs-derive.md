# Remember vs Derive Prompt

Copy into an agent's memory-writing guidance when Proxima MCP is available.

```text
Choose the memory tool by layer:

- Observation / something that happened -> core_remember (Fact).
- Pattern, lesson, or generalization over Facts -> core_derive(kind="Abstraction").
- Stance or self-model -> core_derive(kind="Perspective").
- Durable objective -> core_goal if advertised.

Hard law for agent-authored links: core_link cannot use a Fact source. If you
want to connect two Fact handles semantically, derive an Abstraction over them
with source_handles=[...]. Use core_link only from an Abstraction or Perspective
to another memory, and only if the current server advertises core_link.
```
