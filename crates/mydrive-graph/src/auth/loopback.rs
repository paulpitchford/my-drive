//! The loopback sign-in (F01, J01): listen on the loopback interface on an ephemeral port,
//! accept the one redirect whose `state` matches, show a short page, and stop listening
//! (ADR-0010 R2).
//!
//! The listener binds `127.0.0.1` and, when IPv6 loopback is available, `[::1]` on the
//! same port, so no other local process can claim the IPv6 side of `localhost`. It never
//! binds any other address. The redirect URI is `http://<host>:<port>`, where the host is
//! [`AuthConfig::loopback_host`] (default `localhost`, as ADR-0010 specifies).
//!
//! Connections are handled concurrently, each with a short read deadline, so an idle or
//! hostile connection cannot delay the real redirect. A redirect with a wrong or missing
//! `state` gets a 400 and is otherwise ignored: the listener keeps waiting for the real one
//! until the timeout, so a local process or web page cannot cancel a sign-in.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;
use url::Url;

use super::attempt::{ATTEMPT_LIFETIME, PendingSignIn, RedirectUri};
use super::config::{AuthConfig, Scopes};
use super::error::SignInError;
use super::secret::AuthorizationCode;
use super::token::{TokenClient, TokenSet};

/// How long the loopback flow waits for the redirect by default (F01: 10 minutes).
pub const LOOPBACK_TIMEOUT: Duration = ATTEMPT_LIFETIME;

/// How long one connection may take to send its request head.
pub const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest wait accepted by [`LoopbackSignIn::wait_for_code`].
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The largest request head read from one connection.
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// How many ports to try before giving up on finding one free on both loopback addresses.
const BIND_ATTEMPTS: usize = 16;

const PAGE_DONE: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Sign-in is complete. You can close this tab.</p>";
const PAGE_FAILED: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Sign-in was not completed. See the terminal for details. You can close this tab.</p>";
const PAGE_WRONG_ATTEMPT: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>This sign-in response does not belong to this attempt.</p>";
const PAGE_BAD_REQUEST: &str = "<!doctype html><meta charset=\"utf-8\"><title>my-drive</title>\
<p>Bad request.</p>";

/// A loopback sign-in attempt with its listeners open.
pub struct LoopbackSignIn {
    v4: TcpListener,
    v6: Option<TcpListener>,
    local_addrs: Vec<SocketAddr>,
    pending: PendingSignIn,
}

impl fmt::Debug for LoopbackSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackSignIn")
            .field("local_addrs", &self.local_addrs)
            .field("pending", &self.pending)
            .finish()
    }
}

/// What a request amounted to.
enum Verdict {
    /// Not the redirect: answer 400 with this page and keep waiting.
    Ignore(&'static str),
    /// The redirect: answer with this status and page, and end the attempt.
    Finish(
        &'static str,
        &'static str,
        Result<AuthorizationCode, SignInError>,
    ),
}

/// Binds `127.0.0.1` and, if available, `[::1]` on the same port, trying each port from
/// `ports` (0 means "kernel's choice"). A port taken on `::1` by someone else is given up
/// and another tried; if `::1` cannot be bound for any other reason (IPv6 disabled), the
/// IPv4 listener is used alone.
pub(crate) async fn bind_loopback(
    ports: impl IntoIterator<Item = u16>,
) -> io::Result<(TcpListener, Option<TcpListener>)> {
    let mut last = None;
    for port in ports {
        let v4 = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(l) => l,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let port = v4.local_addr()?.port();
        match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
            Ok(v6) => return Ok((v4, Some(v6))),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                tracing::debug!(port, "port taken on ::1; trying another");
                last = Some(e);
            }
            Err(e) => {
                tracing::info!(error = %e, "IPv6 loopback unavailable; listening on 127.0.0.1 only");
                return Ok((v4, None));
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrInUse,
            "no port is free on both 127.0.0.1 and ::1",
        )
    }))
}

impl LoopbackSignIn {
    /// Binds the loopback interface on a port the kernel picks and prepares the
    /// authorisation URL.
    ///
    /// # Errors
    ///
    /// [`SignInError::Bind`] if no loopback port can be bound, and
    /// [`SignInError::Config`] if the authority's endpoint cannot be built.
    pub async fn start(config: &AuthConfig, scopes: Scopes) -> Result<Self, SignInError> {
        let (v4, v6) = bind_loopback(std::iter::repeat_n(0, BIND_ATTEMPTS))
            .await
            .map_err(|source| SignInError::Bind { source })?;
        let mut local_addrs = vec![
            v4.local_addr()
                .map_err(|source| SignInError::Bind { source })?,
        ];
        if let Some(v6) = &v6 {
            local_addrs.push(
                v6.local_addr()
                    .map_err(|source| SignInError::Bind { source })?,
            );
        }
        let port = local_addrs[0].port();
        let redirect = RedirectUri::parse(&format!("http://{}:{port}", config.loopback_host()))?;
        let pending = PendingSignIn::new(config, redirect, scopes)?;
        tracing::info!(
            port,
            ipv6 = v6.is_some(),
            "waiting for the sign-in redirect"
        );
        Ok(Self {
            v4,
            v6,
            local_addrs,
            pending,
        })
    }

