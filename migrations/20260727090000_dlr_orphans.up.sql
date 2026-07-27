-- Milestone 008 — delivery receipts that correlate to no message (CA-008-04).
--
-- A message centre sends receipts this client cannot attach to anything: a
-- previous installation sharing the `system_id`, a message submitted before
-- this journal existed, a fragment of a partially failed multi-segment message
-- (whose row deliberately keeps no `smsc_message_id` — milestone 006), or a
-- body so mangled that no identifier can be read out of it.
--
-- step-008 §2 is explicit that such a receipt is "kept and reported, never
-- silently discarded". It cannot go in `messages`: there is no message. So it
-- goes here, with the reason it did not correlate, and the log screen shows it.
-- Dropping it would remove the only diagnostic an operator has for "my
-- delivery rate is lower than my provider's".
--
-- IMMUTABLE ONCE SHIPPED (guide §11.2). A change is a NEW migration.

CREATE TABLE dlr_orphans (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    -- ON DELETE SET NULL, as `messages` does: deleting a profile must not
    -- erase the evidence that its message centre sent something odd.
    session_id      TEXT    REFERENCES session_profiles (session_id) ON DELETE SET NULL,
    -- NULL when the receipt carried no readable identifier at all.
    smsc_message_id TEXT,
    -- Closed set, mirroring `messaging::correlation::OrphanReason`.
    reason          TEXT    NOT NULL CHECK (reason IN ('UNKNOWN_ID', 'NO_IDENTIFIER')),
    dlr_stat        TEXT,
    dlr_err         TEXT,
    submit_date     TEXT,
    done_date       TEXT,
    -- The body exactly as it arrived. It may hold message content through the
    -- receipt's `text:` field, so it is masked WHERE IT IS RENDERED
    -- (CLAUDE.md §8) and not on the way in: an orphan whose body has been
    -- redacted before storage is an orphan nobody can diagnose.
    raw             TEXT    NOT NULL,
    -- When THIS application received it. Not the centre's `done date`, which
    -- is a different clock, unverifiable and frequently in local time.
    received_at     TEXT    NOT NULL
);

-- The two ways the log screen reads this table: by arrival (the default
-- ordering, served by the primary key) and by identifier, which is how an
-- operator checks whether a receipt their provider swears they sent ever
-- arrived.
CREATE INDEX idx_dlr_orphans_smscid ON dlr_orphans (smsc_message_id);
CREATE INDEX idx_dlr_orphans_session ON dlr_orphans (session_id);
