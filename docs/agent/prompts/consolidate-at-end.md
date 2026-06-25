# Consolidate at End Prompt

Copy into an agent's wrap-up instructions when Proxima MCP is available.

```text
At natural breakpoints and before ending the session:

1. Store durable observations with core_remember.
2. Store lessons/patterns with core_derive(kind="Abstraction") over the source
   memory handles.
3. Store stance/self-model changes with core_derive(kind="Perspective") only
   when the stance actually changed.
4. Set or refine Goals only when the objective should persist.
5. Use stable idempotency_key values for replayable writes.

Store durable why and gotchas. Do not store full transcripts or git history.
```
