//! Connections that can be cut off part-way through a response body.
//!
//! A truncated-body fault has to put the response head and some body bytes on the wire and
//! then close the connection, as a dropped download looks to a client. Inside hyper a body
//! that ends early is an error raised before the buffered bytes are flushed, so nothing
//! would reach the client. Instead the fault arms a [`ConnTap`] on the request's
//! connection, and the connection's writer lets through the head plus the allowed number
//! of body bytes, then fails every later write, which makes hyper close the socket.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

const HEAD_END: &[u8; 4] = b"\r\n\r\n";

/// Progress of an armed truncation through the next response on the connection.
#[derive(Debug, Clone, Copy)]
struct Truncation {
    /// Body bytes allowed through.
    keep: usize,
    /// How many bytes of `HEAD_END` the last bytes written matched.
    head_matched: usize,
    /// Whether the head has been written in full.
    in_body: bool,
    /// Body bytes written so far.
    body_sent: usize,
}

impl Truncation {
    /// Advances over one written byte. Returns `false` if the byte must not be written.
    fn step(&mut self, byte: u8) -> bool {
        if self.in_body {
            if self.body_sent < self.keep {
                self.body_sent += 1;
                return true;
            }
            return false;
        }
        self.head_matched = if byte == HEAD_END[self.head_matched] {
            self.head_matched + 1
        } else if byte == HEAD_END[0] {
            1
        } else {
            0
        };
        if self.head_matched == HEAD_END.len() {
            self.in_body = true;
        }
        true
    }
}

/// A handle on one connection, given to handlers as connect info.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnTap(Arc<Mutex<Option<Truncation>>>);

impl ConnTap {
    /// Cuts the next response on this connection off after `keep` body bytes.
    pub(crate) fn truncate_next_response(&self, keep: usize) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Truncation {
            keep,
            head_matched: 0,
            in_body: false,
            body_sent: 0,
        });
    }
}

impl Connected<IncomingStream<'_, TapListener>> for ConnTap {
    fn connect_info(stream: IncomingStream<'_, TapListener>) -> Self {
        stream.remote_addr().clone()
    }
}

/// A TCP listener whose connections carry a [`ConnTap`].
#[derive(Debug)]
pub(crate) struct TapListener(pub(crate) TcpListener);

impl Listener for TapListener {
    type Io = TapStream;
    type Addr = ConnTap;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, _)) => {
                    let tap = ConnTap::default();
                    let io = TapStream {
                        inner: stream,
                        tap: tap.clone(),
                    };
                    return (io, tap);
                }
                // Accept errors (such as running out of file descriptors) are transient;
                // retry shortly rather than stop serving.
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(ConnTap::default())
    }
}

/// A TCP stream that obeys its [`ConnTap`] when writing.
#[derive(Debug)]
pub(crate) struct TapStream {
    inner: TcpStream,
    tap: ConnTap,
}

impl AsyncRead for TapStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TapStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let tap = self.tap.clone();
        let mut armed = tap.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(truncation) = armed.as_mut() else {
            drop(armed);
            return Pin::new(&mut self.inner).poll_write(cx, buf);
        };
        let mut probe = *truncation;
        let allowed = buf.iter().take_while(|&&byte| probe.step(byte)).count();
        if allowed == 0 && !buf.is_empty() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted fault: response body truncated",
            )));
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, &buf[..allowed]);
        if let Poll::Ready(Ok(written)) = &result {
            for &byte in &buf[..*written] {
                truncation.step(byte);
            }
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_lets_the_head_and_keep_bytes_through() {
        let mut t = Truncation {
            keep: 3,
            head_matched: 0,
            in_body: false,
            body_sent: 0,
        };
        let wire = b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n0123456789";
        let allowed = wire.iter().take_while(|&&b| t.step(b)).count();
        assert_eq!(
            &wire[..allowed],
            b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n012"
        );
    }

    #[test]
    fn partial_head_end_matches_reset() {
        let mut t = Truncation {
            keep: 0,
            head_matched: 0,
            in_body: false,
            body_sent: 0,
        };
        for &b in b"a\r\n\rb\r\r\n\r\n" {
            assert!(t.step(b));
        }
        assert!(t.in_body);
        assert!(!t.step(b'x'));
    }
}
