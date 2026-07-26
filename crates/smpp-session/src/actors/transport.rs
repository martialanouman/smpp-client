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

use core::future::Future;

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
        let stream = TcpStream::connect(address).await?;

        // Nagle batches small writes, which is exactly wrong here: an SMPP PDU
        // is small and latency-sensitive, and the round-trip time of a
        // `submit_sm` is a metric milestone 007 regulates against. Waiting
        // 40 ms for a co-tenant packet would make that measurement meaningless.
        stream.set_nodelay(true)?;

        Ok(stream)
    }
}
