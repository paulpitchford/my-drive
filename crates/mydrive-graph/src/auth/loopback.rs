//! The loopback sign-in (F01, J01): listen on `127.0.0.1` on an ephemeral port, accept the
//! one redirect whose `state` matches, show a short page, and stop listening
//! (ADR-0010 R2).
//!
//! The listener speaks just enough HTTP/1.1 for one browser redirect. It never binds any
//! address other than `127.0.0.1`. The redirect URI sent to the authority is
//! `http://localhost:<port>`, as ADR-0010 specifies; browsers resolve `localhost` to the
//! loopback interface.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use url::Url;

use super::attempt::{ATTEMPT_LIFETIME, PendingSignIn, RedirectUri};
use super::config::{AuthConfig, Scopes};
use super::error::SignInError;
use super::secret::AuthorizationCode;
use super::token::{TokenClient, TokenSet};

/// How long the loopback flow waits for the redirect by default (F01: 10 minutes).
pub const LOOPBACK_TIMEOUT: Duration = ATTEMPT_LIFETIME;

/// The longest wait accepted by [`LoopbackSignIn::wait_for_code`].
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The largest request head read from one connection.
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// How long one connection may take to send its request head.
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(10);

const PAGE_DONE: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Sign-in is complete. You can close this tab.</p>";
const PAGE_FAILED: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Sign-in was not completed. See the terminal for details. You can close this tab.</p>";
const PAGE_WRONG_ATTEMPT: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>This sign-in response does not belong to this attempt. Start the sign-in again.</p>";
const PAGE_BAD_REQUEST: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Bad request.</p>";

/// A loopback sign-in attempt with its listener open.
pub struct LoopbackSignIn {
    listener: TcpListener,
    local_addr: SocketAddr,
    pending: PendingSignIn,
}

impl fmt::Debug for LoopbackSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackSignIn")
            .field("local_addr", &self.local_addr)
            .field("pending", &self.pending)
            .finish()
    }
}

/// What one connection amounted to.
enum Connection {
    /// Not the redirect: answered with 400 and ignored.
    Ignored,
    /// The redirect: the attempt ends with this result.
    Finished(Result<AuthorizationCode, SignInError>),
}

