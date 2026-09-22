CREATE EXTENSION IF NOT EXISTS pg_stat_statements;

-- Loopback development bootstrap, not production credential provisioning.
-- Existing volumes must replay this file with all old Proxima hosts stopped.
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS btree_gin;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

DO $roles$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxima_platform') THEN
        CREATE ROLE proxima_platform LOGIN PASSWORD 'proxima-platform-dev'
            NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxima_runtime') THEN
        CREATE ROLE proxima_runtime LOGIN PASSWORD 'proxima-runtime-dev'
            NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname IN ('proxima_platform', 'proxima_runtime')
               AND (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb)) THEN
        RAISE EXCEPTION 'development runtime/platform roles have unsafe privileges';
    END IF;
    EXECUTE format('GRANT CREATE ON DATABASE %I TO proxima_platform', current_database());
END
$roles$;
GRANT SET ON PARAMETER app.proxima_scope TO proxima_platform;
GRANT USAGE, CREATE ON SCHEMA public TO proxima_platform;
GRANT USAGE ON SCHEMA public TO proxima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxima_platform
    GRANT USAGE ON SCHEMAS TO proxima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxima_platform
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO proxima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxima_platform
    GRANT USAGE, SELECT ON SEQUENCES TO proxima_runtime;

-- Empty SQLx ledgers: only SQLx records applied migrations. Creating these
-- before the global defaults are used keeps runtime access read-only.
CREATE TABLE IF NOT EXISTS public._sqlx_migrations (
    version bigint PRIMARY KEY, description text NOT NULL,
    installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL,
    checksum bytea NOT NULL, execution_time bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS public._sqlx_migrations_proxima_code
    (LIKE public._sqlx_migrations INCLUDING ALL);
ALTER TABLE public._sqlx_migrations OWNER TO proxima_platform;
ALTER TABLE public._sqlx_migrations_proxima_code OWNER TO proxima_platform;
REVOKE ALL ON public._sqlx_migrations, public._sqlx_migrations_proxima_code
    FROM proxima_runtime;
GRANT SELECT ON public._sqlx_migrations, public._sqlx_migrations_proxima_code
    TO proxima_runtime;

DO $ownership$
DECLARE object record;
BEGIN
    FOR object IN SELECT nspname FROM pg_namespace
                   WHERE nspname IN ('proxima_core', 'proxima_code') LOOP
        EXECUTE format('ALTER SCHEMA %I OWNER TO proxima_platform', object.nspname);
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO proxima_runtime', object.nspname);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO proxima_runtime', object.nspname);
        EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA %I TO proxima_runtime', object.nspname);
    END LOOP;
    -- Transfer tables before their owned sequences.
    FOR object IN SELECT n.nspname, c.relname, c.relkind FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('proxima_core', 'proxima_code') AND c.relkind IN ('r','p','S')
        ORDER BY (c.relkind = 'S') LOOP
        IF object.relkind = 'S' THEN
            EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO proxima_platform', object.nspname, object.relname);
        ELSE
            EXECUTE format('ALTER TABLE %I.%I OWNER TO proxima_platform', object.nspname, object.relname);
        END IF;
    END LOOP;
    FOR object IN SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid) args
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname IN ('proxima_core', 'proxima_code') LOOP
        EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO proxima_platform', object.nspname, object.proname, object.args);
    END LOOP;
    FOR object IN SELECT n.nspname, t.typname FROM pg_type t
        JOIN pg_namespace n ON n.oid = t.typnamespace
        WHERE n.nspname IN ('proxima_core', 'proxima_code') AND t.typtype IN ('e','d') LOOP
        EXECUTE format('ALTER TYPE %I.%I OWNER TO proxima_platform', object.nspname, object.typname);
    END LOOP;
END
$ownership$;
