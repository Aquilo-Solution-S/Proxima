-- 0003 — enforce single-successor supersession for memories (A/P heads).
--
-- Mirrors goals_supersedes_unique. The personality head-by-natural-key
-- projection (lookup_prior_personality_head) reads the current
-- (owner, instance, schema, Perspective) head, then writes a new row that
-- supersedes it. Two concurrent append_personality_memories calls — or two
-- same-schema Perspectives in one batch read against the pool — can both read
-- the same head and both write rows superseding it, forking the chain into
-- parallel heads (silent: the head query's LIMIT 1 just returns one of them).
--
-- A UNIQUE index on `supersedes` makes the second writer fail with a constraint
-- violation instead of forking. Combined with the tx-bound head lookup, the
-- within-batch case becomes a linear chain and the cross-request case fails
-- loudly. Supersession is constant-once-set, so no existing row is rewritten;
-- on a fresh v0.0.1 cutover DB there is no fork to migrate.

DROP INDEX IF EXISTS proxima_core.idx_memories_supersedes;

CREATE UNIQUE INDEX idx_memories_supersedes_uq
    ON proxima_core.memories USING btree (supersedes)
    WHERE (supersedes IS NOT NULL);
