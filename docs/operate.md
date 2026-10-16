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

`DATABASE_URL` and `PROXIMA_PLATFORM_DATABASE_URL` are both required for the
canonical host. Provision the platform role as the schema/migration owner and
the runtime role for serving grants. Development provisioning is executable in
[`scripts/pg-init.sql`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/scripts/pg-init.sql).
Stop older hosts before changing their role/schema contract.

Released migrations are immutable. Existing supported databases append pending
migrations; a checksum mismatch requires restoring the expected migration and
database history, not editing the ledger. Do not repair a failed boot with ad
hoc SQL. The gate and named historical exception are in
[contributing](contributing.md#migrations).

For encrypted PostgreSQL, use the URL's TLS options and the deployment's CA
trust. A successful TCP connection is not proof of the correct runtime grants;
exercise an authenticated owner read and write after deployment.

## Container and network boundary

Build the default Code-flavor MCP image:

```sh
docker build -t proxima-mcp .
```

Supply the two database connections and OIDC configuration through the
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

## Probes and shutdown

With health endpoints enabled, anonymous `/healthz` is liveness and `/readyz`
checks database reachability and shutdown state. These routes sit outside
bearer and Host guards; expose them according to deployment policy.
An authenticated graph-resource read checks the served owner path.

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
cited blobs and cooled objects. Use PostgreSQL WAL/PITR or standard dumps and
object-store versioning with compatible retention.

Restore a logical dump with `pg_restore --single-transaction`, so deferred
memory/sidecar constraints see both halves at commit. If a controlled restore
disables triggers, run `PgSidecarRegistryFrozen::integrity_check` afterwards;
disabled triggers cannot validate the restored pairing. Physical backup/PITR
does not replay table inserts.

After restoring either store, run `proxima-mcp maintain-blobs`. It reports
missing referenced objects and unclaimed objects; it does not repair or delete
them. Restore missing objects from the backup before serving affected reads.
Embeddings can be rebuilt from retained content; lost content cannot.

## External deletion and publication

`proxima-mcp maintain-storage` retries committed object-purge debt and performs
the selected storage maintenance actions. A pending purge is outstanding work,
not completed deletion. Configure the correct object backend before draining;
foreign-backend debt is refused.

Watch the publication publisher and intake health. An unavailable intake retries
captured records; a full pending bound refuses a new listenable write atomically.
Published-record retention reclaims delivered copies only. It is independent of
the host's erasure and retention promises. Hosts must manage copies already
accepted downstream. See [data and erasure](data-and-erasure.md).

For command flags, use `proxima-mcp --help` and each maintenance subcommand's
`--help`. For a failed check, keep the exact command and error; credentials and
bearers stay out of diagnostic artifacts.