    /// The IPv4 address the listener is bound to: always `127.0.0.1:<port>`.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addrs[0]
    }

    /// Every address listened on: `127.0.0.1:<port>`, and `[::1]:<port>` when available.
    #[must_use]
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.local_addrs
    }

    /// The URL the user opens in their browser. See [`PendingSignIn::authorization_url`]
    /// for what it reveals.
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        self.pending.authorization_url()
    }

    /// The redirect URI, `http://<loopback host>:<port>`.
    #[must_use]
    pub fn redirect_uri(&self) -> &RedirectUri {
        self.pending.redirect_uri()
    }

    /// Waits for the redirect, then redeems its code. The listeners are closed before the
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

    /// Accepts connections until the redirect with the matching `state` arrives or
    /// `timeout` passes, then stops listening. Anything else gets a 400 and is ignored,
    /// including a redirect with a wrong or missing `state`.
    ///
    /// # Errors
    ///
    /// [`SignInError::TimedOut`], [`SignInError::Authorization`] for an error redirect
    /// with the matching `state` (such as `access_denied`), or [`SignInError::Listener`]
    /// if accepting fails.
    pub async fn wait_for_code(
        self,
        timeout: Duration,
    ) -> Result<(AuthorizationCode, PendingSignIn), SignInError> {
        // Clamped so an absurd timeout cannot overflow the clock.
        let deadline = Instant::now() + timeout.min(MAX_TIMEOUT);
        let Self {
            v4, v6, pending, ..
        } = self;
        let (heads_tx, mut heads) = mpsc::channel::<(TcpStream, Option<String>)>(16);
        let result = loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    break Err(SignInError::TimedOut { after: timeout });
                }
                accepted = accept_any(&v4, v6.as_ref()) => match accepted {
                    Err(source) => break Err(SignInError::Listener { source }),
                    Ok((stream, peer)) => {
                        if !peer.ip().is_loopback() {
                            // Unreachable while bound to loopback; a second line of defence.
                            continue;
                        }
                        let tx = heads_tx.clone();
                        tokio::spawn(async move {
                            let mut stream = stream;
                            let target = tokio::time::timeout(
                                CONNECTION_READ_TIMEOUT,
                                read_request_target(&mut stream),
                            )
                            .await
                            .ok()
                            .flatten();
                            let _ = tx.send((stream, target)).await;
                        });
                    }
                },
                Some((stream, target)) = heads.recv() => {
                    match judge(&pending, target) {
                        Verdict::Ignore(page) => {
                            tokio::spawn(respond(stream, "400 Bad Request", page));
                        }
                        Verdict::Finish(status, page, result) => {
                            let _ = tokio::time::timeout(
                                CONNECTION_READ_TIMEOUT,
                                respond(stream, status, page),
                            )
                            .await;
                            break result;
                        }
                    }
                }
            }
        };
        drop(v4);
        drop(v6);
        match &result {
            Ok(_) => tracing::info!("sign-in redirect received; listener closed"),
            Err(e) => tracing::warn!(error = %e, "sign-in did not complete; listener closed"),
        }
        result.map(|code| (code, pending))
    }
}

async fn accept_any(
    v4: &TcpListener,
    v6: Option<&TcpListener>,
) -> io::Result<(TcpStream, SocketAddr)> {
    match v6 {
        None => v4.accept().await,
        Some(v6) => tokio::select! {
            r = v4.accept() => r,
            r = v6.accept() => r,
        },
    }
}

/// Decides what a request target means for this attempt.
fn judge(pending: &PendingSignIn, target: Option<String>) -> Verdict {
    let Some(target) = target else {
        return Verdict::Ignore(PAGE_BAD_REQUEST);
    };
    let redirect = pending.redirect_uri();
    let url = match Url::parse(redirect.as_str()).and_then(|base| base.join(&target)) {
        Ok(url) if target.starts_with('/') && redirect.is_target_of(&url) => url,
        _ => return Verdict::Ignore(PAGE_BAD_REQUEST),
    };
    let is_redirect = url
        .query_pairs()
        .any(|(k, _)| matches!(k.as_ref(), "code" | "state" | "error"));
    if !is_redirect {
        return Verdict::Ignore(PAGE_BAD_REQUEST);
    }
    match pending.accept_redirect(&url) {
        Ok(code) => Verdict::Finish("200 OK", PAGE_DONE, Ok(code)),
        Err(SignInError::StateMismatch) => {
            tracing::warn!("ignored a sign-in redirect whose state does not match");
            Verdict::Ignore(PAGE_WRONG_ATTEMPT)
        }
        Err(e @ SignInError::Authorization { .. }) => {
            Verdict::Finish("200 OK", PAGE_FAILED, Err(e))
        }
        Err(_) => Verdict::Ignore(PAGE_BAD_REQUEST),
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
async fn respond(mut stream: TcpStream, status: &'static str, body: &'static str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review item 5: if the port is taken on `::1` by another process, another port is
    /// chosen so both loopback addresses are ours.
    #[tokio::test]
    async fn port_taken_on_ipv6_is_skipped() -> io::Result<()> {
        let squatter = match TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("IPv6 loopback unavailable here ({e}); nothing to check");
                return Ok(());
            }
        };
        let taken = squatter.local_addr()?.port();
        let (v4, v6) = bind_loopback([taken, 0]).await?;
        let v6 = v6.ok_or_else(|| io::Error::other("::1 was available but not bound"))?;
        assert_ne!(v4.local_addr()?.port(), taken);
        assert_eq!(v4.local_addr()?.port(), v6.local_addr()?.port());
        assert!(v6.local_addr()?.ip().is_loopback());
        Ok(())
    }
}
