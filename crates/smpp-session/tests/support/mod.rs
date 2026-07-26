//! A message centre in memory — deliverable L-005-08.
//!
//! No listener, no port, no `sleep` waiting for a connection: every session
//! under test runs on [`tokio::io::duplex`]. Three things follow, and all three
//! are why the double exists.
//!
//! * **Determinism.** Nothing here depends on the host's network stack, so
//!   Tokio's virtual clock is usable end to end — `#[tokio::test(start_paused =
//!   true)]` makes a thirty-second `enquire_link` period cost nothing and
//!   makes a back-off assertion exact.
//! * **Fault injection.** A dropped socket, a message centre that stops
//!   answering, a malformed PDU: each is one variant of [`Script`], not a
//!   contrived firewall rule.
//! * **Observation.** [`Smsc::connections`] counts connection attempts, which
//!   is how CA-005-03 is stated — *zero* new attempts for three times the
//!   minimum back-off.
//!
//! The double shares no code with the crate under test: it has its own codec,
//! deliberately, so that a bug in the session's framing cannot cancel itself
//! out against the same bug on the other side.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use core::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use smpp_core::codec::{self, Command, Pdu};
use smpp_core::types::SessionId;
use smpp_core::values::{CommandId, CommandStatus};
use smpp_session::profile::{Password, SessionProfile};
use smpp_session::reconnect::ReconnectPolicy;
use smpp_session::state::{BindMode, SessionState};
use smpp_session::{Session, SessionHandle, SessionSnapshot, Transport};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder, Framed};

/// Buffer of one direction of the in-memory socket.
///
/// Large enough that no test ever fills it: back-pressure between the session
/// and the double is not what any of these tests is about, and a full buffer
/// would look like a hang.
const DUPLEX_CAPACITY: usize = 64 * 1024;

/// How the message centre behaves once a client connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Script {
    /// Accept the bind, then answer every `enquire_link`.
    Accept,
    /// Refuse the bind with this status.
    Reject(CommandStatus),
    /// Accept the bind, then never answer anything again — the socket stays
    /// open and the session behind it is gone. The failure `enquire_link`
    /// exists to catch.
    AcceptThenGoSilent,
    /// Accept the bind, then close the socket without an `unbind`.
    AcceptThenDrop,
    /// Accept the bind, send one PDU that will not parse, then behave.
    AcceptThenSendGarbage,
    /// Accept the bind, then send an `unbind` of its own.
    AcceptThenUnbind,
    /// Refuse the TCP connection outright.
    RefuseConnection,
}

/// What the double saw, so a test can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seen {
    /// A bind arrived, with the operation and the credential it carried.
    Bind {
        /// Which bind operation.
        operation: CommandId,
        /// The `system_id` field.
        system_id: String,
        /// The password field, so a test can prove it never reached a log.
        password: String,
    },
    /// An `enquire_link` arrived at this instant on the (virtual) clock.
    EnquireLink(Instant),
    /// A `generic_nack` came back.
    GenericNack,
    /// An `unbind` arrived.
    Unbind,
    /// Any other operation.
    Other(CommandId),
}

/// The scripted message centre, and the transport that reaches it.
#[derive(Clone)]
pub(crate) struct Smsc {
    scripts: Arc<tokio::sync::Mutex<Vec<Script>>>,
    fallback: Script,
    connections: Arc<AtomicU32>,
    seen: mpsc::UnboundedSender<Seen>,
}

impl Smsc {
    /// A message centre that follows `script` on every connection.
    #[must_use]
    pub(crate) fn always(script: Script) -> (Self, mpsc::UnboundedReceiver<Seen>) {
        Self::scripted(Vec::new(), script)
    }

    /// A message centre that follows `scripts` in order, then `fallback`.
    ///
    /// This is how a reconnection is exercised: "drop the first connection,
    /// accept the second".
    #[must_use]
    pub(crate) fn scripted(
        scripts: Vec<Script>,
        fallback: Script,
    ) -> (Self, mpsc::UnboundedReceiver<Seen>) {
        let (seen, receiver) = mpsc::unbounded_channel();

        (
            Self {
                scripts: Arc::new(tokio::sync::Mutex::new(scripts)),
                fallback,
                connections: Arc::new(AtomicU32::new(0)),
                seen,
            },
            receiver,
        )
    }

