# Compliance Status

| Area | Status | Public claim |
|---|---|---|
| owner-scoped rows | current | access predicates are owner-based, not org-based |
| append-only memory | current | normal lifecycle is append/supersession, not deletion |
| GDPR owner/source erasure primitives | current primitive inventory | exact operational surface depends on deployment |
| per-memory cascade delete | deferred | not a current public guarantee |
| tool-recipient export | deferred | waits for per-call recipient storage |
| legal-consequence automatic blocking | deferred | human approval remains required pattern |

See [docs/13-compliance.md](../13-compliance.md) for design rationale.
