//! In-process mock of an OAuth 2.0 authorize + token endpoint, for tests (feature `mock`).
//!
//! One listener on 127.0.0.1:0 per [`MockAuth`], served by a std thread, so tests can run in
//! parallel. The authorize endpoint answers with a 302 to the given `redirect_uri` carrying
//! `code` + `state` (or `error` after [`MockAuth::deny_next`]); the token endpoint checks
//! the PKCE S256 verifier, client_id and redirect_uri, issues access/refresh/id tokens,
//! rotates refresh tokens, and answers `invalid_grant` for unknown, used or revoked grants.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use std::sync::atomic::AtomicBool;
//! let mock = rslook_oauth::mock::MockAuth::start()?;
//! mock.set_identity("Ada Example", "ada@example.test");
//! let tokens = rslook_oauth::sign_in(
//!     &mock.provider(rslook_oauth::GRAPH_SCOPES),
//!     &mock.browser(),
//!     std::time::Duration::from_secs(10),
//!     &AtomicBool::new(false),
//! );
//! # Ok(()) }
//! ```

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::{Provider, encode_pairs, parse_query, pkce_challenge, query_value};

/// The client_id the mock's [`MockAuth::provider`] uses.
pub const CLIENT_ID: &str = "rslook-mock-client";

/// Most bytes of one HTTP request (headers + body) the mock reads.
const MAX_MOCK_REQUEST: usize = 64 * 1024;

struct CodeGrant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
}

struct Inner {
    codes: HashMap<String, CodeGrant>,
    /// refresh token -> client_id
    refresh: HashMap<String, String>,
    access: HashSet<String>,
    name: String,
    email: String,
    fixed_access: Option<String>,
    deny: Option<(String, String)>,
    expires_in: u64,
    counter: u64,
    token_requests: u64,
}

impl Inner {
    fn next(&mut self) -> u64 {
        self.counter = self.counter.saturating_add(1);
        self.counter
    }

    fn mint_refresh(&mut self, client_id: &str) -> String {
        let rt = format!("mock-rt-{}", self.next());
        self.refresh.insert(rt.clone(), client_id.to_string());
        rt
    }
}

