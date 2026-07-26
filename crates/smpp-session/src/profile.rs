//! The session profile of spec §8.2, and the credential that is *not* in it.
//!
//! # Two types, and why the split is the point
//!
//! [`SessionProfile`] is everything about a session that can be written down:
//! where the SMSC is, how to bind to it, how fast to go, how the octets are
//! laid out. It is persisted, it crosses the IPC boundary, it is displayed.
//!
//! [`Password`] is not part of it. The two are separate types so that no
//! refactor, no `derive(Serialize)` and no `tracing::debug!(?profile)` can put
//! a credential where a profile goes. Milestone 015 owns encryption at rest
//! (spec §17.2); until then step-005 §2 puts it plainly — **no real password is
//! persisted**, it is typed in and lives in memory for the duration of the
//! bind. [`Password`] enforces that shape: it has no `Display`, its `Debug`
//! shows nothing, and reading it back takes a call named
//! [`Password::expose`], which is one `grep` away from a review.
//!
//! # What this milestone does not carry
//!
//! Spec §8.2 also lists `addr_ton`, `addr_npi` and `address_range`. The
//! `session_profiles` table has no column for them, and the bind sends the
//! ESME defaults — `Unknown` / `Unknown` / empty — which is what an ESME that
//! does not receive on an address range wants. Exposing them belongs with the
//! receiving side, milestone 008.

use core::time::Duration;

use persistence::{BindType, SessionProfile as SessionProfileRecord, Timestamp};
use smpp_core::types::SessionId;
use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking, SmppVersion};

use crate::error::{ProfileRejection, SessionError};
use crate::reconnect::ReconnectPolicy;
use crate::state::BindMode;

/// Longest profile name the interface will show without truncating.
const MAX_NAME_LENGTH: usize = 64;

/// Longest hostname, per DNS.
const MAX_HOST_LENGTH: usize = 255;

/// Longest `system_id`: `COctetString<1, 16>`, so fifteen octets plus the NUL.
const MAX_SYSTEM_ID_LENGTH: usize = 15;

/// Longest `system_type`: `COctetString<1, 13>`.
const MAX_SYSTEM_TYPE_LENGTH: usize = 12;

/// Longest password: `COctetString<1, 9>`.
///
/// Some message centres accept longer and truncate; the field cannot carry it,
/// so a password that does not fit is refused rather than silently cut — a
/// truncated password fails the bind with `ESME_RINVPASWD`, which reads as
/// "wrong credentials" and sends the operator looking in the wrong place.
pub const MAX_PASSWORD_LENGTH: usize = 8;

/// Longest `enquire_link` period accepted, in seconds.
const MAX_ENQUIRE_LINK_S: u32 = 3_600;

/// Longest response timeout accepted, in seconds.
const MAX_RESPONSE_TIMEOUT_S: u32 = 300;

/// Largest send window accepted (spec §9.2).
const MAX_WINDOW_SIZE: u32 = 1_000;

/// Largest number of parallel binds accepted (spec §8.5).
const MAX_BIND_COUNT: u32 = 16;

/// An SMSC password, held in memory and nowhere else.
///
/// ```
/// use smpp_session::profile::Password;
///
/// let password = Password::parse("s3cr3t")?;
///
/// // Neither formatting nor logging can reveal it.
/// assert_eq!(format!("{password:?}"), "Password(<redacted>)");
/// assert_eq!(password.expose(), "s3cr3t");
/// # Ok::<(), smpp_session::SessionError>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    /// Parses a password into the field the protocol can carry.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidProfile`] when the value is longer than
    /// [`MAX_PASSWORD_LENGTH`] or contains a NUL, which would terminate the
    /// C-Octet String early. The message names neither the value nor its
    /// length (CLAUDE.md §8).
    pub fn parse(raw: &str) -> Result<Self, SessionError> {
        if raw.len() > MAX_PASSWORD_LENGTH {
            return Err(SessionError::invalid_profile(
                "password",
                ProfileRejection::TooLong,
            ));
        }

        if raw.contains('\0') {
            return Err(SessionError::invalid_profile(
                "password",
                ProfileRejection::IllegalCharacter,
            ));
        }

        Ok(Self(raw.to_owned()))
    }

    /// An empty password, for a message centre that does not require one.
    #[must_use]
    pub fn empty() -> Self {
        Self(String::new())
    }

    /// The credential in clear.
    ///
    /// **Every call site of this function is a place a password can leak.**
    /// There is exactly one in this crate — where the bind PDU is built — and
    /// a review should treat a second one as a finding.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for Password {
    /// Shows nothing. Not the value, not its length — a length is a hint.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Password(<redacted>)")
    }
}

