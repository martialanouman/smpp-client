-- Milestone 007 — the floor of the adaptive throughput band (spec §9.5).
--
-- Spec §9.4 clamps the effective rate into `min_tps ..= throughput_tps`:
-- `throughput_tps` is the ceiling the operator asked for, and this is the
-- floor below which the congestion adaptation may not push it. The `max_tps`
-- that spec §9.5 also lists is `throughput_tps` itself — a second column for
-- it would be a second copy of the same number, free to disagree.
--
-- Milestone 007 wires the parameter and applies a constant adaptive factor of
-- 1.0, so the floor is carried and never reached; milestone 012 is what moves
-- the factor and makes the clamp bite. The column arrives now because a
-- setting the operator can enter and the application cannot remember is worse
-- than no setting at all.
--
-- The default of 1 reproduces milestone 007's behaviour exactly: every profile
-- written before this migration keeps working unchanged. Zero is refused
-- rather than read as "unlimited" — that meaning belongs to `throughput_tps`,
-- and a floor of zero would let the adaptation of milestone 012 stop the
-- session outright.

ALTER TABLE session_profiles
    ADD COLUMN min_tps INTEGER NOT NULL DEFAULT 1
    CHECK (min_tps >= 1);