/// A running mock authorization server. Stops when dropped.
pub struct MockAuth {
    /// `http://127.0.0.1:{port}`
    pub base: String,
    inner: Arc<Mutex<Inner>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MockAuth {
    /// Binds 127.0.0.1:0 and starts serving on a background thread.
    pub fn start() -> std::io::Result<MockAuth> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        listener.set_nonblocking(true)?;
        let base = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let inner = Arc::new(Mutex::new(Inner {
            codes: HashMap::new(),
            refresh: HashMap::new(),
            access: HashSet::new(),
            name: "Mock User".into(),
            email: "mock.user@example.test".into(),
            fixed_access: None,
            deny: None,
            expires_in: 3600,
            counter: 0,
            token_requests: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (inner, stop, base) = (inner.clone(), stop.clone(), base.clone());
            std::thread::Builder::new().name("rslook-oauth-mock".into()).spawn(move || serve(&listener, &inner, &stop, &base))?
        };
        Ok(MockAuth { base, inner, stop, thread: Some(thread) })
    }

    /// Provider pointing at this mock: `{base}/authorize`, `{base}/token`, [`CLIENT_ID`].
    pub fn provider(&self, scopes: &[&str]) -> Provider {
        Provider {
            authorize_url: format!("{}/authorize", self.base),
            token_url: format!("{}/token", self.base),
            client_id: CLIENT_ID.to_string(),
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A stand-in for the user's browser: on a background thread it GETs the authorize URL and
    /// follows the 302 to the loopback redirect. Refuses URLs not aimed at this mock.
    pub fn browser(&self) -> impl Fn(&str) -> Result<(), String> + Send + Sync + 'static {
        let prefix = format!("{}/authorize", self.base);
        move |url: &str| {
            if !url.starts_with(&prefix) {
                return Err(format!("mock browser: not this mock's authorize URL: {url}"));
            }
            let url = url.to_string();
            std::thread::spawn(move || {
                if let Ok(resp) = http_get(&url)
                    && let Some(location) = resp.location
                {
                    let _ = http_get(&location);
                }
            });
            Ok(())
        }
    }

    /// Name and email claims of id_tokens issued from now on.
    pub fn set_identity(&self, name: &str, email: &str) {
        let mut g = lock(&self.inner);
        g.name = name.to_string();
        g.email = email.to_string();
    }

    /// Every access token issued from now on (code and refresh grants) is exactly `token`.
    pub fn set_access_token(&self, token: &str) {
        lock(&self.inner).fixed_access = Some(token.to_string());
    }

    /// `expires_in` of tokens issued from now on (default 3600).
    pub fn set_expires_in(&self, secs: u64) {
        lock(&self.inner).expires_in = secs;
    }

    /// The next authorize request redirects with `error` / `error_description` instead of a code.
    pub fn deny_next(&self, error: &str, description: &str) {
        lock(&self.inner).deny = Some((error.to_string(), description.to_string()));
    }

    /// Mints a valid refresh token for [`CLIENT_ID`] without a browser flow.
    pub fn issue_refresh_token(&self) -> String {
        lock(&self.inner).mint_refresh(CLIENT_ID)
    }

    /// Revokes one refresh token.
    pub fn revoke_refresh_token(&self, refresh_token: &str) {
        lock(&self.inner).refresh.remove(refresh_token);
    }

    /// Revokes every refresh and access token issued so far (pending codes too).
    pub fn revoke_all(&self) {
        let mut g = lock(&self.inner);
        g.refresh.clear();
        g.access.clear();
        g.codes.clear();
    }

    /// True for an access token this mock issued and has not revoked.
    pub fn access_token_valid(&self, token: &str) -> bool {
        lock(&self.inner).access.contains(token)
    }

    /// Number of requests the token endpoint has received.
    pub fn token_requests(&self) -> u64 {
        lock(&self.inner).token_requests
    }
}

impl Drop for MockAuth {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(listener: &TcpListener, inner: &Mutex<Inner>, stop: &AtomicBool, base: &str) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, inner, base),
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() >= MAX_MOCK_REQUEST {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(chunk.get(..n)?);
    };
    let head = std::str::from_utf8(buf.get(..header_end)?).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_MOCK_REQUEST);
    let mut body = buf.get(header_end.saturating_add(4)..).unwrap_or(&[]).to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(chunk.get(..n)?);
    }
    body.truncate(content_length);
    Some(Request { method, path: path.to_string(), query: query.to_string(), body: String::from_utf8(body).ok()? })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn handle(mut stream: TcpStream, inner: &Mutex<Inner>, base: &str) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let reply = match read_request(&mut stream) {
        Some(r) if r.method == "GET" && r.path == "/authorize" => authorize(&r, inner),
        Some(r) if r.method == "POST" && r.path == "/token" => token(&r, inner, base),
        Some(_) => response(404, "text/plain", "not found", None),
        None => response(400, "text/plain", "bad request", None),
    };
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn response(status: u16, content_type: &str, body: &str, location: Option<&str>) -> String {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        _ => "Not Found",
    };
    let loc = location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
    format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{loc}Connection: close\r\n\r\n{body}", body.len())
}

fn oauth_error(error: &str, description: &str) -> String {
    let body = serde_json::json!({ "error": error, "error_description": description }).to_string();
    response(400, "application/json", &body, None)
}

fn authorize(r: &Request, inner: &Mutex<Inner>) -> String {
    let q = parse_query(&r.query);
    let get = |k: &str| query_value(&q, k).unwrap_or("").to_string();
    let (client_id, redirect_uri, state, challenge) = (get("client_id"), get("redirect_uri"), get("state"), get("code_challenge"));
    if get("response_type") != "code" || client_id.is_empty() || !redirect_uri.starts_with("http://") || challenge.is_empty() {
        return response(400, "text/plain", "bad authorize request", None);
    }
    if get("code_challenge_method") != "S256" {
        return response(400, "text/plain", "code_challenge_method must be S256", None);
    }
    let mut g = lock(inner);
    let params = if let Some((error, description)) = g.deny.take() {
        encode_pairs(&[("error", &error), ("error_description", &description), ("state", &state)])
    } else {
        let code = format!("mock-code-{}", g.next());
        g.codes.insert(code.clone(), CodeGrant { client_id, redirect_uri: redirect_uri.clone(), challenge });
        encode_pairs(&[("code", &code), ("state", &state)])
    };
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    response(302, "text/plain", "redirecting", Some(&format!("{redirect_uri}{sep}{params}")))
}

