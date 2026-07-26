//! The session commands of spec §15.2, and the `sessions:state` event.
//!
//! Seven commands, one shape: validate, call, serialise. Every one of them
//! treats its input as untrusted (CLAUDE.md §3) — the WebView constrains its
//! own form, but nothing stops a hand-crafted `invoke`, so the profile is
//! rebuilt through `smpp_session`'s validating builder and a rejection comes
//! back as a stable code.
//!
//! # Where the password is, and where it is not
//!
//! [`SessionProfileDto`] has no password field, on purpose. The credential
//! arrives with [`session_bind`] alone, is turned into a
//! [`Password`](smpp_session::profile::Password) at once, and never goes back
//! across the bridge or into storage — milestone 015 owns encryption at rest
//! (spec §17.2), and step-005 §2 keeps it out of the database until then. A
//! `password` field on the profile DTO would be persisted by
//! `session_create` the day someone wired the two together without thinking
//! about it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State};

use persistence::ports::SessionProfileRepository as _;
use smpp_core::types::SessionId;
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};
use smpp_session::profile::{Password, SessionProfile};
use smpp_session::reconnect::ReconnectPolicy;
use smpp_session::state::BindMode;
use smpp_session::{SessionHandle, SessionSnapshot};

use crate::error::ErrorDto;
use crate::events::SessionsState;
use crate::state::AppState;

/// Which bind operation opens a session (EF-CNX-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BindTypeDto {
    /// Sends only.
    Transmitter,
    /// Receives only.
    Receiver,
    /// Both, on one connection.
    Transceiver,
}

impl From<BindTypeDto> for BindMode {
    fn from(value: BindTypeDto) -> Self {
        match value {
            BindTypeDto::Transmitter => Self::Transmitter,
            BindTypeDto::Receiver => Self::Receiver,
            BindTypeDto::Transceiver => Self::Transceiver,
        }
    }
}

impl From<BindMode> for BindTypeDto {
    fn from(value: BindMode) -> Self {
        match value {
            BindMode::Transmitter => Self::Transmitter,
            BindMode::Receiver => Self::Receiver,
            _ => Self::Transceiver,
        }
    }
}

/// The protocol version announced at bind time (EF-CNX-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub(crate) enum InterfaceVersionDto {
    /// SMPP v3.4, `interface_version` `0x34`.
    #[serde(rename = "v3.4")]
    V34,
    /// SMPP v5.0, `interface_version` `0x50`.
    #[serde(rename = "v5.0")]
    V50,
}

impl From<InterfaceVersionDto> for SmppVersion {
    fn from(value: InterfaceVersionDto) -> Self {
        match value {
            InterfaceVersionDto::V34 => Self::V3_4,
            InterfaceVersionDto::V50 => Self::V5_0,
        }
    }
}

impl From<SmppVersion> for InterfaceVersionDto {
    fn from(value: SmppVersion) -> Self {
        match value {
            SmppVersion::V3_4 => Self::V34,
            _ => Self::V50,
        }
    }
}

/// How GSM 7-bit septets sit in `short_message` (ADR 0008).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Gsm7PackingDto {
    /// One septet per octet. The default and the common case.
    Unpacked,
    /// Eight septets in seven octets.
    Packed,
}

impl From<Gsm7PackingDto> for Gsm7BitPacking {
    fn from(value: Gsm7PackingDto) -> Self {
        match value {
            Gsm7PackingDto::Unpacked => Self::Unpacked,
            Gsm7PackingDto::Packed => Self::Packed,
        }
    }
}

impl From<Gsm7BitPacking> for Gsm7PackingDto {
    fn from(value: Gsm7BitPacking) -> Self {
        match value {
            Gsm7BitPacking::Unpacked => Self::Unpacked,
            Gsm7BitPacking::Packed => Self::Packed,
        }
    }
}

/// What those octets mean (ADR 0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Gsm7CharsetDto {
    /// GSM 03.38 alphabet positions. The default.
    Gsm0338,
    /// ISO-8859-1 code points; the message centre transcodes.
    Latin1,
}

