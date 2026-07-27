-- Reverses 20260727120000_session_dlr_id_matching.up.sql (guide §11.2).
--
-- Reverting loses the per-centre setting; every profile falls back to `relaxed`
-- on the next upgrade, which is the safe policy. A profile that had been set to
-- `bases` for a centre that changes base would stop correlating that centre's
-- receipts — visibly, as orphans, rather than silently.

ALTER TABLE session_profiles DROP COLUMN dlr_id_matching;
