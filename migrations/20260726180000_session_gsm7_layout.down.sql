-- Reverses 20260726180000_session_gsm7_layout.up.sql (guide §11.2).
--
-- `ALTER TABLE ... DROP COLUMN` needs SQLite 3.35; the bundled `libsqlite3-sys`
-- is well past that, and the columns carry no constraint that would block the
-- drop. Reverting loses the layout settings of every profile, which then fall
-- back to the milestone-004 behaviour — unpacked, GSM 03.38 — on the next
-- upgrade.

ALTER TABLE session_profiles DROP COLUMN gsm7_charset;

ALTER TABLE session_profiles DROP COLUMN gsm7_packing;
