CREATE SCHEMA embedded_minimal;

CREATE TABLE embedded_minimal.document_filed_v1 (
    memory_id uuid NOT NULL,
    source_path text NOT NULL,
    title text NOT NULL,
    CONSTRAINT document_filed_v1_title_nonempty CHECK ((length(btrim(title)) > 0))
);