// NO `impl Display`, and no `impl AsRef<str>`, deliberately: either would make
// `format!("{password}")` or a `&str` coercion compile, and both are how a
// credential reaches a log without anyone deciding to put it there.

/// A connection profile (spec §8.2).
///
/// Built through [`SessionProfile::builder`] or read back from storage with
/// [`SessionProfile::from_record`]; the fields are private so a value that
/// failed validation is unrepresentable (*parse, don't validate*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProfile {
    session_id: SessionId,
    name: String,
    host: String,
    port: u16,
    bind_mode: BindMode,
    version: SmppVersion,
    system_id: String,
    system_type: String,
    window_size: u32,
    throughput_tps: u32,
    enquire_link_interval: Duration,
    response_timeout: Duration,
    reconnect: ReconnectPolicy,
    gsm7_packing: Gsm7BitPacking,
    gsm7_charset: Gsm7BitCharset,
    bind_count: u32,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl SessionProfile {
    /// Starts building a profile with the defaults of spec §8.2.
    #[must_use]
    pub fn builder(session_id: SessionId, name: &str, host: &str, port: u16) -> ProfileBuilder {
        ProfileBuilder {
            session_id,
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            bind_mode: BindMode::Transceiver,
            version: SmppVersion::default(),
            system_id: String::new(),
            system_type: String::new(),
            window_size: 50,
            throughput_tps: 100,
            enquire_link_s: 30,
            response_timeout_s: 10,
            reconnect: ReconnectPolicy::default(),
            gsm7_packing: Gsm7BitPacking::default(),
            gsm7_charset: Gsm7BitCharset::default(),
            bind_count: 1,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    /// Primary key, and the `tracing` span field of every line of this session.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Name shown in the interface.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// SMSC hostname or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// SMSC port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// `host:port`, as `TcpStream::connect` wants it.
    #[must_use]
    pub fn socket_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Which bind operation opens the session.
    #[must_use]
    pub const fn bind_mode(&self) -> BindMode {
        self.bind_mode
    }

    /// The `interface_version` announced at bind time (EF-CNX-04).
    #[must_use]
    pub const fn version(&self) -> SmppVersion {
        self.version
    }

    /// ESME identity presented to the SMSC.
    #[must_use]
    pub fn system_id(&self) -> &str {
        &self.system_id
    }

    /// `system_type` of the bind PDU; empty when unused.
    #[must_use]
    pub fn system_type(&self) -> &str {
        &self.system_type
    }

    /// Unacknowledged PDUs allowed in flight (spec §9.2).
    ///
    /// Carried and validated here; enforced at milestone 007.
    #[must_use]
    pub const fn window_size(&self) -> u32 {
        self.window_size
    }

    /// Target throughput, in messages per second (spec §9.5).
    ///
    /// Carried and validated here; enforced at milestone 007.
    #[must_use]
    pub const fn throughput_tps(&self) -> u32 {
        self.throughput_tps
    }

    /// `enquire_link` period (EF-CNX-05).
    ///
    /// [`Duration::ZERO`] disables the keep-alive. Legitimate against a message
    /// centre that closes on an unexpected `enquire_link`, and a bad idea
    /// otherwise: without it a session can sit `BOUND` on a socket that no
    /// longer carries anything.
    ///
    /// When it is not zero it is strictly greater than
    /// [`Self::response_timeout`] — see [`ProfileBuilder::build`].
    #[must_use]
    pub const fn enquire_link_interval(&self) -> Duration {
        self.enquire_link_interval
    }

    /// How long a response may take before its request is abandoned.
    #[must_use]
    pub const fn response_timeout(&self) -> Duration {
        self.response_timeout
    }

    /// Reconnection policy (EF-CNX-06).
    #[must_use]
    pub const fn reconnect(&self) -> ReconnectPolicy {
        self.reconnect
    }

    /// How GSM 7-bit septets sit in `short_message` (ADR 0008).
    #[must_use]
    pub const fn gsm7_packing(&self) -> Gsm7BitPacking {
        self.gsm7_packing
    }

    /// What those octets mean (ADR 0009).
    #[must_use]
    pub const fn gsm7_charset(&self) -> Gsm7BitCharset {
        self.gsm7_charset
    }

    /// Parallel binds for this logical session (spec §8.5).
    ///
    /// Validated here; milestone 005 opens **one**, and milestone 011 opens
    /// the rest.
    #[must_use]
    pub const fn bind_count(&self) -> u32 {
        self.bind_count
    }

    /// When the profile was created.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// When the profile was last written.
    #[must_use]
    pub const fn updated_at(&self) -> Timestamp {
        self.updated_at
    }

    /// Reads a profile back from its stored row.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidProfile`] when a stored value no longer passes
    /// validation — a row edited by hand, or written by a version with looser
    /// rules. Refusing is the right answer: a profile that cannot be validated
    /// cannot be bound safely either.
    pub fn from_record(record: &SessionProfileRecord) -> Result<Self, SessionError> {
        let reconnect = reconnect_from_document(record.reconnect_config.as_deref())?;

        ProfileBuilder {
            session_id: record.session_id,
            name: record.name.clone(),
            host: record.host.clone(),
            port: record.port,
            bind_mode: bind_mode_of(record.bind_type),
            version: record.interface_version,
            system_id: record.system_id.clone(),
            system_type: record.system_type.clone(),
            window_size: record.window_size,
            throughput_tps: record.throughput_tps,
            enquire_link_s: record.enquire_link_s,
            response_timeout_s: record.response_timeout_s,
            reconnect,
            gsm7_packing: record.gsm7_packing,
            gsm7_charset: record.gsm7_charset,
            bind_count: record.bind_count,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
        .build()
    }

    /// Projects the profile onto the row the repository writes.
    ///
    /// `password_enc` comes back **empty**. That is not an omission: step-005
    /// §2 keeps the credential out of storage until milestone 015 provides the
    /// encryption, and writing an unencrypted one "for now" is exactly the
    /// shortcut CLAUDE.md §8 forbids.
    #[must_use]
    pub fn to_record(&self) -> SessionProfileRecord {
        SessionProfileRecord {
            session_id: self.session_id,
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            bind_type: bind_type_of(self.bind_mode),
            interface_version: self.version,
            system_id: self.system_id.clone(),
            password_enc: Vec::new(),
            system_type: self.system_type.clone(),
            tls_config: None,
            window_size: self.window_size,
            throughput_tps: self.throughput_tps,
            enquire_link_s: seconds_of(self.enquire_link_interval),
            response_timeout_s: seconds_of(self.response_timeout),
            reconnect_config: Some(reconnect_document(self.reconnect)),
            gsm7_packing: self.gsm7_packing,
            gsm7_charset: self.gsm7_charset,
            bind_count: self.bind_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// A profile under construction, before validation.
///
/// Every field is public-by-setter and nothing is checked until
/// [`ProfileBuilder::build`], which is the single gate: there is no way to
/// obtain a [`SessionProfile`] that did not go through it.
#[derive(Debug, Clone)]
pub struct ProfileBuilder {
    session_id: SessionId,
    name: String,
    host: String,
    port: u16,
    bind_mode: BindMode,
    version: SmppVersion,
    system_id: String,
    system_type: String,
    window_size: u32,
    throughput_tps: u32,
    enquire_link_s: u32,
    response_timeout_s: u32,
    reconnect: ReconnectPolicy,
    gsm7_packing: Gsm7BitPacking,
    gsm7_charset: Gsm7BitCharset,
    bind_count: u32,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl ProfileBuilder {
    /// Sets the bind type (EF-CNX-02).
    #[must_use]
    pub const fn bind_mode(mut self, bind_mode: BindMode) -> Self {
        self.bind_mode = bind_mode;
        self
    }

    /// Sets the protocol version announced at bind time (EF-CNX-04).
    #[must_use]
    pub const fn version(mut self, version: SmppVersion) -> Self {
        self.version = version;
        self
    }

    /// Sets the ESME identity.
    #[must_use]
    pub fn system_id(mut self, system_id: &str) -> Self {
        self.system_id = system_id.to_owned();
        self
    }

    /// Sets the `system_type`.
    #[must_use]
    pub fn system_type(mut self, system_type: &str) -> Self {
        self.system_type = system_type.to_owned();
        self
    }

    /// Sets the send window (spec §9.2).
    #[must_use]
    pub const fn window_size(mut self, window_size: u32) -> Self {
        self.window_size = window_size;
        self
    }

    /// Sets the target throughput (spec §9.5).
    #[must_use]
    pub const fn throughput_tps(mut self, throughput_tps: u32) -> Self {
        self.throughput_tps = throughput_tps;
        self
    }

    /// Sets the `enquire_link` period, in seconds. Zero disables it.
    #[must_use]
    pub const fn enquire_link_s(mut self, seconds: u32) -> Self {
        self.enquire_link_s = seconds;
        self
    }

    /// Sets the response timeout, in seconds.
    #[must_use]
    pub const fn response_timeout_s(mut self, seconds: u32) -> Self {
        self.response_timeout_s = seconds;
        self
    }

    /// Sets the reconnection policy (EF-CNX-06).
    #[must_use]
    pub const fn reconnect(mut self, reconnect: ReconnectPolicy) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Sets how GSM 7-bit septets are laid out (ADR 0008).
    #[must_use]
    pub const fn gsm7_packing(mut self, packing: Gsm7BitPacking) -> Self {
        self.gsm7_packing = packing;
        self
    }

    /// Sets what those octets mean (ADR 0009).
    #[must_use]
    pub const fn gsm7_charset(mut self, charset: Gsm7BitCharset) -> Self {
        self.gsm7_charset = charset;
        self
    }

    /// Sets the number of parallel binds (spec §8.5, milestone 011).
    #[must_use]
    pub const fn bind_count(mut self, bind_count: u32) -> Self {
        self.bind_count = bind_count;
        self
    }

    /// Sets the creation and update timestamps.
    #[must_use]
    pub const fn timestamps(mut self, created_at: Timestamp, updated_at: Timestamp) -> Self {
        self.created_at = created_at;
        self.updated_at = updated_at;
        self
    }

    /// Validates every field and produces the profile.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidProfile`], naming the field and the reason. The
    /// offending value is never echoed: a hostname and a `system_id` are
    /// identifying data, and this message crosses the IPC boundary.
    pub fn build(self) -> Result<SessionProfile, SessionError> {
        check_text("name", &self.name, 1, MAX_NAME_LENGTH)?;
        check_text("host", &self.host, 1, MAX_HOST_LENGTH)?;
        check_text("system_id", &self.system_id, 1, MAX_SYSTEM_ID_LENGTH)?;
        check_text("system_type", &self.system_type, 0, MAX_SYSTEM_TYPE_LENGTH)?;

        if self.port == 0 {
            return Err(SessionError::invalid_profile(
                "port",
                ProfileRejection::OutOfRange,
            ));
        }

        check_range("window_size", self.window_size, 1, MAX_WINDOW_SIZE)?;
        check_range("enquire_link_s", self.enquire_link_s, 0, MAX_ENQUIRE_LINK_S)?;
        check_range(
            "response_timeout_s",
            self.response_timeout_s,
            1,
            MAX_RESPONSE_TIMEOUT_S,
        )?;
        check_range("bind_count", self.bind_count, 1, MAX_BIND_COUNT)?;

        // ADR 0009 §7 — Latin-1 octets use all eight bits and packing throws
        // the top one away. The pair is not "risky", it is unrecoverable.
        if !self.gsm7_charset.is_compatible_with(self.gsm7_packing) {
            return Err(SessionError::invalid_profile(
                "gsm7_charset",
                ProfileRejection::Contradictory,
            ));
        }

        // An `enquire_link` must be able to time out *before* the next one is
        // due. Otherwise every period opens a request the period after it
        // replaces, no verdict is ever reached, and the keep-alive stops being
        // able to detect anything at all — the session sits `BOUND` on a
        // socket that carries nothing (CA-005-04).
        //
        // Both bounds were validated on their own and their *relation* was
        // not, which is exactly how the hole stayed open: `enquire_link_s = 10`
        // with `response_timeout_s = 30` is a plausible setting for a distant
        // message centre, and nothing refused it.
        if self.enquire_link_s != 0 && self.response_timeout_s >= self.enquire_link_s {
            return Err(SessionError::invalid_profile(
                "response_timeout_s",
                ProfileRejection::Contradictory,
            ));
        }

        Ok(SessionProfile {
            session_id: self.session_id,
            name: self.name,
            host: self.host,
            port: self.port,
            bind_mode: self.bind_mode,
            version: self.version,
            system_id: self.system_id,
            system_type: self.system_type,
            window_size: self.window_size,
            throughput_tps: self.throughput_tps,
            enquire_link_interval: Duration::from_secs(u64::from(self.enquire_link_s)),
            response_timeout: Duration::from_secs(u64::from(self.response_timeout_s)),
            reconnect: self.reconnect,
            gsm7_packing: self.gsm7_packing,
            gsm7_charset: self.gsm7_charset,
            bind_count: self.bind_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Rejects an empty, over-long or NUL-bearing text field.
fn check_text(
    field: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), SessionError> {
    // Octets, not characters: the protocol field is a C-Octet String, and a
    // `system_id` of fifteen accented characters is thirty octets on the wire.
    let length = value.len();

    if length < minimum {
        return Err(SessionError::invalid_profile(
            field,
            ProfileRejection::Empty,
        ));
    }

    if length > maximum {
        return Err(SessionError::invalid_profile(
            field,
            ProfileRejection::TooLong,
        ));
    }

    if value.contains('\0') {
        return Err(SessionError::invalid_profile(
            field,
            ProfileRejection::IllegalCharacter,
        ));
    }

    Ok(())
}

/// Rejects a numeric field outside its documented range.
const fn check_range(
    field: &'static str,
    value: u32,
    minimum: u32,
    maximum: u32,
) -> Result<(), SessionError> {
    if value < minimum || value > maximum {
        return Err(SessionError::invalid_profile(
            field,
            ProfileRejection::OutOfRange,
        ));
    }

    Ok(())
}

/// Whole seconds of a duration, saturating rather than wrapping.
fn seconds_of(duration: Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

/// The domain bind mode for a stored bind type.
const fn bind_mode_of(bind_type: BindType) -> BindMode {
    match bind_type {
        BindType::Transmitter => BindMode::Transmitter,
        BindType::Receiver => BindMode::Receiver,
        // `BindType` is `#[non_exhaustive]`, so a wildcard arm is required.
        // Transceiver is the safe fallback: it is the only mode that can both
        // send and receive, so an unknown value degrades to "ask the SMSC"
        // rather than to a session that silently refuses to submit.
        _ => BindMode::Transceiver,
    }
}

/// The stored bind type for a domain bind mode.
const fn bind_type_of(bind_mode: BindMode) -> BindType {
    match bind_mode {
        BindMode::Transmitter => BindType::Transmitter,
        BindMode::Receiver => BindType::Receiver,
        _ => BindType::Transceiver,
    }
}

/// Renders the reconnection policy into the JSON column of spec §8.2.
///
/// Hand-written rather than `serde`-derived, and that is a deliberate trade:
/// the document is four scalar fields whose shape spec §8.2 fixes, and pulling
/// `serde` plus `serde_json` into this crate to write eleven tokens would be a
/// dependency the guide asks to justify. The round trip is covered by a test,
/// which is what makes the hand-written pair safe.
fn reconnect_document(policy: ReconnectPolicy) -> String {
    format!(
        r#"{{"enabled":{},"min_backoff_s":{},"max_backoff_s":{},"jitter":{}}}"#,
        policy.is_enabled(),
        policy.min_backoff().as_secs(),
        policy.max_backoff().as_secs(),
        policy.has_jitter(),
    )
}

/// Reads the reconnection policy back out of the JSON column.
///
/// An absent, empty or unreadable document falls back to the defaults of spec
/// §8.2 rather than failing: the column is nullable, every profile written
/// before this milestone has it `NULL`, and refusing to load such a profile
/// would make an upgrade look like data loss.
fn reconnect_from_document(document: Option<&str>) -> Result<ReconnectPolicy, SessionError> {
    let Some(document) = document else {
        return Ok(ReconnectPolicy::default());
    };

    let defaults = ReconnectPolicy::default();
    let enabled = read_bool(document, "enabled").unwrap_or_else(|| defaults.is_enabled());
    let jitter = read_bool(document, "jitter").unwrap_or_else(|| defaults.has_jitter());
    let min_backoff_s =
        read_u32(document, "min_backoff_s").unwrap_or_else(|| seconds_of(defaults.min_backoff()));
    let max_backoff_s =
        read_u32(document, "max_backoff_s").unwrap_or_else(|| seconds_of(defaults.max_backoff()));

    ReconnectPolicy::new(enabled, min_backoff_s, max_backoff_s, jitter)
}

/// The text following `"<key>":` in a flat JSON object, up to the delimiter.
fn read_field<'a>(document: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":");
    let start = document.find(&needle)? + needle.len();
    let rest = document.get(start..)?;
    let end = rest.find([',', '}']).unwrap_or(rest.len());

    Some(rest.get(..end)?.trim())
}

/// Reads a boolean field of a flat JSON object.
fn read_bool(document: &str, key: &str) -> Option<bool> {
    read_field(document, key)?.parse().ok()
}

/// Reads an integer field of a flat JSON object.
fn read_u32(document: &str, key: &str) -> Option<u32> {
    read_field(document, key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_profile() -> SessionProfile {
        SessionProfile::builder(SessionId::new(), "Operator A", "smsc.example.test", 2775)
            .system_id("esme01")
            .build()
            .expect("the fixture is valid")
    }

    /// CA-005-11, at the level of the type: no formatting of a password can
    /// produce it. The test is written against `Debug` because that is what a
    /// `tracing` call reaches for.
    #[test]
    fn a_password_never_renders_itself() {
        let password = Password::parse("hunter2").expect("eight octets or fewer");

        assert_eq!(format!("{password:?}"), "Password(<redacted>)");
        assert!(!format!("{password:?}").contains("hunter2"));
        // Not even the length, which narrows a brute force.
        assert!(!format!("{password:?}").contains('7'));
        assert_eq!(password.expose(), "hunter2");
    }

    #[test]
    fn a_password_the_protocol_field_cannot_carry_is_refused_rather_than_truncated() {
        assert!(Password::parse("12345678").is_ok());
        assert!(Password::parse("123456789").is_err());
        assert!(Password::parse("ab\0cd").is_err());
        assert_eq!(Password::empty().expose(), "");
    }

    #[test]
    fn a_password_rejection_never_echoes_the_value() {
        let error = Password::parse("this-is-far-too-long").expect_err("nine octets or more");

        assert!(!error.to_string().contains("far-too-long"));
        assert_eq!(
            error.to_string(),
            "invalid value for `password`: value is too long"
        );
    }

    #[test]
    fn the_defaults_of_a_new_profile_are_the_ones_of_the_specification() {
        let profile = a_profile();

        assert_eq!(profile.bind_mode(), BindMode::Transceiver);
        assert_eq!(profile.version(), SmppVersion::V5_0);
        assert_eq!(profile.window_size(), 50);
        assert_eq!(profile.throughput_tps(), 100);
        assert_eq!(profile.enquire_link_interval(), Duration::from_secs(30));
        assert_eq!(profile.response_timeout(), Duration::from_secs(10));
        assert_eq!(profile.bind_count(), 1);
        assert_eq!(profile.gsm7_packing(), Gsm7BitPacking::Unpacked);
        assert_eq!(profile.gsm7_charset(), Gsm7BitCharset::Gsm0338);
        assert_eq!(profile.socket_address(), "smsc.example.test:2775");
    }

    #[test]
    fn a_profile_field_outside_its_range_is_refused() {
        let base = || SessionProfile::builder(SessionId::new(), "a", "h", 2775).system_id("esme");

        assert!(base().window_size(0).build().is_err());
        assert!(base().window_size(1_001).build().is_err());
        assert!(base().response_timeout_s(0).build().is_err());
        assert!(base().response_timeout_s(301).build().is_err());
        assert!(base().enquire_link_s(3_601).build().is_err());
        assert!(base().bind_count(0).build().is_err());
        assert!(base().bind_count(17).build().is_err());

        // Zero is legal for the keep-alive alone: it means "disabled".
        assert!(base().enquire_link_s(0).build().is_ok());
    }

    #[test]
    fn a_profile_text_field_that_the_protocol_cannot_carry_is_refused() {
        let base = || SessionProfile::builder(SessionId::new(), "name", "host", 2775);

        assert!(base().system_id("").build().is_err());
        assert!(base().system_id(&"e".repeat(15)).build().is_ok());
        assert!(base().system_id(&"e".repeat(16)).build().is_err());
        assert!(base().system_id("esme").system_type("").build().is_ok());
        assert!(base()
            .system_id("esme")
            .system_type(&"t".repeat(13))
            .build()
            .is_err());
        assert!(base().system_id("es\0me").build().is_err());

        assert!(SessionProfile::builder(SessionId::new(), "", "host", 2775)
            .system_id("esme")
            .build()
            .is_err());
        assert!(SessionProfile::builder(SessionId::new(), "name", "", 2775)
            .system_id("esme")
            .build()
            .is_err());
        assert!(SessionProfile::builder(SessionId::new(), "name", "host", 0)
            .system_id("esme")
            .build()
            .is_err());
    }

    /// ADR 0009 §7 — the one combination the profile refuses outright.
    #[test]
    fn the_alt_charset_cannot_be_combined_with_septet_packing() {
        let base = || SessionProfile::builder(SessionId::new(), "n", "h", 2775).system_id("esme");

        let rejection = base()
            .gsm7_charset(Gsm7BitCharset::Latin1)
            .gsm7_packing(Gsm7BitPacking::Packed)
            .build()
            .expect_err("Latin-1 octets cannot be packed seven bits at a time");

        assert_eq!(
            rejection.to_string(),
            "invalid value for `gsm7_charset`: value contradicts another setting"
        );

        // The other three combinations are all legitimate.
        assert!(base()
            .gsm7_charset(Gsm7BitCharset::Latin1)
            .gsm7_packing(Gsm7BitPacking::Unpacked)
            .build()
            .is_ok());
        assert!(base()
            .gsm7_charset(Gsm7BitCharset::Gsm0338)
            .gsm7_packing(Gsm7BitPacking::Packed)
            .build()
            .is_ok());
        assert!(base()
            .gsm7_charset(Gsm7BitCharset::Gsm0338)
            .gsm7_packing(Gsm7BitPacking::Unpacked)
            .build()
            .is_ok());
    }

    #[test]
    fn a_profile_survives_a_round_trip_through_its_stored_row() {
        let profile = SessionProfile::builder(SessionId::new(), "Operator A", "smsc.test", 2776)
            .system_id("esme01")
            .system_type("SMPP")
            .bind_mode(BindMode::Receiver)
            .version(SmppVersion::V3_4)
            .window_size(10)
            .throughput_tps(25)
            .enquire_link_s(45)
            .response_timeout_s(7)
            .reconnect(ReconnectPolicy::new(true, 2, 120, false).expect("valid bounds"))
            .gsm7_charset(Gsm7BitCharset::Latin1)
            .bind_count(3)
            .build()
            .expect("valid profile");

        let read_back =
            SessionProfile::from_record(&profile.to_record()).expect("our own row is valid");

        assert_eq!(read_back, profile);
    }

    /// CLAUDE.md §8 and step-005 §2: the credential does not go to storage at
    /// this milestone, encrypted or otherwise.
    #[test]
    fn projecting_a_profile_onto_a_row_writes_no_credential() {
        assert!(a_profile().to_record().password_enc.is_empty());
    }

    #[test]
    fn the_reconnection_policy_round_trips_through_its_json_column() {
        for policy in [
            ReconnectPolicy::default(),
            ReconnectPolicy::new(false, 1, 60, true).expect("valid"),
            ReconnectPolicy::new(true, 5, 3_600, false).expect("valid"),
        ] {
            assert_eq!(
                reconnect_from_document(Some(&reconnect_document(policy))).expect("own output"),
                policy
            );
        }
    }

    /// Every profile written before this milestone has `reconnect_config`
    /// NULL. Refusing to load them would make an upgrade look like data loss.
    #[test]
    fn an_absent_or_unreadable_reconnection_document_falls_back_to_the_defaults() {
        assert_eq!(
            reconnect_from_document(None).expect("no document"),
            ReconnectPolicy::default()
        );
        assert_eq!(
            reconnect_from_document(Some("{}")).expect("empty document"),
            ReconnectPolicy::default()
        );
        assert_eq!(
            reconnect_from_document(Some("not json at all")).expect("garbage"),
            ReconnectPolicy::default()
        );
        // A partial document keeps the fields it does carry.
        assert!(!reconnect_from_document(Some(r#"{"jitter":false}"#))
            .expect("partial")
            .has_jitter());
    }

    /// A stored row whose bounds contradict each other is refused rather than
    /// silently repaired: a profile that cannot be validated cannot be bound
    /// safely either.
    #[test]
    fn a_stored_row_that_no_longer_validates_is_refused() {
        let mut record = a_profile().to_record();
        record.window_size = 0;

        assert!(SessionProfile::from_record(&record).is_err());

        let mut record = a_profile().to_record();
        record.reconnect_config = Some(String::from(
            r#"{"enabled":true,"min_backoff_s":600,"max_backoff_s":60,"jitter":true}"#,
        ));

        assert!(SessionProfile::from_record(&record).is_err());
    }

    #[test]
    fn the_bind_types_map_both_ways_without_losing_a_variant() {
        for mode in [
            BindMode::Transmitter,
            BindMode::Receiver,
            BindMode::Transceiver,
        ] {
            assert_eq!(bind_mode_of(bind_type_of(mode)), mode);
        }
    }
}
