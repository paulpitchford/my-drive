//! Gate G1 experiment: is `Range` honoured on authenticated `/content` requests?
//!
//! The owner runs this against a real account; the method, how to run it and what the
//! observations mean are in `docs/gates/G1.md`. Tests run it against `mydrive-graph-sim`.
//!
//! What it does, and nothing more:
//!
//! 1. Signs in with the paste flow (F01, headless variant), requesting only
//!    `Files.Read offline_access openid`. `openid` grants no access to Graph or to files;
//!    it is there so the ID token names the tenant (lead's ruling in WP-011).
//! 2. Reads the tenant from the ID token, shows the kind of account (personal or work) and
//!    the tenant ID on standard error, and sends nothing to Graph until the user types `yes`.
//! 3. Finds the user's drive and pages its delta until it meets a file of at least
//!    `--min-size` bytes, then reads that item's metadata.
//! 4. Fetches the first bytes of the file up to the end of the test range without `Range`,
//!    as a reference, then requests the range two ways: following the redirect (the
//!    bearer token only ever goes to the Graph origin, as ADR-0011 R10 requires), and with
//!    the bearer token and redirects disabled. The third way, with a header that opts into
//!    the post-April-2027 behaviour, is recorded as not tried: no such header is documented.
//!
//! Every request it sends to Graph or to a download host is a `GET`; it has no code path
//! that writes to the account (AC2). The report on standard output holds status codes,
//! selected headers and byte counts. It never holds a token, a pre-authenticated URL, a
//! drive or item ID, a file name, a tenant ID or any other account identifier (AC3); a
//! `Location` is described only as "same origin as Graph" or "another origin".
//!
//! This is an experiment, not the product's sign-in: the scopes differ from ADR-0010 R5 on
//! purpose, and nothing is stored. Tokens live in memory for the length of the run.

#![forbid(unsafe_code)]

use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mydrive_core::ids::{DriveId, ItemId};
use mydrive_graph::GraphClient;
use mydrive_graph::auth::{
    AccessToken, AuthConfig, BoxFuture, ClientId, DEFAULT_AUTHORITY, NATIVE_CLIENT_REDIRECT_URI,
    PasteSignIn, Scopes, SignInError, TokenClient, TokenError, TokenProvider, TokenRequestError,
    TokenSet,
};
use mydrive_graph::client::{GRAPH_BASE_URL, UserAgent};
use mydrive_graph::model::DeltaContinuation;
use reqwest::header::{self, HeaderMap, HeaderValue};
use url::Url;

use account::{AccountKind, Terminal};

/// The scopes the experiment requests, and no others (AC2). Read-only file access, a
/// refresh token, and `openid` for the tenant claim.
pub(crate) const SCOPES: [&str; 3] = ["Files.Read", "offline_access", "openid"];

/// Default smallest file: just over the 4 MiB that F03 fetches whole, so the file is one
/// the product would read in ranges.
pub(crate) const DEFAULT_MIN_SIZE: u64 = 5 * 1024 * 1024;

/// Default length of the requested range.
pub(crate) const DEFAULT_RANGE_LEN: u64 = 64 * 1024;

/// Longest range the program asks for.
pub(crate) const MAX_RANGE_LEN: u64 = 16 * 1024 * 1024;

/// Default number of delta pages searched for a large enough file.
pub(crate) const DEFAULT_MAX_PAGES: usize = 50;

/// The range starts at half the file, but never beyond this offset, so the reference read
/// (which reads from byte 0) stays small.
pub(crate) const MAX_RANGE_START: u64 = 1024 * 1024;

/// Most redirects followed in one way.
pub(crate) const MAX_HOPS: usize = 5;

/// Most bytes of an error body read to find its error codes.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// How many times the user may paste an address before the program gives up.
const MAX_PASTES: usize = 5;

/// Response headers the report shows, verbatim apart from non-printable characters.
/// `ETag` and `Location` are shown only as a relation, never as their value.
pub(crate) const SHOWN_HEADERS: [&str; 7] = [
    "content-range",
    "content-length",
    "accept-ranges",
    "content-type",
    "content-encoding",
    "transfer-encoding",
    "retry-after",
];

/// Why no header was sent for the third way. Checked 2026-11-15: the download
/// documentation lists only `Authorization` and `if-none-match`, and MC1492958 describes a
/// tenant control for early validation, not a request header.
pub(crate) const WAY3_NOT_TRIED: &str = "not tried: no header documented";

pub(crate) const USAGE: &str = "\
Usage: g1_range [options]

Signs in (paste flow, scopes Files.Read offline_access openid), asks you to confirm the
account, then requests a byte range of one file three ways and prints a report.

Options:
  --min-size BYTES     smallest file to use (default 5242880)
  --range-len BYTES    bytes to request, 1 to 16777216 (default 65536)
  --max-pages N        delta pages to search for such a file (default 50)
  --redirect-uri URI   redirect URI of the paste flow (default the native-client page)
  --client-id ID       app registration to use (default the compiled-in one)
  --help               show this text

Options for running against a simulator (tests):
  --authority URL      sign-in authority base URL
  --graph-base URL     Graph base URL
  --drive-id ID        use this drive instead of looking up /me/drive
";

/// The account check shared with the G5 example, which includes this file.
pub(crate) mod account {
    use std::io::{self, BufRead as _, Write as _};

    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    /// Microsoft's public consumer tenant: the `tid` of every personal Microsoft account.
    pub(crate) const CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

