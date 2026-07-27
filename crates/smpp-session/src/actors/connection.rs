//! Opening the socket and completing the bind.
//!
//! The one place in the workspace where [`Password::expose`] is called. Keep it
//! that way: a second call site is a second place a credential can reach a log.

use futures_util::{SinkExt as _, StreamExt as _};
use smpp_core::codec::{Command, Pdu};
use smpp_core::octets::COctetString;
use smpp_core::pdus::{BindReceiver, BindTransceiver, BindTransmitter};
use smpp_core::status_codes;
use smpp_core::values::{CommandStatus, InterfaceVersion, Npi, Ton};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

use crate::actors::framing::SessionCodec;
use crate::error::SessionError;
use crate::profile::{Password, SessionProfile};
use crate::state::BindMode;

/// The `sequence_number` of the bind request.
///
/// One, always: the bind is the first PDU of a session, and nothing else is in
/// flight when it goes out. The correlation table is not involved — there is
/// nothing to correlate against.
const BIND_SEQUENCE_NUMBER: u32 = 1;

/// Wraps a stream in the SMPP framing.
///
/// The returned value is the **only** handle to the socket, and it is neither
/// `Clone` nor `Copy` (CA-005-10): sharing it is not a discipline anyone has to
/// keep, it does not typecheck.
pub(crate) fn frame<S>(stream: S) -> Framed<S, SessionCodec>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    Framed::new(stream, SessionCodec::new())
}

/// Sends the bind and waits for its response.
///
/// # Errors
///
/// * [`SessionError::Transport`] if the socket fails or closes first;
/// * [`SessionError::Protocol`] if what comes back is not a decodable PDU;
/// * [`SessionError::UnexpectedResponse`] if it is a PDU but not the bind
///   response;
/// * [`SessionError::BindRejected`] if the message centre says no — carrying
///   the classification that decides whether the supervisor retries.
pub(crate) async fn bind<S>(
    framed: &mut Framed<S, SessionCodec>,
    profile: &SessionProfile,
    password: &Password,
    watcher: &crate::actors::reader::Watcher,
) -> Result<(), SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = bind_request(profile, password)?;
    let expected = profile.bind_mode().bind_operation().matching_response();

    let command = Command::new(CommandStatus::EsmeRok, BIND_SEQUENCE_NUMBER, request);

    // The handshake is observed like everything else, and that is deliberate
    // rather than an oversight nobody excluded: an operator debugging a bind
    // rejection wants exactly these two PDUs, and they are the ones that never
    // reach the supervisor's write path.
    //
    // It does mean the recorded bind carries the credential. That is why the
    // recorder is off by default and behind `DebugDumpAuthorisation`
    // (CLAUDE.md §8) — the protection is the switch, not a hole in what the
    // switch covers.
    watcher.saw(crate::PduFlow::Outbound, &command);

    framed
        .send(command)
        .await
        .map_err(|source| SessionError::Transport {
            operation: "sending the bind",
            source,
        })?;

    let response = match framed.next().await {
        Some(Ok(Ok(response))) => response,
        // A well-framed PDU that would not parse. Recoverable in the bound
        // phase (CA-005-07), but here it means the bind response itself is
        // unreadable, and there is nothing to carry on with.
        Some(Ok(Err(error))) => return Err(SessionError::Protocol(error)),
        Some(Err(source)) => {
            return Err(SessionError::Transport {
                operation: "reading the bind response",
                source,
            })
        }
        None => {
            return Err(SessionError::Transport {
                operation: "reading the bind response",
                source: std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            })
        }
    };

    watcher.saw(crate::PduFlow::Inbound, &response);

    if response.id() != expected {
        return Err(SessionError::UnexpectedResponse {
            expected,
            actual: response.id(),
        });
    }

    let status = response.status();

    if status != CommandStatus::EsmeRok {
        // The classification of milestone 003, carried rather than
        // re-derived. It is what CA-005-03 turns on: `ESME_RINVPASWD` is
        // `Fatal`, and a fatal bind rejection must not open a reconnection
        // loop.
        let described = status_codes::describe(status);

        return Err(SessionError::BindRejected {
            operation: profile.bind_mode().bind_operation(),
            status,
            symbol: described.map_or("ESME_UNKNOWN", |entry| entry.symbol),
            class: status_codes::classify(status),
        });
    }

    Ok(())
}

