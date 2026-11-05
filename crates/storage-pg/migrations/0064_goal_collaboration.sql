-- Goal collaboration: bounded audience routes over one durable owner.
ALTER TABLE proxiima_core.memory_head
    ADD COLUMN authority_generation bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT memory_head_authority_generation_chk CHECK (authority_generation >= 0);
ALTER TABLE proxiima_core.goal_head
    ADD COLUMN authority_generation bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT goal_head_authority_generation_chk CHECK (authority_generation >= 0);

CREATE TYPE proxiima_core.goal_publication_use AS ENUM ('read', 'fork');

CREATE TABLE proxiima_core.goal_access_grant (
    goal_handle uuid NOT NULL REFERENCES proxiima_core.goal_head(handle) ON DELETE CASCADE,
    subject_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id) ON DELETE CASCADE,
    read_ceiling smallint NOT NULL CHECK (read_ceiling BETWEEN 0 AND 4),
    write_ceiling smallint NOT NULL CHECK (write_ceiling BETWEEN 0 AND read_ceiling),
    manage boolean NOT NULL,
    controller_generation bigint NOT NULL CHECK (controller_generation >= 0),
    expires_at timestamptz,
    granted_by uuid NOT NULL,
    PRIMARY KEY (goal_handle, subject_id)
);
CREATE INDEX goal_access_grant_subject_idx ON proxiima_core.goal_access_grant(subject_id, goal_handle);

CREATE TABLE proxiima_core.goal_memory_publication (
    goal_handle uuid NOT NULL REFERENCES proxiima_core.goal_head(handle) ON DELETE CASCADE,
    memory_t uuid NOT NULL REFERENCES proxiima_core.memory(t) ON DELETE CASCADE,
    source_owner uuid NOT NULL REFERENCES proxiima_core.owners(owner_id) ON DELETE CASCADE,
    source_generation bigint NOT NULL CHECK (source_generation >= 0),
    controller_generation bigint NOT NULL CHECK (controller_generation >= 0),
    permitted_use proxiima_core.goal_publication_use NOT NULL,
    cited_upload_id uuid REFERENCES proxiima_core.blob_uploads(upload_id) ON DELETE SET NULL,
    published_by uuid NOT NULL,
    PRIMARY KEY (goal_handle, memory_t)
);
CREATE INDEX goal_memory_publication_source_idx ON proxiima_core.goal_memory_publication(source_owner,goal_handle,memory_t);
CREATE INDEX goal_memory_publication_upload_idx ON proxiima_core.goal_memory_publication(cited_upload_id) WHERE cited_upload_id IS NOT NULL;
CREATE INDEX goal_memory_publication_memory_idx ON proxiima_core.goal_memory_publication(memory_t, goal_handle);

CREATE TABLE proxiima_core.goal_work_binding (
    goal_handle uuid NOT NULL REFERENCES proxiima_core.goal_head(handle) ON DELETE CASCADE,
    memory_handle uuid NOT NULL REFERENCES proxiima_core.memory_head(handle) ON DELETE CASCADE,
    source_owner uuid NOT NULL REFERENCES proxiima_core.owners(owner_id) ON DELETE CASCADE,
    source_generation bigint NOT NULL CHECK (source_generation >= 0),
    controller_generation bigint NOT NULL CHECK (controller_generation >= 0),
    bound_by uuid NOT NULL,
    PRIMARY KEY (goal_handle, memory_handle)
);
CREATE INDEX goal_work_binding_source_idx ON proxiima_core.goal_work_binding(source_owner,goal_handle,memory_handle);
CREATE INDEX goal_work_binding_memory_idx ON proxiima_core.goal_work_binding(memory_handle, goal_handle);

-- Provenance is retained through erase witnesses and is intentionally not a
-- live FK: hard deletion must remain an authorized inverse operation.
ALTER TABLE proxiima_core.memory ADD COLUMN forked_from uuid;
CREATE INDEX memory_forked_from_idx ON proxiima_core.memory(forked_from) WHERE forked_from IS NOT NULL;
