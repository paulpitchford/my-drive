//! Shared helpers for the gate experiment tests (G1 and G5): a fake sign-in authority on
//! 127.0.0.1 that issues ID tokens with a `tid` claim, and a scripted terminal.
//!
//! The simulator has no authority and no `/me` routes until WP-023 merges, so the token
//! endpoint and the device-code endpoint are faked here; every Graph request still goes
//! to `mydrive-graph-sim`.
//!
//! Included by the `g1_*` and `g5_*` test files with
//! `#[path = "g1_g5_support.rs"] mod support;`. Cargo also builds it as a test binary of
//! its own, with no tests; each including file uses only part of it, hence the
//! `dead_code` allowance.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State as AxState;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Form, Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use url::Url;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Microsoft's public consumer tenant, the `tid` of personal accounts.
pub(crate) const CONSUMER_TID: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// A made-up work tenant. Not any real organisation's.
pub(crate) const WORK_TID: &str = "0a0b0c0d-1111-4222-8333-444455556666";

/// The paste-flow redirect URI the tests use.
pub(crate) const PASTE_REDIRECT: &str = "https://auth.example.invalid/nativeclient";

/// The authorisation code the fake browser "receives".
pub(crate) const AUTH_CODE: &str = "fake-auth-code-SECRET-0001";

/// The refresh token the fake authority issues.
pub(crate) const REFRESH_TOKEN: &str = "fake-refresh-token-SECRET-0002";

/// The device code the fake authority issues.
pub(crate) const DEVICE_CODE: &str = "fake-device-code-SECRET-0003";

/// The user code the fake authority issues.
pub(crate) const USER_CODE: &str = "FAKEUSER";

/// A made-up account name that an error description may carry. It must never reach a
/// report.
pub(crate) const ACCOUNT_NAME: &str = "someone@example.invalid";

/// An unsigned JWT with these claims: enough for a display-only `tid` read.
pub(crate) fn jwt(claims: &Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{payload}.c2lnbmF0dXJl")
}

/// An ID token for a tenant, with an account name that must never be reported.
pub(crate) fn id_token(tid: &str) -> String {
    jwt(&json!({
        "tid": tid,
        "preferred_username": ACCOUNT_NAME,
        "oid": "fake-object-id-0004",
        "aud": "client",
    }))
}

/// One scripted answer of the fake authority.
#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) body: Value,
}

impl Reply {
    /// An OAuth error reply.
    pub(crate) fn error(status: StatusCode, error: &str, description: &str) -> Self {
        Self {
            status,
            body: json!({ "error": error, "error_description": description }),
        }
    }
}

#[derive(Debug, Default)]
struct AuthState {
    access_token: String,
    tid: String,
    /// Replies of the token endpoint, in order; when empty, tokens are granted.
    token_replies: VecDeque<Reply>,
    /// The device-code endpoint's reply; when `None`, a code is issued.
    device_reply: Option<Reply>,
    /// Every form posted, with the endpoint's last path segment.
    forms: Vec<(String, HashMap<String, String>)>,
}

/// A fake sign-in authority: `oauth2/v2.0/token` and `oauth2/v2.0/devicecode`.
#[derive(Debug, Clone)]
pub(crate) struct FakeAuthority {
    state: Arc<Mutex<AuthState>>,
    base: String,
}

impl FakeAuthority {
    /// Starts an authority that grants `access_token` and an ID token for `tid`.
    pub(crate) async fn start(access_token: &str, tid: &str) -> Self {
        let state = Arc::new(Mutex::new(AuthState {
            access_token: access_token.to_owned(),
            tid: tid.to_owned(),
            ..AuthState::default()
        }));
        let app = Router::new()
            .route("/tenant/oauth2/v2.0/token", post(token))
            .route("/tenant/oauth2/v2.0/devicecode", post(devicecode))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            base: format!("http://{addr}/tenant/"),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AuthState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The authority base URL.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// The next token-endpoint request gets `reply`.
    pub(crate) fn push_token_reply(&self, reply: Reply) {
        self.lock().token_replies.push_back(reply);
    }

    /// The device-code endpoint answers `reply`.
    pub(crate) fn set_device_reply(&self, reply: Reply) {
        self.lock().device_reply = Some(reply);
    }

    /// Every form posted, as (endpoint, fields).
    pub(crate) fn forms(&self) -> Vec<(String, HashMap<String, String>)> {
        self.lock().forms.clone()
    }
}

/// The success body of the token endpoint.
pub(crate) fn granted(access_token: &str, tid: &str) -> Value {
    json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": REFRESH_TOKEN,
        "id_token": id_token(tid),
        "scope": "Files.Read openid offline_access",
    })
}

