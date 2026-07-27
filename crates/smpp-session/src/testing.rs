//! A message centre in memory — deliverable L-005-08, shared since 006.
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
//!   answering, a malformed PDU, a `submit_sm` answered `ESME_RTHROTTLED`:
//!   each is one variant of [`Script`] or [`SubmitReply`], not a contrived
//!   firewall rule.
//! * **Observation.** [`Smsc::connections`] counts connection attempts, and
//!   [`Seen::Submit`] carries the whole decoded PDU — which is how CA-006-06
//!   is stated: the fields the operator typed are compared against the ones
//!   that crossed the socket.
//!
//! # Why it lives in `src/` rather than in `tests/`
//!
//! It was written at milestone 005 inside `tests/support/`, reachable by that
//! crate's integration tests and by nothing else. Milestone 006 needs the same
//! double from `messaging`, to exercise the send path against a real session
//! rather than a hand-written stub — and a second copy of a test double is a
//! second thing to keep honest.
//!
//! It is behind the `test-support` feature, so nothing of it reaches the
//! application binary. `messaging` enables the feature as a
//! **dev**-dependency; the resulting `messaging (dev) → smpp-session →
//! messaging` cycle is one Cargo allows precisely because a dev-dependency
//! cannot affect the library it tests.
//!
//! # Relaxed lints, and why that is safe here
//!
//! `unwrap`, `expect` and `panic!` are relaxed below. `clippy.toml` reopens
//! them under `cfg(test)`, which this module is not — it is library code, so
//! the workspace `deny` would otherwise apply. In a test double a panic **is**
//! the failure report, and the alternative is an error path no test would ever
//! read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use core::future::Future;
use core::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use smpp_core::codec::{self, Command, Pdu};
use smpp_core::pdus::SubmitSm;
use smpp_core::types::SessionId;
use smpp_core::values::{CommandId, CommandStatus};
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder, Framed};

use crate::profile::{Password, SessionProfile};
use crate::reconnect::ReconnectPolicy;
use crate::state::{BindMode, SessionState};
use crate::{Session, SessionHandle, SessionSnapshot, Transport};

/// Buffer of one direction of the in-memory socket.
///
/// Large enough that no test ever fills it: back-pressure between the session
/// and the double is not what any of these tests is about, and a full buffer
/// would look like a hang.
const DUPLEX_CAPACITY: usize = 64 * 1024;

/// Responses the double queues before its writer has to catch up.
///
/// Comfortably above the largest window any test configures: this queue is not
/// what is under test, and a full one would look like a message centre that
/// stopped answering.
const RESPONSE_QUEUE_CAPACITY: usize = 4 * 1024;

/// `sequence_number` of the first PDU the centre pushes on its own.
///
/// Above anything a client allocates in a test, so a `deliver_sm_resp` echoing
/// one of these can only be answering a pushed PDU. A real message centre
/// numbers its own requests from its own space too — spec §7.1 makes the
/// sequence space per-originator.
const FIRST_PUSHED_SEQUENCE: u32 = 900_000;

/// How the message centre behaves once a client connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Script {
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

/// How the message centre answers one `submit_sm`.
///
/// Consumed in order from the queue [`Smsc::answering_submits_with`] sets, then
/// the fallback repeats for ever. That is how a partial failure is expressed:
/// "accept, reject, accept" over a three-segment message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitReply {
    /// `ESME_ROK`, with a fresh identifier the double invents.
    Accept,
    /// `ESME_ROK`, with exactly this identifier.
    AcceptAs(String),
    /// This status, and an **empty** `message_id` — which is what a real
    /// message centre sends on a rejection, and what stops a client from
    /// storing an identifier that identifies nothing.
    Reject(CommandStatus),
    /// Read the request and never answer it. Drives the response timeout.
    Silent,
}