impl LoopbackSignIn {
    /// Binds `127.0.0.1` on a port the kernel picks and prepares the authorisation URL.
    ///
    /// # Errors
    ///
    /// [`SignInError::Bind`] if no loopback port can be bound, and
    /// [`SignInError::Config`] if the authority's endpoint cannot be built.
    pub async fn start(config: &AuthConfig, scopes: Scopes) -> Result<Self, SignInError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|source| SignInError::Bind { source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| SignInError::Bind { source })?;
        let redirect = RedirectUri::parse(&format!("http://localhost:{}", local_addr.port()))?;
        let pending = PendingSignIn::new(config, redirect, scopes)?;
        tracing::info!(port = local_addr.port(), "waiting for the sign-in redirect");
        Ok(Self {
            listener,
            local_addr,
            pending,
        })
    }

    /// The address the listener is bound to; always `127.0.0.1:<port>`.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The URL the user opens in their browser.
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        self.pending.authorization_url()
    }

    /// The redirect URI, `http://localhost:<port>`.
    #[must_use]
    pub fn redirect_uri(&self) -> &RedirectUri {
        self.pending.redirect_uri()
    }

    /// Waits for the redirect, then redeems its code. The listener is closed before the
    /// token request is made.
    ///
    /// # Errors
    ///
    /// As [`LoopbackSignIn::wait_for_code`] and [`PendingSignIn::redeem`].
    pub async fn complete(
        self,
        client: &TokenClient,
        timeout: Duration,
    ) -> Result<TokenSet, SignInError> {
        let (code, pending) = self.wait_for_code(timeout).await?;
        pending.redeem(code, client).await
    }

    /// Accepts connections until the redirect arrives or `timeout` passes, then stops
    /// listening. Requests that are not the redirect get a 400 and are ignored. A redirect
    /// whose `state` is missing or wrong cancels the attempt (F01).
    ///
    /// # Errors
    ///
    /// [`SignInError::TimedOut`], [`SignInError::StateMismatch`],
    /// [`SignInError::Authorization`] for an error redirect such as `access_denied`, or
    /// [`SignInError::Listener`] if accepting fails.
    pub async fn wait_for_code(
        self,
        timeout: Duration,
    ) -> Result<(AuthorizationCode, PendingSignIn), SignInError> {
        // Clamped so an absurd timeout cannot overflow the clock.
        let deadline = Instant::now() + timeout.min(MAX_TIMEOUT);
        let Self {
            listener, pending, ..
        } = self;
        let result = loop {
            let accepted = tokio::time::timeout_at(deadline, listener.accept()).await;
            let (stream, peer) = match accepted {
                Err(_) => break Err(SignInError::TimedOut { after: timeout }),
                Ok(Err(source)) => break Err(SignInError::Listener { source }),
                Ok(Ok(conn)) => conn,
            };
            if !peer.ip().is_loopback() {
                // Unreachable while bound to 127.0.0.1; kept as a second line of defence.
                continue;
            }
            match handle_connection(stream, &pending, deadline).await {
                Connection::Ignored => {}
                Connection::Finished(result) => break result,
            }
        };
        drop(listener);
        match &result {
            Ok(_) => tracing::info!("sign-in redirect received; listener closed"),
            Err(e) => tracing::warn!(error = %e, "sign-in redirect not accepted; listener closed"),
        }
        result.map(|code| (code, pending))
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    pending: &PendingSignIn,
    deadline: Instant,
) -> Connection {
    let read_deadline = deadline.min(Instant::now() + CONNECTION_READ_TIMEOUT);
    let target =
        match tokio::time::timeout_at(read_deadline, read_request_target(&mut stream)).await {
            Ok(Some(target)) => target,
            Ok(None) | Err(_) => {
                respond(&mut stream, "400 Bad Request", PAGE_BAD_REQUEST).await;
                return Connection::Ignored;
            }
        };
    let redirect = pending.redirect_uri();
    let url = match Url::parse(redirect.as_str()).and_then(|base| base.join(&target)) {
        Ok(url) if target.starts_with('/') && redirect.is_target_of(&url) => url,
        _ => {
            respond(&mut stream, "400 Bad Request", PAGE_BAD_REQUEST).await;
            return Connection::Ignored;
        }
    };
    let is_redirect = url
        .query_pairs()
        .any(|(k, _)| matches!(k.as_ref(), "code" | "state" | "error"));
    if !is_redirect {
        respond(&mut stream, "400 Bad Request", PAGE_BAD_REQUEST).await;
        return Connection::Ignored;
    }
    match pending.accept_redirect(&url) {
        Ok(code) => {
            respond(&mut stream, "200 OK", PAGE_DONE).await;
            Connection::Finished(Ok(code))
        }
        Err(e @ SignInError::StateMismatch) => {
            respond(&mut stream, "400 Bad Request", PAGE_WRONG_ATTEMPT).await;
            Connection::Finished(Err(e))
        }
        Err(e @ SignInError::Authorization { .. }) => {
            respond(&mut stream, "200 OK", PAGE_FAILED).await;
            Connection::Finished(Err(e))
        }
        Err(_) => {
            respond(&mut stream, "400 Bad Request", PAGE_BAD_REQUEST).await;
            Connection::Ignored
        }
    }
}

/// Reads the request head and returns the target of a `GET`, or `None` for anything else.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > MAX_REQUEST_HEAD {
            return None;
        }
    }
    let head = std::str::from_utf8(&head).ok()?;
    let line = head.split("\r\n").next()?;
    let mut parts = line.split(' ');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("GET"), Some(target), Some(version), None) if version.starts_with("HTTP/1.") => {
            Some(target.to_owned())
        }
        _ => None,
    }
}

/// Writes a complete response and closes the connection. Write errors are ignored: the
/// browser may already have gone, and the outcome of the attempt does not depend on it.
async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}
