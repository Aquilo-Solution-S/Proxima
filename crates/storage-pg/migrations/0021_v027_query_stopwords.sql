-- Pick query stopwords once, before parsing the same text in each row language.
-- Dictionary NULL means unrecognized; only an empty lexeme array is a stopword.
-- Source segments retain punctuation, whitespace and websearch operators.
CREATE OR REPLACE FUNCTION proxima_core.lexical_query_text(config regconfig, query_text text)
RETURNS text
LANGUAGE sql STABLE PARALLEL SAFE
SET search_path = pg_catalog, proxima_core, pg_temp
AS $$
    WITH query_source AS MATERIALIZED (
        SELECT segment[1] AS token, ordinal
          FROM regexp_matches(query_text, '([[:alnum:]]+|[^[:alnum:]]+)', 'g')
               WITH ORDINALITY AS parts(segment, ordinal)
    ), query_tokens AS MATERIALIZED (
        SELECT token, ordinal,
               sum(length(token) - length(replace(token, '"', '')))
                   OVER (ORDER BY ordinal) % 2 = 1 AS in_phrase,
               sum(length(token)) OVER (ORDER BY ordinal) AS end_position
          FROM query_source
    ), query_words AS MATERIALIZED (
        SELECT DISTINCT token
          FROM query_tokens
         WHERE token ~ '[[:alpha:]]'
    ), query_languages AS MATERIALIZED (
        SELECT l.config FROM proxima_core.lexical_languages l
        UNION ALL
        SELECT config WHERE NOT EXISTS (SELECT 1 FROM proxima_core.lexical_languages)
    ), query_stopwords AS MATERIALIZED (
        SELECT l.config, w.token
          FROM query_languages l
          CROSS JOIN query_words w
         WHERE EXISTS (
             SELECT 1 FROM pg_catalog.ts_debug(l.config, w.token) d
              WHERE d.token = w.token AND d.lexemes = ARRAY[]::text[]
         )
    ), query_stopword_counts AS (
        SELECT s.config, count(*) AS stopword_count
          FROM query_stopwords s
          JOIN query_tokens t USING (token)
         GROUP BY s.config
    ), query_dominant_languages AS (
        SELECT config FROM query_stopword_counts
         WHERE stopword_count = (SELECT max(stopword_count) FROM query_stopword_counts)
    ), query_removed_words AS MATERIALIZED (
        SELECT DISTINCT s.token FROM query_stopwords s
        JOIN query_dominant_languages l USING (config)
    ), query_or_probes AS MATERIALIZED (
        -- Let PostgreSQL's parser distinguish a binary OR from an ordinary word.
        -- A sentinel operand exposes parser state even for consecutive OR tokens.
        SELECT t.ordinal,
               websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, t.end_position::integer) || ' proximaoperatorprobe')::text AS with_or,
               websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, (t.end_position - length(t.token))::integer)
                       || ' proximaoperatorprobe')::text AS without_or
          FROM query_tokens t
         WHERE lower(t.token) = 'or' AND NOT t.in_phrase
           AND substr(query_text, (t.end_position + 1)::integer, 1) NOT IN ('-', '_')
           AND substr(query_text, (t.end_position + 2)::integer) ~ '[^[:space:]]'
    ), query_or_operators AS MATERIALIZED (
        SELECT ordinal FROM query_or_probes
         WHERE length(with_or) - length(replace(with_or, ' | ', ''))
             > length(without_or) - length(replace(without_or, ' | ', ''))
    ), query_removed_tokens AS MATERIALIZED (
        SELECT t.ordinal FROM query_tokens t
        JOIN query_removed_words w USING (token)
         WHERE NOT EXISTS (SELECT 1 FROM query_or_operators o WHERE o.ordinal = t.ordinal)
    ), query_operand_counts AS (
        SELECT t.ordinal,
               count(*) FILTER (WHERE t.token ~ '[[:alnum:]]'
                   AND removed.ordinal IS NULL AND o.ordinal IS NULL)
                   OVER (ORDER BY t.ordinal) AS operands_before,
               count(*) FILTER (WHERE t.token ~ '[[:alnum:]]'
                   AND removed.ordinal IS NULL AND o.ordinal IS NULL)
                   OVER () AS operands_total
          FROM query_tokens t
          LEFT JOIN query_removed_tokens removed USING (ordinal)
          LEFT JOIN query_or_operators o USING (ordinal)
    ), query_retained_or AS (
        -- Removed operands can leave a leading OR or a run of adjacent ORs.
        SELECT min(o.ordinal) AS ordinal FROM query_or_operators o
        JOIN query_operand_counts counts USING (ordinal)
         WHERE operands_before > 0 AND operands_before < operands_total
         GROUP BY operands_before
    ), query_rendered_tokens AS MATERIALIZED (
        SELECT t.ordinal,
               CASE WHEN EXISTS (
                   SELECT 1 FROM query_removed_tokens removed WHERE removed.ordinal = t.ordinal
               ) THEN ''
               WHEN EXISTS (SELECT 1 FROM query_removed_tokens)
                AND EXISTS (SELECT 1 FROM query_or_operators o WHERE o.ordinal = t.ordinal)
                AND NOT EXISTS (SELECT 1 FROM query_retained_or o WHERE o.ordinal = t.ordinal)
               THEN ''
               -- A removed negated word must not transfer its minus to the next word.
               WHEN right(t.token, 1) = '-' AND EXISTS (
                   SELECT 1 FROM query_removed_tokens removed
                    WHERE removed.ordinal = t.ordinal + 1
               ) AND strpos(websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, t.end_position::integer)
                       || 'proximaoperatorprobe')::text, '!''proximaoperatorprobe''') > 0
               THEN rtrim(t.token, '-')
               ELSE t.token END AS token
          FROM query_tokens t
    )
    SELECT COALESCE(string_agg(
               -- A deleted compound prefix must not turn its following dash into NOT.
               CASE WHEN left(t.token, 1) = '-' AND EXISTS (
                   SELECT 1 FROM query_removed_tokens removed
                    WHERE removed.ordinal = t.ordinal - 1
               ) AND strpos(websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   (SELECT string_agg(prefix.token, '' ORDER BY prefix.ordinal)
                      FROM query_rendered_tokens prefix WHERE prefix.ordinal <= t.ordinal)
                       || 'proximaoperatorprobe')::text, '!''proximaoperatorprobe''') > 0
               THEN ltrim(t.token, '-')
               ELSE t.token END,
               '' ORDER BY t.ordinal
           ), query_text)
      FROM query_rendered_tokens t
$$;