impl From<Gsm7CharsetDto> for Gsm7BitCharset {
    fn from(value: Gsm7CharsetDto) -> Self {
        match value {
            Gsm7CharsetDto::Gsm0338 => Self::Gsm0338,
            Gsm7CharsetDto::Latin1 => Self::Latin1,
        }
    }
}

impl From<Gsm7BitCharset> for Gsm7CharsetDto {
    fn from(value: Gsm7BitCharset) -> Self {
        match value {
            Gsm7BitCharset::Gsm0338 => Self::Gsm0338,
            Gsm7BitCharset::Latin1 => Self::Latin1,
        }
    }
}

/// A connection profile as the interface sees it (spec §8.2).
///
/// **No password.** See the module header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionProfileDto {
    /// Primary key. Absent when the interface is creating a profile.
    pub(crate) session_id: Option<String>,
    /// Name shown in the interface.
    pub(crate) name: String,
    /// SMSC hostname or address.
    pub(crate) host: String,
    /// SMSC port.
    pub(crate) port: u16,
    /// Which bind operation opens the session.
    pub(crate) bind_type: BindTypeDto,
    /// Protocol version requested at bind time.
    pub(crate) interface_version: InterfaceVersionDto,
    /// ESME identity presented to the SMSC.
    pub(crate) system_id: String,
    /// `system_type` of the bind PDU; empty when unused.
    pub(crate) system_type: String,
    /// Unacknowledged PDUs allowed in flight (spec §9.2).
    pub(crate) window_size: u32,
    /// Target throughput, in messages per second (spec §9.5).
    pub(crate) throughput_tps: u32,
    /// `enquire_link` period, in seconds. Zero disables the keep-alive.
    pub(crate) enquire_link_s: u32,
    /// How long a response may take before its request is abandoned.
    pub(crate) response_timeout_s: u32,
    /// Whether the session reconnects on its own.
    pub(crate) reconnect_enabled: bool,
    /// Shortest back-off, in seconds.
    pub(crate) min_backoff_s: u32,
    /// Longest back-off, in seconds.
    pub(crate) max_backoff_s: u32,
    /// Whether the back-off is spread out.
    pub(crate) jitter: bool,
    /// How GSM 7-bit septets sit in `short_message`.
    pub(crate) gsm7_packing: Gsm7PackingDto,
    /// What those octets mean.
    pub(crate) gsm7_charset: Gsm7CharsetDto,
    /// Parallel binds for this logical session (spec §8.5, milestone 011).
    pub(crate) bind_count: u32,
}

impl SessionProfileDto {
    /// Rebuilds the domain profile, validating every field.
    ///
    /// `session_id` is parsed rather than trusted, and a missing one mints a
    /// fresh identifier: the interface never invents a UUID.
    fn parse(&self) -> Result<SessionProfile, ErrorDto> {
        let session_id = match self.session_id.as_deref() {
            Some(raw) => SessionId::parse(raw).map_err(|_| ErrorDto::session_invalid_id())?,
            None => SessionId::new(),
        };

        let reconnect = ReconnectPolicy::new(
            self.reconnect_enabled,
            self.min_backoff_s,
            self.max_backoff_s,
            self.jitter,
        )
        .map_err(|error| ErrorDto::from(&error))?;

        SessionProfile::builder(session_id, &self.name, &self.host, self.port)
            .bind_mode(self.bind_type.into())
            .version(self.interface_version.into())
            .system_id(&self.system_id)
            .system_type(&self.system_type)
            .window_size(self.window_size)
            .throughput_tps(self.throughput_tps)
            .enquire_link_s(self.enquire_link_s)
            .response_timeout_s(self.response_timeout_s)
            .reconnect(reconnect)
            .gsm7_packing(self.gsm7_packing.into())
            .gsm7_charset(self.gsm7_charset.into())
            .bind_count(self.bind_count)
            .build()
            .map_err(|error| ErrorDto::from(&error))
    }
}

