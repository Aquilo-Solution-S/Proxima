-- A forget or hard erase of one memory deletes its queued embedding jobs by
-- `entity_id` alone. The unique key leads with `owner_id`, so without this
-- index every such delete reads the whole queue.
CREATE INDEX embedding_jobs_entity_id_idx ON proxima_core.embedding_jobs USING btree (entity_id);
