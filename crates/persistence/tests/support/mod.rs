//! Shared harness: one temporary database per test, and minimal fixtures.
//!
//! CLAUDE.md §7 requires each integration test to own its database. That is
//! not a formality here: `cargo nextest` runs every test in its own process
//! and several at once, and SQLite serialises writers per **file** — sharing
//! one would make the suite's outcome depend on its scheduling.
//!
//! Every fixture takes the values that identify it as arguments and reads no
//! clock and no random source beyond the identifiers themselves, so a failure
//! reproduces exactly.

// This module is compiled into every integration test binary, and no single
// binary uses all of it.
#![allow(dead_code)]

use std::time::Duration;

use persistence::{
    BindType, Campaign, CampaignId, CampaignStatus, Contact, ContactId, ContactList, Database,
    DatabaseConfig, LineType, ListId, Message, MessageState, SessionProfile, Timestamp,
};
use smpp_core::types::{ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};
use tempfile::TempDir;

/// An open database in a directory that disappears with it.
///
/// Holds the [`TempDir`] rather than leaking it: dropping the harness removes
/// the `.db`, the `-wal` and the `-shm` files together.
pub(crate) struct TempDatabase {
    directory: TempDir,
    database: Database,
}

impl TempDatabase {
    /// The open database.
    pub(crate) fn database(&self) -> &Database {
        &self.database
    }

    /// A second, independent handle on the same file.
    ///
    /// Used by the concurrency tests: a separate pool is the only way to
    /// observe what WAL actually buys, since two handles on the *same* pool
    /// could be served by the same connection.
    pub(crate) async fn reopen(&self) -> Database {
        Database::open(self.config())
            .await
            .expect("reopening a database that is already open")
    }

    /// The configuration pointing at this harness's file.
    pub(crate) fn config(&self) -> DatabaseConfig {
        DatabaseConfig::new(self.directory.path().join("shinobismpp.db"))
            // Short on purpose: a test that deadlocks on the write lock should
            // fail in seconds, not sit on the default five.
            .with_busy_timeout(Duration::from_secs(2))
    }
}

/// Opens a database in a fresh temporary directory, migrations applied.
pub(crate) async fn temp_database() -> TempDatabase {
    let directory = TempDir::new().expect("creating a temporary directory");
    let config = DatabaseConfig::new(directory.path().join("shinobismpp.db"))
        .with_busy_timeout(Duration::from_secs(2));

    let database = Database::open(config)
        .await
        .expect("opening a fresh database");

    TempDatabase {
        directory,
        database,
    }
}

/// A fixed instant, so an assertion can name the value it expects.
pub(crate) fn instant(text: &str) -> Timestamp {
    Timestamp::parse(text).expect("the fixture instant is valid RFC 3339")
}

/// A session profile with a deliberately fake credential.
///
/// The blob is `b"not-a-real-secret"`, and that is the point: milestone 015
/// owns the encryption, and CLAUDE.md §8 forbids a real credential in a
/// fixture whether or not it is encrypted.
pub(crate) fn a_session_profile(session_id: SessionId, name: &str) -> SessionProfile {
    SessionProfile {
        session_id,
        name: name.to_owned(),
        host: String::from("smsc.example.test"),
        port: 2775,
        bind_type: BindType::Transceiver,
        interface_version: SmppVersion::V5_0,
        system_id: String::from("esme-test"),
        password_enc: b"not-a-real-secret".to_vec(),
        system_type: String::new(),
        tls_config: None,
        window_size: 50,
        throughput_tps: 100,
        min_tps: 1,
        enquire_link_s: 30,
        response_timeout_s: 10,
        reconnect_config: None,
        gsm7_packing: Gsm7BitPacking::Unpacked,
        gsm7_charset: Gsm7BitCharset::Gsm0338,
        bind_count: 1,
        dlr_id_matching: persistence::IdMatching::default(),
        created_at: instant("2026-07-26T10:00:00Z"),
        updated_at: instant("2026-07-26T10:00:00Z"),
    }
}

/// A contact carrying the given number.
pub(crate) fn a_contact(contact_id: ContactId, msisdn: &str) -> Contact {
    Contact {
        contact_id,
        msisdn: Msisdn::parse(msisdn).expect("the fixture number is valid"),
        country: Some(String::from("CI")),
        valid: true,
        line_type: Some(LineType::Mobile),
        attributes: Some(String::from(r#"{"prenom":"Awa"}"#)),
        source: Some(String::from("fixture")),
        created_at: instant("2026-07-26T10:00:00Z"),
    }
}

/// A contact list.
pub(crate) fn a_contact_list(list_id: ListId, name: &str) -> ContactList {
    ContactList {
        list_id,
        name: name.to_owned(),
        created_at: instant("2026-07-26T10:00:00Z"),
    }
}

/// A campaign in its initial status.
pub(crate) fn a_campaign(campaign_id: CampaignId, name: &str) -> Campaign {
    Campaign {
        campaign_id,
        name: name.to_owned(),
        status: CampaignStatus::Created,
        template: String::from("Bonjour {{prenom}}"),
        send_config: String::from(r#"{"sessions":[]}"#),
        total_count: 0,
        sent_count: 0,
        delivered_count: 0,
        failed_count: 0,
        created_at: instant("2026-07-26T10:00:00Z"),
        started_at: None,
        completed_at: None,
    }
}

/// A queued message, which is the state every message is written in.
pub(crate) fn a_queued_message(client_message_id: ClientMessageId, dest: &str) -> Message {
    Message {
        client_message_id,
        campaign_id: None,
        session_id: None,
        smsc_message_id: None,
        source_addr: Some(String::from("SHINOBI")),
        source_ton: None,
        source_npi: None,
        dest_addr: Some(Msisdn::parse(dest).expect("the fixture number is valid")),
        dest_ton: None,
        dest_npi: None,
        data_coding: None,
        segments: 1,
        text: Some(String::from("Bonjour")),
        state: MessageState::Queued,
        command_status: None,
        dlr_stat: None,
        dlr_err: None,
        attempts: 0,
        created_at: instant("2026-07-26T10:00:00Z"),
        sent_at: None,
        resp_at: None,
        dlr_at: None,
    }
}

/// A number that is valid and unique for the given index.
pub(crate) fn numbered_msisdn(index: usize) -> String {
    format!("+225{:010}", index)
}
