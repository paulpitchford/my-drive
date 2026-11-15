//! Gate G5 experiment: does device-code sign-in work for personal accounts?
//!
//! The owner runs this against a real account; the method, how to run it and what the
//! observations mean are in `docs/gates/G5.md`. Tests run it against `mydrive-graph-sim`
//! and a fake authority.
//!
//! What it does, and nothing more:
//!
//! 1. Asks the identity platform for a device code (RFC 8628), requesting only
//!    `Files.Read offline_access openid`, and shows the code and the address to open on
//!    standard error.
//! 2. Polls the token endpoint at the interval the platform asks for until the user signs
//!    in, declines, or the code expires.
//! 3. Reads the tenant from the ID token, shows the kind of account and the tenant ID on
//!    standard error, and sends nothing to Graph until the user types `yes`.
//! 4. Sends one `GET /me/drive` to check that Graph accepts the token.
//!
//! The report on standard output holds the outcome, OAuth error codes and status codes. It
//! never holds a token, the device code, the user code, a tenant ID or any other account
//! identifier. The account check is the one in `g1_range.rs`, included below, so both
//! programs apply the same rule.

#![forbid(unsafe_code)]

#[path = "g1_range.rs"]
#[allow(dead_code)]
pub(crate) mod g1;

use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use mydrive_graph::auth::{
    AccessToken, AuthConfig, ClientId, DEFAULT_AUTHORITY, IdToken, RefreshToken,
};
use mydrive_graph::client::{GRAPH_BASE_URL, UserAgent};
use serde::Deserialize;
use tokio::time::Instant;

use g1::account::{self, AccountKind, Terminal};

/// The scopes requested: the same read-only set as the G1 experiment (AC2).
pub(crate) const SCOPES: [&str; 3] = g1::SCOPES;

/// The OAuth grant type of a device-code token request (RFC 8628 section 3.4).
pub(crate) const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The polling interval when the platform names none (RFC 8628 section 3.2).
pub(crate) const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// How much `slow_down` adds to the interval (RFC 8628 section 3.5).
pub(crate) const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// Longest interval honoured, so a bad response cannot stall the run.
const MAX_INTERVAL: Duration = Duration::from_secs(60);

/// Longest code lifetime honoured.
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Most bytes of an endpoint's response read.
const MAX_BODY: usize = 64 * 1024;

/// The time out of one request to the identity platform.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const USAGE: &str = "\
Usage: g5_device_code [options]

Tries device-code sign-in (scopes Files.Read offline_access openid), asks you to confirm
the account, checks the token with one GET /me/drive, and prints a report.

Options:
  --client-id ID       app registration to use (default the compiled-in one)
  --help               show this text

Options for running against a simulator (tests):
  --authority URL      sign-in authority base URL
  --graph-base URL     Graph base URL
";

/// Command-line options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Args {
    /// Client ID override.
    pub(crate) client_id: Option<String>,
    /// Sign-in authority base URL.
    pub(crate) authority: String,
    /// Graph base URL.
    pub(crate) graph_base: String,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            client_id: None,
            authority: DEFAULT_AUTHORITY.to_owned(),
            graph_base: GRAPH_BASE_URL.to_owned(),
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

/// Parses the options after the program name. `--name value` and `--name=value` both work.
pub(crate) fn parse_args<I, S>(args: I) -> Result<Command, g1::UsageError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    const OPTIONS: [&str; 3] = ["--client-id", "--authority", "--graph-base"];
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
            return Err(g1::UsageError::Unknown(arg));
        };
        let value = match inline {
            Some(v) => v,
            None => args.next().ok_or(g1::UsageError::MissingValue(option))?,
        };
        match option {
            "--client-id" => out.client_id = Some(value),
            "--authority" => out.authority = value,
            _ => out.graph_base = value,
        }
    }
    Ok(Command::Run(out))
}

/// How the poll loop waits. The real run uses the RFC defaults; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PollTiming {
    /// The interval when the platform names none.
    pub(crate) default_interval: Duration,
    /// What `slow_down` adds.
    pub(crate) slow_down_step: Duration,
}

impl Default for PollTiming {
    fn default() -> Self {
        Self {
            default_interval: DEFAULT_INTERVAL,
            slow_down_step: SLOW_DOWN_STEP,
        }
    }
}