/// Builds the bind PDU for the profile's mode.
///
/// # Errors
///
/// [`SessionError::Protocol`] if a field does not fit its C-Octet String. The
/// profile validates the same bounds at construction time, so this is the
/// second line of defence rather than the first.
fn bind_request(profile: &SessionProfile, password: &Password) -> Result<Pdu, SessionError> {
    let system_id = c_octet_string(profile.system_id(), "system_id")?;
    // THE ONE PLACE THE PASSWORD IS READ. See `Password::expose`.
    let password = c_octet_string(password.expose(), "password")?;
    let system_type = c_octet_string(profile.system_type(), "system_type")?;
    let interface_version = InterfaceVersion::from(profile.version());

    // Spec §8.2 lists `addr_ton`, `addr_npi` and `address_range`; the profile
    // does not carry them yet (see `profile`), and the values below are what
    // the specification prescribes for an ESME that does not serve a specific
    // address range: "set to NULL if not known".
    let addr_ton = Ton::Unknown;
    let addr_npi = Npi::Unknown;
    let address_range = COctetString::empty();

    Ok(match profile.bind_mode() {
        BindMode::Transmitter => Pdu::BindTransmitter(BindTransmitter::new(
            system_id,
            password,
            system_type,
            interface_version,
            addr_ton,
            addr_npi,
            address_range,
        )),
        BindMode::Receiver => Pdu::BindReceiver(BindReceiver::new(
            system_id,
            password,
            system_type,
            interface_version,
            addr_ton,
            addr_npi,
            address_range,
        )),
        // `BindMode` is `#[non_exhaustive]`; transceiver is both the default
        // and the safe fallback.
        _ => Pdu::BindTransceiver(BindTransceiver::new(
            system_id,
            password,
            system_type,
            interface_version,
            addr_ton,
            addr_npi,
            address_range,
        )),
    })
}

/// Builds a C-Octet String, reporting which field refused the value.
///
/// The rejected value is never in the message: one of these fields is the
/// password (CLAUDE.md §8).
fn c_octet_string<const MIN: usize, const MAX: usize>(
    value: &str,
    field: &'static str,
) -> Result<COctetString<MIN, MAX>, SessionError> {
    // `from_string`, not `from_slice`: the slice constructors expect the NUL
    // terminator to be **in** the bytes already, and a profile field does not
    // carry one. `from_string` appends it. Getting this wrong fails every bind
    // with `NotNullTerminated`, which is exactly how it was found.
    COctetString::from_string(value.to_owned())
        .map_err(|_| SessionError::invalid_profile(field, crate::error::ProfileRejection::TooLong))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smpp_core::types::SessionId;

    fn a_profile(mode: BindMode) -> SessionProfile {
        SessionProfile::builder(SessionId::new(), "test", "smsc.test", 2775)
            .system_id("esme01")
            .bind_mode(mode)
            .build()
            .expect("valid profile")
    }

    /// CA-005-02 and EF-CNX-02 — each bind type produces its own PDU.
    #[test]
    fn each_bind_mode_builds_its_own_bind_pdu() {
        let password = Password::parse("pw").expect("short enough");

        assert!(matches!(
            bind_request(&a_profile(BindMode::Transmitter), &password),
            Ok(Pdu::BindTransmitter(_))
        ));
        assert!(matches!(
            bind_request(&a_profile(BindMode::Receiver), &password),
            Ok(Pdu::BindReceiver(_))
        ));
        assert!(matches!(
            bind_request(&a_profile(BindMode::Transceiver), &password),
            Ok(Pdu::BindTransceiver(_))
        ));
    }

    /// EF-CNX-04 — the version the profile names is the one announced.
    #[test]
    fn the_announced_interface_version_is_the_one_the_profile_carries() {
        use smpp_core::values::SmppVersion;

        let password = Password::empty();

        for (version, expected) in [
            (SmppVersion::V3_4, InterfaceVersion::Smpp3_4),
            (SmppVersion::V5_0, InterfaceVersion::Smpp5_0),
        ] {
            let profile = SessionProfile::builder(SessionId::new(), "t", "h", 2775)
                .system_id("esme")
                .version(version)
                .build()
                .expect("valid profile");

            let Ok(Pdu::BindTransceiver(request)) = bind_request(&profile, &password) else {
                panic!("a transceiver profile builds a transceiver bind");
            };

            assert_eq!(request.interface_version, expected);
        }
    }
}
