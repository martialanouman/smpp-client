-- Milestone 005 — the two GSM 7-bit layout characteristics of a session.
--
-- ADR 0008 settled the packing and left the octet *reading* open; ADR 0009
-- settles it and puts both in the session profile, because both are decided by
-- the message centre and neither can be inferred from the message.
--
-- Getting either wrong is silent in production and invisible in testing: a
-- pure-ASCII message travels identically under every combination, and only
-- `@ £ $ €` and the accented letters come out wrong on the handset. Hence
-- columns rather than a guess, and hence the CHECK constraints — the second
-- line of defence that survives a hand-written UPDATE on the file.
--
-- The defaults reproduce milestone 004's behaviour exactly, so every profile
-- written before this migration keeps working unchanged.

ALTER TABLE session_profiles
    ADD COLUMN gsm7_packing TEXT NOT NULL DEFAULT 'unpacked'
    CHECK (gsm7_packing IN ('unpacked', 'packed'));

ALTER TABLE session_profiles
    ADD COLUMN gsm7_charset TEXT NOT NULL DEFAULT 'gsm0338'
    CHECK (gsm7_charset IN ('gsm0338', 'latin1'));