/// The device-code response. `device_code` is a credential for this sign-in and is never
/// shown; `user_code` is shown on the terminal only.
#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: Option<String>,
    verification_url: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

/// An OAuth error body. Entra adds `error_codes`, numbers only.
#[derive(Deserialize, Default)]
struct OAuthError {
    error: Option<String>,
    error_description: Option<String>,
    error_codes: Option<Vec<u64>>,
}

/// The token response, as far as the experiment reads it.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    scope: Option<String>,
}

/// Tokens granted by a device-code sign-in. Redacted in `Debug`.
pub(crate) struct Granted {
    pub(crate) access_token: AccessToken,
    pub(crate) refresh_token: Option<RefreshToken>,
    pub(crate) id_token: Option<IdToken>,
    pub(crate) scope: Option<String>,
}

impl fmt::Debug for Granted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Granted")
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("id_token", &self.id_token)
            .field("scope", &self.scope)
            .finish()
    }
}

/// How the run went, for the report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Report {
    /// UTC date of the run.
    pub(crate) date: String,
    /// The device-code request's outcome.
    pub(crate) device_code: Option<String>,
    /// Polls answered `authorization_pending`.
    pub(crate) pending: usize,
    /// Polls answered `slow_down`.
    pub(crate) slow_down: usize,
    /// The sign-in outcome.
    pub(crate) sign_in: Option<String>,
    /// Which tokens came back: access, refresh, ID.
    pub(crate) tokens: Option<(bool, bool, bool)>,
    /// The scopes the token endpoint reported.
    pub(crate) scopes_granted: Option<String>,
    /// The account kind, once known.
    pub(crate) kind: Option<AccountKind>,
    /// Whether the user typed `yes`.
    pub(crate) confirmed: bool,
    /// The outcome of the Graph check.
    pub(crate) graph: Option<String>,
    /// Why the run stopped early, if it did.
    pub(crate) stopped: Option<String>,
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

impl Report {
    /// Whether the run reached the Graph check.
    pub(crate) fn completed(&self) -> bool {
        self.stopped.is_none()
    }

    /// The report text: Markdown, ready to paste into `docs/gates/G5.md`.
    pub(crate) fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# G5 report");
        let _ = writeln!(s);
        let _ = writeln!(s, "- date (UTC): {}", self.date);
        let _ = writeln!(
            s,
            "- program: g5_device_code from my-drive {}",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(s, "- scopes requested: {}", SCOPES.join(" "));
        if let Some(v) = &self.device_code {
            let _ = writeln!(s, "- device-code request: {v}");
        }
        if self.pending + self.slow_down > 0 {
            let _ = writeln!(
                s,
                "- polls: {} authorization_pending, {} slow_down",
                self.pending, self.slow_down
            );
        }
        if let Some(v) = &self.sign_in {
            let _ = writeln!(s, "- sign-in: {v}");
        }
        if let Some((a, r, i)) = self.tokens {
            let _ = writeln!(
                s,
                "- tokens returned: access {}, refresh {}, ID {}",
                yes_no(a),
                yes_no(r),
                yes_no(i)
            );
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
        if let Some(v) = &self.graph {
            let _ = writeln!(s, "- Graph check: {v}");
        }
        if let Some(reason) = &self.stopped {
            let _ = writeln!(s);
            let _ = writeln!(s, "Stopped: {reason}");
        }
        s
    }
}

/// An OAuth error as the report may show it: the error code, Entra's numeric
/// `error_codes` and the `AADSTS` numbers, never the description, which can name the
/// account.
fn error_summary(status: u16, e: &OAuthError) -> String {
    let mut parts = vec![format!("HTTP {status}")];
    if let Some(error) = &e.error {
        parts.push(account::printable(error, 64));
    }
    let mut codes: Vec<String> = e
        .error_description
        .as_deref()
        .map(account::aadsts_codes)
        .unwrap_or_default();
    for n in e.error_codes.iter().flatten() {
        let code = format!("AADSTS{n}");
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    parts.extend(codes);
    parts.join(" ")
}

/// Reads a whole response body of at most [`MAX_BODY`] bytes.
async fn read_small(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_BODY {
                    return Err("the response is too large".to_owned());
                }
            }
            Ok(None) => return Ok(body),
            Err(e) => return Err(account::printable(&e.without_url().to_string(), 200)),
        }
    }
}