/// What the double saw, so a test can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
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
    /// A `submit_sm` arrived, whole.
    ///
    /// Boxed: a `SubmitSm` is by far the largest variant, and an unboxed one
    /// would make every `Seen` that size.
    Submit {
        /// The `sequence_number` of the request, so a test can check that the
        /// response carrying it was the one the sender waited for.
        sequence: u32,
        /// When it arrived, on the (virtual) clock.
        ///
        /// CA-007-01 is stated as a measurement **on the message centre**, and
        /// it has to be: an instant recorded on the client side is the instant
        /// the sender was admitted, not the instant the PDU crossed the
        /// socket, and the whole question is whether the pacing survives the
        /// hop.
        at: Instant,
        /// The decoded PDU, exactly as it crossed the socket.
        pdu: Box<SubmitSm>,
    },
    /// A `generic_nack` came back.
    GenericNack,
    /// An `unbind` arrived.
    Unbind,
    /// A `deliver_sm_resp` came back, acknowledging a receipt this double sent.
    ///
    /// What CA-008-06 is stated in: the sequence number ties the answer to the
    /// `deliver_sm` it answers, so a test can prove that **every** receipt was
    /// acknowledged rather than that some were.
    DeliverSmResp {
        /// The `sequence_number` echoed back.
        sequence: u32,
    },
    /// Any other operation.
    Other(CommandId),
}

/// The scripted message centre, and the transport that reaches it.
#[derive(Clone)]
pub struct Smsc {
    scripts: Arc<tokio::sync::Mutex<Vec<Script>>>,
    fallback: Script,
    submits: Arc<tokio::sync::Mutex<Vec<SubmitReply>>>,
    submit_fallback: SubmitReply,
    submitted: Arc<AtomicU32>,
    connections: Arc<AtomicU32>,
    latency: Duration,
    seen: mpsc::UnboundedSender<Seen>,
    /// PDUs the centre pushes at the client, unprompted.
    ///
    /// Milestone 008 needs a message centre that sends `deliver_sm`, and needs
    /// to decide **when**: after a delay, out of order, twice, for an
    /// identifier nobody submitted. A queue the test fills gives all four with
    /// one mechanism, where a "send receipts automatically" flag would give
    /// only the first and would bury the ordering the tests are about.
    pushes: mpsc::UnboundedSender<Command>,
    /// The other end, taken by the first connection that is served.
    inbox: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<Command>>>>,
    /// `sequence_number` of the next pushed PDU.
    ///
    /// Starts high so a pushed sequence can never be confused with one the
    /// client allocated, which is what makes an assertion on
    /// [`Seen::DeliverSmResp`] unambiguous.
    pushed: Arc<AtomicU32>,
}

impl core::fmt::Debug for Smsc {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Smsc")
            .field("fallback", &self.fallback)
            .field("connections", &self.connections())
            .finish_non_exhaustive()
    }
}

impl Smsc {
    /// A message centre that follows `script` on every connection.
    #[must_use]
    pub fn always(script: Script) -> (Self, mpsc::UnboundedReceiver<Seen>) {
        Self::scripted(Vec::new(), script)
    }

    /// A message centre that follows `scripts` in order, then `fallback`.
    ///
    /// This is how a reconnection is exercised: "drop the first connection,
    /// accept the second".
    #[must_use]
    pub fn scripted(
        scripts: Vec<Script>,
        fallback: Script,
    ) -> (Self, mpsc::UnboundedReceiver<Seen>) {
        let (seen, receiver) = mpsc::unbounded_channel();
        let (pushes, inbox) = mpsc::unbounded_channel();

        (
            Self {
                scripts: Arc::new(tokio::sync::Mutex::new(scripts)),
                fallback,
                submits: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                submit_fallback: SubmitReply::Accept,
                submitted: Arc::new(AtomicU32::new(0)),
                connections: Arc::new(AtomicU32::new(0)),
                latency: Duration::ZERO,
                seen,
                pushes,
                inbox: Arc::new(tokio::sync::Mutex::new(Some(inbox))),
                pushed: Arc::new(AtomicU32::new(FIRST_PUSHED_SEQUENCE)),
            },
            receiver,
        )
    }