    /// How many times a client has tried to connect.
    ///
    /// The number CA-005-03 is stated in.
    #[must_use]
    pub(crate) fn connections(&self) -> u32 {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Transport for Smsc {
    type Stream = DuplexStream;

    async fn connect(&self, _address: &str) -> std::io::Result<DuplexStream> {
        self.connections.fetch_add(1, Ordering::SeqCst);

        let script = {
            let mut scripts = self.scripts.lock().await;

            if scripts.is_empty() {
                self.fallback.clone()
            } else {
                scripts.remove(0)
            }
        };

        if script == Script::RefuseConnection {
            return Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        }

        let (client, server) = tokio::io::duplex(DUPLEX_CAPACITY);
        let seen = self.seen.clone();

        tokio::spawn(async move {
            serve(server, script, seen).await;
        });

        Ok(client)
    }
}

/// Drives one connection according to its script.
async fn serve(stream: DuplexStream, script: Script, seen: mpsc::UnboundedSender<Seen>) {
    let mut framed = Framed::new(stream, ServerCodec);

    // --- The bind ----------------------------------------------------------
    let Some(Ok(request)) = framed.next().await else {
        return;
    };

    let (system_id, password) = credentials(&request);
    let _ignored = seen.send(Seen::Bind {
        operation: request.id(),
        system_id,
        password,
    });

    let status = match script {
        Script::Reject(status) => status,
        _ => CommandStatus::EsmeRok,
    };

    let response = Command::new(
        status,
        request.sequence_number(),
        bind_response(request.id()),
    );

    if framed.send(response).await.is_err() || status != CommandStatus::EsmeRok {
        return;
    }

    match script {
        Script::AcceptThenDrop => return,
        Script::AcceptThenSendGarbage => {
            // A well-framed PDU whose body will not parse: `command_length` is
            // honest, the body is not a `submit_sm`.
            let mut garbage = BytesMut::new();
            garbage.extend_from_slice(&[0x00, 0x00, 0x00, 0x14]);
            garbage.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]);
            garbage.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            garbage.extend_from_slice(&[0x00, 0x00, 0x00, 0x63]);
            garbage.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);

            if framed.get_mut().write_raw(&garbage).await.is_err() {
                return;
            }
        }
        Script::AcceptThenUnbind
            if framed
                .send(Command::new(CommandStatus::EsmeRok, 9_001, Pdu::Unbind))
                .await
                .is_err() =>
        {
            return;
        }
        _ => {}
    }

    // --- The bound phase ---------------------------------------------------
    while let Some(Ok(command)) = framed.next().await {
        let sequence = command.sequence_number();

        let note = match command.id() {
            CommandId::EnquireLink => Seen::EnquireLink(Instant::now()),
            CommandId::GenericNack => Seen::GenericNack,
            CommandId::Unbind => Seen::Unbind,
            other => Seen::Other(other),
        };
        let _ignored = seen.send(note);

        // A silent message centre reads but never answers. The socket stays
        // perfectly healthy, which is the whole point.
        if script == Script::AcceptThenGoSilent {
            continue;
        }

        let answer = match command.id() {
            CommandId::EnquireLink => Pdu::EnquireLinkResp,
            CommandId::Unbind => Pdu::UnbindResp,
            CommandId::SubmitSm => Pdu::SubmitSmResp(smpp_core::pdus::SubmitSmResp::default()),
            _ => continue,
        };

        let closing = command.id() == CommandId::Unbind;

        if framed
            .send(Command::new(CommandStatus::EsmeRok, sequence, answer))
            .await
            .is_err()
            || closing
        {
            return;
        }
    }
}

/// The `system_id` and password a bind carried.
fn credentials(command: &Command) -> (String, String) {
    let text = |value: &smpp_core::octets::COctetString<1, 16>| value.as_str().to_owned();
    let secret = |value: &smpp_core::octets::COctetString<1, 9>| value.as_str().to_owned();

    match command.pdu() {
        Some(Pdu::BindTransmitter(bind)) => (text(&bind.system_id), secret(&bind.password)),
        Some(Pdu::BindReceiver(bind)) => (text(&bind.system_id), secret(&bind.password)),
        Some(Pdu::BindTransceiver(bind)) => (text(&bind.system_id), secret(&bind.password)),
        _ => (String::new(), String::new()),
    }
}

/// The response PDU that answers a bind request.
fn bind_response(request: CommandId) -> Pdu {
    match request {
        CommandId::BindTransmitter => {
            Pdu::BindTransmitterResp(smpp_core::pdus::BindTransmitterResp::default())
        }
        CommandId::BindReceiver => {
            Pdu::BindReceiverResp(smpp_core::pdus::BindReceiverResp::default())
        }
        _ => Pdu::BindTransceiverResp(smpp_core::pdus::BindTransceiverResp::default()),
    }
}

/// The double's own framing.
///
/// Written out rather than shared with the crate under test: two sides using
/// the same codec would agree about a framing bug as readily as about a correct
/// framing, and the whole point of the double is to disagree when the session
/// is wrong.
struct ServerCodec;

impl Decoder for ServerCodec {
    type Item = Command;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Command>, std::io::Error> {
        if src.len() < 16 {
            return Ok(None);
        }

        let announced = usize::try_from(u32::from_be_bytes([src[0], src[1], src[2], src[3]]))
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;

        if announced < 16 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }

        if src.len() < announced {
            return Ok(None);
        }

        let frame = src.split_to(announced);

        codec::decode(&frame)
            .map(Some)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }
}

impl Encoder<Command> for ServerCodec {
    type Error = std::io::Error;

    fn encode(&mut self, command: Command, dst: &mut BytesMut) -> Result<(), std::io::Error> {
        let bytes = codec::encode(&command)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;

        dst.extend_from_slice(&bytes);

        Ok(())
    }
}

/// Writing bytes that are not a `Command`, for the malformed-PDU script.
trait WriteRaw {
    /// Writes `bytes` verbatim.
    fn write_raw(&mut self, bytes: &[u8]) -> impl Future<Output = std::io::Result<()>> + Send;
}

impl WriteRaw for DuplexStream {
    async fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt as _;

        self.write_all(bytes).await
    }
}

use core::future::Future;

// --- Building the session under test ----------------------------------------

/// A profile pointing at the double, with everything else at its default.
#[must_use]
pub(crate) fn a_profile() -> SessionProfile {
    SessionProfile::builder(SessionId::new(), "double", "in-memory", 2775)
        .system_id("esme01")
        .build()
        .expect("the fixture is valid")
}

/// The credential every test uses.
///
/// A fixture value, and one no message centre would accept. CLAUDE.md §8
/// forbids a real credential in a fixture whether or not it is encrypted.
#[must_use]
pub(crate) fn a_password() -> Password {
    Password::parse("n0tr34l").expect("eight octets or fewer")
}

/// The exact string [`a_password`] carries, for the leak test.
pub(crate) const PASSWORD_TEXT: &str = "n0tr34l";

/// Starts a session against `smsc`.
#[must_use]
pub(crate) fn start(profile: SessionProfile, smsc: Smsc) -> Session {
    smpp_session::spawn(profile, a_password(), smsc)
}

/// Waits until the session reaches a state satisfying `predicate`.
///
/// Under `#[tokio::test(start_paused = true)]` this is instantaneous whatever
/// the back-off: Tokio advances its clock as soon as every task is idle.
///
/// # Panics
///
/// If the session ends without ever satisfying the predicate.
pub(crate) async fn wait_for(
    handle: &SessionHandle,
    predicate: impl Fn(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let mut watch = handle.watch();

    loop {
        {
            let snapshot = watch.borrow_and_update();

            if predicate(&snapshot) {
                return snapshot.clone();
            }
        }

        if watch.changed().await.is_err() {
            // The supervisor returned, so its `watch::Sender` is gone. The
            // last value it published is still readable, and it is often the
            // one being waited for — a clean `unbind` publishes `UNBOUND` and
            // exits in the same breath.
            let snapshot = watch.borrow();

            assert!(
                predicate(&snapshot),
                "the session ended in {} without reaching the expected state",
                snapshot.state
            );

            return snapshot.clone();
        }
    }
}

/// Waits for the session to be bound.
pub(crate) async fn wait_until_bound(handle: &SessionHandle) -> BindMode {
    let snapshot = wait_for(handle, |snapshot| snapshot.state.is_bound()).await;

    snapshot
        .state
        .bind_mode()
        .expect("a bound state carries its mode")
}

/// Waits for the session to reach a state whose code is `code`.
pub(crate) async fn wait_for_code(handle: &SessionHandle, code: &str) -> SessionSnapshot {
    wait_for(handle, |snapshot| snapshot.state.code() == code).await
}

/// A reconnection policy with tight, exact bounds and no jitter.
///
/// Jitter is what CA-005-05 is about and it has its own test; everywhere else
/// it only makes an assertion fuzzy.
#[must_use]
pub(crate) fn tight_backoff() -> ReconnectPolicy {
    ReconnectPolicy::new(true, 1, 4, false).expect("valid bounds")
}

/// Drains everything the double has seen so far.
pub(crate) fn drain(seen: &mut mpsc::UnboundedReceiver<Seen>) -> Vec<Seen> {
    let mut notes = Vec::new();

    while let Ok(note) = seen.try_recv() {
        notes.push(note);
    }

    notes
}

/// Asserts that a session is in a given state, with a readable failure.
///
/// # Panics
///
/// If the state differs.
pub(crate) fn assert_state(handle: &SessionHandle, expected: SessionState) {
    let snapshot = handle.snapshot();

    assert_eq!(
        snapshot.state, expected,
        "session is {} (last error: {:?})",
        snapshot.state, snapshot.last_error
    );
}

/// How long to let a paused clock run before declaring "nothing happened".
///
/// Long enough for several back-offs of [`tight_backoff`], short enough that a
/// real-clock accident would be obvious.
pub(crate) const QUIET_PERIOD: Duration = Duration::from_secs(30);
