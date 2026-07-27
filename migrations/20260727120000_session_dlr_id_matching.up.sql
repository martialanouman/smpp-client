-- Milestone 008 — how hard to look for a message when a delivery receipt
-- quotes its identifier differently (step-008 §6).
--
-- step-008 §6 names the first cause of uncorrelated receipts in production: a
-- message centre that answers `submit_sm` with an identifier in one form and
-- quotes it in another. Two kinds of difference exist, and they are NOT equally
-- safe to paper over.
--
-- `relaxed` — the default — also tries the case variants and the unpadded form.
-- Both are lossless: two identifiers differing only in case or in leading
-- zeroes are the same identifier written twice.
--
-- `bases` also reads the identifier in the other base. That is LOSSY: `101`
-- read as hexadecimal is `257`, and as decimal-rendered-hex is `65` — three
-- distinct identifiers, any of which another message may carry. Since the
-- "identifier not found" path is the nominal one (a split message produces one
-- orphaned receipt per extra segment), leaving this on for every centre credits
-- unrelated messages reliably rather than improbably. So it is opt-in, per
-- centre, by the operator who knows their provider changes base.
--
-- `exact` refuses every normalisation, for a centre whose identifiers are
-- opaque strings.
--
-- The default reproduces the safe half of milestone 008's original behaviour:
-- every profile written before this migration keeps working, and none of them
-- keeps the base conversion that made the correlation unsafe.
--
-- IMMUTABLE ONCE SHIPPED (guide §11.2). A change is a NEW migration.

ALTER TABLE session_profiles
    ADD COLUMN dlr_id_matching TEXT NOT NULL DEFAULT 'relaxed'
    CHECK (dlr_id_matching IN ('exact', 'relaxed', 'bases'));
