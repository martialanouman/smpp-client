-- Reverses 20260726120000_initial_schema.up.sql (guide §11.2: every migration
-- is reversible or documented as not being so).
--
-- Reversing the INITIAL migration means an empty database: this script DROPS
-- every table, and therefore every message, contact and campaign. It exists so
-- that `sqlx migrate revert` behaves during development; it is never run
-- against a user's database.
--
-- Reverse order of creation, so a referencing table always goes before the
-- table it references.
DROP TABLE IF EXISTS pdu_log;
DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS campaigns;
DROP TABLE IF EXISTS contact_list_members;
DROP TABLE IF EXISTS contact_lists;
DROP TABLE IF EXISTS contacts;
DROP TABLE IF EXISTS session_profiles;
