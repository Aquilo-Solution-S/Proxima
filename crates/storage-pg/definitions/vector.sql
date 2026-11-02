-- Vector candidate functions of a store that keeps its vectors in pgvector.
-- Applied by the definitions pass only for that store kind, after functions.sql
-- (generated: vector_index.rs). A store with a host vector index never applies
-- this file and needs no `vector` extension.

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d384(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 384 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 384
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(384) OPERATOR(public.<=>) $3::public.vector(384)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 384
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 384
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d384(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d768(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 768 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 768
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(768) OPERATOR(public.<=>) $3::public.vector(768)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 768
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 768
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d768(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d1024(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 1024 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 1024
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(1024) OPERATOR(public.<=>) $3::public.vector(1024)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1024
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1024
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d1024(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d1536(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 1536 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 1536
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(1536) OPERATOR(public.<=>) $3::public.vector(1536)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1536
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1536
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d1536(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d2048(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 2048 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 2048
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.halfvec(2048) OPERATOR(public.<=>) $3::public.halfvec(2048)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 2048
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 2048
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d2048(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.embeddings, proxiima_core.embedding_heads, proxiima_core.memory TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embeddings;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embeddings FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.embedding_heads;
CREATE POLICY proxiima_search_candidates ON proxiima_core.embedding_heads FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.vector_candidates_d3072(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxiima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 3072 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxiima_core.embeddings emb
             WHERE emb.dim = 3072
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.halfvec(3072) OPERATOR(public.<=>) $3::public.halfvec(3072)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 3072
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxiima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 3072
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxiima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.vector_candidates_d3072(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;
