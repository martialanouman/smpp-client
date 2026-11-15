//! How a session obtains a byte stream.
//!
//! One trait, so the session logic never names `TcpStream`. Two things fall out
//! of that, and both are the point:
//!
//! * the whole of step-005's integration suite runs on
//!   [`tokio::io::duplex`](tokio::io::duplex) — no listener, no port, no
//!   `sleep` waiting for a connection to come up, and therefore tests that are
//!   deterministic and that can use Tokio's virtual clock;
//! * TLS (milestone 015) is another implementation of this trait rather than a
//!   branch inside the session.
//!
//! # Why there is a second, object-safe form of the same trait
//!
//! [`Transport`] has an associated `Stream` and an `async fn`, so it is not
//! object-safe: a holder has to name the implementation in its own type, which
//! is right for [`crate::SessionRegistry`] and wrong for the application above
//! it. `src-tauri` keeps the registry inside a state that every
//! `#[tauri::command]` receives, and a command cannot be generic — so a
//! registry parameterised by `TcpTransport` makes that choice at **compile
//! time**, for the whole application, and the only way to reach it with another
//! transport is to put a real socket in front.
//!
//! [`DynTransport`] is the same contract with the stream type erased, and
//! [`SharedTransport`] is what the application holds instead. The choice then
//! happens where it belongs — once, at startup — which is what lets the test
//! suite inject the in-memory message centre and what milestone 015 will need
//! to pick TLS from a setting rather than from a type.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Opens the byte stream a session runs on.
pub trait Transport: Send + Sync + 'static {
    /// The stream this transport produces.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Opens a stream to `address`, given as `host:port`.
    ///
    /// # Errors
    ///
    /// Whatever the underlying connector reports: refused, unreachable,
    /// timed out, name not resolved.
    fn connect(&self, address: &str) -> impl Future<Output = std::io::Result<Self::Stream>> + Send;
}

/// What a session needs of a byte stream, as one nameable bound.
///
/// Exists so [`BoxedStream`] can be written down: `dyn AsyncRead + AsyncWrite`
/// is not a type, because only one of the two can be the principal trait of a
/// trait object. The blanket implementation means nothing ever implements this
/// by hand.
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> ByteStream for T {}

/// A byte stream whose concrete type is no longer named.
pub type BoxedStream = Box<dyn ByteStream>;

/// [`Transport`], with its stream type erased.
///
/// Implemented for **every** [`Transport`] by the blanket below, so no
/// transport implements it by hand and the two cannot drift apart. What holders
/// pass around is [`SharedTransport`], never this trait on its own.
pub trait DynTransport: Send + Sync + 'static {
    /// Opens a stream to `address`, given as `host:port`.
    ///
    /// # Errors
    ///
    /// Whatever the underlying connector reports — the errors of
    /// [`Transport::connect`], unchanged.
    fn connect_boxed<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<BoxedStream>> + Send + 'a>>;
}

impl<T: Transport> DynTransport for T {
    fn connect_boxed<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<BoxedStream>> + Send + 'a>> {
        Box::pin(async move { Ok(Box::new(self.connect(address).await?) as BoxedStream) })
    }
}

/// The transport an application chooses at startup.
///
/// A [`Transport`] in its own right — see the implementation below — so it goes
/// into [`crate::SessionRegistry`] like any other, and `Clone`, which the
/// registry requires of its transport.
pub type SharedTransport = Arc<dyn DynTransport>;

impl Transport for SharedTransport {
    type Stream = BoxedStream;

    async fn connect(&self, address: &str) -> std::io::Result<BoxedStream> {
        self.as_ref().connect_boxed(address).await
    }
}

/// How long a TCP connection may take to establish.
///
/// The operating system's own timeout is 75 to 130 seconds depending on the
/// platform, and it only applies when the SYN goes unanswered — a host behind
/// a firewall that drops packets rather than refusing them. Two minutes with
/// the interface stuck on `CONNECTING`, the back-off never engaging and the
/// reconnection policy never consulted, is not a failure mode worth inheriting
/// from the kernel.
///
/// Ten seconds is long for a TCP handshake to a message centre one is expected
/// to hold a session with, and short enough that the operator sees the
/// reconnection start.
pub const CONNECT_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(10);

/// A plain TCP connection.
///
/// No TLS: milestone 015 owns that, and step-005 §2 puts it out of scope. The
/// explicit interface warning for a cleartext session (CLAUDE.md §8) is posted
/// there too, next to the setting that would turn TLS on.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpTransport;

impl Transport for TcpTransport {
    type Stream = TcpStream;

    async fn connect(&self, address: &str) -> std::io::Result<TcpStream> {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the message centre did not answer the TCP handshake",
                )
            })??;

        // Nagle batches small writes, which is exactly wrong here: an SMPP PDU
        // is small and latency-sensitive, and the round-trip time of a
        // `submit_sm` is a metric milestone 007 regulates against. Waiting
        // 40 ms for a co-tenant packet would make that measurement meaningless.
        stream.set_nodelay(true)?;

        Ok(stream)
    }
}
