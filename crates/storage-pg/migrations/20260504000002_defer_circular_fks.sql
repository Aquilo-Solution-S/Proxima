-- M2.4b — defer the circular Fact ↔ CitationMapping FKs so a
-- single-tx insert can write both rows.

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_citation_mapping_id_fkey;
ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_citation_mapping_id_fkey
    FOREIGN KEY (citation_mapping_id)
    REFERENCES proxima_core.citation_mappings(citation_mapping_id)
    DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE proxima_core.citation_mappings
    DROP CONSTRAINT citation_mappings_memory_fk;
ALTER TABLE proxima_core.citation_mappings
    ADD CONSTRAINT citation_mappings_memory_fk
    FOREIGN KEY (memory_id)
    REFERENCES proxima_core.memories(memory_id)
    DEFERRABLE INITIALLY DEFERRED;
