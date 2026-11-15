//! The request log tests assert on: its types, the middleware that records every request
//! on every origin, and the control API that reads and clears it. Documented in
//! `docs/faults.md`; what the log masks is documented in `docs/identity.md`.

use std::borrow::Cow;

use axum::extract::{Request, State as AxState};
use axum::middleware::Next;
use axum::response::Response;

use crate::GraphSim;
use crate::server::Ctx;

/// Which listener a request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The Graph API origin, `GraphSim::base_url`.
    Graph,
    /// The separate origin that serves pre-authenticated download URLs, standing in for a
    /// storage host such as `*.files.1drv.com`.
    Download,
    /// The fake sign-in authority's listener, `GraphSim::authority_url`, standing in for
    /// `login.microsoftonline.com`.
    Authority,
}

/// The request headers the log records, lowercase. Others are not kept.
pub const LOGGED_HEADERS: &[&str] = &[
    "authorization",
    "range",
    "if-match",
    "if-none-match",
    "user-agent",
    "prefer",
    "deltaexcludeparent",
];

/// One request the simulator received, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    /// The listener it arrived on.
    pub origin: Origin,
    /// The HTTP method, for example `GET`.
    pub method: String,
    /// The path, without the query string, exactly as sent (still percent-encoded), except
    /// that a secret the fake authority issued is masked.
    pub path: String,
    /// The query string without the leading `?`, if there was one, with the same masking
    /// as the headers; on the authority's origin the values of the parameters in
    /// [`MASKED_AUTHORITY_PARAMS`] are masked too.
    pub query: Option<String>,
    /// The headers named in [`LOGGED_HEADERS`] that were present, as (lowercase name,
    /// value). A value that is not visible ASCII is recorded as `<non-ascii>`. A secret
    /// the fake authority issued is masked (see [`REDACTED`]), and on the authority's
    /// origin the `authorization` value is masked whatever it holds.
    pub headers: Vec<(String, String)>,
    /// The status the simulator answered with, including a scripted fault's status.
    pub status: u16,
}

