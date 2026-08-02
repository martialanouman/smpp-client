-- Milestone 009 — reusable column-mapping profiles (CA-009-09).
--
-- A customer sends the same file every month with the same columns in the same
-- order. The criterion asks that the mapping chosen once be reusable on the
-- next file "without retyping", which means it has to outlive the import that
-- created it.
--
-- `mapping` is an opaque JSON document, exactly like `contacts.attributes`,
-- `session_profiles.tls_config` and `campaigns.send_config` (spec §14.2). The
-- shape belongs to `contacts::import::mapping::ColumnMapping`, which is where
-- it is validated; a schema mirroring the roles would need a migration every
-- time a role is added, and this table would then know about column mapping,
-- which is not storage's business.
--
-- `name` is UNIQUE: the interface offers profiles by name, and two profiles
-- called "fichier client" is a choice nobody can make.
--
-- IMMUTABLE ONCE SHIPPED (guide §11.2). A change is a NEW migration.

CREATE TABLE import_profiles (
    profile_id TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL UNIQUE,
    mapping    TEXT NOT NULL,
    created_at TEXT NOT NULL
);