impl From<&SessionProfile> for SessionProfileDto {
    fn from(profile: &SessionProfile) -> Self {
        let reconnect = profile.reconnect();

        Self {
            session_id: Some(profile.session_id().to_string()),
            name: profile.name().to_owned(),
            host: profile.host().to_owned(),
            port: profile.port(),
            bind_type: profile.bind_mode().into(),
            interface_version: profile.version().into(),
            system_id: profile.system_id().to_owned(),
            system_type: profile.system_type().to_owned(),
            window_size: profile.window_size(),
            throughput_tps: profile.throughput_tps(),
            enquire_link_s: seconds(profile.enquire_link_interval()),
            response_timeout_s: seconds(profile.response_timeout()),
            reconnect_enabled: reconnect.is_enabled(),
            min_backoff_s: seconds(reconnect.min_backoff()),
            max_backoff_s: seconds(reconnect.max_backoff()),
            jitter: reconnect.has_jitter(),
            gsm7_packing: profile.gsm7_packing().into(),
            gsm7_charset: profile.gsm7_charset().into(),
            bind_count: profile.bind_count(),
        }
    }
}

/// Whole seconds of a duration, saturating rather than wrapping.
fn seconds(duration: core::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

/// The live state of one session (spec §7.9), for the banner and the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionStatusDto {
    /// Which profile this is about.
    pub(crate) session_id: String,
    /// `CLOSED`, `CONNECTING`, `BINDING`, `BOUND`, `UNBOUND`, `RECONNECT`,
    /// `ERROR` — the names of spec §7.9, which the interface translates.
    pub(crate) state: String,
    /// The bind type in force, when the session is bound.
    pub(crate) bind_type: Option<BindTypeDto>,
    /// The last failure, rendered. Never carries a credential.
    pub(crate) last_error: Option<String>,
    /// Why the session stopped for good: `FATAL_STATUS`,
    /// `RECONNECT_DISABLED`. A code, so the interface translates it.
    pub(crate) give_up: Option<String>,
    /// Requests waiting for a response (spec §18.1).
    pub(crate) in_flight: u32,
}

impl SessionStatusDto {
    /// The state of a session that has never been bound.
    fn closed(session_id: SessionId) -> Self {
        Self {
            session_id: session_id.to_string(),
            state: "CLOSED".to_owned(),
            bind_type: None,
            last_error: None,
            give_up: None,
            in_flight: 0,
        }
    }

    /// Projects a live snapshot.
    async fn of(session_id: SessionId, handle: &SessionHandle) -> Self {
        let snapshot = handle.snapshot();

        Self::from_snapshot(session_id, &snapshot, handle.in_flight().await)
    }

    /// Projects a snapshot with an already-read in-flight count.
    pub(crate) fn from_snapshot(
        session_id: SessionId,
        snapshot: &SessionSnapshot,
        in_flight: usize,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            state: snapshot.state.code().to_owned(),
            bind_type: snapshot.state.bind_mode().map(Into::into),
            last_error: snapshot.last_error.clone(),
            give_up: snapshot.give_up.map(ToOwned::to_owned),
            in_flight: u32::try_from(in_flight).unwrap_or(u32::MAX),
        }
    }
}

/// A string that must never be formatted.
///
/// Transparent on the wire — the generated TypeScript still sees a plain
/// `string` — and opaque in Rust: its `Debug` shows nothing, so a future
/// `tracing::debug!(?input)` on [`SessionBindInput`] cannot put an SMSC
/// password in a log file. A bare `String` field would have, and nothing in
/// the type system would have objected.
///
/// The same reasoning as `smpp_session::profile::Password`, applied one layer
/// up: this is the shape the credential has for the few microseconds between
/// crossing the bridge and becoming a `Password`.
#[derive(Clone, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub(crate) struct Secret(String);

