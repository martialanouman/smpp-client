-- Initial schema — spec §14.2.
--
-- IMMUTABLE ONCE SHIPPED (guide §11.2). Changing a single byte of this file
-- changes its checksum: `sqlx migrate run` then refuses to touch any database
-- where it was already applied, and `migration_checksums_are_stable` in
-- `tests/migrations.rs` fails. A schema change is a NEW migration.
--
-- Conventions applied throughout:
--   · identifiers are TEXT holding a UUID in its canonical form — spec §14.2,
--     and `smpp_core::types` parses exactly that form;
--   · timestamps are TEXT in ISO-8601 UTC, produced only by
--     `persistence::time::Timestamp` so that no second format can appear;
--   · a column commented "JSON" holds an opaque document. The persistence
--     layer never reads inside it; the crate that owns the shape does.

-- Connection profiles — spec §14.2, §8.2.
CREATE TABLE session_profiles (
    session_id         TEXT    PRIMARY KEY NOT NULL,
    name               TEXT    NOT NULL,
    host               TEXT    NOT NULL,
    port               INTEGER NOT NULL,
    -- CHECK constraints on the two closed sets of spec §14.2. The Rust side
    -- already models them as enums; this is the second line of defence, the
    -- one that survives a hand-written UPDATE on the file.
    bind_type          TEXT    NOT NULL CHECK (bind_type IN ('transmitter', 'receiver', 'transceiver')),
    interface_version  TEXT    NOT NULL CHECK (interface_version IN ('v3.4', 'v5.0')),
    system_id          TEXT    NOT NULL,
    -- Opaque AES-256-GCM blob. Milestone 002 stores and returns bytes and
    -- never interprets them; the cryptography is milestone 015's (spec §17.2).
    -- Until then no real password is written here.
    password_enc       BLOB    NOT NULL,
    system_type        TEXT    NOT NULL DEFAULT '',
    tls_config         TEXT,
    window_size        INTEGER NOT NULL DEFAULT 50,
    throughput_tps     INTEGER NOT NULL DEFAULT 100,
    enquire_link_s     INTEGER NOT NULL DEFAULT 30,
    response_timeout_s INTEGER NOT NULL DEFAULT 10,
    reconnect_config   TEXT,
    bind_count         INTEGER NOT NULL DEFAULT 1,
    created_at         TEXT    NOT NULL,
    updated_at         TEXT    NOT NULL
);

-- Contacts — spec §14.2, §11.1.
CREATE TABLE contacts (
    contact_id TEXT    PRIMARY KEY NOT NULL,
    msisdn     TEXT    NOT NULL,
    country    TEXT,
    valid      INTEGER NOT NULL DEFAULT 1,
    line_type  TEXT,
    attributes TEXT,
    source     TEXT,
    created_at TEXT    NOT NULL
);

-- NOT unique: the same number may legitimately arrive from two imports with
-- different attributes. Deduplication is a decision taken at import time
-- (spec §11.6, milestone 006), not a constraint imposed by the schema.
CREATE INDEX idx_contacts_msisdn ON contacts (msisdn);

CREATE TABLE contact_lists (
    list_id    TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE contact_list_members (
    -- NOT NULL is spelled out: in a rowid table SQLite does NOT derive it from
    -- PRIMARY KEY, a documented legacy quirk. Without it a (NULL, NULL) row is
    -- insertable and the primary key stops being one.
    list_id    TEXT NOT NULL REFERENCES contact_lists (list_id) ON DELETE CASCADE,
    contact_id TEXT NOT NULL REFERENCES contacts (contact_id) ON DELETE CASCADE,
    PRIMARY KEY (list_id, contact_id)
);

-- Campaigns — spec §14.2, §10.3.
CREATE TABLE campaigns (
    campaign_id     TEXT    PRIMARY KEY NOT NULL,
    name            TEXT    NOT NULL,
    -- No CHECK here, unlike `messages.state`: spec §14.2 writes the status set
    -- as "CREATED|RUNNING|PAUSED|COMPLETED|..." — the ellipsis is the spec
    -- saying the list is open. A CHECK would freeze a set the spec left ajar.
    status          TEXT    NOT NULL,
    template        TEXT    NOT NULL,
    send_config     TEXT    NOT NULL,
    total_count     INTEGER NOT NULL DEFAULT 0,
    sent_count      INTEGER NOT NULL DEFAULT 0,
    delivered_count INTEGER NOT NULL DEFAULT 0,
    failed_count    INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL,
    started_at      TEXT,
    completed_at    TEXT
);

-- Messages — the write-ahead business journal (spec §14.2, CLAUDE.md §4).
CREATE TABLE messages (
    client_message_id TEXT    PRIMARY KEY NOT NULL,
    -- ON DELETE SET NULL, never CASCADE. This table is the audit trail of what
    -- actually left the machine (spec §17.6); deleting a campaign must not
    -- erase the evidence that its messages were sent.
    campaign_id       TEXT    REFERENCES campaigns (campaign_id) ON DELETE SET NULL,
    session_id        TEXT    REFERENCES session_profiles (session_id) ON DELETE SET NULL,
    smsc_message_id   TEXT,
    source_addr       TEXT,
    source_ton        INTEGER,
    source_npi        INTEGER,
    dest_addr         TEXT,
    dest_ton          INTEGER,
    dest_npi          INTEGER,
    data_coding       INTEGER,
    segments          INTEGER NOT NULL DEFAULT 1,
    text              TEXT,
    -- Closed set, spelled out in full by spec §14.3.
    state             TEXT    NOT NULL CHECK (state IN ('QUEUED', 'SENT', 'ACCEPTED', 'DELIVERED', 'FAILED', 'EXPIRED')),
    command_status    INTEGER,
    dlr_stat          TEXT,
    dlr_err           TEXT,
    attempts          INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL,
    sent_at           TEXT,
    resp_at           TEXT,
    dlr_at            TEXT
);

CREATE INDEX idx_messages_campaign ON messages (campaign_id);
CREATE INDEX idx_messages_state ON messages (state);

-- Created now although DLR correlation only lands at milestone 008 (step-002
-- §6): adding an index to a `messages` table already holding hundreds of
-- thousands of rows is a long write lock on the user's machine.
CREATE INDEX idx_messages_smscid ON messages (smsc_message_id);

-- PDU log — spec §14.2, optional and debug-only.
CREATE TABLE pdu_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT,
    direction       TEXT    NOT NULL CHECK (direction IN ('in', 'out')),
    command_id      INTEGER,
    command_status  INTEGER,
    sequence_number INTEGER,
    -- `raw_hex` and `decoded` may hold message content and, on a bind PDU, a
    -- password. Spec §17.7 and CLAUDE.md §8 confine both to explicit debug
    -- mode; the schema cannot enforce that, the writer must.
    raw_hex         TEXT,
    decoded         TEXT,
    ts              TEXT    NOT NULL
);
