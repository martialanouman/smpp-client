-- Reverses 20260727150000_import_profiles.up.sql.
--
-- Development only, like every `down` here: `sqlx migrate revert` is never run
-- against a user's database.

DROP TABLE IF EXISTS import_profiles;
