//! Byte-stream transports from the trusted core to the inference runtime.
//!
//! - [`Connector::Loopback`]: TCP to a loopback address. Used for the
//!   unsandboxed development path and for platforms where the runtime
//!   listens on loopback under OS restrictions.
//! - [`Connector::Pipe`]: pre-connected named-pipe streams handed over by the
//!   supervisor's accept loop (Windows sandbox, ADR-012 §4.1). Every stream
//!   has already passed the client-PID check.
//!
//! The HTTP layer ([`crate::client`]) is identical for both.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};

/// Any bidirectional byte stream.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

/// How long to wait for a pre-connected stream before giving up.
const STREAM_WAIT: Duration = Duration::from_secs(15);
const TCP_CONNECT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub enum Connector {
    Loopback(SocketAddr),
    Pipe(Arc<Mutex<mpsc::Receiver<BoxStream>>>),
}

impl fmt::Debug for Connector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback(addr) => write!(f, "Loopback({addr})"),
            Self::Pipe(_) => f.write_str("Pipe"),
        }
    }
}

impl Connector {
    /// A loopback TCP connector. Non-loopback addresses are refused: the
    /// runtime must never be reached over a real network interface.
    pub fn loopback(addr: SocketAddr) -> io::Result<Self> {
        if !addr.ip().is_loopback() || addr.port() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inference endpoint must be a loopback address",
            ));
        }
        Ok(Self::Loopback(addr))
    }

    /// A connector fed by an accept loop that sends verified streams.
    pub fn pipe(receiver: mpsc::Receiver<BoxStream>) -> Self {
        Self::Pipe(Arc::new(Mutex::new(receiver)))
    }

    /// Whether a failed stream may be replaced by another one (pre-connected
    /// streams can be stale if the relay restarted).
    pub(crate) fn retries_stale_streams(&self) -> bool {
        matches!(self, Self::Pipe(_))
    }

    pub async fn connect(&self) -> io::Result<BoxStream> {
        match self {
            Self::Loopback(addr) => {
                let stream =
                    tokio::time::timeout(TCP_CONNECT, tokio::net::TcpStream::connect(addr))
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "connect timed out")
                        })??;
                stream.set_nodelay(true)?;
                Ok(Box::new(stream))
            }
            Self::Pipe(receiver) => {
                let mut receiver = receiver.lock().await;
                match tokio::time::timeout(STREAM_WAIT, receiver.recv()).await {
                    Ok(Some(stream)) => Ok(stream),
                    Ok(None) => Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "inference relay is no longer connected",
                    )),
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "no connection from the inference relay",
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only() {
        assert!(Connector::loopback("127.0.0.1:8080".parse().unwrap()).is_ok());
        assert!(Connector::loopback("[::1]:8080".parse().unwrap()).is_ok());
        assert!(Connector::loopback("0.0.0.0:8080".parse().unwrap()).is_err());
        assert!(Connector::loopback("192.168.1.10:8080".parse().unwrap()).is_err());
        assert!(Connector::loopback("127.0.0.1:0".parse().unwrap()).is_err());
    }

    #[tokio::test]
    async fn pipe_connector_yields_streams_then_reports_disconnect() {
        let (tx, rx) = mpsc::channel::<BoxStream>(2);
        let connector = Connector::pipe(rx);
        let (a, _b) = tokio::io::duplex(64);
        tx.send(Box::new(a)).await.unwrap();
        assert!(connector.connect().await.is_ok());
        drop(tx);
        let err = connector.connect().await.err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::NotConnected);
    }
}
