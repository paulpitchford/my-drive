//! The Graph client for the read side: drives, items, delta and content download.
//!
//! It follows the ADR-0011 rules a client can enforce by itself, so the engine never
//! handles raw HTTP:
//!
//! - Every request carries the `ISV|<company>|<app>/<version>` User-Agent (R8).
//! - The client never sleeps or retries on throttling: `429` and `503` come back as
//!   [`GraphErrorKind::Throttled`] with `Retry-After`, for the tenant's scheduler (R6).
//!   RateLimit headers are not read (R7).
//! - A `401` causes exactly one forced token refresh through the [`TokenProvider`] and one
//!   retry; a provider error is returned as [`GraphErrorKind::Token`] and never retried.
//! - Downloads request `/content` with the bearer token, accept a redirect or direct
//!   content, and never send the bearer token to a redirect target on another origin
//!   (R10). Every ranged response's status is checked, and a `200` is reported as the
//!   whole file from offset 0 (R11).
//! - A body that ends before its announced length is an error, never a short read.
//! - The bearer token is only ever sent to the configured Graph origin.
//!
//! Nothing here logs a token, a URL or a response body.

mod download;
mod error;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use mydrive_core::ids::{DriveId, ItemId};
use reqwest::header::{self, HeaderMap};
use reqwest::{Response, StatusCode};
use serde_json::Value;
use url::Url;

use crate::auth::{AccessToken, TokenProvider};
use crate::model::{DeltaLink, DeltaPage, Drive, DriveItem};

pub use download::{ByteRange, Coverage, Download};
pub use error::{ErrorCodes, GraphError, GraphErrorKind, Operation, Resync};

/// Microsoft Graph v1.0, the production base URL.
pub const GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";

/// The company part of the default User-Agent. Provisional until the product name is
/// decided (open question Q21).
pub const DEFAULT_USER_AGENT_COMPANY: &str = "my-drive";

/// The app part of the default User-Agent. Provisional until the product name is decided
/// (open question Q21).
pub const DEFAULT_USER_AGENT_APP: &str = "mydrive";

/// How long establishing a connection may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a single read from an open connection may take before the request fails as a
/// transport error. It bounds a stalled connection; it does not bound a long download
/// that keeps making progress.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The largest JSON response body accepted. A delta page of a few hundred items is far
/// smaller; anything bigger is treated as malformed rather than held in memory.
pub const MAX_JSON_BODY: usize = 64 * 1024 * 1024;

/// The most error-body bytes read to find the Graph error code.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// The most redirects one download follows.
pub const MAX_REDIRECTS: usize = 5;

/// A client could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ClientBuildError {
    /// The base URL cannot be used. The URL itself is not echoed.
    #[error("invalid Graph base URL: {reason}")]
    InvalidBaseUrl {
        /// Why.
        reason: &'static str,
    },
    /// A User-Agent part cannot be used.
    #[error("invalid User-Agent: {reason}")]
    InvalidUserAgent {
        /// Why.
        reason: &'static str,
    },
    /// The HTTP client could not be created.
    #[error("cannot create the HTTP client: {0}")]
    Http(#[source] reqwest::Error),
}

/// The decorated User-Agent `ISV|<company>|<app>/<version>` that Graph throttling guidance
/// prioritises (ADR-0011 R8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAgent(String);

impl UserAgent {
    /// Builds `ISV|<company>|<app>/<version>`.
    ///
    /// # Errors
    ///
    /// [`ClientBuildError::InvalidUserAgent`] if a part is empty, contains `|` or a
    /// character outside printable ASCII, or if `app` or `version` contains `/`.
    pub fn new(company: &str, app: &str, version: &str) -> Result<Self, ClientBuildError> {
        fn check(part: &str, slash_allowed: bool) -> Result<(), ClientBuildError> {
            let bad = |reason| Err(ClientBuildError::InvalidUserAgent { reason });
            if part.trim().is_empty() {
                return bad("a part is empty");
            }
            if !part.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
                return bad("a part has a character outside printable ASCII");
            }
            if part.contains('|') {
                return bad("a part contains '|'");
            }
            if !slash_allowed && part.contains('/') {
                return bad("the app or version contains '/'");
            }
            Ok(())
        }
        check(company, true)?;
        check(app, false)?;
        check(version, false)?;
        Ok(Self(format!("ISV|{company}|{app}/{version}")))
    }

    /// The header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for UserAgent {
    /// `ISV|my-drive|mydrive/<crate version>`.
    fn default() -> Self {
        Self(format!(
            "ISV|{DEFAULT_USER_AGENT_COMPANY}|{DEFAULT_USER_AGENT_APP}/{}",
            env!("CARGO_PKG_VERSION")
        ))
    }
}