impl Secret {
    /// The credential in clear.
    ///
    /// One call site, in [`session_bind`], where it becomes a `Password`.
    fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for Secret {
    /// Shows nothing — not the value, and not its length, which is a hint.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Input of [`session_bind`].
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionBindInput {
    /// Which profile to bind.
    pub(crate) session_id: String,
    /// The SMSC password.
    ///
    /// Travels once, on this call, and is turned into a
    /// `smpp_session::profile::Password` immediately. It is not persisted and
    /// never comes back across the bridge.
    pub(crate) password: Secret,
}

/// Creates or replaces a connection profile (EF-CNX-01).
///
/// # Errors
///
/// * `SESSION_INVALID_PROFILE` if a field fails validation;
/// * `SESSION_STORAGE` if the profile cannot be written.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_create(
    state: State<'_, AppState>,
    input: SessionProfileDto,
) -> Result<SessionProfileDto, ErrorDto> {
    let profile = input.parse()?;

    state
        .sessions()
        .profiles()
        .upsert_session_profile(&profile.to_record())
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(SessionProfileDto::from(&profile))
}

/// Updates a connection profile.
///
/// The same upsert as [`session_create`]: the caller is a form, it always
/// holds the whole profile, and the distinction between "new" and "edited" is
/// one the interface has already made. Two commands rather than one because
/// spec §15.2 names two, and because the interface's intent is worth recording
/// in the log.
///
/// # Errors
///
/// Same as [`session_create`], plus `SESSION_INVALID_ID` when the identifier
/// is missing — an update must say what it is updating.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_update(
    state: State<'_, AppState>,
    input: SessionProfileDto,
) -> Result<SessionProfileDto, ErrorDto> {
    if input.session_id.is_none() {
        return Err(ErrorDto::session_invalid_id());
    }

    session_create(state, input).await
}

/// Deletes a connection profile, closing its session first.
///
/// # Errors
///
/// `SESSION_STORAGE` if the delete fails.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, ErrorDto> {
    let session_id = SessionId::parse(&session_id).map_err(|_| ErrorDto::session_invalid_id())?;

    // Closing first, and not merely for tidiness: `ON DELETE SET NULL` would
    // detach the messages of a session still writing to them.
    let _closed = state
        .sessions()
        .registry()
        .unbind(session_id)
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    let deleted = state
        .sessions()
        .profiles()
        .delete_session_profile(session_id)
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    state.sessions().publish(&app).await;

    Ok(deleted)
}

/// Lists every connection profile, oldest first.
///
/// # Errors
///
/// `SESSION_STORAGE` if the read fails, `SESSION_INVALID_PROFILE` if a stored
/// row no longer validates.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_list(
    state: State<'_, AppState>,
) -> Result<Vec<SessionProfileDto>, ErrorDto> {
    let records = state
        .sessions()
        .profiles()
        .list_session_profiles()
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    records
        .iter()
        .map(|record| {
            SessionProfile::from_record(record)
                .map(|profile| SessionProfileDto::from(&profile))
                .map_err(|error| ErrorDto::from(&error))
        })
        .collect()
}

/// Opens a session: connect, then bind (EF-CNX-01, EF-CNX-02, EF-CNX-04).
///
/// Returns as soon as the session's tasks are running, with the state at that
/// instant — usually `CONNECTING`. The interface follows the rest through
/// `sessions:state`, which is what CA-005-01 measures.
///
/// # Errors
///
/// * `SESSION_INVALID_ID` if the identifier is malformed;
/// * `SESSION_NOT_FOUND` if no profile carries it;
/// * `SESSION_INVALID_PROFILE` if the password does not fit its protocol
///   field;
/// * `SESSION_BUSY` if another session is already live (milestone 011);
/// * `SESSION_STORAGE` if the profile cannot be read.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_bind(
    app: AppHandle,
    state: State<'_, AppState>,
    input: SessionBindInput,
) -> Result<SessionStatusDto, ErrorDto> {
    let session_id =
        SessionId::parse(&input.session_id).map_err(|_| ErrorDto::session_invalid_id())?;

    let record = state
        .sessions()
        .profiles()
        .find_session_profile(session_id)
        .await
        .map_err(|error| ErrorDto::from(&error))?
        .ok_or_else(ErrorDto::session_not_found)?;

    let profile = SessionProfile::from_record(&record).map_err(|error| ErrorDto::from(&error))?;
    let password =
        Password::parse(input.password.expose()).map_err(|error| ErrorDto::from(&error))?;

    let handle = state.sessions().bind(&app, profile, password).await?;
    let status = SessionStatusDto::of(session_id, &handle).await;

    state.sessions().publish(&app).await;

    Ok(status)
}