/// The identity platform's two endpoints.
struct Authority {
    http: reqwest::Client,
    client_id: ClientId,
    devicecode: url::Url,
    token: url::Url,
}

impl Authority {
    fn new(args: &Args) -> Result<Self, String> {
        let config = AuthConfig::with_authority(
            ClientId::from_config(args.client_id.as_deref()),
            &args.authority,
        )
        .map_err(|e| e.to_string())?;
        let devicecode = config
            .authority()
            .join("oauth2/v2.0/devicecode")
            .map_err(|_| "cannot build the device-code endpoint".to_owned())?;
        let token = config.token_endpoint().map_err(|e| e.to_string())?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(UserAgent::default().as_str())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {}", e.without_url()))?;
        Ok(Self {
            http,
            client_id: config.client_id().clone(),
            devicecode,
            token,
        })
    }

    async fn post(&self, url: &url::Url, form: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String> {
        let response = self
            .http
            .post(url.clone())
            .form(form)
            .send()
            .await
            .map_err(|e| account::printable(&e.without_url().to_string(), 200))?;
        let status = response.status().as_u16();
        Ok((status, read_small(response).await?))
    }
}

/// Runs the device-code flow to an outcome. On failure the report's fields and `stopped`
/// are set.
async fn device_sign_in(
    args: &Args,
    timing: PollTiming,
    term: &mut dyn Terminal,
    report: &mut Report,
) -> Option<Granted> {
    let authority = match Authority::new(args) {
        Ok(a) => a,
        Err(e) => {
            report.stopped = Some(format!("bad sign-in settings: {e}"));
            return None;
        }
    };
    let scope = SCOPES.join(" ");
    let (status, body) = match authority
        .post(
            &authority.devicecode,
            &[
                ("client_id", authority.client_id.as_str()),
                ("scope", scope.as_str()),
            ],
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            report.device_code = Some(format!("no response: {e}"));
            report.stopped = Some("the device-code request failed".to_owned());
            return None;
        }
    };
    if status != 200 {
        let e: OAuthError = serde_json::from_slice(&body).unwrap_or_default();
        show_description(term, &e);
        report.device_code = Some(format!("refused: {}", error_summary(status, &e)));
        report.stopped = Some("the identity platform refused the device-code request".to_owned());
        return None;
    }
    let Ok(code) = serde_json::from_slice::<DeviceCodeResponse>(&body) else {
        report.device_code = Some("accepted, but the response is unusable".to_owned());
        report.stopped = Some("the device-code response is unusable".to_owned());
        return None;
    };
    report.device_code = Some("accepted".to_owned());
    let address = code
        .verification_uri
        .as_deref()
        .or(code.verification_url.as_deref())
        .map_or_else(|| "(none given)".to_owned(), |u| account::printable(u, 200));
    let _ = term.say(&format!(
        "On any device, open {address}\nand enter the code {}\n\
         Waiting for you to sign in (press Ctrl-C to give up).\n",
        account::printable(&code.user_code, 32)
    ));
    let lifetime = Duration::from_secs(code.expires_in.unwrap_or(900)).min(MAX_LIFETIME);
    let deadline = Instant::now() + lifetime;
    let mut interval = code
        .interval
        .map_or(timing.default_interval, Duration::from_secs)
        .min(MAX_INTERVAL);
    loop {
        if Instant::now() >= deadline {
            report.sign_in = Some("timed out: the code's lifetime ended".to_owned());
            report.stopped = Some("device-code sign-in did not complete".to_owned());
            return None;
        }
        tokio::time::sleep(interval).await;
        let (status, body) = match authority
            .post(
                &authority.token,
                &[
                    ("grant_type", DEVICE_CODE_GRANT),
                    ("client_id", authority.client_id.as_str()),
                    ("device_code", code.device_code.as_str()),
                ],
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                report.sign_in = Some(format!("token request failed: {e}"));
                report.stopped = Some("device-code sign-in did not complete".to_owned());
                return None;
            }
        };
        if status == 200 {
            return granted(&body, report);
        }
        let e: OAuthError = serde_json::from_slice(&body).unwrap_or_default();
        let outcome = match e.error.as_deref() {
            Some("authorization_pending") => {
                report.pending += 1;
                continue;
            }
            Some("slow_down") => {
                report.slow_down += 1;
                interval = (interval + timing.slow_down_step).min(MAX_INTERVAL);
                continue;
            }
            Some("authorization_declined") => "declined by the user".to_owned(),
            Some("expired_token") => "the code expired before sign-in finished".to_owned(),
            _ => format!("refused: {}", error_summary(status, &e)),
        };
        show_description(term, &e);
        report.sign_in = Some(outcome);
        report.stopped = Some("device-code sign-in did not complete".to_owned());
        return None;
    }
}