    /// The kind of account, derived from the tenant. Never typed by the user.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum AccountKind {
        /// A personal Microsoft account.
        Personal,
        /// A work or school account (any tenant other than the consumer tenant).
        Work,
    }

    impl AccountKind {
        /// The kind as the report and the prompt write it.
        pub(crate) fn as_str(self) -> &'static str {
            match self {
                Self::Personal => "personal",
                Self::Work => "work",
            }
        }
    }

    /// A tenant ID from an ID token: a GUID, lowercase.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Tenant(String);

    impl Tenant {
        /// The tenant ID.
        pub(crate) fn as_str(&self) -> &str {
            &self.0
        }

        /// Personal for the consumer tenant, work for any other.
        pub(crate) fn kind(&self) -> AccountKind {
            if self.0 == CONSUMER_TENANT {
                AccountKind::Personal
            } else {
                AccountKind::Work
            }
        }
    }

    /// Why the tenant could not be read from an ID token. No variant repeats the token.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub(crate) enum ClaimError {
        /// The token does not have three dot-separated parts.
        #[error("the ID token is not a JWT")]
        NotAJwt,
        /// The payload is not base64url.
        #[error("the ID token's payload is not base64url")]
        NotBase64,
        /// The payload is not a JSON object.
        #[error("the ID token's payload is not a JSON object")]
        NotJson,
        /// The payload has no string `tid` claim.
        #[error("the ID token has no tid claim")]
        NoTid,
        /// The `tid` claim is not a GUID.
        #[error("the ID token's tid claim is not a GUID")]
        BadTid,
    }

    /// Reads the `tid` claim of an ID token.
    ///
    /// For display only: the signature is not checked, and no signature check is needed
    /// for that. The value decides nothing by itself; it is shown to the user, who decides
    /// whether to go on. The token came straight from the token endpoint over TLS in the
    /// same run, and it grants nothing: the access token is what Graph checks.
    pub(crate) fn tenant_from_id_token(id_token: &str) -> Result<Tenant, ClaimError> {
        let mut parts = id_token.trim().split('.');
        let (Some(_header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ClaimError::NotAJwt);
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|_| ClaimError::NotBase64)?;
        let claims: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ClaimError::NotJson)?;
        if !claims.is_object() {
            return Err(ClaimError::NotJson);
        }
        let tid = claims
            .get("tid")
            .and_then(serde_json::Value::as_str)
            .ok_or(ClaimError::NoTid)?;
        if !is_guid(tid) {
            return Err(ClaimError::BadTid);
        }
        Ok(Tenant(tid.to_ascii_lowercase()))
    }

    /// Whether `s` is a GUID in the 8-4-4-4-12 hexadecimal form.
    fn is_guid(s: &str) -> bool {
        let groups: Vec<&str> = s.split('-').collect();
        groups.len() == 5
            && groups
                .iter()
                .zip([8, 4, 4, 4, 12])
                .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
    }

    /// The terminal: standard error for what the user reads, standard input for answers.
    /// Nothing written here is part of the report.
    pub(crate) trait Terminal {
        /// Shows `text` to the user.
        fn say(&mut self, text: &str) -> io::Result<()>;
        /// Shows `prompt` and reads one line; `None` at end of input.
        fn ask(&mut self, prompt: &str) -> io::Result<Option<String>>;
    }

    /// The real terminal.
    #[derive(Debug, Default)]
    pub(crate) struct StdTerminal;

    impl Terminal for StdTerminal {
        fn say(&mut self, text: &str) -> io::Result<()> {
            let mut err = io::stderr().lock();
            err.write_all(text.as_bytes())?;
            err.flush()
        }

        fn ask(&mut self, prompt: &str) -> io::Result<Option<String>> {
            self.say(prompt)?;
            let mut line = String::new();
            if io::stdin().lock().read_line(&mut line)? == 0 {
                return Ok(None);
            }
            Ok(Some(line))
        }
    }

    /// Shows which account was signed in and asks the user to type `yes`.
    ///
    /// Only the exact answer `yes` (surrounding white space aside) goes on; anything else,
    /// including end of input, stops the run before any request reaches Graph.
    pub(crate) fn confirm(term: &mut dyn Terminal, tenant: &Tenant) -> io::Result<bool> {
        term.say(&format!(
            "\nSigned in to a {} account, tenant {}.\n\
             Nothing has been sent to Graph yet. Check that this is the account you meant.\n",
            tenant.kind().as_str(),
            tenant.as_str()
        ))?;
        let answer = term.ask("Type yes to continue with this account, anything else to stop: ")?;
        Ok(answer.is_some_and(|a| a.trim() == "yes"))
    }

    /// `text` cut to printable ASCII and at most `max` characters, for anything a server
    /// sent that the report or the terminal shows.
    pub(crate) fn printable(text: &str, max: usize) -> String {
        text.chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(max)
            .collect()
    }

    /// The `AADSTS` error numbers in an Entra error description, without the rest of it.
    /// The description itself can name the account, so it is never put in the report.
    pub(crate) fn aadsts_codes(description: &str) -> Vec<String> {
        let mut codes = Vec::new();
        let mut rest = description;
        while let Some(at) = rest.find("AADSTS") {
            let digits: String = rest[at + 6..]
                .chars()
                .take_while(char::is_ascii_digit)
                .take(10)
                .collect();
            if !digits.is_empty() {
                let code = format!("AADSTS{digits}");
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            rest = &rest[at + 6..];
        }
        codes
    }
}

/// Command-line options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// Smallest file to use, in bytes.
    pub(crate) min_size: u64,
    /// Bytes to request.
    pub(crate) range_len: u64,
    /// Delta pages to search.
    pub(crate) max_pages: usize,
    /// Redirect URI of the paste flow.
    pub(crate) redirect_uri: String,
    /// Client ID override.
    pub(crate) client_id: Option<String>,
    /// Sign-in authority base URL.
    pub(crate) authority: String,
    /// Graph base URL.
    pub(crate) graph_base: String,
    /// Drive to use instead of `/me/drive`.
    pub(crate) drive_id: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            min_size: DEFAULT_MIN_SIZE,
            range_len: DEFAULT_RANGE_LEN,
            max_pages: DEFAULT_MAX_PAGES,
            redirect_uri: NATIVE_CLIENT_REDIRECT_URI.to_owned(),
            client_id: None,
            authority: DEFAULT_AUTHORITY.to_owned(),
            graph_base: GRAPH_BASE_URL.to_owned(),
            drive_id: None,
        }
    }
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Run the experiment.
    Run(Args),
    /// Print the usage text.
    Help,
}