    /// A message centre that takes `latency` to answer a `submit_sm`.
    ///
    /// The delay is applied **per request, concurrently**: the double keeps
    /// reading while an answer is pending, so a client with a window of fifty
    /// really does get fifty PDUs in flight. A double that slept before
    /// reading again would cap the window at one and make every windowing
    /// assertion vacuously true.
    ///
    /// What CA-007-08 is stated against: the round-trip time the client
    /// reports has to be the latency injected here.
    #[must_use]
    pub const fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Answers the first submissions from `replies`, then `fallback` for ever.
    #[must_use]
    pub fn answering_submits_with(
        mut self,
        replies: Vec<SubmitReply>,
        fallback: SubmitReply,
    ) -> Self {
        self.submits = Arc::new(tokio::sync::Mutex::new(replies));
        self.submit_fallback = fallback;
        self
    }

    /// How many times a client has tried to connect.
    ///
    /// The number CA-005-03 is stated in.
    #[must_use]
    pub fn connections(&self) -> u32 {
        self.connections.load(Ordering::SeqCst)
    }

    /// How many `submit_sm` the double has answered.
    ///
    /// CA-006-04 counts three for a 400-character message, and the count is
    /// also what proves the sender **stopped** after a rejection rather than
    /// pushing the remaining segments out.
    #[must_use]
    pub fn submissions(&self) -> u32 {
        self.submitted.load(Ordering::SeqCst)
    }

    /// Pushes one PDU at the client, unprompted, and returns its sequence
    /// number.
    ///
    /// Queued rather than written: the connection may not be up yet, and a
    /// test that had to wait for the bind before scheduling a receipt would be
    /// a test about the bind. The queue is unbounded and drained by the
    /// connection when it starts, so a receipt pushed before the socket exists
    /// arrives as soon as it does.
    pub fn push(&self, pdu: Pdu) -> u32 {
        let sequence = self.pushed.fetch_add(1, Ordering::SeqCst);
        let _ignored = self
            .pushes
            .send(Command::new(CommandStatus::EsmeRok, sequence, pdu));

        sequence
    }

    /// Pushes a delivery receipt whose body is `body`.
    ///
    /// `esm_class` carries the receipt bit of spec §7.8 — without it the client
    /// is right to treat the PDU as an incoming message however the body reads,
    /// so a double that forgot it would make every correlation test fail for
    /// the wrong reason.
    pub fn deliver_receipt(&self, body: &str) -> u32 {
        self.push(Pdu::DeliverSm(receipt_pdu(body)))
    }

    /// Pushes an ordinary incoming message — a mobile-originated SMS.
    ///
    /// `esm_class` says "normal message" whatever the body holds, which is what
    /// makes it the counterexample: a client that reads the body instead of the
    /// flag would treat this as a receipt.
    pub fn deliver_message(&self, body: &str) -> u32 {
        let mut pdu = receipt_pdu(body);
        pdu.esm_class = smpp_core::values::EsmClass::default();

        self.push(Pdu::DeliverSm(pdu))
    }

    /// The reply for the next `submit_sm`.
    async fn next_submit_reply(&self) -> SubmitReply {
        let mut queued = self.submits.lock().await;

        if queued.is_empty() {
            self.submit_fallback.clone()
        } else {
            queued.remove(0)
        }
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
        let centre = self.clone();

        tokio::spawn(async move {
            serve(server, script, centre).await;
        });

        Ok(client)
    }
}