/// Shows an error description on the terminal only: it can name the account.
fn show_description(term: &mut dyn Terminal, e: &OAuthError) {
    if let Some(d) = &e.error_description {
        let _ = term.say(&format!(
            "The identity platform said (not part of the report): {}\n",
            account::printable(d, 500)
        ));
    }
}

fn granted(body: &[u8], report: &mut Report) -> Option<Granted> {
    let parsed = serde_json::from_slice::<TokenResponse>(body).ok();
    let Some(access) = parsed
        .as_ref()
        .and_then(|p| p.access_token.as_deref())
        .filter(|t| !t.is_empty())
    else {
        report.sign_in = Some("the token response has no access token".to_owned());
        report.stopped = Some("device-code sign-in did not complete".to_owned());
        return None;
    };
    let parsed = parsed.as_ref()?;
    let tokens = Granted {
        access_token: AccessToken::new(access),
        refresh_token: parsed.refresh_token.clone().map(RefreshToken::new),
        id_token: parsed.id_token.clone().map(IdToken::new),
        scope: parsed.scope.as_deref().map(|s| account::printable(s, 300)),
    };
    report.sign_in = Some("completed".to_owned());
    report.tokens = Some((
        true,
        tokens.refresh_token.is_some(),
        tokens.id_token.is_some(),
    ));
    report.scopes_granted.clone_from(&tokens.scope);
    Some(tokens)
}

/// The account check, then the one Graph request.
pub(crate) async fn after_sign_in(
    args: &Args,
    tokens: &Granted,
    term: &mut dyn Terminal,
    report: &mut Report,
) {
    let tenant = match tokens.id_token.as_ref() {
        None => {
            report.stopped =
                Some("the token response has no ID token, so the account kind is unknown".into());
            return;
        }
        Some(t) => match account::tenant_from_id_token(t.expose_secret()) {
            Ok(t) => t,
            Err(e) => {
                report.stopped = Some(format!("{e}, so the account kind is unknown"));
                return;
            }
        },
    };
    report.kind = Some(tenant.kind());
    match account::confirm(term, &tenant) {
        Ok(true) => report.confirmed = true,
        Ok(false) => {
            report.stopped =
                Some("the user did not confirm the account; nothing was sent to Graph".into());
            return;
        }
        Err(_) => {
            report.stopped = Some("cannot read the answer; nothing was sent to Graph".into());
            return;
        }
    }
    let probe = match g1::Probe::new(&args.graph_base, tokens.access_token.clone()) {
        Ok(p) => p,
        Err(e) => {
            report.stopped = Some(e);
            return;
        }
    };
    report.graph = Some(match probe.my_drive().await {
        Ok((_, kind)) => format!("GET /me/drive answered 200, driveType {kind}"),
        Err(e) => e,
    });
}

/// The whole run. Returns the report.
pub(crate) async fn run(
    args: &Args,
    timing: PollTiming,
    term: &mut dyn Terminal,
    date: String,
) -> Report {
    let mut report = Report {
        date,
        ..Report::default()
    };
    if let Some(tokens) = device_sign_in(args, timing, term, &mut report).await {
        after_sign_in(args, &tokens, term, &mut report).await;
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
            eprintln!("g5_device_code: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut term = account::StdTerminal;
    let report = run(
        &args,
        PollTiming::default(),
        &mut term,
        g1::utc_date(SystemTime::now()),
    )
    .await;
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