/// A command line that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UsageError {
    /// An option the program does not know.
    #[error("unknown option {0:?}")]
    Unknown(String),
    /// An option without its value.
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    /// A value that is not usable for its option.
    #[error("{option}: {reason}")]
    BadValue {
        /// The option.
        option: &'static str,
        /// Why.
        reason: &'static str,
    },
}

fn number(option: &'static str, value: &str) -> Result<u64, UsageError> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| UsageError::BadValue {
            option,
            reason: "not a whole number",
        })
}

/// Parses the options after the program name. `--name value` and `--name=value` both work.
pub(crate) fn parse_args<I, S>(args: I) -> Result<Command, UsageError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    const OPTIONS: [&str; 8] = [
        "--min-size",
        "--range-len",
        "--max-pages",
        "--redirect-uri",
        "--client-id",
        "--authority",
        "--graph-base",
        "--drive-id",
    ];
    let mut out = Args::default();
    let mut args = args.into_iter().map(Into::into);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(Command::Help);
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
            None => (arg.clone(), None),
        };
        let Some(option) = OPTIONS.iter().copied().find(|o| *o == name) else {
            return Err(UsageError::Unknown(arg));
        };
        let value = match inline {
            Some(v) => v,
            None => args.next().ok_or(UsageError::MissingValue(option))?,
        };
        match option {
            "--min-size" => {
                out.min_size = number(option, &value)?;
                if out.min_size == 0 {
                    return Err(UsageError::BadValue {
                        option,
                        reason: "must be at least 1",
                    });
                }
            }
            "--range-len" => {
                out.range_len = number(option, &value)?;
                if !(1..=MAX_RANGE_LEN).contains(&out.range_len) {
                    return Err(UsageError::BadValue {
                        option,
                        reason: "must be from 1 to 16777216",
                    });
                }
            }
            "--max-pages" => {
                out.max_pages = usize::try_from(number(option, &value)?)
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or(UsageError::BadValue {
                        option,
                        reason: "must be at least 1",
                    })?;
            }
            "--redirect-uri" => out.redirect_uri = value,
            "--client-id" => out.client_id = Some(value),
            "--authority" => out.authority = value,
            "--graph-base" => out.graph_base = value,
            _ => {
                if value.trim().is_empty() {
                    return Err(UsageError::BadValue {
                        option,
                        reason: "must not be empty",
                    });
                }
                out.drive_id = Some(value);
            }
        }
    }
    Ok(Command::Run(out))
}

/// The UTC calendar date of `now`, as `YYYY-MM-DD`.
pub(crate) fn utc_date(now: SystemTime) -> String {
    let days = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    // Days to civil date (proleptic Gregorian), after Howard Hinnant's algorithm.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A token provider that hands out one access token. The experiment is short, so it never
/// refreshes; a `401` is reported as it comes.
pub(crate) struct FixedToken(pub(crate) AccessToken);

impl fmt::Debug for FixedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FixedToken").field(&self.0).finish()
    }
}

impl TokenProvider for FixedToken {
    fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }

    fn force_refresh<'a>(
        &'a self,
        _rejected: &'a AccessToken,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

/// How a response's `ETag` relates to the item's eTag from its metadata (Q33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EtagRelation {
    /// No `ETag` header.
    Absent,
    /// The same value as the item's eTag, ignoring quotes and a weak prefix.
    SameAsItem,
    /// Some other value.
    Differs,
}

/// Where a request went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// The Graph origin.
    Graph,
    /// Another origin, reached through a redirect.
    OtherOrigin,
}

/// What a `Location` header pointed at, without its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocationInfo {
    /// Whether it resolves to the Graph origin.
    pub(crate) same_origin: bool,
    /// Whether it carries a query string (where a pre-authenticated URL keeps its key).
    pub(crate) has_query: bool,
    /// Whether it could be parsed at all.
    pub(crate) usable: bool,
}

/// How much of a body was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BodyInfo {
    /// Bytes read.
    pub(crate) read: u64,
    /// Whether the body was read to its end.
    pub(crate) complete: bool,
    /// Whether reading failed part-way.
    pub(crate) failed: bool,
}

/// One request and its response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hop {
    /// Where it went.
    pub(crate) target: Target,
    /// Whether the bearer token was sent.
    pub(crate) bearer: bool,
    /// The `Range` header sent, if any.
    pub(crate) range: Option<String>,
    /// The status, or `None` if no response came.
    pub(crate) status: Option<u16>,
    /// The transport error, without any URL, if no response came.
    pub(crate) error: Option<String>,
    /// The [`SHOWN_HEADERS`] present, in that order.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The `ETag` relation.
    pub(crate) etag: EtagRelation,
    /// The `Location`, described.
    pub(crate) location: Option<LocationInfo>,
    /// Graph error codes from an error body.
    pub(crate) error_codes: Vec<String>,
    /// The body of a `200` or `206`.
    pub(crate) body: Option<BodyInfo>,
}

/// One way of requesting the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Way {
    /// The heading.
    pub(crate) title: &'static str,
    /// The requests, in order.
    pub(crate) hops: Vec<Hop>,
    /// Set when the way was not tried.
    pub(crate) not_tried: Option<&'static str>,
    /// The bytes of the requested range, when the response held all of them. Compared,
    /// never printed.
    pub(crate) range_bytes: Option<Vec<u8>>,
}

