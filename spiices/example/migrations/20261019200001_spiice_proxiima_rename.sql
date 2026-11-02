
-- rename-finalization-begin

-- Catalog objects are renamed only after every immutable historical lane.
-- The temporary mapping and snapshots disappear when this session closes.
CREATE OR REPLACE FUNCTION pg_temp.rename_spiice_word(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $mapping$
DECLARE rest text := value; result text := ''; at integer; word text; replacement text; i integer;
BEGIN
    LOOP
        at := regexp_instr(rest, 'flavou?r|(?<!ap)proxima', 1, 1, 0, 'i');
        EXIT WHEN at = 0;
        word := (regexp_match(substr(rest, at), '^(flavou?r|proxima)', 'i'))[1];
        replacement := CASE WHEN lower(word) LIKE 'fla%' THEN 'spiice' ELSE 'proxiima' END;
        IF word = upper(word) THEN replacement := upper(replacement);
        ELSIF word = initcap(lower(word)) THEN replacement := initcap(replacement);
        ELSIF word <> lower(word) THEN
            FOR i IN 1..length(replacement) LOOP
                IF substr(word, least(i, length(word)), 1) = upper(substr(word, least(i, length(word)), 1)) THEN
                    replacement := overlay(replacement placing upper(substr(replacement,i,1)) from i for 1);
                END IF;
            END LOOP;
        END IF;
        IF lower(word)='proxima' AND (
            (word=upper(word) AND (substr(rest,at+length(word),1)='L' OR substr(rest,at+length(word),2)='TE'))
            OR (word<>upper(word) AND (substr(rest,at+length(word),1)='l' OR substr(rest,at+length(word),2)='te'))
        ) THEN replacement:=word; END IF;
        result := result || substr(rest, 1, at-1) || replacement;
        rest := substr(rest, at + length(word));
    END LOOP;
    RETURN regexp_replace(result || rest, '(Aquilo-Solution-S/)proxiima', '\1proxiima', 'gi');
END $mapping$;

CREATE TEMP TABLE rename_spiice_definitions(kind text, schema_name text, table_name text, object_name text, statement text, owner_name text) ON COMMIT DROP;
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement,owner_name)
SELECT 'function', n.nspname, '', p.proname, pg_temp.rename_spiice_word(pg_get_functiondef(p.oid)), pg_get_userbyid(p.proowner)
FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND p.prokind IN ('f','p')
AND pg_get_functiondef(p.oid) <> pg_temp.rename_spiice_word(pg_get_functiondef(p.oid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'trigger', n.nspname, c.relname, t.tgname, pg_temp.rename_spiice_word(pg_get_triggerdef(t.oid, true))
FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND NOT t.tgisinternal
AND pg_get_triggerdef(t.oid, true) <> pg_temp.rename_spiice_word(pg_get_triggerdef(t.oid, true));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'view', n.nspname, c.relname, c.relname,
       pg_temp.rename_spiice_word(format('CREATE OR REPLACE VIEW %I.%I AS %s', n.nspname, c.relname, pg_get_viewdef(c.oid, true)))
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind='v'
AND format('%I.%I %s',n.nspname,c.relname,pg_get_viewdef(c.oid,true)) <>
    pg_temp.rename_spiice_word(format('%I.%I %s',n.nspname,c.relname,pg_get_viewdef(c.oid,true)));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'policy', n.nspname, c.relname, p.polname,
       pg_temp.rename_spiice_word(format('CREATE POLICY %I ON %I.%I AS %s FOR %s TO %s%s%s',
         p.polname,n.nspname,c.relname,CASE WHEN p.polpermissive THEN 'PERMISSIVE' ELSE 'RESTRICTIVE' END,
         CASE p.polcmd WHEN '*' THEN 'ALL' WHEN 'r' THEN 'SELECT' WHEN 'a' THEN 'INSERT' WHEN 'w' THEN 'UPDATE' WHEN 'd' THEN 'DELETE' END,
         (SELECT string_agg(CASE WHEN role_oid=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(role_oid)) END, ', ' ORDER BY role_oid) FROM unnest(p.polroles) role_oid),
         CASE WHEN p.polqual IS NULL THEN '' ELSE ' USING ('||pg_get_expr(p.polqual,p.polrelid)||')' END,
         CASE WHEN p.polwithcheck IS NULL THEN '' ELSE ' WITH CHECK ('||pg_get_expr(p.polwithcheck,p.polrelid)||')' END))
FROM pg_policy p JOIN pg_class c ON c.oid=p.polrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)';

INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'constraint', n.nspname,c.relname,k.conname,
       pg_temp.rename_spiice_word(format('ALTER TABLE %I.%I ADD CONSTRAINT %I %s',n.nspname,c.relname,k.conname,pg_get_constraintdef(k.oid)))
FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND k.contype='c'
AND pg_get_constraintdef(k.oid)<>pg_temp.rename_spiice_word(pg_get_constraintdef(k.oid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'default',n.nspname,c.relname,a.attname,
       pg_temp.rename_spiice_word(CASE WHEN a.attgenerated<>'' THEN format('ALTER TABLE %I.%I ALTER COLUMN %I SET EXPRESSION AS (%s)',n.nspname,c.relname,a.attname,pg_get_expr(d.adbin,d.adrelid)) ELSE format('ALTER TABLE %I.%I ALTER COLUMN %I SET DEFAULT %s',n.nspname,c.relname,a.attname,pg_get_expr(d.adbin,d.adrelid)) END)
FROM pg_attrdef d JOIN pg_attribute a ON a.attrelid=d.adrelid AND a.attnum=d.adnum JOIN pg_class c ON c.oid=d.adrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)'
AND pg_get_expr(d.adbin,d.adrelid)<>pg_temp.rename_spiice_word(pg_get_expr(d.adbin,d.adrelid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'index',n.nspname,c.relname,i.relname,pg_temp.rename_spiice_word(pg_get_indexdef(x.indexrelid))
FROM pg_index x JOIN pg_class i ON i.oid=x.indexrelid JOIN pg_class c ON c.oid=x.indrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND NOT EXISTS(SELECT 1 FROM pg_constraint k WHERE k.conindid=x.indexrelid)
AND coalesce(pg_get_expr(x.indexprs,x.indrelid),'')||coalesce(pg_get_expr(x.indpred,x.indrelid),'') <>
    pg_temp.rename_spiice_word(coalesce(pg_get_expr(x.indexprs,x.indrelid),'')||coalesce(pg_get_expr(x.indpred,x.indrelid),''));

DO $rename$
DECLARE object record; statement record; new_name text; temporary_create boolean;
BEGIN
    -- Drop policies/triggers before renaming columns; their saved expressions
    -- include literal sidecar names which Postgres cannot update by object ID.
    FOR object IN SELECT * FROM rename_spiice_definitions WHERE kind IN ('policy','trigger','constraint','index') ORDER BY kind,schema_name,table_name,object_name LOOP
        IF object.kind='constraint' THEN
            EXECUTE format('ALTER TABLE %I.%I DROP CONSTRAINT %I',object.schema_name,object.table_name,object.object_name);
        ELSIF object.kind='index' THEN
            EXECUTE format('DROP INDEX %I.%I',object.schema_name,object.object_name);
        ELSE EXECUTE format('DROP %s %I ON %I.%I', upper(object.kind),object.object_name,object.schema_name,object.table_name);
        END IF;
    END LOOP;
    FOR object IN SELECT n.nspname,c.relname,a.attname FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind IN ('r','p','v','m','f','i','I') AND a.attnum>0 AND NOT a.attisdropped
        AND a.attname<>pg_temp.rename_spiice_word(a.attname) ORDER BY n.nspname,c.relname,a.attnum LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME COLUMN %I TO %I',object.nspname,object.relname,object.attname,pg_temp.rename_spiice_word(object.attname));
    END LOOP;
    FOR object IN SELECT n.nspname,c.relname,k.conname FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND k.conname<>pg_temp.rename_spiice_word(k.conname) ORDER BY n.nspname,c.relname,k.conname LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',object.nspname,object.relname,object.conname,pg_temp.rename_spiice_word(object.conname));
    END LOOP;
    FOR object IN SELECT n.nspname,t.typname,e.enumlabel FROM pg_enum e JOIN pg_type t ON t.oid=e.enumtypid JOIN pg_namespace n ON n.oid=t.typnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND e.enumlabel<>pg_temp.rename_spiice_word(e.enumlabel) ORDER BY n.nspname,t.typname,e.enumsortorder LOOP
        EXECUTE format('ALTER TYPE %I.%I RENAME VALUE %L TO %L',object.nspname,object.typname,object.enumlabel,pg_temp.rename_spiice_word(object.enumlabel));
    END LOOP;
    FOR object IN SELECT n.nspname,p.proname,pg_get_function_identity_arguments(p.oid) args FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND p.prokind IN ('f','p') AND p.proname<>pg_temp.rename_spiice_word(p.proname) ORDER BY n.nspname,p.proname,p.oid LOOP
        EXECUTE format('ALTER ROUTINE %I.%I(%s) RENAME TO %I',object.nspname,object.proname,object.args,pg_temp.rename_spiice_word(object.proname));
    END LOOP;
    FOR object IN SELECT n.nspname,t.typname FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND t.typrelid=0 AND t.typelem=0
        AND t.typname<>pg_temp.rename_spiice_word(t.typname) ORDER BY n.nspname,t.typname LOOP
        EXECUTE format('ALTER TYPE %I.%I RENAME TO %I',object.nspname,object.typname,pg_temp.rename_spiice_word(object.typname));
    END LOOP;
    -- Constraint-backed indexes were renamed with their constraint above.
    FOR object IN SELECT n.nspname,c.relname,c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind IN ('r','p','v','m','S','i','I','f')
        AND c.relname<>pg_temp.rename_spiice_word(c.relname) ORDER BY n.nspname,c.relname LOOP
        EXECUTE format('ALTER %s %I.%I RENAME TO %I',
          CASE object.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'i' THEN 'INDEX' WHEN 'I' THEN 'INDEX' WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END,
          object.nspname,object.relname,pg_temp.rename_spiice_word(object.relname));
    END LOOP;
    FOR object IN SELECT nspname FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname<>pg_temp.rename_spiice_word(nspname) ORDER BY nspname LOOP
        EXECUTE format('ALTER SCHEMA %I RENAME TO %I',object.nspname,pg_temp.rename_spiice_word(object.nspname));
    END LOOP;
    -- Bodies are restored from catalog definitions, with names rewritten by
    -- the same rule as the operator's source transformation.
    PERFORM set_config('check_function_bodies','off',true);
    FOR statement IN SELECT * FROM rename_spiice_definitions ORDER BY CASE kind WHEN 'function' THEN 0 WHEN 'view' THEN 1 WHEN 'constraint' THEN 2 WHEN 'index' THEN 3 WHEN 'default' THEN 4 WHEN 'trigger' THEN 5 ELSE 6 END,schema_name,table_name,object_name LOOP
        IF statement.kind='function' THEN
            new_name:=pg_temp.rename_spiice_word(statement.schema_name);
            temporary_create:=NOT has_schema_privilege(statement.owner_name,new_name,'CREATE');
            IF temporary_create THEN EXECUTE format('GRANT CREATE ON SCHEMA %I TO %I',new_name,statement.owner_name); END IF;
            EXECUTE format('SET LOCAL ROLE %I',statement.owner_name);
            EXECUTE statement.statement;
            RESET ROLE;
            IF temporary_create THEN EXECUTE format('REVOKE CREATE ON SCHEMA %I FROM %I',new_name,statement.owner_name); END IF;
        ELSE EXECUTE statement.statement;
        END IF;
    END LOOP;
    -- Registry data contains SQL identifiers, rather than user prose.
    IF to_regclass('proxiima_core.spiice_surface') IS NOT NULL THEN
        UPDATE proxiima_core.spiice_surface SET table_name=pg_temp.rename_spiice_word(table_name),spiice_id=pg_temp.rename_spiice_word(spiice_id);
    END IF;
    IF to_regclass('proxiima_core.memory') IS NOT NULL THEN
        UPDATE proxiima_core.memory SET sidecar_tables=ARRAY(SELECT pg_temp.rename_spiice_word(x) FROM unnest(sidecar_tables) x)
        WHERE EXISTS(SELECT 1 FROM unnest(sidecar_tables) x WHERE x<>pg_temp.rename_spiice_word(x));
    END IF;

    FOR object IN
        SELECT d.description, identified.type AS kind, identified.identity
          FROM pg_description d
          CROSS JOIN LATERAL pg_identify_object(d.classoid,d.objoid,d.objsubid) identified
         WHERE d.objoid>=16384 AND coalesce(identified.schema,'') !~ '^pg_'
           AND d.description<>pg_temp.rename_spiice_word(d.description)
         ORDER BY identified.type,identified.identity
    LOOP
        new_name:=CASE object.kind
            WHEN 'table column' THEN 'COLUMN' WHEN 'view column' THEN 'COLUMN'
            WHEN 'materialized view column' THEN 'COLUMN' WHEN 'foreign table column' THEN 'COLUMN'
            WHEN 'table constraint' THEN 'CONSTRAINT'
            WHEN 'table' THEN 'TABLE' WHEN 'foreign table' THEN 'FOREIGN TABLE'
            WHEN 'view' THEN 'VIEW' WHEN 'materialized view' THEN 'MATERIALIZED VIEW'
            WHEN 'sequence' THEN 'SEQUENCE' WHEN 'index' THEN 'INDEX'
            WHEN 'function' THEN 'FUNCTION' WHEN 'procedure' THEN 'PROCEDURE'
            WHEN 'type' THEN 'TYPE' WHEN 'domain' THEN 'DOMAIN' WHEN 'schema' THEN 'SCHEMA'
            WHEN 'trigger' THEN 'TRIGGER' WHEN 'policy' THEN 'POLICY'
            ELSE NULL END;
        IF new_name IS NULL THEN RAISE EXCEPTION 'unsupported catalog comment kind %',object.kind; END IF;
        EXECUTE format('COMMENT ON %s %s IS %L',new_name,object.identity,pg_temp.rename_spiice_word(object.description));
    END LOOP;
END $rename$;
DROP TABLE rename_spiice_definitions;
DROP FUNCTION pg_temp.rename_spiice_word(text);
