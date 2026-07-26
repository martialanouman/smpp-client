-- Reverses 20260726210000_session_min_tps.up.sql (guide §11.2).
--
-- `ALTER TABLE ... DROP COLUMN` needs SQLite 3.35; the bundled
-- `libsqlite3-sys` is well past that. Reverting loses the floor of every
-- profile, which then falls back to 1 — the milestone-007 behaviour — on the
-- next upgrade.

ALTER TABLE session_profiles DROP COLUMN min_tps;