impl Way {
    fn new(title: &'static str) -> Self {
        Self {
            title,
            hops: Vec::new(),
            not_tried: None,
            range_bytes: None,
        }
    }

    /// A one-line result.
    pub(crate) fn outcome(&self) -> String {
        if let Some(reason) = self.not_tried {
            return reason.to_owned();
        }
        match self.hops.last() {
            None => "no request made".to_owned(),
            Some(hop) => match (hop.status, &hop.error) {
                (Some(206), _) => "206, the range".to_owned(),
                (Some(200), _) if hop.range.is_some() => {
                    "200, the whole file: Range was ignored".to_owned()
                }
                (Some(200), _) => "200, the whole file".to_owned(),
                (Some(s), _) if (300..400).contains(&s) => {
                    format!("{s}, a redirect, not followed")
                }
                (Some(s), _) => format!("HTTP {s}"),
                (None, Some(e)) => format!("no response: {e}"),
                (None, None) => "no response".to_owned(),
            },
        }
    }
}

/// How the run went, for the report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Report {
    /// UTC date of the run.
    pub(crate) date: String,
    /// The sign-in outcome.
    pub(crate) sign_in: Option<String>,
    /// The scopes the token endpoint reported.
    pub(crate) scopes_granted: Option<String>,
    /// The account kind, once known.
    pub(crate) kind: Option<AccountKind>,
    /// Whether the user typed `yes`.
    pub(crate) confirmed: bool,
    /// The drive lookup outcome, including its `driveType`.
    pub(crate) drive: Option<String>,
    /// The file search outcome.
    pub(crate) file: Option<String>,
    /// Whether the item's metadata carried an eTag.
    pub(crate) item_etag: Option<bool>,
    /// The requested range, inclusive.
    pub(crate) range: Option<(u64, u64)>,
    /// The reference read of the file's first bytes.
    pub(crate) reference: Option<Way>,
    /// The ways the range was requested.
    pub(crate) ways: Vec<Way>,
    /// Why the run stopped early, if it did.
    pub(crate) stopped: Option<String>,
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

impl Report {
    /// Whether the run reached the range requests.
    pub(crate) fn completed(&self) -> bool {
        self.stopped.is_none()
    }

    /// The report text: Markdown, ready to paste into `docs/gates/G1.md`.
    pub(crate) fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# G1 report");
        let _ = writeln!(s);
        let _ = writeln!(s, "- date (UTC): {}", self.date);
        let _ = writeln!(
            s,
            "- program: g1_range from my-drive {}",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(s, "- scopes requested: {}", SCOPES.join(" "));
        if let Some(v) = &self.sign_in {
            let _ = writeln!(s, "- sign-in: {v}");
        }
        if let Some(v) = &self.scopes_granted {
            let _ = writeln!(s, "- scopes granted: {v}");
        }
        if let Some(k) = self.kind {
            let _ = writeln!(s, "- account kind: {}", k.as_str());
            let _ = writeln!(
                s,
                "- account confirmed by the user: {}",
                yes_no(self.confirmed)
            );
        }
        if let Some(v) = &self.drive {
            let _ = writeln!(s, "- drive: {v}");
        }
        if let Some(v) = &self.file {
            let _ = writeln!(s, "- file: {v}");
        }
        if let Some(v) = self.item_etag {
            let _ = writeln!(s, "- item metadata has an eTag: {}", yes_no(v));
        }
        if let Some((start, end)) = self.range {
            let _ = writeln!(
                s,
                "- range requested: bytes={start}-{end} ({} bytes)",
                end - start + 1
            );
        }
        if let Some(r) = &self.reference {
            render_way(&mut s, r, None);
        }
        let reference = self.reference.as_ref().and_then(|r| r.range_bytes.as_ref());
        for way in &self.ways {
            render_way(&mut s, way, Some(reference));
        }
        if !self.ways.is_empty() {
            let _ = writeln!(s);
            let _ = writeln!(s, "## Summary");
            let _ = writeln!(s);
            for line in self.summary() {
                let _ = writeln!(s, "- {line}");
            }
        }
        if let Some(reason) = &self.stopped {
            let _ = writeln!(s);
            let _ = writeln!(s, "Stopped: {reason}");
        }
        s
    }

    /// The answers the gate needs, one line each.
    pub(crate) fn summary(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let first_hop = self.ways.iter().find_map(|w| w.hops.first());
        if let Some(hop) = first_hop {
            let answer = match hop.status {
                Some(206) => "answered 206: Range honoured on /content itself".to_owned(),
                Some(200) => "answered 200: content directly, Range ignored".to_owned(),
                Some(s) if (300..400).contains(&s) => {
                    format!("answered {s}: a redirect, no content on /content itself")
                }
                Some(s) => format!("answered HTTP {s}"),
                None => "no response".to_owned(),
            };
            lines.push(format!("Range on /content with the bearer token: {answer}"));
        }
        if let Some(hop) = self
            .ways
            .iter()
            .flat_map(|w| w.hops.iter())
            .find(|h| h.target == Target::OtherOrigin)
        {
            let answer = match hop.status {
                Some(206) => "honoured (206)".to_owned(),
                Some(200) => "ignored (200, whole file)".to_owned(),
                Some(s) => format!("HTTP {s}"),
                None => "no response".to_owned(),
            };
            lines.push(format!("Range on the redirect target: {answer}"));
        }
        if let Some(hop) = self
            .ways
            .iter()
            .flat_map(|w| w.hops.iter())
            .find(|h| matches!(h.status, Some(200 | 206)))
        {
            let etag = match hop.etag {
                EtagRelation::Absent => "no ETag header",
                EtagRelation::SameAsItem => "ETag header equal to the item's eTag",
                EtagRelation::Differs => "ETag header that differs from the item's eTag",
            };
            lines.push(format!("Content responses carry: {etag}"));
        }
        let reference = self.reference.as_ref().and_then(|r| r.range_bytes.as_ref());
        for way in &self.ways {
            if way.not_tried.is_none() {
                lines.push(format!(
                    "{}: {}; {}",
                    way.title,
                    way.outcome(),
                    comparison(way, reference)
                ));
            }
        }
        lines
    }
}

