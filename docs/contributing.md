# Contributing
> Status: current.

Proxima has two authorities: the Lean kernel under
[`docs/lean/Causa`](https://github.com/Aquilo-Solution-S/Proxima/tree/main/docs/lean/Causa)
defines domainless invariants; Rust code, SQL, tests, and the live MCP/REST
catalog implement and expose them. A prose page explains a contract but does
not amend the kernel. If the implementation and kernel disagree, stop and
resolve the contract before coding.

## Before editing

Read [concepts](concepts.md), the relevant integration page, and the
repository's `AGENTS.md` instructions. Map the data stream from its external input
to its consumer. Join existing contracts instead of adding a second source of
truth. Keep scratch plans, review notes, and bulky evidence under ignored
`.local/`; tracked docs contain durable decisions and executable checks.

The core boundary is deliberate: strict F/A/P/Goal layering, server-resolved
owner scope, typed payloads and sidecars, build-time flavor registration,
atomic command-port writes, and hard deletion only through an authorized
`wipeable` path. Similarity is query-time behavior and never authors an edge.

## Code and tests

Prefer a type that makes an invalid state unrepresentable. Parse untrusted
configuration, network data, database rows, and serialized payloads at the
boundary, returning a typed error. Recheck facts that can become stale at the
commit boundary. Tests should prove intent and boundary refusal, not duplicate
the implementation.

Do not add a runtime flavor registry, raw flavor SQL against core tables,
caller-supplied owner authorization, a third edge kind, generic JSON payload
escape hatches, or compatibility aliases for removed ontology. If a feature
needs a connection with its own reason or confidence, add the node that owns
that statement; do not add an edge sidecar.

## Migrations

Released migration files are immutable from v0.0.48 onward; a schema fix is a
new migration whose version is above every released version in its directory.
Definitions in `crates/storage-pg/definitions/` are edited in place.
Run `scripts/check-migrations.py` before changing SQL.
At a reset, number the new baseline above the old head, move mutable
objects into definitions, and refuse every earlier ledger with
`SchemaResetRequired`: pre-alpha stores install fresh, and no compatibility
arm stays. Delete the replaced migration files, and prove the new baseline
equals the old head's schema and seed rows. A migration that only a
pgvector-backed store needs has a description starting with `pgvector`; a store
with a host vector index skips it, and its definitions live in
`definitions/vector.sql`.

## Local verification

Use the smallest relevant check during iteration, then the complete gates
before pushing. This is the one list; CI runs the same commands.

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --locked
cargo nextest run --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
cd docs/lean && lake build
cd ../..
python3 scripts/check-migrations.py --self-test
python3 scripts/check-migrations.py
python3 scripts/check-doc-links.py
python3 scripts/check-doc-status.py
python3 scripts/check-settings.py --self-test
python3 scripts/check-settings.py
python3 scripts/check-upgrade-notes.py --self-test
python3 scripts/check-release-version.py --self-test
python3 scripts/check-release-version.py
python3 scripts/verify-culprit.py --self-test
python3 scripts/check-architecture-guardrails.py --self-test
python3 scripts/check-architecture-guardrails.py
python3 scripts/check-sql-policy.py --self-test
python3 scripts/check-sql-policy.py
python3 scripts/check-schema-ids.py --self-test
python3 scripts/check-schema-ids.py
python3 scripts/check-lean-axioms.py --self-test
python3 scripts/check-lean-axioms.py
python3 -m pip install -r requirements-docs.txt
python3 -m mkdocs build --strict
```

Postgres-backed tests need `PROXIMA_TEST_PG_URL`; the local development
bootstrap is described in [get started](get-started.md).

The suite runs eight test processes at once with no retries
(`.config/nextest.toml`), and they share the server's `max_connections`.
The dev compose file and CI set it to 200. A full run peaks near 60 client
backends; re-measure (`pg_stat_activity`) before raising `test-threads`.
A fixture boot that times out on its pool reports the server's connection
use in its panic.

Every `DROP DATABASE` forces a checkpoint. With `fsync` on, those
checkpoints wait on disk and a full local run pays seconds per drop. The
dev compose file and CI turn `fsync`, `synchronous_commit`, and
`full_page_writes` off on that disposable server; crash safety is
irrelevant for per-test databases. Do not restore them in CI.

The PgBouncer lane runs the storage suites that depend on what a connection
carries between transactions (export/erase fence, maintenance lease, pool
startup parameters) with their serving pools behind PgBouncer 1.26.1 or later
in transaction mode. Set `PROXIMA_TEST_PGBOUNCER_URL` to that PgBouncer (in
front of the `PROXIMA_TEST_PG_URL` server, configured like the `pgbouncer`
job in `.github/workflows/ci.yml`) and run the job's nextest filter. Without
the variable, `pgbouncer_pg` returns early and the other suites connect
direct.

The cited-blob tests return early and pass when the `PROXIMA_S3_*` variables
are unset. A complete run needs an S3-compatible store with a versioned
bucket and the five variables CI sets in `.github/workflows/ci.yml`. CI
serves bucket `proxima-cited-blobs` at `http://localhost:9000`. Locally, the
dev compose file serves `http://127.0.0.1:9100` (`PROXIMA_DEV_S3_PORT`
overrides the port) with keys `proxima` / `proxima-dev`, and
`docker compose -f docker-compose.dev.yml --profile init up s3-init` creates
its versioned bucket `proxima-dev`; region `us-east-1` works for both. Report
a run without the variables as skipping that lane. Do not hide a failing
required check. Fix it, record a precise `.local` ledger entry with command
and output, or escalate the blocker.

## Review and release

Keep commits narrow and use the repository subjects: `docs(<scope>): …`,
`feat(<component>): …`, `fix(<component>): …`, or `chore(<component>): …`.
The orchestrating engineer reviews actual diffs, reruns load-bearing gates,
and owns commits, pushes, and publication. Do not commit from delegated work.

Breaking commits carry an `Upgrade:` footer with concrete operator/client
actions. `cliff.toml` renders these in the release notes;
`scripts/check-upgrade-notes.py --self-test` tests the actual parser/template.
Per-release changes live only in those notes.

Changes are pushed to `main` directly; `verified` names the last `main`
commit that passed CI, and new work starts from it. A release is cut by a
commit that bumps `proxima_core::RELEASE_VERSION`: once `verify.yml` advances
`verified` onto it, the value becomes the `v${VERSION}` tag and git-cliff
generates notes. A commit that leaves the constant unchanged cuts no release.
Run full local gates before pushing, and `scripts/check-release-version.py`
for a version bump. Do not tag by hand.

For a flavor change, also verify the served catalog and the host e2e path. For
an outbox or host-state change, exercise the transaction and retry or rollback
boundary against real Postgres. For an API change, update the facade rustdoc,
the relevant integration page, and the executable example together.

Continue with [build a flavor](build-a-flavor.md) or [host integration](host-integration.md).
