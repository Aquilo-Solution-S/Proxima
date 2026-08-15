-- Flavor SQL still names the pre-v0.0.8 enum. Same labels as owner_kind.
CREATE TYPE proxima_core.owner_ref_kind AS ENUM (
    'world',
    'personal',
    'group'
);