fn comparison(way: &Way, reference: Option<&Vec<u8>>) -> &'static str {
    match (&way.range_bytes, reference) {
        (Some(a), Some(b)) if a == b => "bytes match the reference",
        (Some(_), Some(_)) => "BYTES DIFFER from the reference",
        (Some(_), None) => "no reference bytes to compare with",
        (None, _) => "no range bytes received",
    }
}

fn render_way(s: &mut String, way: &Way, reference: Option<Option<&Vec<u8>>>) {
    let _ = writeln!(s);
    let _ = writeln!(s, "## {}", way.title);
    let _ = writeln!(s);
    if let Some(reason) = way.not_tried {
        let _ = writeln!(s, "{reason}");
        return;
    }
    for (n, hop) in way.hops.iter().enumerate() {
        let target = match hop.target {
            Target::Graph => "Graph origin",
            Target::OtherOrigin => "another origin",
        };
        let status = hop
            .status
            .map_or_else(|| "no response".to_owned(), |s| s.to_string());
        let _ = writeln!(
            s,
            "{}. GET to {target}; bearer token sent: {}; Range: {} -> {status}",
            n + 1,
            yes_no(hop.bearer),
            hop.range.as_deref().unwrap_or("none")
        );
        if let Some(e) = &hop.error {
            let _ = writeln!(s, "   - error: {e}");
        }
        for (name, value) in &hop.headers {
            let _ = writeln!(s, "   - {name}: {value}");
        }
        let etag = match hop.etag {
            EtagRelation::Absent => "absent",
            EtagRelation::SameAsItem => "present, equal to the item's eTag",
            EtagRelation::Differs => "present, differs from the item's eTag",
        };
        let _ = writeln!(s, "   - etag: {etag}");
        if let Some(l) = hop.location {
            let _ = writeln!(
                s,
                "   - location: present; {}; query string: {}",
                if !l.usable {
                    "unusable"
                } else if l.same_origin {
                    "same origin as Graph"
                } else {
                    "another origin"
                },
                yes_no(l.has_query)
            );
        }
        if !hop.error_codes.is_empty() {
            let _ = writeln!(s, "   - error codes: {}", hop.error_codes.join(", "));
        }
        if let Some(b) = hop.body {
            let how = if b.failed {
                "the connection failed"
            } else if b.complete {
                "to its end"
            } else {
                "then stopped: only the bytes up to the range end were needed"
            };
            let _ = writeln!(s, "   - body: {} bytes read, {how}", b.read);
        }
    }
    let _ = writeln!(s);
    match reference {
        None => {
            let got = way.range_bytes.as_ref().map_or(0, Vec::len);
            let _ = writeln!(
                s,
                "Result: {}; bytes of the range held: {got}",
                way.outcome()
            );
        }
        Some(r) => {
            let _ = writeln!(s, "Result: {}; {}", way.outcome(), comparison(way, r));
        }
    }
}

/// A sign-in error as the report may show it: OAuth error codes and `AADSTS` numbers,
/// never the free-text description, which can name the account.
pub(crate) fn sign_in_summary(e: &SignInError) -> String {
    let codes = |error: &str, description: Option<&str>| {
        let mut parts = vec![account::printable(error, 64)];
        parts.extend(description.map(account::aadsts_codes).unwrap_or_default());
        parts.join(" ")
    };
    match e {
        SignInError::Authorization { error, description } => {
            format!(
                "refused at the authorise step: {}",
                codes(error, description.as_deref())
            )
        }
        SignInError::Token(TokenRequestError::Rejected {
            status,
            error,
            description,
        }) => format!(
            "token request refused (HTTP {status}): {}",
            codes(error, description.as_deref())
        ),
        SignInError::Token(TokenRequestError::Status { status }) => {
            format!("token request refused with HTTP {status}")
        }
        SignInError::Token(TokenRequestError::Network { .. }) => {
            "token request failed: network error".to_owned()
        }
        SignInError::Token(TokenRequestError::InvalidResponse { reason }) => {
            format!("token response unusable: {reason}")
        }
        // The remaining variants carry fixed text only.
        other => format!("failed: {other}"),
    }
}

/// The free-text description of a refused sign-in, for the terminal only.
fn sign_in_description(e: &SignInError) -> Option<&str> {
    match e {
        SignInError::Authorization { description, .. }
        | SignInError::Token(TokenRequestError::Rejected { description, .. }) => {
            description.as_deref()
        }
        _ => None,
    }
}