/// Closes a session cleanly: `unbind`, then the socket (CA-005-08).
///
/// # Errors
///
/// `SESSION_INVALID_ID`, or `SESSION_CLOSED` if the session's tasks ended
/// abnormally.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_unbind(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, ErrorDto> {
    let session_id = SessionId::parse(&session_id).map_err(|_| ErrorDto::session_invalid_id())?;

    let closed = state
        .sessions()
        .registry()
        .unbind(session_id)
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    state.sessions().publish(&app).await;

    Ok(closed)
}

/// The live state of one session.
///
/// A profile that is not live answers `CLOSED` rather than an error: "not
/// bound" is a state, not a failure.
///
/// # Errors
///
/// `SESSION_INVALID_ID` if the identifier is malformed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn session_status(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionStatusDto, ErrorDto> {
    let session_id = SessionId::parse(&session_id).map_err(|_| ErrorDto::session_invalid_id())?;

    match state.sessions().registry().handle(session_id).await {
        Some(handle) => Ok(SessionStatusDto::of(session_id, &handle).await),
        None => Ok(SessionStatusDto::closed(session_id)),
    }
}

/// Every live session's state, as the `sessions:state` event carries it.
pub(crate) async fn statuses(
    registry: &Arc<smpp_session::SessionRegistry<smpp_session::TcpTransport>>,
) -> SessionsState {
    let mut sessions = Vec::new();

    for (session_id, snapshot) in registry.statuses().await {
        let in_flight = match registry.handle(session_id).await {
            Some(handle) => handle.in_flight().await,
            None => 0,
        };

        sessions.push(SessionStatusDto::from_snapshot(
            session_id, &snapshot, in_flight,
        ));
    }

    // A stable order, so the interface does not reshuffle its list on every
    // tick: a `HashMap` iterates in whatever order it pleases.
    sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));

    SessionsState { sessions }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_dto() -> SessionProfileDto {
        SessionProfileDto {
            session_id: None,
            name: "Operator A".to_owned(),
            host: "smsc.example.test".to_owned(),
            port: 2775,
            bind_type: BindTypeDto::Transceiver,
            interface_version: InterfaceVersionDto::V50,
            system_id: "esme01".to_owned(),
            system_type: String::new(),
            window_size: 50,
            throughput_tps: 100,
            enquire_link_s: 30,
            response_timeout_s: 10,
            reconnect_enabled: true,
            min_backoff_s: 1,
            max_backoff_s: 60,
            jitter: true,
            gsm7_packing: Gsm7PackingDto::Unpacked,
            gsm7_charset: Gsm7CharsetDto::Gsm0338,
            bind_count: 1,
        }
    }

    /// The round trip the interface actually performs: `session_list` returns
    /// a DTO, the form edits it, and it comes back to `session_update`.
    /// Nothing else checks that the two directions agree.
    #[test]
    fn a_profile_dto_survives_a_round_trip_through_the_domain_type() {
        let profile = a_dto().parse().expect("the fixture is valid");
        let back = SessionProfileDto::from(&profile);

        assert_eq!(back.session_id, Some(profile.session_id().to_string()));
        assert_eq!(
            SessionProfileDto {
                session_id: None,
                ..back
            },
            a_dto()
        );
    }

    /// CLAUDE.md §8 — the one DTO that does carry a credential cannot print
    /// it. A bare `String` field would have, and a single
    /// `tracing::debug!(?input)` added later is all it would have taken.
    #[test]
    fn the_bind_input_never_renders_its_password() {
        let input = SessionBindInput {
            session_id: SessionId::new().to_string(),
            password: Secret("n0tr34l".to_owned()),
        };

        let rendered = format!("{input:?}");

        assert!(!rendered.contains("n0tr34l"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        // The secret on its own shows nothing at all — not the value, and not
        // its length, which narrows a brute force. Asserted on the field
        // rather than on the whole struct, whose `session_id` is a UUID full
        // of digits.
        assert_eq!(format!("{:?}", input.password), "<redacted>");

        // And it still deserialises from, and serialises to, a plain string:
        // the wire contract is unchanged.
        assert_eq!(
            serde_json::to_value(&input.password).expect("a secret serialises"),
            serde_json::Value::String("n0tr34l".to_owned())
        );
        assert_eq!(input.password.expose(), "n0tr34l");
    }

    /// CLAUDE.md §8 — the profile that crosses the bridge has no credential
    /// field at all, so no amount of wiring can put one there.
    #[test]
    fn the_profile_dto_carries_no_password() {
        let json = serde_json::to_value(a_dto()).expect("the DTO serialises");
        let keys: Vec<&String> = json.as_object().expect("an object").keys().collect();

        assert!(
            !keys.iter().any(|key| key.contains("assword")),
            "the profile DTO must have no password field: {keys:?}"
        );
    }

    #[test]
    fn an_invalid_field_is_refused_rather_than_repaired() {
        let too_wide = SessionProfileDto {
            window_size: 0,
            ..a_dto()
        };
        assert!(too_wide.parse().is_err());

        let contradictory = SessionProfileDto {
            min_backoff_s: 600,
            max_backoff_s: 60,
            ..a_dto()
        };
        assert!(contradictory.parse().is_err());

        // ADR 0009 §7 — the alt-charset cannot be packed.
        let impossible = SessionProfileDto {
            gsm7_charset: Gsm7CharsetDto::Latin1,
            gsm7_packing: Gsm7PackingDto::Packed,
            ..a_dto()
        };
        assert!(impossible.parse().is_err());
    }

    #[test]
    fn a_malformed_identifier_is_refused_and_an_absent_one_is_minted() {
        let malformed = SessionProfileDto {
            session_id: Some("not-a-uuid".to_owned()),
            ..a_dto()
        };
        assert!(malformed.parse().is_err());

        assert!(a_dto().parse().is_ok(), "an absent id mints a fresh one");
    }

    /// The state names cross the bridge and the interface keys off them.
    #[test]
    fn a_closed_status_reports_the_state_name_of_the_specification() {
        let status = SessionStatusDto::closed(SessionId::new());

        assert_eq!(status.state, "CLOSED");
        assert!(status.bind_type.is_none());
        assert_eq!(status.in_flight, 0);
    }

    #[test]
    fn every_enum_of_the_contract_maps_both_ways() {
        for value in [
            BindTypeDto::Transmitter,
            BindTypeDto::Receiver,
            BindTypeDto::Transceiver,
        ] {
            assert_eq!(BindTypeDto::from(BindMode::from(value)), value);
        }

        for value in [InterfaceVersionDto::V34, InterfaceVersionDto::V50] {
            assert_eq!(InterfaceVersionDto::from(SmppVersion::from(value)), value);
        }

        for value in [Gsm7PackingDto::Unpacked, Gsm7PackingDto::Packed] {
            assert_eq!(Gsm7PackingDto::from(Gsm7BitPacking::from(value)), value);
        }

        for value in [Gsm7CharsetDto::Gsm0338, Gsm7CharsetDto::Latin1] {
            assert_eq!(Gsm7CharsetDto::from(Gsm7BitCharset::from(value)), value);
        }
    }
}