/// A Graph client for one account.
///
/// Cheap to clone; clones share the connection pool and the token provider.
#[derive(Clone)]
pub struct GraphClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// The Graph base URL without a trailing slash, for example
    /// `https://graph.microsoft.com/v1.0`.
    base: Url,
    user_agent: UserAgent,
    tokens: Arc<dyn TokenProvider>,
}

impl fmt::Debug for GraphClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphClient")
            .field("base", &self.inner.base.as_str())
            .field("user_agent", &self.inner.user_agent.as_str())
            .finish_non_exhaustive()
    }
}

/// Which side answered a request: the Graph origin, or another origin a download was
/// redirected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    Graph,
    Redirected,
}

fn transport(e: reqwest::Error) -> GraphErrorKind {
    GraphErrorKind::Transport(e.without_url())
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// `Retry-After` in delta-seconds. The HTTP-date form is not used by Graph's throttling
/// guidance and would need the wall clock, so it is reported as absent.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Collects `error.code` and every nested `innerError.code` (Graph uses both spellings
/// of `innerError`).
fn error_codes(body: &[u8]) -> ErrorCodes {
    let mut codes = Vec::new();
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        let mut level = value.get("error");
        while let Some(object) = level {
            if let Some(code) = object.get("code").and_then(Value::as_str) {
                codes.push(code.to_owned());
            }
            level = object
                .get("innerError")
                .or_else(|| object.get("innererror"));
        }
    }
    ErrorCodes::new(codes)
}

/// The announced `Content-Length`, if any.
fn content_length(headers: &HeaderMap) -> Result<Option<u64>, GraphErrorKind> {
    headers
        .get(header::CONTENT_LENGTH)
        .map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .ok_or_else(|| GraphErrorKind::Malformed("Content-Length is not a number".into()))
        })
        .transpose()
}

