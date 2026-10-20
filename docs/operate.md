# Operate
> Status: current.

One deployment links core, storage, transports, and its flavor crates into one
host binary. [Get started](get-started.md) supplies the local bootstrap;
[configuration](configuration.md) owns the settings inventory.

## PostgreSQL and migrations

Use PostgreSQL with `vector`, `pg_trgm`, and `btree_gin`. Boot checks the
supported pgvector version and search capabilities. The serving and platform
connections use separate roles with `NOBYPASSRLS`. Serving requests enter an
owner-scoped transaction; platform maintenance enters the separately authorized
platform scope. Owner RLS is always enforced.

The boot RLS guard covers composed schemas and schemas recorded in
`proxima_core.flavor_surface`, including stored flavors absent from the binary.
The platform connection reads that catalog; the runtime connection is checked
against the combined inventory.

`DATABASE_URL`, `PROXIMA_PLATFORM_DATABASE_URL`, and the host-assigned
`PROXIMA_STORE_NUMBER` are required for the canonical host. Provision the
platform role as the schema/migration owner and
the runtime role for serving grants. Development provisioning is executable in
[`scripts/pg-init.sql`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/scripts/pg-init.sql).
Stop older hosts before changing their role/schema contract.

Before migrating, an administrator provisions the fixed candidate-function
owner and grants the platform role permission to switch to it. Replace
`platform_role` below with the deployment's migration role:

```sql
CREATE ROLE proxima_search NOLOGIN NOSUPERUSER NOBYPASSRLS
    NOCREATEDB NOCREATEROLE NOINHERIT;
GRANT proxima_search TO platform_role WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
GRANT USAGE ON SCHEMA public TO proxima_search;
```

Keep the runtime role outside this membership. Migrations temporarily give
`proxima_search` schema creation rights while replacing its candidate
functions, then revoke those rights. Preserve this function ownership when
reassigning restored objects. Candidate functions have no public execution
grant; the runtime grant phase grants execution on their catalog signatures
after migrations. Deployments that manage grants themselves must grant
`EXECUTE` on the `proxima_search`-owned candidate functions in each composed
schema to their runtime role. These functions find candidates through the
indexes; ordinary memory RLS still gates every returned id and score.