impl LoggedRequest {
    /// The value of a logged header, by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What the log writes in place of a masked secret.
pub const REDACTED: &str = "<redacted>";

/// The prefixes of the secrets the fake authority issues: access tokens, refresh tokens
/// and authorisation codes. Each is `<prefix><instance>-<n>.<secret part>`; the log keeps
/// everything up to and including the `.`, which tells tokens apart and is not secret, and
/// writes [`REDACTED`] for the secret part.
pub const ISSUED_SECRET_PREFIXES: [&str; 3] = ["sim-at-", "sim-rt-", "sim-code-"];

/// Query parameters whose values the log masks on the authority's origin, whatever they
/// hold: a client sending one of these in a URL leaks it to every log on the way.
pub const MASKED_AUTHORITY_PARAMS: &[&str] = &[
    "code",
    "code_verifier",
    "refresh_token",
    "access_token",
    "id_token",
    "client_secret",
    "client_assertion",
    "assertion",
    "password",
];

/// The characters of a secret part and of a JWT segment: base64url.
fn is_secret_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// The length of the token-ID part after an issued-secret prefix, up to and including the
/// `.` that starts the secret part, if `rest` has that shape.
fn issued_label_len(rest: &str) -> Option<usize> {
    let dot = rest.find('.')?;
    let label = &rest[..dot];
    (!label.is_empty()
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then_some(dot + 1)
}

/// The length of a JWT (`eyJ...` then two dots) at the start of `text`, if there is one.
fn jwt_len(text: &str) -> Option<usize> {
    if !text.starts_with("eyJ") {
        return None;
    }
    let bytes = text.as_bytes();
    let mut dots = 0;
    let mut len = 0;
    while len < bytes.len() && (is_secret_char(bytes[len]) || (bytes[len] == b'.' && dots < 2)) {
        if bytes[len] == b'.' {
            dots += 1;
        }
        len += 1;
    }
    (dots == 2).then_some(len)
}

/// Masks every secret the fake authority issued in `text`: issued tokens and codes keep
/// their non-secret label, ID tokens (JWTs) are replaced whole. Anything else is kept.
pub(crate) fn mask_issued(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let issued = ISSUED_SECRET_PREFIXES
            .iter()
            .find(|p| rest.starts_with(**p))
            .and_then(|p| Some(p.len() + issued_label_len(&rest[p.len()..])?));
        if let Some(keep) = issued {
            let secret = rest[keep..]
                .bytes()
                .take_while(|b| is_secret_char(*b))
                .count();
            out.push_str(&text[copied..at + keep]);
            out.push_str(REDACTED);
            at += keep + secret;
            copied = at;
        } else if let Some(len) = jwt_len(rest) {
            out.push_str(&text[copied..at]);
            out.push_str(REDACTED);
            at += len;
            copied = at;
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// A header value as the log records it.
fn logged_header(origin: Origin, name: &str, value: &str) -> String {
    if origin == Origin::Authority && name == "authorization" {
        return match value.split_once(' ') {
            Some((scheme, _)) => format!("{scheme} {REDACTED}"),
            None => REDACTED.to_owned(),
        };
    }
    mask_issued(value).into_owned()
}

/// Whether a raw (still percent-encoded) query parameter name is one whose value the
/// authority's log masks: compared after decoding `%XX` and `+`, without regard to case.
fn is_masked_param(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match raw
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                Some(byte) => {
                    decoded.push(byte);
                    i += 3;
                }
                None => {
                    decoded.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            byte => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    let name = String::from_utf8_lossy(&decoded);
    MASKED_AUTHORITY_PARAMS
        .iter()
        .any(|masked| name.eq_ignore_ascii_case(masked))
}

/// A query string as the log records it.
fn logged_query(origin: Origin, query: &str) -> String {
    let masked = mask_issued(query);
    if origin != Origin::Authority {
        return masked.into_owned();
    }
    masked
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if is_masked_param(name) => format!("{name}={REDACTED}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) async fn log_request(
    AxState((shared, origin)): AxState<Ctx>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = mask_issued(req.uri().path()).into_owned();
    let query = req.uri().query().map(|q| logged_query(origin, q));
    let headers = LOGGED_HEADERS
        .iter()
        .filter_map(|name| {
            req.headers().get(*name).map(|value| {
                let text = value.to_str().unwrap_or("<non-ascii>");
                ((*name).to_owned(), logged_header(origin, name, text))
            })
        })
        .collect();
    let response = next.run(req).await;
    shared.lock().push_log(LoggedRequest {
        origin,
        method,
        path,
        query,
        headers,
        status: response.status().as_u16(),
    });
    response
}

impl GraphSim {
    /// The requests received so far, in the order the responses were completed. Only the
    /// newest [`MAX_LOGGED_REQUESTS`](crate::MAX_LOGGED_REQUESTS) are kept; older ones are
    /// dropped first. A long test that asserts on the log should call
    /// [`GraphSim::clear_requests`] before the part it checks.
    #[must_use]
    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.shared.lock().log.iter().cloned().collect()
    }

    /// Empties the request log.
    pub fn clear_requests(&self) {
        self.shared.lock().log.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC7: issued tokens keep their label and lose their secret part; JWTs go whole;
    /// anything else is untouched.
    #[test]
    fn masks_issued_secrets_only() {
        assert_eq!(
            mask_issued("Bearer sim-at-123-4.SecretPart_-x"),
            "Bearer sim-at-123-4.<redacted>"
        );
        assert_eq!(
            mask_issued("a=sim-rt-9-1.abc&b=sim-code-9-2.def&c=1"),
            "a=sim-rt-9-1.<redacted>&b=sim-code-9-2.<redacted>&c=1"
        );
        assert_eq!(mask_issued("x eyJhbGc.eyJzdWIi. y"), "x <redacted> y");
        assert_eq!(mask_issued("x eyJhbGc.eyJzdWIi.sig y"), "x <redacted> y");
        for kept in [
            "Bearer sim-token-123",
            "Bearer stale-token",
            "sim-at-",
            "sim-at-.x",
            "eyJ.only-one-dot",
            "é sim-rt-x",
        ] {
            assert!(matches!(mask_issued(kept), Cow::Borrowed(_)), "{kept}");
        }
    }

    #[test]
    fn authority_origin_masks_named_parameters_and_authorization() {
        assert_eq!(
            logged_query(Origin::Authority, "code=abc&state=s&code_verifier=v"),
            "code=<redacted>&state=s&code_verifier=<redacted>"
        );
        assert_eq!(
            logged_query(
                Origin::Authority,
                "CODE=a&Code_Verifier=b&%63ode=c&refresh%5Ftoken=d&access%5ftoken=e&state=s&%zz=f"
            ),
            "CODE=<redacted>&Code_Verifier=<redacted>&%63ode=<redacted>&refresh%5Ftoken=<redacted>\
             &access%5ftoken=<redacted>&state=s&%zz=f",
            "names match without regard to case and after percent-decoding"
        );
        assert_eq!(
            logged_query(Origin::Graph, "code=abc"),
            "code=abc",
            "other origins keep their query as it was"
        );
        assert_eq!(
            logged_header(Origin::Authority, "authorization", "Bearer anything"),
            "Bearer <redacted>"
        );
        assert_eq!(
            logged_header(Origin::Graph, "authorization", "Bearer sim-token-1"),
            "Bearer sim-token-1"
        );
    }
}
