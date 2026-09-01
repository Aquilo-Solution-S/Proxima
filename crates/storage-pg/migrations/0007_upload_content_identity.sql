-- Upload publication and Memory transfer need one owner-free identity for the
-- staged bytes before finish attaches blob_id. SHA-256 is the transport audit
-- digest; this BLAKE3 value is the cited-object content address and therefore
-- the value that can be compared to proxima_core.blob without a flavor
-- sidecar.
ALTER TABLE proxima_core.blob_uploads
    ADD COLUMN content_hash bytea;

ALTER TABLE proxima_core.blob_uploads
    ADD CONSTRAINT blob_uploads_content_hash_chk
        CHECK (content_hash IS NULL OR octet_length(content_hash) = 32);

-- Same-owner completed rows inherit the address only when every field needed
-- for a readable publication is present and the object key was minted from
-- this upload lineage. Pending, terminal, malformed-locator and cross-owner
-- legacy rows are left NULL because guessing their bytes would turn malformed
-- bookkeeping into authority over an unrelated cited object. A pending row
-- already carrying a canonical locator and SHA-256 is a pre-migration staged
-- upload; the transfer fence holds its owner until a completion retry re-hashes
-- the retained bytes and fills this identity.
UPDATE proxima_core.blob_uploads u
   SET content_hash = b.content_hash
  FROM proxima_core.blob b
 WHERE u.blob_id = b.blob_id
   AND u.owner_id = b.owner_id
   AND u.status = 'completed'
   AND u.completed_at IS NOT NULL
   AND octet_length(u.sha256) = 32
   AND u.expected_byte_len >= 0
   AND btrim(u.bucket) <> ''
   AND btrim(u.filename) <> ''
   AND btrim(u.mime) <> ''
   AND u.object_key =
       'objects/' || COALESCE(u.mounted_from_upload_id, u.upload_id)::text
   AND u.content_hash IS NULL;

CREATE INDEX blob_uploads_terminal_content_idx
    ON proxima_core.blob_uploads (owner_id, content_hash)
    WHERE status IN ('aborted', 'expired');