async fn token(
    AxState(state): AxState<Arc<Mutex<AuthState>>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
    st.forms.push(("token".to_owned(), form));
    match st.token_replies.pop_front() {
        Some(r) => (r.status, Json(r.body)).into_response(),
        None => (StatusCode::OK, Json(granted(&st.access_token, &st.tid))).into_response(),
    }
}

async fn devicecode(
    AxState(state): AxState<Arc<Mutex<AuthState>>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
    st.forms.push(("devicecode".to_owned(), form));
    match st.device_reply.clone() {
        Some(r) => (r.status, Json(r.body)).into_response(),
        None => (
            StatusCode::OK,
            Json(json!({
                "device_code": DEVICE_CODE,
                "user_code": USER_CODE,
                "verification_uri": "https://login.example.invalid/device",
                "expires_in": 900,
                "interval": 0,
                "message": "To sign in, open https://login.example.invalid/device and enter FAKEUSER.",
            })),
        )
            .into_response(),
    }
}

type Answer = Box<dyn FnMut(&str, &str) -> Option<String> + Send>;

/// A terminal whose answers the test scripts. Each answer is computed from everything
/// said so far and the prompt.
pub(crate) struct ScriptedTerminal {
    /// Everything said and every prompt shown, in order: what standard error would show.
    pub(crate) said: String,
    answers: VecDeque<Answer>,
    /// How many prompts were shown.
    pub(crate) asked: usize,
}

impl ScriptedTerminal {
    pub(crate) fn new() -> Self {
        Self {
            said: String::new(),
            answers: VecDeque::new(),
            asked: 0,
        }
    }

    /// Adds the next answer.
    pub(crate) fn then(
        mut self,
        answer: impl FnMut(&str, &str) -> Option<String> + Send + 'static,
    ) -> Self {
        self.answers.push_back(Box::new(answer));
        self
    }

    /// Adds a fixed answer.
    pub(crate) fn then_line(self, line: &str) -> Self {
        let line = line.to_owned();
        self.then(move |_, _| Some(line.clone()))
    }

    /// Adds the answer a browser would give: the redirect with the code and the attempt's
    /// `state`, taken from the authorisation URL shown.
    pub(crate) fn then_paste(self) -> Self {
        self.then(|said, _| Some(paste_answer(said)))
    }

    pub(crate) fn say_impl(&mut self, text: &str) {
        self.said.push_str(text);
    }

    pub(crate) fn ask_impl(&mut self, prompt: &str) -> Option<String> {
        self.said.push_str(prompt);
        self.asked += 1;
        let said = self.said.clone();
        self.answers.pop_front().and_then(|mut a| a(&said, prompt))
    }
}

/// The address a browser ends on after signing in: [`PASTE_REDIRECT`] with
/// [`AUTH_CODE`] and the `state` of the authorisation URL found in `said`.
pub(crate) fn paste_answer(said: &str) -> String {
    let url = said
        .split_whitespace()
        .find(|w| w.starts_with("http://") && w.contains("authorize"))
        .expect("an authorisation URL was shown");
    let url = Url::parse(url).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    format!("{PASTE_REDIRECT}?code={AUTH_CODE}&state={state}\n")
}

/// The `scope` parameter of the authorisation URL shown in `said`.
pub(crate) fn shown_scope(said: &str) -> Option<String> {
    let url = said
        .split_whitespace()
        .find(|w| w.starts_with("http://") && w.contains("authorize"))?;
    Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "scope")
        .map(|(_, v)| v.into_owned())
}