/// Drives one connection according to its script.
async fn serve(stream: DuplexStream, script: Script, centre: Smsc) {
    let seen = centre.seen.clone();
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
    //
    // Reading and answering are split apart since milestone 007. A message
    // centre with a latency that answered inline would stop reading while it
    // slept, so a client with a window of fifty would only ever get one PDU in
    // flight — and every windowing assertion would hold for the wrong reason.
    // The answers go through a bounded queue that a writer task drains, and a
    // delayed answer is a task that sleeps and then queues.
    let (mut sink, mut stream) = framed.split();
    let (answers, mut pending) = mpsc::channel::<Command>(RESPONSE_QUEUE_CAPACITY);

    let writer = tokio::spawn(async move {
        while let Some(answer) = pending.recv().await {
            if sink.send(answer).await.is_err() {
                return;
            }
        }
    });

    // The unprompted half: whatever a test pushed goes out through the same
    // writer, so a `deliver_sm` and a `submit_sm_resp` cannot interleave
    // halfway through a frame. Only the first connection takes the inbox — a
    // reconnection test would otherwise have two consumers racing for it, and
    // the receipts would land on whichever won.
    let inbox = centre.inbox.lock().await.take();

    let pusher = inbox.map(|mut inbox| {
        let answers = answers.clone();

        tokio::spawn(async move {
            while let Some(pdu) = inbox.recv().await {
                if answers.send(pdu).await.is_err() {
                    return;
                }
            }
        })
    });

    while let Some(Ok(command)) = stream.next().await {
        let sequence = command.sequence_number();

        let note = match (command.id(), command.pdu()) {
            (CommandId::SubmitSm, Some(Pdu::SubmitSm(body))) => Seen::Submit {
                sequence,
                at: Instant::now(),
                pdu: Box::new(body.clone()),
            },
            (CommandId::EnquireLink, _) => Seen::EnquireLink(Instant::now()),
            (CommandId::GenericNack, _) => Seen::GenericNack,
            (CommandId::Unbind, _) => Seen::Unbind,
            (CommandId::DeliverSmResp, _) => Seen::DeliverSmResp { sequence },
            (other, _) => Seen::Other(other),
        };
        let _ignored = seen.send(note);

        // A silent message centre reads but never answers. The socket stays
        // perfectly healthy, which is the whole point.
        if script == Script::AcceptThenGoSilent {
            continue;
        }

        let (status, answer) = match command.id() {
            CommandId::EnquireLink => (CommandStatus::EsmeRok, Pdu::EnquireLinkResp),
            CommandId::Unbind => (CommandStatus::EsmeRok, Pdu::UnbindResp),
            CommandId::SubmitSm => {
                let ordinal = centre.submitted.fetch_add(1, Ordering::SeqCst) + 1;

                match centre.next_submit_reply().await {
                    SubmitReply::Silent => continue,
                    SubmitReply::Accept => (
                        CommandStatus::EsmeRok,
                        submit_response(&format!("SMSC-{ordinal}")),
                    ),
                    SubmitReply::AcceptAs(identifier) => {
                        (CommandStatus::EsmeRok, submit_response(&identifier))
                    }
                    // An empty `message_id`, as a real message centre sends on
                    // a rejection.
                    SubmitReply::Reject(status) => (status, submit_response("")),
                }
            }
            _ => continue,
        };

        let response = Command::new(status, sequence, answer);
        let closing = command.id() == CommandId::Unbind;

        // The latency applies to submissions alone. Delaying the keep-alive or
        // the unbind would be modelling a slow *socket*, which is a different
        // fault and one milestone 005 already has scripts for.
        if centre.latency.is_zero() || command.id() != CommandId::SubmitSm {
            if answers.send(response).await.is_err() || closing {
                break;
            }
        } else {
            let answers = answers.clone();
            let latency = centre.latency;

            tokio::spawn(async move {
                tokio::time::sleep(latency).await;

                let _ignored = answers.send(response).await;
            });
        }
    }

    drop(answers);

    if let Some(pusher) = pusher {
        pusher.abort();
    }

    let _ignored = writer.await;
}

/// A `deliver_sm` carrying a delivery receipt, as spec §7.8 describes one.
///
/// The `esm_class` is what makes it a receipt rather than an incoming message,
/// and it is set explicitly rather than left to `EsmClass::default()`: the
/// default carries a zero `message_type`, which is "normal message".
fn receipt_pdu(body: &str) -> smpp_core::pdus::DeliverSm {
    use smpp_core::values::{Ansi41Specific, EsmClass, GsmFeatures, MessageType, MessagingMode};

    smpp_core::pdus::DeliverSm::builder()
        .esm_class(EsmClass::new(
            MessagingMode::Default,
            MessageType::ShortMessageContainsMCDeliveryReceipt,
            Ansi41Specific::ShortMessageContainsDeliveryAcknowledgement,
            GsmFeatures::NotSelected,
        ))
        .short_message(
            smpp_core::octets::OctetString::from_slice(body.as_bytes())
                .expect("the fixture receipt body fits 255 octets"),
        )
        .build()
}

