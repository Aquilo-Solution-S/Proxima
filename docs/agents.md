# Agents
> Status: current

This page is the compact operating ritual for an agent using Proxima. The
running host is authoritative for available actions and resources: read
`proxima://how-to` for current guidance and discover the catalog with
`tools/list`, `proxima://tools`, or `/v1/openapi.json`. Those surfaces can
change with the host build, so this page does not copy their schemas.

## Session ritual

1. **Discover first.** Read `proxima://how-to`, then inspect the live catalog.
   This is a judgment call: do it because a prose page cannot describe a
   particular deployment's capabilities; for a self-contained answer, discovery
   adds no value.
2. **Recall before work.** Query owner-scoped memory for the task and its
   constraints before changing files or taking an external action. This is a
   judgment call: recall is useful when prior decisions could change the
   action; skip it for a self-contained one-line answer.
3. **Separate observation from interpretation.** Admit external observations
   as Facts. Derive an Abstraction or Perspective only when the typed payload
   and declared inputs support it. The core write contract enforces layering
   and ownership.
4. **Write only through the host contract.** Use the discovered tool or host
   API, never direct storage access. Authorization is resolved by the host;
   caller-supplied owner roles are not accepted by the core boundary.
5. **Keep pins meaningful.** Let the write operation derive origin and
   reference pins from typed payloads. The schema and write contracts enforce
   this; no tool writes an arbitrary connection. The two pin kinds occupy
   `origins[]`, `refs[]`, and `goal_refs[]`.
6. **Consolidate at a natural break.** If a durable pattern or decision was
   learned, write the smallest typed memory that can be retrieved later. This
   is a judgment call: do not record transient narration or duplicate an
   existing Fact.
7. **Verify the effect.** Re-query the result and inspect the host response.
   The operation's transaction and response are the evidence; a previous read
   is not proof that a later write succeeded.

## Fact, interpretation, and action

| Situation | Use | Why |
|---|---|---|
| External event or observed result | Fact admission | Records what entered through a source |
| Pattern over admitted material | Abstraction derivation | Makes inputs and re-derivation explicit |
| Causal or situated interpretation | Perspective derivation | Keeps claims in the interpretation layer |
| Desired future state | Goal write | Separates intent from memory and scheduling |
| Host-side effect | Host action/tool | Effects belong to the host boundary |

## Do not

- Do not infer authorization from an owner string, organization, Self, or a
  caller-provided role; the host resolves `OwnerRef`.
- Do not treat similarity as a persisted connection; similarity is query-time.
- Do not invent a third pin kind or an edge-writing verb; the node-home test
  says a missing statement needs a node.
- Do not copy the runtime catalog, settings table, defaults, or database schema
  into an agent prompt. Link to the live catalog, [Configuration](configuration.md),
  rustdoc, or migrations respectively.
- Do not present an inference as an observation. Name the source and the
  transformation that produced the claim.

## Durable memory at the end

At a natural break, ask:

```text
What changed in the world?
What did I infer from it?
Which decision or constraint should survive this session?
```

Store only the answer that is useful for a future owner-scoped recall. If no
answer is durable, leave the graph unchanged. The concepts and live host
catalog provide the memory boundary and available operations.
