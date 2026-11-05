CREATE EXTENSION IF NOT EXISTS pg_stat_statements;

-- Loopback development bootstrap, not production credential provisioning.
-- Existing volumes must replay this file with all old Proxiima hosts stopped.
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS btree_gin;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

DO $roles$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_platform') THEN
        CREATE ROLE proxiima_platform LOGIN PASSWORD 'proxiima-platform-dev'
            NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_runtime') THEN
        CREATE ROLE proxiima_runtime LOGIN PASSWORD 'proxiima-runtime-dev'
            NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_search') THEN
        CREATE ROLE proxiima_search NOLOGIN NOSUPERUSER NOBYPASSRLS
            NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_goal_reader') THEN
        CREATE ROLE proxiima_goal_reader NOLOGIN NOSUPERUSER NOBYPASSRLS
            NOCREATEDB NOCREATEROLE NOINHERIT;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_search'
               AND (rolcanlogin OR rolsuper OR rolbypassrls OR rolcreaterole
                    OR rolcreatedb OR rolinherit)) THEN
        RAISE EXCEPTION 'search role has unsafe privileges';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'proxiima_goal_reader'
               AND (rolcanlogin OR rolsuper OR rolbypassrls OR rolcreaterole
                    OR rolcreatedb OR rolinherit)) THEN
        RAISE EXCEPTION 'Goal reader role has unsafe privileges';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname IN ('proxiima_platform', 'proxiima_runtime')
               AND (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb)) THEN
        RAISE EXCEPTION 'development runtime/platform roles have unsafe privileges';
    END IF;
    EXECUTE format('GRANT CREATE ON DATABASE %I TO proxiima_platform', current_database());
END
$roles$;
GRANT proxiima_search TO proxiima_platform WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
REVOKE proxiima_search FROM proxiima_runtime;
GRANT proxiima_goal_reader TO proxiima_platform WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
REVOKE proxiima_goal_reader FROM proxiima_runtime;
GRANT USAGE, CREATE ON SCHEMA public TO proxiima_platform;
GRANT USAGE ON SCHEMA public TO proxiima_runtime, proxiima_search, proxiima_goal_reader;
ALTER DEFAULT PRIVILEGES FOR ROLE proxiima_platform
    GRANT USAGE ON SCHEMAS TO proxiima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxiima_platform
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO proxiima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxiima_platform
    GRANT USAGE, SELECT ON SEQUENCES TO proxiima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxiima_search
    GRANT EXECUTE ON FUNCTIONS TO proxiima_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE proxiima_goal_reader
    GRANT EXECUTE ON FUNCTIONS TO proxiima_runtime;

-- Empty SQLx ledgers: only SQLx records applied migrations. Creating these
-- before the global defaults are used keeps runtime access read-only.
CREATE TABLE IF NOT EXISTS public._sqlx_migrations (
    version bigint PRIMARY KEY, description text NOT NULL,
    installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL,
    checksum bytea NOT NULL, execution_time bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS public._sqlx_migrations_proxiima_example
    (LIKE public._sqlx_migrations INCLUDING ALL);
ALTER TABLE public._sqlx_migrations OWNER TO proxiima_platform;
ALTER TABLE public._sqlx_migrations_proxiima_example OWNER TO proxiima_platform;
REVOKE ALL ON public._sqlx_migrations, public._sqlx_migrations_proxiima_example
    FROM proxiima_runtime;
GRANT SELECT ON public._sqlx_migrations, public._sqlx_migrations_proxiima_example
    TO proxiima_runtime;

DO $ownership$
DECLARE object record;
BEGIN
    FOR object IN SELECT nspname FROM pg_namespace
                   WHERE nspname IN ('proxiima_core', 'proxiima_example') LOOP
        EXECUTE format('ALTER SCHEMA %I OWNER TO proxiima_platform', object.nspname);
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO proxiima_runtime', object.nspname);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO proxiima_runtime', object.nspname);
        EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA %I TO proxiima_runtime', object.nspname);
    END LOOP;
    -- Transfer tables before their owned sequences.
    FOR object IN SELECT n.nspname, c.relname, c.relkind FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('proxiima_core', 'proxiima_example') AND c.relkind IN ('r','p','S')
        ORDER BY (c.relkind = 'S') LOOP
        IF object.relkind = 'S' THEN
            EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO proxiima_platform', object.nspname, object.relname);
        ELSE
            EXECUTE format('ALTER TABLE %I.%I OWNER TO proxiima_platform', object.nspname, object.relname);
        END IF;
    END LOOP;
    FOR object IN SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid) args
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        JOIN pg_roles owner_role ON owner_role.oid = p.proowner
        WHERE n.nspname IN ('proxiima_core', 'proxiima_example')
          AND owner_role.rolname NOT IN ('proxiima_search', 'proxiima_goal_reader') LOOP
        EXECUTE format('ALTER FUNCTION %I.%I(%s) OWNER TO proxiima_platform', object.nspname, object.proname, object.args);
    END LOOP;
    FOR object IN SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid) args
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        JOIN pg_roles owner_role ON owner_role.oid = p.proowner
        WHERE n.nspname IN ('proxiima_core', 'proxiima_example')
          AND owner_role.rolname IN ('proxiima_search', 'proxiima_goal_reader') AND p.prosecdef LOOP
        EXECUTE format('GRANT EXECUTE ON FUNCTION %I.%I(%s) TO proxiima_runtime', object.nspname, object.proname, object.args);
    END LOOP;
    FOR object IN SELECT n.nspname, t.typname FROM pg_type t
        JOIN pg_namespace n ON n.oid = t.typnamespace
        WHERE n.nspname IN ('proxiima_core', 'proxiima_example') AND t.typtype IN ('e','d') LOOP
        EXECUTE format('ALTER TYPE %I.%I OWNER TO proxiima_platform', object.nspname, object.typname);
    END LOOP;
END
$ownership$;