/// Reads a whole body of at most `limit` bytes, failing if it ends before its announced
/// length.
async fn read_body(mut response: Response, limit: usize) -> Result<Vec<u8>, GraphErrorKind> {
    let expected = content_length(response.headers())?;
    if expected.is_some_and(|n| n > limit as u64) {
        return Err(GraphErrorKind::Malformed(format!(
            "the body is larger than {limit} bytes"
        )));
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > limit {
                    return Err(GraphErrorKind::Malformed(format!(
                        "the body is larger than {limit} bytes"
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(match expected {
                    Some(expected) if (body.len() as u64) < expected => GraphErrorKind::Truncated {
                        expected,
                        received: body.len() as u64,
                    },
                    _ => transport(e),
                });
            }
        }
    }
    if let Some(expected) = expected
        && body.len() as u64 != expected
    {
        let received = body.len() as u64;
        return Err(if received < expected {
            GraphErrorKind::Truncated { expected, received }
        } else {
            GraphErrorKind::Malformed("the body is longer than its Content-Length".into())
        });
    }
    Ok(body)
}

impl GraphClient {
    /// A client for the Graph API at `base_url` (normally [`GRAPH_BASE_URL`]), getting
    /// access tokens from `tokens` and identifying itself with `user_agent`.
    ///
    /// The bearer token is only ever sent to `base_url`'s origin.
    ///
    /// # Errors
    ///
    /// [`ClientBuildError::InvalidBaseUrl`] if `base_url` is not an `http` or `https` URL
    /// without credentials, query or fragment; [`ClientBuildError::Http`] if the HTTP
    /// client cannot be created.
    pub fn new(
        base_url: &str,
        tokens: Arc<dyn TokenProvider>,
        user_agent: UserAgent,
    ) -> Result<Self, ClientBuildError> {
        let invalid = |reason| ClientBuildError::InvalidBaseUrl { reason };
        let mut base = Url::parse(base_url).map_err(|_| invalid("it is not a URL"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(invalid("the scheme is not http or https"));
        }
        if base.host_str().is_none() {
            return Err(invalid("it has no host"));
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(invalid("it carries credentials"));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(invalid("it has a query or fragment"));
        }
        let trimmed = base.path().trim_end_matches('/').to_owned();
        base.set_path(&trimmed);
        let http = reqwest::Client::builder()
            // Redirects are followed by hand, so the bearer token is never forwarded to
            // another origin (ADR-0011 R10).
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .user_agent(user_agent.as_str())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| ClientBuildError::Http(e.without_url()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base,
                user_agent,
                tokens,
            }),
        })
    }

    /// The Graph base URL, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// The User-Agent every request carries.
    #[must_use]
    pub fn user_agent(&self) -> &UserAgent {
        &self.inner.user_agent
    }

    /// `GET /drives/{drive}`.
    ///
    /// # Errors
    ///
    /// Any [`GraphErrorKind`] except the resync and download ones.
    pub async fn get_drive(&self, drive: &DriveId) -> Result<Drive, GraphError> {
        let op = Operation::GetDrive {
            drive: drive.clone(),
        };
        let url = self.endpoint(&["drives", drive.as_str()]);
        let body = self.get_json(&op, &url).await?;
        Drive::from_json(&body).map_err(|e| GraphError::new(op, e.into()))
    }

    /// `GET /drives/{drive}/root`: the root item.
    ///
    /// # Errors
    ///
    /// As for [`GraphClient::get_drive`].
    pub async fn get_root(&self, drive: &DriveId) -> Result<DriveItem, GraphError> {
        let op = Operation::GetRoot {
            drive: drive.clone(),
        };
        let url = self.endpoint(&["drives", drive.as_str(), "root"]);
        let body = self.get_json(&op, &url).await?;
        DriveItem::from_json(&body).map_err(|e| GraphError::new(op, e.into()))
    }

    /// `GET /drives/{drive}/items/{item}`.
    ///
    /// # Errors
    ///
    /// As for [`GraphClient::get_drive`].
    pub async fn get_item(&self, drive: &DriveId, item: &ItemId) -> Result<DriveItem, GraphError> {
        let op = Operation::GetItem {
            drive: drive.clone(),
            item: item.clone(),
        };
        let url = self.endpoint(&["drives", drive.as_str(), "items", item.as_str()]);
        let body = self.get_json(&op, &url).await?;
        DriveItem::from_json(&body).map_err(|e| GraphError::new(op, e.into()))
    }

    /// The first delta page of the drive from its root with no token, which enumerates
    /// the drive's current state. Follow [`DeltaPage::continuation`] with
    /// [`GraphClient::delta_from_link`] until a page carries a delta link.
    ///
    /// # Errors
    ///
    /// Any [`GraphErrorKind`] except the download ones; [`GraphErrorKind::ResyncRequired`]
    /// on `410 Gone`.
    pub async fn delta_from_root(&self, drive: &DriveId) -> Result<DeltaPage, GraphError> {
        let url = self.endpoint(&["drives", drive.as_str(), "root", "delta"]);
        self.delta_at(drive, &url).await
    }

    /// A delta page from a saved next link, delta link or resync restart link.
    ///
    /// # Errors
    ///
    /// As for [`GraphClient::delta_from_root`], and [`GraphErrorKind::ForeignLink`] if the
    /// link does not point at this client's Graph base URL (no request is made).
    pub async fn delta_from_link(
        &self,
        drive: &DriveId,
        link: &DeltaLink,
    ) -> Result<DeltaPage, GraphError> {
        let op = Operation::Delta {
            drive: drive.clone(),
        };
        let url = Url::parse(link.as_str())
            .ok()
            .filter(|url| self.is_own(url))
            .ok_or_else(|| GraphError::new(op, GraphErrorKind::ForeignLink))?;
        self.delta_at(drive, &url).await
    }

    async fn delta_at(&self, drive: &DriveId, url: &Url) -> Result<DeltaPage, GraphError> {
        let op = Operation::Delta {
            drive: drive.clone(),
        };
        let body = self.get_json(&op, url).await?;
        let page = DeltaPage::from_json(&body).map_err(|e| GraphError::new(op, e.into()))?;
        tracing::debug!(%drive, items = page.items.len(), "delta page received");
        Ok(page)
    }

    /// Downloads an item's content, or the byte range `range` of it.
    ///
    /// The request goes to `/content` with the bearer token. A redirect is followed; the
    /// bearer token goes with it only if the target is on the Graph origin (ADR-0011 R10).
    /// `Range` is sent to `/content` and to every redirect target. The returned
    /// [`Download`] says what is actually arriving: the range Graph confirmed with `206`,
    /// or the whole file from offset 0 for a `200`, whether or not a range was asked for
    /// (ADR-0011 R11).
    ///
    /// # Errors
    ///
    /// Any [`GraphErrorKind`] except the resync one. A `206` that does not match the
    /// request is [`GraphErrorKind::Malformed`].
    pub async fn download(
        &self,
        drive: &DriveId,
        item: &ItemId,
        range: Option<ByteRange>,
    ) -> Result<Download, GraphError> {
        let op = Operation::Download {
            drive: drive.clone(),
            item: item.clone(),
        };
        let url = self.endpoint(&["drives", drive.as_str(), "items", item.as_str(), "content"]);
        match self.download_at(&op, url, range).await {
            Ok(response) => Download::new(op, response, range),
            Err(kind) => Err(GraphError::new(op, kind)),
        }
    }

    async fn download_at(
        &self,
        op: &Operation,
        url: Url,
        range: Option<ByteRange>,
    ) -> Result<Response, GraphErrorKind> {
        let mut current = url;
        // Once a redirect leaves the Graph origin, the token is never sent again on this
        // download, even if a later hop comes back.
        let mut bearer = true;
        let mut response = self.send_authed(op, &current, range).await?;
        let mut redirects = 0;
        loop {
            if !response.status().is_redirection() {
                let host = if bearer {
                    Host::Graph
                } else {
                    Host::Redirected
                };
                return if matches!(
                    response.status(),
                    StatusCode::OK | StatusCode::PARTIAL_CONTENT
                ) {
                    Ok(response)
                } else {
                    Err(self.status_error(op, response, host).await)
                };
            }
            if redirects == MAX_REDIRECTS {
                return Err(GraphErrorKind::Malformed(format!(
                    "more than {MAX_REDIRECTS} redirects"
                )));
            }
            redirects += 1;
            let next = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|location| current.join(location).ok())
                .ok_or_else(|| {
                    GraphErrorKind::Malformed("a redirect without a usable Location".into())
                })?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(GraphErrorKind::Malformed(
                    "a redirect to a scheme other than http or https".into(),
                ));
            }
            if current.scheme() == "https" && next.scheme() == "http" {
                return Err(GraphErrorKind::Malformed(
                    "a redirect from https to http".into(),
                ));
            }
            bearer = bearer && self.is_own(&next);
            tracing::debug!(
                operation = %op,
                status = response.status().as_u16(),
                bearer_forwarded = bearer,
                "following download redirect"
            );
            drop(response);
            current = next;
            response = if bearer {
                self.send_authed(op, &current, range).await?
            } else {
                self.send_once(&current, None, range).await?
            };
        }
    }

    /// `base` + `/` + each segment, percent-encoded as path segments.
    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.inner.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        url
    }

    /// Whether `url` is on the Graph origin and under the base path, so it may receive
    /// the bearer token.
    fn is_own(&self, url: &Url) -> bool {
        let base = &self.inner.base;
        let base_path = base.path();
        same_origin(url, base)
            && url.username().is_empty()
            && url.password().is_none()
            && (url.path() == base_path
                || url
                    .path()
                    .strip_prefix(base_path)
                    .is_some_and(|rest| rest.starts_with('/')))
    }

    async fn send_once(
        &self,
        url: &Url,
        token: Option<&AccessToken>,
        range: Option<ByteRange>,
    ) -> Result<Response, GraphErrorKind> {
        let mut request = self.inner.http.get(url.clone());
        if let Some(token) = token {
            request = request.bearer_auth(token.expose_secret());
        }
        if let Some(range) = range {
            request = request.header(header::RANGE, range.header_value());
        }
        request.send().await.map_err(transport)
    }

    /// Sends a GET with the bearer token. On `401` it forces one refresh of that token and
    /// sends once more; whatever the second answer is, it is returned. A token provider
    /// failure is returned at once.
    async fn send_authed(
        &self,
        op: &Operation,
        url: &Url,
        range: Option<ByteRange>,
    ) -> Result<Response, GraphErrorKind> {
        let token = self
            .inner
            .tokens
            .access_token()
            .await
            .map_err(GraphErrorKind::Token)?;
        let response = self.send_once(url, Some(&token), range).await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        drop(response);
        tracing::debug!(operation = %op, "Graph answered 401; forcing one token refresh");
        let fresh = self
            .inner
            .tokens
            .force_refresh(&token)
            .await
            .map_err(GraphErrorKind::Token)?;
        self.send_once(url, Some(&fresh), range).await
    }

    async fn get_json(&self, op: &Operation, url: &Url) -> Result<Vec<u8>, GraphError> {
        let result = async {
            let response = self.send_authed(op, url, None).await?;
            let status = response.status();
            tracing::debug!(operation = %op, status = status.as_u16(), "graph response");
            if status != StatusCode::OK {
                return Err(self.status_error(op, response, Host::Graph).await);
            }
            read_body(response, MAX_JSON_BODY).await
        }
        .await;
        result.map_err(|kind| GraphError::new(op.clone(), kind))
    }

    /// Maps a response that is not a success to its error.
    async fn status_error(&self, op: &Operation, response: Response, host: Host) -> GraphErrorKind {
        let status = response.status().as_u16();
        let retry_after = retry_after(response.headers());
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|location| response.url().join(location).ok());
        let body = read_body(response, MAX_ERROR_BODY)
            .await
            .unwrap_or_default();
        let codes = error_codes(&body);
        tracing::debug!(operation = %op, status, %codes, "graph request failed");
        match (status, host) {
            (401, Host::Graph) => GraphErrorKind::Unauthorized { codes },
            (401, Host::Redirected) => GraphErrorKind::DownloadUrlRejected { status },
            (403, _) => GraphErrorKind::Forbidden { codes },
            (404, _) => GraphErrorKind::NotFound { codes },
            (410, Host::Graph) if matches!(op, Operation::Delta { .. }) => {
                let resync = if codes.contains("resyncChangesApplyDifferences") {
                    Resync::ApplyDifferences
                } else if codes.contains("resyncChangesUploadDifferences") {
                    Resync::UploadDifferences
                } else {
                    Resync::Unknown { codes }
                };
                let restart = match location {
                    Some(url) if self.is_own(&url) => Some(DeltaLink::new(url.as_str())),
                    Some(_) => {
                        tracing::warn!(
                            operation = %op,
                            "ignoring a 410 restart link that is not on the Graph origin"
                        );
                        None
                    }
                    None => None,
                };
                GraphErrorKind::ResyncRequired { resync, restart }
            }
            (429 | 503, _) => GraphErrorKind::Throttled {
                status,
                retry_after,
            },
            (500..=599, _) => GraphErrorKind::ServerError { status, codes },
            _ => GraphErrorKind::UnexpectedStatus { status, codes },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_follow_inner_errors() {
        let codes = error_codes(
            br#"{"error":{"code":"accessDenied","innerError":{"code":"a","innererror":{"code":"b"}}}}"#,
        );
        assert_eq!(codes.all(), ["accessDenied", "a", "b"]);
        assert_eq!(codes.primary(), Some("accessDenied"));
        assert!(error_codes(b"<html>").all().is_empty());
    }

    /// ADR-0011 R8: the User-Agent has the decorated form.
    #[test]
    fn adr_0011_r8_user_agent_format() {
        assert_eq!(
            UserAgent::new("Example Co", "app", "1.2.3")
                .unwrap()
                .as_str(),
            "ISV|Example Co|app/1.2.3"
        );
        let default = UserAgent::default();
        assert!(default.as_str().starts_with("ISV|my-drive|mydrive/"));
        for (c, a, v) in [
            ("", "a", "1"),
            ("c|d", "a", "1"),
            ("c", "a/b", "1"),
            ("c", "a", "1/2"),
            ("c", "a", "1\n"),
            ("c", "é", "1"),
        ] {
            assert!(UserAgent::new(c, a, v).is_err(), "{c:?} {a:?} {v:?}");
        }
    }

    #[test]
    fn retry_after_takes_delta_seconds_only() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
        headers.insert(
            header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
    }
}