/// Signs in with the paste flow. On failure the report's `sign_in` and `stopped` are set.
async fn sign_in(args: &Args, term: &mut dyn Terminal, report: &mut Report) -> Option<TokenSet> {
    let fail = |report: &mut Report, why: String| {
        report.sign_in = Some(why.clone());
        report.stopped = Some(format!("sign-in did not complete ({why})"));
    };
    let config = match AuthConfig::with_authority(
        ClientId::from_config(args.client_id.as_deref()),
        &args.authority,
    ) {
        Ok(c) => c,
        Err(e) => {
            fail(report, format!("bad authority: {e}"));
            return None;
        }
    };
    let client = match TokenClient::new(&config) {
        Ok(c) => c,
        Err(e) => {
            fail(report, sign_in_summary(&e));
            return None;
        }
    };
    let mut attempt = match PasteSignIn::start(&config, &args.redirect_uri, Scopes::new(SCOPES)) {
        Ok(a) => a,
        Err(e) => {
            fail(report, sign_in_summary(&e));
            return None;
        }
    };
    let Some(url) = attempt.authorization_url().map(Url::to_string) else {
        fail(report, "the attempt closed before it started".to_owned());
        return None;
    };
    let shown = term.say(&format!(
        "Open this address in a browser on any device and sign in:\n\n{url}\n\n\
         The browser ends on a page whose address holds the sign-in code (the page itself \
         may be blank or say it cannot be reached). Copy that whole address.\n"
    ));
    if shown.is_err() {
        fail(report, "cannot write to the terminal".to_owned());
        return None;
    }
    for _ in 0..MAX_PASTES {
        let pasted = match term.ask("Paste the address here: ") {
            Ok(Some(p)) => p,
            Ok(None) | Err(_) => {
                fail(report, "no address was pasted".to_owned());
                return None;
            }
        };
        match attempt.complete(&pasted, &client).await {
            Ok(tokens) => {
                report.sign_in = Some("completed".to_owned());
                return Some(tokens);
            }
            Err(e) => {
                if let Some(d) = sign_in_description(&e) {
                    // The description can name the account: terminal only.
                    let _ = term.say(&format!(
                        "The identity platform said (not part of the report): {}\n",
                        account::printable(d, 500)
                    ));
                }
                if attempt.is_pending() {
                    let _ = term.say(&format!("{}. Try again.\n", sign_in_summary(&e)));
                } else {
                    fail(report, sign_in_summary(&e));
                    return None;
                }
            }
        }
    }
    fail(report, "too many unusable addresses".to_owned());
    None
}

/// Reads the account from the ID token and asks the user to confirm it. Sets `stopped`
/// unless the user typed `yes`.
pub(crate) fn check_account(
    tokens: &TokenSet,
    term: &mut dyn Terminal,
    report: &mut Report,
) -> bool {
    let tenant = match tokens.id_token.as_ref() {
        None => {
            report.stopped =
                Some("the token response has no ID token, so the account kind is unknown".into());
            return false;
        }
        Some(t) => match account::tenant_from_id_token(t.expose_secret()) {
            Ok(t) => t,
            Err(e) => {
                report.stopped = Some(format!("{e}, so the account kind is unknown"));
                return false;
            }
        },
    };
    report.kind = Some(tenant.kind());
    match account::confirm(term, &tenant) {
        Ok(true) => {
            report.confirmed = true;
            true
        }
        Ok(false) => {
            report.stopped =
                Some("the user did not confirm the account; nothing was sent to Graph".into());
            false
        }
        Err(_) => {
            report.stopped = Some("cannot read the answer; nothing was sent to Graph".into());
            false
        }
    }
}

/// The raw HTTP side: requests sent by hand so every status and header can be reported.
/// The G5 example uses it for its one Graph request.
pub(crate) struct Probe {
    http: reqwest::Client,
    graph: Url,
    token: AccessToken,
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

fn strip_etag(v: &str) -> &str {
    let v = v.trim();
    let v = v.strip_prefix("W/").unwrap_or(v);
    v.trim_matches('"')
}

/// Graph error codes (`error.code` and nested `innerError.code`) of an error body.
fn error_codes(body: &[u8]) -> Vec<String> {
    let mut codes = Vec::new();
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        let mut level = value.get("error");
        while let Some(object) = level {
            if let Some(code) = object.get("code").and_then(serde_json::Value::as_str) {
                codes.push(account::printable(code, 64));
            }
            level = object
                .get("innerError")
                .or_else(|| object.get("innererror"));
        }
    }
    codes
}

/// The first byte offset of a `206` body, from `Content-Range: bytes a-b/c`.
fn content_range_start(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let spec = value.trim().strip_prefix("bytes ")?;
    let (range, _) = spec.split_once('/')?;
    let (first, _) = range.split_once('-')?;
    first.trim().parse().ok()
}

/// Reads a body that starts at file offset `first`, keeping the bytes of `want`
/// (inclusive) and stopping after `limit` bytes.
async fn read_content(
    mut response: reqwest::Response,
    first: Option<u64>,
    want: (u64, u64),
    limit: u64,
) -> (BodyInfo, Option<Vec<u8>>) {
    let mut info = BodyInfo {
        read: 0,
        complete: false,
        failed: false,
    };
    let mut kept = Vec::new();
    loop {
        if info.read >= limit {
            break;
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if let Some(first) = first {
                    let chunk_start = first + info.read;
                    for (i, b) in chunk.iter().enumerate() {
                        let at = chunk_start + i as u64;
                        if at >= want.0 && at <= want.1 {
                            kept.push(*b);
                        }
                    }
                }
                info.read += chunk.len() as u64;
            }
            Ok(None) => {
                info.complete = true;
                break;
            }
            Err(_) => {
                info.failed = true;
                break;
            }
        }
    }
    let full = first.is_some() && kept.len() as u64 == want.1 - want.0 + 1;
    (info, full.then_some(kept))
}

/// The drive ID and `driveType` of a `/me/drive` body. The ID is used, never reported.
pub(crate) fn parse_my_drive(body: &[u8]) -> Result<(DriveId, String), String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "the /me/drive body is not JSON")?;
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| DriveId::new(id).ok())
        .ok_or("the /me/drive body has no drive ID")?;
    let kind = value
        .get("driveType")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| "not stated".to_owned(), |t| account::printable(t, 40));
    Ok((id, kind))
}

