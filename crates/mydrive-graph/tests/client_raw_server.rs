//! A tiny raw-TCP HTTP server for the client tests that need responses the Graph
//! simulator does not produce: content encodings, unframed or lying bodies, odd
//! `Content-Range` values, connection resets, RateLimit headers and scripted redirects.
//!
//! Each connection reads one request head, answers with the next scripted response
//! exactly as written (every scripted head carries `Connection: close`), and then closes
//! or resets the connection. It listens on `127.0.0.1` only.
//!
//! Included by `client_raw_http.rs` with `#[path = "client_raw_server.rs"] mod raw;`.
//! Cargo also builds it as a test binary of its own, with no tests, hence the
//! `dead_code` allowance.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How a scripted response ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// Shut the connection down cleanly after the bytes.
    Close,
    /// Reset the connection (RST) after the bytes.
    Reset,
}

/// One scripted response: raw bytes and how the connection ends.
#[derive(Debug, Clone)]
pub(crate) struct Scripted {
    bytes: Vec<u8>,
    end: End,
}

impl Scripted {
    /// Ends the connection with a reset instead of a clean close.
    pub(crate) fn reset(mut self) -> Self {
        self.end = End::Reset;
        self
    }
}

/// A response with `status` (for example `"200 OK"`), `headers` and `body`, sent
/// exactly so. No `Content-Length` is added: put one in `headers` if the test wants it.
pub(crate) fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Scripted {
    let mut bytes = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        bytes.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body);
    Scripted {
        bytes,
        end: End::Close,
    }
}

/// A JSON response with a correct `Content-Length`, plus `headers`.
pub(crate) fn json(status: &str, headers: &[(&str, &str)], body: &str) -> Scripted {
    let length = body.len().to_string();
    let mut all = vec![
        ("Content-Type", "application/json"),
        ("Content-Length", length.as_str()),
    ];
    all.extend_from_slice(headers);
    response(status, &all, body.as_bytes())
}

/// One request head the server received.
#[derive(Debug, Clone)]
pub(crate) struct RawRequest {
    /// The request line's target, for example `/v1.0/drives/d`.
    pub(crate) target: String,
    /// The headers, names lowercased.
    pub(crate) headers: Vec<(String, String)>,
}

impl RawRequest {
    /// A header value by lowercase name.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
struct State {
    script: VecDeque<Scripted>,
    requests: Vec<RawRequest>,
}

/// The running server. Dropping it stops it.
#[derive(Debug)]
pub(crate) struct RawServer {
    origin: String,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn read_head(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 || head.len() > 64 * 1024 {
            return None;
        }
        head.extend_from_slice(&buf[..n]);
    }
    Some(head)
}

fn parse_head(head: &[u8]) -> RawRequest {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let target = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    RawRequest { target, headers }
}

async fn serve(state: Arc<Mutex<State>>, mut stream: TcpStream) {
    let Some(head) = read_head(&mut stream).await else {
        return;
    };
    let scripted = {
        let mut st = lock(&state);
        st.requests.push(parse_head(&head));
        st.script.pop_front()
    };
    let scripted = scripted.unwrap_or_else(|| response("599 Unscripted", &[], b""));
    let _ = stream.write_all(&scripted.bytes).await;
    let _ = stream.flush().await;
    match scripted.end {
        End::Close => {
            let _ = stream.shutdown().await;
        }
        End::Reset => {
            let _ = stream.set_zero_linger();
            drop(stream);
        }
    }
}

impl RawServer {
    /// Starts a server on `127.0.0.1` with an ephemeral port and an empty script.
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(shared.clone(), stream));
            }
        });
        Self {
            origin,
            state,
            task,
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    /// `http://127.0.0.1:<port>/v1.0`, the base URL to give the client.
    pub(crate) fn base_url(&self) -> String {
        format!("{}/v1.0", self.origin)
    }

    /// Queues the response for the next connection.
    pub(crate) fn push(&self, scripted: Scripted) {
        lock(&self.state).script.push_back(scripted);
    }

    /// The scripted responses not yet sent.
    pub(crate) fn pending(&self) -> usize {
        lock(&self.state).script.len()
    }

    /// The requests received so far.
    pub(crate) fn requests(&self) -> Vec<RawRequest> {
        lock(&self.state).requests.clone()
    }
}
