# Compliance Status

Proxima models no legal regime. It stores, and for everything it stores it
can undo. A hosting application that promises its users a right to erasure or
a right to a copy calls the inverses when its own rules say to; the
mechanism is here, every judgement is the host's.

| Area | Status | Public claim |
|---|---|---|
| owner-scoped rows | current | access predicates are owner-based, not org-based |
| append-only memory | current | normal lifecycle is append/supersession, not deletion |
| owner and source-scope erase | current | complete over every declared surface; the host decides when it is owed |
| owner export bundle | current | one entry per surface the contracts declare exportable, with derived counts |
| erase/export receipts | current | returned to the caller; core keeps no history of the operation |
| retention windows | removed | a retention schedule is a promise about someone's data; core holds none and never did enforce one for a host that had not asked |
| legal/security holds | removed | a litigation hold is a judgement about a host's obligations, not a fact about a store |
| erase/export audit journal | removed | the receipt goes to the host; a host that must be able to show what it did records it |
| per-memory cascade delete | deferred | not a current public guarantee |
| re-ingest suppression after erase | deferred | the `Suppressed` error code exists; no durable suppression list does |
| tool-recipient export | deferred | waits for per-call recipient storage |
| legal-consequence automatic blocking | deferred | human approval remains required pattern |

See [docs/13 — The Inverses of Storing](../13-compliance.md) for the
rationale, and `OwnerEraseAuthorityPort` for the provider seam that answers
"who may erase an owner?".