fn token(r: &Request, inner: &Mutex<Inner>, base: &str) -> String {
    let f = parse_query(&r.body);
    let get = |k: &str| query_value(&f, k).unwrap_or("").to_string();
    let client_id = get("client_id");
    let mut g = lock(inner);
    g.token_requests = g.token_requests.saturating_add(1);
    match get("grant_type").as_str() {
        "authorization_code" => {
            let Some(grant) = g.codes.remove(&get("code")) else {
                return oauth_error("invalid_grant", "The authorization code is unknown or was already used.");
            };
            if grant.client_id != client_id {
                return oauth_error("invalid_client", "client_id does not match the authorization request.");
            }
            if grant.redirect_uri != get("redirect_uri") {
                return oauth_error("invalid_grant", "redirect_uri does not match the authorization request.");
            }
            let verifier = get("code_verifier");
            if !(43..=128).contains(&verifier.len()) || pkce_challenge(&verifier) != grant.challenge {
                return oauth_error("invalid_grant", "PKCE code_verifier does not match the code_challenge.");
            }
            issue(&mut g, &client_id, base)
        }
        "refresh_token" => {
            let rt = get("refresh_token");
            if g.refresh.get(&rt) != Some(&client_id) {
                return oauth_error("invalid_grant", "The refresh token is unknown, expired or revoked.");
            }
            g.refresh.remove(&rt);
            issue(&mut g, &client_id, base)
        }
        _ => oauth_error("unsupported_grant_type", "Only authorization_code and refresh_token are supported."),
    }
}

fn issue(g: &mut Inner, client_id: &str, base: &str) -> String {
    let n = g.next();
    let access = g.fixed_access.clone().unwrap_or_else(|| format!("mock-at-{n}"));
    g.access.insert(access.clone());
    let refresh = g.mint_refresh(client_id);
    let id_token = make_id_token(&g.name, &g.email, client_id, base);
    let body = serde_json::json!({
        "token_type": "Bearer",
        "expires_in": g.expires_in,
        "access_token": access,
        "refresh_token": refresh,
        "id_token": id_token,
    })
    .to_string();
    response(200, "application/json", &body, None)
}

/// An unsigned JWT (`alg: none`) carrying name/email claims.
fn make_id_token(name: &str, email: &str, aud: &str, iss: &str) -> String {
    let header = serde_json::json!({ "alg": "none", "typ": "JWT" }).to_string();
    let claims = serde_json::json!({ "iss": iss, "aud": aud, "name": name, "email": email, "preferred_username": email }).to_string();
    format!("{}.{}.", URL_SAFE_NO_PAD.encode(header), URL_SAFE_NO_PAD.encode(claims))
}

/// A minimal HTTP/1.1 response as seen by the mock browser.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) location: Option<String>,
    pub(crate) body: String,
}

/// Plain-HTTP GET of `http://host:port/path?query` with std sockets (the mock "browser").
pub(crate) fn http_get(url: &str) -> Result<HttpResponse, String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// URLs")?;
    let (host, path) = match rest.find(['/', '?']) {
        Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i..).unwrap_or("")),
        None => (rest, ""),
    };
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let addrs: Vec<SocketAddr> = host.to_socket_addrs().map_err(|e| e.to_string())?.collect();
    let mut stream = TcpStream::connect_timeout(addrs.first().ok_or("no address")?, Duration::from_secs(5))
        .or_else(|first| addrs.iter().skip(1).find_map(|a| TcpStream::connect_timeout(a, Duration::from_secs(5)).ok()).ok_or(first))
        .map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: rslook-mock-browser\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.take(1024 * 1024).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).ok_or("bad status line")?;
    let location =
        lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string());
    Ok(HttpResponse { status, location, body: body.to_string() })
}

/// Sends raw bytes to `addr` and returns whatever comes back (for hostile-input tests).
#[cfg(test)]
pub(crate) fn raw_exchange(addr: &str, bytes: &[u8]) -> Vec<u8> {
    let Ok(mut s) = TcpStream::connect(addr) else { return Vec::new() };
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = s.write_all(bytes);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    out
}