/// A `submit_sm_resp` carrying `message_id`.
fn submit_response(message_id: &str) -> Pdu {
    use core::str::FromStr as _;

    let identifier = smpp_core::octets::COctetString::from_str(message_id)
        .expect("the fixture identifier fits the 65-octet field");

    Pdu::SubmitSmResp(smpp_core::pdus::SubmitSmResp::new(identifier, Vec::new()))
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
/// the same framing would agree about a framing bug as readily as about a
/// correct framing, and the whole point of the double is to disagree when the
/// session is wrong.
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

// --- Building the session under test ----------------------------------------

/// A profile pointing at the double, with everything else at its default.
#[must_use]
pub fn a_profile() -> SessionProfile {
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
pub fn a_password() -> Password {
    Password::parse("n0tr34l").expect("eight octets or fewer")
}

/// The exact string [`a_password`] carries, for the leak test.
pub const PASSWORD_TEXT: &str = "n0tr34l";

/// Starts a session against `smsc`.
#[must_use]
pub fn start(profile: SessionProfile, smsc: Smsc) -> Session {
    crate::spawn(profile, a_password(), smsc)
}

/// Waits until the session reaches a state satisfying `predicate`.
///
/// Under `#[tokio::test(start_paused = true)]` this is instantaneous whatever
/// the back-off: Tokio advances its clock as soon as every task is idle.
///
/// # Panics
///
/// If the session ends without ever satisfying the predicate.
pub async fn wait_for(
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
///
/// # Panics
///
/// If the session ends without ever binding.
pub async fn wait_until_bound(handle: &SessionHandle) -> BindMode {
    let snapshot = wait_for(handle, |snapshot| snapshot.state.is_bound()).await;

    snapshot
        .state
        .bind_mode()
        .expect("a bound state carries its mode")
}

/// Waits for the session to reach a state whose code is `code`.
///
/// # Panics
///
/// If the session ends without ever reaching it.
pub async fn wait_for_code(handle: &SessionHandle, code: &str) -> SessionSnapshot {
    wait_for(handle, |snapshot| snapshot.state.code() == code).await
}

/// A reconnection policy with tight, exact bounds and no jitter.
///
/// Jitter is what CA-005-05 is about and it has its own test; everywhere else
/// it only makes an assertion fuzzy.
#[must_use]
pub fn tight_backoff() -> ReconnectPolicy {
    ReconnectPolicy::new(true, 1, 4, false).expect("valid bounds")
}

/// Drains everything the double has seen so far.
pub fn drain(seen: &mut mpsc::UnboundedReceiver<Seen>) -> Vec<Seen> {
    let mut notes = Vec::new();

    while let Ok(note) = seen.try_recv() {
        notes.push(note);
    }

    notes
}

/// Every `deliver_sm_resp` the double received so far, by sequence number.
///
/// The set CA-008-06 is stated against: it must hold every sequence number
/// [`Smsc::deliver_receipt`] returned, with no exception and no duplicate
/// requirement — one acknowledgement per receipt.
#[must_use]
pub fn acknowledged(notes: &[Seen]) -> Vec<u32> {
    notes
        .iter()
        .filter_map(|note| match note {
            Seen::DeliverSmResp { sequence } => Some(*sequence),
            _ => None,
        })
        .collect()
}

/// Every `submit_sm` the double received so far, in order.
#[must_use]
pub fn submissions(notes: &[Seen]) -> Vec<(u32, SubmitSm)> {
    notes
        .iter()
        .filter_map(|note| match note {
            Seen::Submit { sequence, pdu, .. } => Some((*sequence, (**pdu).clone())),
            _ => None,
        })
        .collect()
}

/// Asserts that a session is in a given state, with a readable failure.
///
/// # Panics
///
/// If the state differs.
pub fn assert_state(handle: &SessionHandle, expected: SessionState) {
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
pub const QUIET_PERIOD: Duration = Duration::from_secs(30);