impl Probe {
    /// A probe of the Graph API at `graph_base`, sending `token`.
    pub(crate) fn new(graph_base: &str, token: AccessToken) -> Result<Self, String> {
        let graph = Url::parse(graph_base).map_err(|_| "the Graph base URL is not a URL")?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .user_agent(UserAgent::default().as_str())
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {}", e.without_url()))?;
        Ok(Self { http, graph, token })
    }

    fn graph_url(&self, segments: &[&str]) -> Url {
        let mut url = self.graph.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        url
    }

    async fn send(
        &self,
        url: &Url,
        bearer: bool,
        range: Option<(u64, u64)>,
    ) -> Result<reqwest::Response, String> {
        let mut request = self.http.get(url.clone());
        if bearer {
            request = request.bearer_auth(self.token.expose_secret());
        }
        if let Some((start, end)) = range {
            request = request.header(header::RANGE, format!("bytes={start}-{end}"));
        }
        request
            .send()
            .await
            .map_err(|e| account::printable(&e.without_url().to_string(), 200))
    }

    /// `GET /me/drive`: whether the drive was found, its `driveType`, and its ID.
    pub(crate) async fn my_drive(&self) -> Result<(DriveId, String), String> {
        let url = self.graph_url(&["me", "drive"]);
        let response = self.send(&url, true, None).await?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| account::printable(&e.without_url().to_string(), 200))?;
        if status != 200 {
            let codes = error_codes(&body);
            return Err(if codes.is_empty() {
                format!("GET /me/drive answered HTTP {status}")
            } else {
                format!(
                    "GET /me/drive answered HTTP {status} ({})",
                    codes.join(", ")
                )
            });
        }
        parse_my_drive(&body)
    }

    /// One request and its described response. A `200` or `206` body is read as `read`
    /// says; the response is not returned, so nothing of it outlives the hop.
    async fn hop(
        &self,
        url: &Url,
        bearer: bool,
        range: Option<(u64, u64)>,
        item_etag: Option<&str>,
        read: Option<((u64, u64), u64)>,
    ) -> (Hop, Option<Url>, Option<Vec<u8>>) {
        let target = if same_origin(url, &self.graph) {
            Target::Graph
        } else {
            Target::OtherOrigin
        };
        let mut hop = Hop {
            target,
            bearer,
            range: range.map(|(s, e)| format!("bytes={s}-{e}")),
            status: None,
            error: None,
            headers: Vec::new(),
            etag: EtagRelation::Absent,
            location: None,
            error_codes: Vec::new(),
            body: None,
        };
        let response = match self.send(url, bearer, range).await {
            Ok(r) => r,
            Err(e) => {
                hop.error = Some(e);
                return (hop, None, None);
            }
        };
        let status = response.status().as_u16();
        hop.status = Some(status);
        let headers = response.headers().clone();
        for name in SHOWN_HEADERS {
            for value in headers.get_all(name) {
                hop.headers.push((
                    name,
                    account::printable(&String::from_utf8_lossy(value.as_bytes()), 120),
                ));
            }
        }
        hop.etag = match headers.get(header::ETAG) {
            None => EtagRelation::Absent,
            Some(v) => {
                let v = String::from_utf8_lossy(v.as_bytes());
                if item_etag.is_some_and(|i| strip_etag(i) == strip_etag(&v)) {
                    EtagRelation::SameAsItem
                } else {
                    EtagRelation::Differs
                }
            }
        };
        let mut next = None;
        if let Some(location) = headers.get(header::LOCATION) {
            let parsed = location
                .to_str()
                .ok()
                .and_then(|l| url.join(l).ok())
                .filter(|u| matches!(u.scheme(), "http" | "https"));
            hop.location = Some(LocationInfo {
                same_origin: parsed.as_ref().is_some_and(|u| same_origin(u, &self.graph)),
                has_query: parsed.as_ref().is_some_and(|u| u.query().is_some()),
                usable: parsed.is_some(),
            });
            next = parsed;
        }
        let mut bytes = None;
        match status {
            200 | 206 => {
                if let Some((want, limit)) = read {
                    let first = if status == 200 {
                        Some(0)
                    } else {
                        content_range_start(&headers)
                    };
                    let (info, kept) = read_content(response, first, want, limit).await;
                    hop.body = Some(info);
                    bytes = kept;
                }
            }
            s if s >= 400 => {
                let mut body = Vec::new();
                let mut response = response;
                while let Ok(Some(chunk)) = response.chunk().await {
                    body.extend_from_slice(&chunk);
                    if body.len() > MAX_ERROR_BODY {
                        break;
                    }
                }
                hop.error_codes = error_codes(&body);
            }
            _ => {}
        }
        (hop, next, bytes)
    }

    /// Requests `/content`, following redirects by hand if `follow`. The bearer token goes
    /// only to the Graph origin, and never again once a redirect has left it (ADR-0011 R10).
    async fn way(
        &self,
        title: &'static str,
        content: &Url,
        range: Option<(u64, u64)>,
        follow: bool,
        item_etag: Option<&str>,
        read: ((u64, u64), u64),
    ) -> Way {
        let mut way = Way::new(title);
        let mut url = content.clone();
        let mut bearer = true;
        for _ in 0..=MAX_HOPS {
            bearer = bearer && same_origin(&url, &self.graph);
            let (hop, next, bytes) = self.hop(&url, bearer, range, item_etag, Some(read)).await;
            let redirect = hop.status.is_some_and(|s| (300..400).contains(&s));
            way.hops.push(hop);
            way.range_bytes = bytes;
            match next {
                Some(next) if follow && redirect => url = next,
                _ => break,
            }
        }
        way
    }
}

