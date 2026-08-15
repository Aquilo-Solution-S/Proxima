-- Lexical helpers flavor generated columns call. Reused from v0.0.7.

CREATE FUNCTION proxima_core.lexical_scrub(txt text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $$;

CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT 'english'::regconfig $$;

CREATE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(proxima_core.lexical_config(), proxima_core.lexical_scrub(txt)) $$;

CREATE FUNCTION proxima_core.lexical_tsv(config regconfig, txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(config, proxima_core.lexical_scrub(txt)) $$;

CREATE FUNCTION proxima_core.lexical_text_array(parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT NULLIF(array_to_string(parts, ' '), '') $$;

CREATE FUNCTION proxima_core.lexical_join(VARIADIC parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT NULLIF(concat_ws(' ', VARIADIC parts), '') $$;

CREATE FUNCTION proxima_core.set_lexical_config(cfg text) RETURNS void
LANGUAGE sql VOLATILE AS
$$ SELECT NULL $$;
