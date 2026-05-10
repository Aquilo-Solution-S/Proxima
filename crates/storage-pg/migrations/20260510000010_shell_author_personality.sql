-- Substrate-managed marker: each owner has a `proxima/shell-author`
-- personality with display_name = 'shell-author' (lives on its Root
-- Perspective memory.text) that authors the audit Fact memories
-- emitted by master-token MCP-CRUD calls. Empty WakeConfig means the
-- dispatcher never fires it.
--
-- This migration adds nothing schema-wise; the shell-author personality
-- is materialised lazily via Storage::ensure_shell_author_personality
-- on the first master-token MCP-CRUD call per owner.

SELECT 1;  -- intentional no-op; runtime path handles backfill