/// Finds a file of at least `min_size` bytes by paging the drive's delta.
async fn find_file(
    client: &GraphClient,
    drive: &DriveId,
    args: &Args,
) -> Result<(ItemId, usize), String> {
    let describe = |e: mydrive_graph::GraphError| format!("delta failed: {}", e.kind());
    let mut page = client.delta_from_root(drive).await.map_err(describe)?;
    let mut pages = 1;
    loop {
        if let Some(item) = page
            .items()
            .find(|i| i.is_file() && !i.deleted && i.size.is_some_and(|s| s >= args.min_size))
        {
            return Ok((item.id.clone(), pages));
        }
        let next = match &page.continuation {
            DeltaContinuation::NextLink(link) if pages < args.max_pages => link.clone(),
            DeltaContinuation::NextLink(_) => {
                return Err(format!(
                    "no file of at least {} bytes in the first {pages} delta pages",
                    args.min_size
                ));
            }
            DeltaContinuation::DeltaLink(_) => {
                return Err(format!(
                    "no file of at least {} bytes in the drive ({pages} delta pages)",
                    args.min_size
                ));
            }
        };
        page = client
            .delta_from_link(drive, &next)
            .await
            .map_err(describe)?;
        pages += 1;
    }
}

/// The range to request in a file of `size` bytes: from half the file (at most
/// [`MAX_RANGE_START`], rounded down to 4 KiB) for `len` bytes, cut at the end of the file.
pub(crate) fn pick_range(size: u64, len: u64) -> Option<(u64, u64)> {
    if size == 0 || len == 0 {
        return None;
    }
    let start = (size / 2).min(MAX_RANGE_START) & !0xFFF;
    let end = start.saturating_add(len).min(size) - 1;
    Some((start, end))
}

/// Runs the experiment after sign-in: the account check, then the requests.
pub(crate) async fn run_with_tokens(
    args: &Args,
    tokens: &TokenSet,
    term: &mut dyn Terminal,
    report: &mut Report,
) {
    report.scopes_granted = tokens.scope.as_deref().map(|s| account::printable(s, 300));
    if !check_account(tokens, term, report) {
        return;
    }
    let token = tokens.access_token.clone();
    let probe = match Probe::new(&args.graph_base, token.clone()) {
        Ok(p) => p,
        Err(e) => {
            report.stopped = Some(e);
            return;
        }
    };
    let drive = match &args.drive_id {
        Some(id) => match DriveId::new(id.trim()) {
            Ok(d) => {
                report.drive = Some("given on the command line".to_owned());
                d
            }
            Err(_) => {
                report.stopped = Some("the drive ID is empty".to_owned());
                return;
            }
        },
        None => match probe.my_drive().await {
            Ok((d, kind)) => {
                report.drive = Some(format!("found with GET /me/drive, driveType {kind}"));
                d
            }
            Err(e) => {
                report.drive = Some(e.clone());
                report.stopped = Some(format!("cannot find the drive: {e}"));
                return;
            }
        },
    };
    let provider: Arc<dyn TokenProvider> = Arc::new(FixedToken(token));
    let client = match GraphClient::new(&args.graph_base, provider, UserAgent::default()) {
        Ok(c) => c,
        Err(e) => {
            report.stopped = Some(format!("cannot build the Graph client: {e}"));
            return;
        }
    };
    let (item_id, pages) = match find_file(&client, &drive, args).await {
        Ok(found) => found,
        Err(e) => {
            report.file = Some(e.clone());
            report.stopped = Some(e);
            return;
        }
    };
    let item = match client.get_item(&drive, &item_id).await {
        Ok(i) => i,
        Err(e) => {
            report.stopped = Some(format!("reading the file's metadata failed: {}", e.kind()));
            return;
        }
    };
    let Some(size) = item.size else {
        report.stopped = Some("the file's metadata has no size".to_owned());
        return;
    };
    report.file = Some(format!(
        "{size} bytes, found after {pages} delta page{}",
        if pages == 1 { "" } else { "s" }
    ));
    report.item_etag = Some(item.etag.is_some());
    let Some(range) = pick_range(size, args.range_len) else {
        report.stopped = Some("the file is empty".to_owned());
        return;
    };
    report.range = Some(range);
    let content = probe.graph_url(&[
        "drives",
        drive.as_str(),
        "items",
        item_id.as_str(),
        "content",
    ]);
    let etag = item.etag.as_deref();
    let range_limit = range.1 - range.0 + 1 + 1024 * 1024;
    report.reference = Some(
        probe
            .way(
                "Reference: the file's first bytes, no Range, redirects followed",
                &content,
                None,
                true,
                etag,
                (range, range.1 + 1),
            )
            .await,
    );
    report.ways.push(
        probe
            .way(
                "Way 1: Range, redirects followed (bearer token to the Graph origin only)",
                &content,
                Some(range),
                true,
                etag,
                (range, range_limit),
            )
            .await,
    );
    report.ways.push(
        probe
            .way(
                "Way 2: Range, bearer token, redirects disabled",
                &content,
                Some(range),
                false,
                etag,
                (range, range_limit),
            )
            .await,
    );
    let mut way3 =
        Way::new("Way 3: Range with the header that opts into the post-April-2027 behaviour");
    way3.not_tried = Some(WAY3_NOT_TRIED);
    report.ways.push(way3);
}

/// The whole run: sign-in, account check, requests. Returns the report.
pub(crate) async fn run(args: &Args, term: &mut dyn Terminal, date: String) -> Report {
    let mut report = Report {
        date,
        ..Report::default()
    };
    if let Some(tokens) = sign_in(args, term, &mut report).await {
        run_with_tokens(args, &tokens, term, &mut report).await;
    }
    report
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Command::Run(a)) => a,
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("g1_range: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut term = account::StdTerminal;
    let report = run(&args, &mut term, utc_date(SystemTime::now())).await;
    let mut out = io::stdout().lock();
    if out.write_all(report.render().as_bytes()).is_err() || out.flush().is_err() {
        return ExitCode::from(1);
    }
    if report.completed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