Released migrations are immutable. This release adopts databases at v0.0.35 or
later; older databases must be upgraded to v0.0.35 first, or reset when used
for development. A database migrated by a newer release refuses an older
binary unless the host [allows missing migration versions](configuration.md);
then the older binary leaves newer core definitions in place. Existing
supported databases append pending migrations; a migration checksum mismatch
requires restoring the expected migration and database history, not editing
the ledger. Do not repair a failed boot with ad hoc SQL. See the
[migration procedure](contributing.md#migrations).

For encrypted PostgreSQL, use the URL's TLS options and the deployment's CA
trust. A successful TCP connection is not proof of the correct runtime grants;
exercise an authenticated owner read and write after deployment.

## Container and network boundary

Build the default substrate-only MCP image:

```sh
docker build -t proxima-mcp .
```

The image serves the core only: proxima-mcp links no flavor. A deployment that
needs a flavor builds its own host that links it.

Supply the database connections, store number, and OIDC configuration through the
deployment's secret/config mechanism. Use a loopback bind for local use. A
non-loopback listener requires explicit network exposure, public URL, Host and
Origin allowlists. Terminate TLS at the trusted ingress and preserve streaming
HTTP. Configure proxy buffering, idle timeouts, request size, and shutdown grace
for the deployed workload.

REST needs a host built with `--features rest` and the runtime switch. Discover
its routes from `/v1/openapi.json`; see [protocol](protocol.md#rest-projection).

OIDC validates issuer, audience, signature, expiry, and the configured subject
binding. Use the issuer-aware JSON subject map. The local issuer is a
development tool; production uses an operated issuer and trusted JWKS. A
forwarder policy is a separate host decision and never inferred from arbitrary
request headers.

## Object storage

Set `PROXIMA_S3_PUBLIC_ENDPOINT_URL` when holders of presigned upload and
download links reach a different address from the host. The presigner shares
the configured credentials, region, bucket, path style and lifetimes with the
object client. Staging, verified reads, reconciliation and object purge
use `PROXIMA_S3_ENDPOINT_URL`.

The upload `PUT` signs the declared `byte_len` as its `Content-Length`, so the
store refuses a body of any other length before it lands; a proxy in front of
it must pass the header through. Staging reads an object into memory up to
the declared length, at most eight uploads at a time per process; a ninth
waits for a slot.

SigV4 signs the Host, including its port, the complete path, including any
endpoint prefix, and the query. The public proxy must forward these unchanged
to the store, or verify that original signed request itself before translating
it. Configure the public address before signing; a signed link cannot be
rewritten to another address or path. Both endpoint settings use the same
HTTPS or allowed-plaintext host policy.

Every upload and every purge debt names the object store that holds its
bytes. By default the name is derived from `PROXIMA_S3_ENDPOINT_URL` and
`PROXIMA_S3_BUCKET` (scheme and host lowercased, default port and trailing
slash dropped; `aws/<bucket>` with no endpoint). Set `PROXIMA_S3_STORE_ID`
to a stable name (`[A-Za-z0-9._-]`, up to 64 characters) when processes
that share one store reach it through different endpoint URLs, or when
several stores share one endpoint and bucket. A purge drain only deletes
debt owed to the store it is wired to; debt for any other store is left in
place, never cleared. A store does not move, so a server, or a library host's
`proxima::object_maintenance_pass`, refuses to start while the queue holds
debt for another store. The error names the configured store, the store found
and the count: fix the `PROXIMA_S3_*` settings, or drain that debt with
`maintain-storage` configured for the store it names.

## Probes and shutdown

With health endpoints enabled, anonymous `/healthz` is liveness and `/readyz`
checks database reachability, shutdown state and observed embedding-client
configuration faults. A paused client keeps readiness at HTTP 200 with a named
degraded body, such as `degraded: embedding configuration fault: HTTP 401`,
containing only sanitized status codes. With an object store, the body adds the
purge queue as the last drain pass left it, counts only:
`object-purge: owed=… due=… deferred=… stuck=…`. Purge debt never changes
readiness: readiness answers whether this replica can serve traffic now, and
background debt does not change that. Alert on `stuck` above zero, and run
`maintain-storage --retry-cold-object-purges` to see each stuck debt's key,
owner and last error. Readiness returns 503 only when the
database is unavailable or shutdown has started; liveness continues to answer.
These routes sit outside bearer and Host guards; expose them according to
deployment policy.
An authenticated graph-resource read checks the served owner path.

Accepted memory writes enqueue embedding work atomically without calling the
provider. Only typed input refusals (413, 422 or a local input cap) enter chunk
rescue and can become permanent job failures. Transient failures keep jobs
pending and wait for backoff and any longer `Retry-After`. Configuration faults
(400, 401, 403, 404) pause the original client shared by its owners. Replace its
binding with a new client when configuration changes; mutating the same client
does not reset the pause. Drains observe changed routes and clear faults once
their old bindings are no longer active. Healthy clients continue draining.
Calls through one original client serialize; the request deadline includes
waiting for its dispatch slot and the provider call together.

`proxima::serve` and `RunningProxima::until_shutdown_signal` drain on
SIGTERM/SIGINT: readiness fails, listeners stop accepting, requests and streams
finish, then workers join. Set the orchestrator's grace above the longest
allowed request. Stream authorization is rechecked during long-lived streams;
expiry or revoked access terminates them.

## Embeddings

An embedding client enables semantic retrieval; lexical retrieval works without
one. A host may route clients per owner. The configured embedding space includes
model identity, dimension, and preprocessing; rows from a different space are
not interchangeable. Use the embedding migration/coverage APIs when moving an
owner between models.

| Signal | Action |
|---|---|
| No client configured | Use lexical search or install the owner's embedding route |
| Pending jobs falling | Allow normal catch-up |
| Pending/failed jobs persist | Inspect provider reachability, route refusal, and job errors |
| Stale processing claims persist | Inspect worker/storage health; expired claims are reclaimed |
| Orphan embedding rows | Run authorized embedding maintenance |
| Poor exact-versus-ANN recall | Inspect canary results and HNSW tuning/index health |

`proxima-mcp maintain-embeddings` performs one maintenance pass. The host's
operator-only observability APIs expose backlog, coverage, orphan counts, and
recall checks. Their authorization is distinct from ordinary owner retrieval.
Vacuum and index rebuild scheduling depend on measured tuples, bytes, latency,
and recall; they are database operations.

## Backup and restore

Back up PostgreSQL and every configured object store. PostgreSQL contains the
graph, typed sidecars, delivery state, and embedding jobs. Object storage holds
cited blobs. Forgotten memories stay in PostgreSQL with their payloads, so a
backup holds them until they are erased. Use PostgreSQL WAL/PITR or standard dumps and
object-store versioning with compatible retention.

Restore a logical dump with `pg_restore --single-transaction`, so deferred
memory/sidecar constraints see both halves at commit. If a controlled restore
disables triggers, run `PgSidecarRegistryFrozen::integrity_check` afterwards;
disabled triggers cannot validate the restored pairing. Physical backup/PITR
does not replay table inserts.

After restoring either store, run `proxima-mcp maintain-blobs`. It reports
missing referenced objects and unclaimed objects; it does not repair or delete
them, with one exception. Every pending or completed upload row claims the
canonical object its id implies, whatever key it records, so a staged upload's
object is never an orphan. A canonical object that only an aborted or expired
upload implies was put after that upload ended. The command owes it as purge
debt and counts it as `newly-owed`, and the drain removes it. Objects with purge
debt are reported as `owed` (the drain removes them), and transfer keys under
`pending/` that no in-flight upload claims as `stale-transfers`. Restore missing objects from the backup before serving affected reads.
Embeddings can be rebuilt from retained content; lost content cannot.

## External deletion and publication

A server with an object store drains what it owes by itself, as it drains
embedding jobs: every `PROXIMA_OBJECT_PURGE_INTERVAL_SECS` (default 60, `0`
turns it off) it sweeps uploads whose TTL passed before they were staged and
retries due object-purge debt. The first pass runs at boot, so a restart pays
what the previous process left owed. Scans and reports stay commands
(`maintain-blobs`, `maintain-embeddings`). `proxima-mcp maintain-storage` runs
the same sweep (`--sweep-expired-uploads`) and drain
(`--retry-cold-object-purges`) for a host that runs no server, to list stuck
debt by key, or to name and drain another store's debt. Every pass, in-process or not, takes one storage
maintenance advisory lock, so passes on one database never overlap; a pass that
finds the lock held skips. A pending purge is outstanding work, not completed
deletion. A failed delete backs its debt off, doubling the delay
per attempt, so it never holds back other debts. The report gives `due`
(selectable now), `deferred` (backing off) and `stuck` (failed repeatedly; each
is listed with its key, owner, attempt count and last error). Debt owed to another
object store is left in place and listed by store, and the command exits
non-zero until a process configured for that store drains it.

Watch the publication publisher and intake health. An unavailable intake retries
captured records; a full pending bound refuses a new listenable write atomically.
Published-record retention reclaims delivered copies only. It is independent of
the host's erasure and retention promises. Hosts must manage copies already
accepted downstream. See [data and erasure](data-and-erasure.md).

For command flags, use `proxima-mcp --help` and each maintenance subcommand's
`--help`. For a failed check, keep the exact command and error; credentials and
bearers stay out of diagnostic artifacts.
