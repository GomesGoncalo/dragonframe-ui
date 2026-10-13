//! Listening for plain http, and turning https away the way a closed door does.
//!
//! A phone's browser often tries https first, even on an address it was given
//! as http, and only falls back to http if that attempt looks like there is
//! nothing to connect to. This server has no certificate a phone would trust,
//! so the kindest answer to an https attempt is none at all: the connection is
//! reset the moment its first byte shows it to be the start of a TLS handshake.
//! (Answering with an http error instead makes the browser report a broken
//! secure connection and stop.)

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tracing::debug;

/// The first byte of every TLS connection: a handshake record. No http request starts with it.
const TLS_HANDSHAKE: u8 = 0x16;

/// A listener whose connections are dropped if they turn out to speak TLS.
pub struct PlainHttp(pub TcpListener);

/// One accepted connection, not yet known to be http until its first byte has been seen.
pub struct Connection {
    stream: TcpStream,
    seen_first_byte: bool,
}

/// Where a request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer(pub SocketAddr);

impl Connected<IncomingStream<'_, PlainHttp>> for Peer {
    fn connect_info(stream: IncomingStream<'_, PlainHttp>) -> Self {
        Peer(*stream.remote_addr())
    }
}

impl Listener for PlainHttp {
    type Io = Connection;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Connection, SocketAddr) {
        let (stream, peer) = Listener::accept(&mut self.0).await;
        (Connection { stream, seen_first_byte: false }, peer)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

impl AsyncRead for Connection {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let read = Pin::new(&mut self.stream).poll_read(cx, buf);
        if self.seen_first_byte || !matches!(read, Poll::Ready(Ok(()))) || buf.filled().len() == before {
            return read;
        }
        self.seen_first_byte = true;
        if buf.filled()[before] != TLS_HANDSHAKE {
            return read;
        }
        // Closing with nothing left to linger over sends a reset rather than a tidy goodbye.
        if let Err(e) = socket2::SockRef::from(&self.stream).set_linger(Some(Duration::ZERO)) {
            debug!(error = %e, "could not arrange for an https attempt to be reset");
        }
        debug!("an https attempt on the http port was turned away");
        Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted, "https is not spoken here")))
    }
}

impl AsyncWrite for Connection {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        parts: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, parts)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::routing::get;
    use axum::Router;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A server that says who called, listening the way the real one does.
    async fn server() -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(|ConnectInfo(peer): ConnectInfo<Peer>| async move { format!("hello {}", peer.0.ip()) }));
        tokio::spawn(async move {
            axum::serve(PlainHttp(listener), app.into_make_service_with_connect_info::<Peer>()).await
        });
        address
    }

    #[tokio::test]
    async fn plain_http_is_served_and_knows_who_called() {
        let address = server().await;
        let reply = reqwest::get(format!("http://{address}/")).await.unwrap();
        assert_eq!(reply.text().await.unwrap(), "hello 127.0.0.1");
    }

    #[tokio::test]
    async fn an_https_attempt_gets_no_answer_at_all() {
        let address = server().await;
        let mut phone = TcpStream::connect(address).await.unwrap();
        // The opening of a TLS 1.2 ClientHello.
        phone.write_all(&[TLS_HANDSHAKE, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03]).await.unwrap();
        let mut answer = Vec::new();
        let ended = phone.read_to_end(&mut answer).await;
        assert!(answer.is_empty(), "not one byte comes back: {:?}", String::from_utf8_lossy(&answer));
        assert_eq!(
            ended.unwrap_err().kind(),
            io::ErrorKind::ConnectionReset,
            "and the connection is reset, not closed politely"
        );
        // The server is none the worse for it.
        assert!(reqwest::get(format!("http://{address}/")).await.unwrap().status().is_success());
    }

    #[tokio::test]
    async fn anything_else_is_left_to_the_http_server_to_answer() {
        let address = server().await;
        let mut caller = TcpStream::connect(address).await.unwrap();
        caller.write_all(b"\x00\x01 nonsense\r\n\r\n").await.unwrap();
        let mut answer = Vec::new();
        let _ = caller.read_to_end(&mut answer).await;
        assert!(String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 400"), "{:?}", String::from_utf8_lossy(&answer));
    }
}
