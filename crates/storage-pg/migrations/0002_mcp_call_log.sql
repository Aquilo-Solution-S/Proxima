CREATE TABLE proxima_core.mcp_call_logged_v1 (
    memory_id uuid NOT NULL,
    tool_name text NOT NULL,
    actor_oid text NOT NULL,
    actor_upn text NOT NULL,
    ok boolean NOT NULL,
    error text,
    latency_ms integer NOT NULL,
    io_byte_len bigint NOT NULL,
    io_truncated boolean NOT NULL,
    io_content_hash bytea NOT NULL,
    CONSTRAINT mcp_call_logged_v1_io_content_hash_len_chk CHECK ((octet_length(io_content_hash) = 32)),
    CONSTRAINT mcp_call_logged_v1_io_byte_len_chk CHECK ((io_byte_len >= 0)),
    CONSTRAINT mcp_call_logged_v1_latency_ms_chk CHECK ((latency_ms >= 0))
);

CREATE TABLE proxima_core.cited_mcp_call_io_v1 (
    cited_object_id uuid NOT NULL,
    byte_len bigint NOT NULL,
    truncated boolean NOT NULL,
    body bytea NOT NULL,
    CONSTRAINT cited_mcp_call_io_v1_byte_len_chk CHECK ((byte_len >= 0))
);

CREATE TABLE proxima_core.citation_mcp_call_io_v1 (
    citation_mapping_id uuid NOT NULL
);

ALTER TABLE ONLY proxima_core.mcp_call_logged_v1
    ADD CONSTRAINT mcp_call_logged_v1_pkey PRIMARY KEY (memory_id);

ALTER TABLE ONLY proxima_core.cited_mcp_call_io_v1
    ADD CONSTRAINT cited_mcp_call_io_v1_pkey PRIMARY KEY (cited_object_id);

ALTER TABLE ONLY proxima_core.citation_mcp_call_io_v1
    ADD CONSTRAINT citation_mcp_call_io_v1_pkey PRIMARY KEY (citation_mapping_id);

ALTER TABLE ONLY proxima_core.mcp_call_logged_v1
    ADD CONSTRAINT mcp_call_logged_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);

ALTER TABLE ONLY proxima_core.cited_mcp_call_io_v1
    ADD CONSTRAINT cited_mcp_call_io_v1_cited_object_id_fkey FOREIGN KEY (cited_object_id) REFERENCES proxima_core.cited_objects(cited_object_id);

ALTER TABLE ONLY proxima_core.citation_mcp_call_io_v1
    ADD CONSTRAINT citation_mcp_call_io_v1_citation_mapping_id_fkey FOREIGN KEY (citation_mapping_id) REFERENCES proxima_core.citation_mappings(citation_mapping_id);
