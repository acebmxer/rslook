//! In-process fake Microsoft Graph server for tests (feature `mock`; never used by the app).
//!
//! Listens on `127.0.0.1:0` with a minimal HTTP/1.1 parser (one request per connection,
//! `Content-Length` bodies), and serves the endpoints [`crate::Client`] uses from a mutable
//! in-memory mailbox. Ids contain `/`, `+` and `=` on purpose so callers must percent-encode
//! them. Moves keep the id, like `Prefer: IdType="ImmutableId"` on the real service.
//!
//! ```ignore
//! let mock = MockGraph::start()?;
//! let inbox = mock.folder_id(WellKnown::Inbox);
//! mock.add_message(&inbox, b"Subject: hi\r\n\r\nbody", false, false);
//! let client = Client::new(&mock.base, DEFAULT_TOKEN);
//! assert_eq!(client.messages(&inbox)?.len(), 1);
//! ```

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::WellKnown;

/// Token the mock accepts until [`MockGraph::set_token`] changes it.
pub const DEFAULT_TOKEN: &str = "mock-token";
/// Mailbox owner returned by `GET /me`.
pub const MOCK_NAME: &str = "Mock User";
pub const MOCK_MAIL: &str = "mock.user@example.com";

const ROOT_ID: &str = "AAMkROOT/+root==";
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 128 * 1024 * 1024;

struct MFolder {
    id: String,
    parent: Option<String>,
    name: String,
    well_known: Option<WellKnown>,
}

struct MMessage {
    id: String,
    folder: String,
    read: bool,
    flagged: bool,
    mime: Vec<u8>,
}

struct State {
    token: String,
    folders: Vec<MFolder>,
    messages: Vec<MMessage>,
    next_id: u64,
    page_size: usize,
    throttle: u32,
    requests: u64,
    missing_prefer: u64,
    sent: Vec<Vec<u8>>,
}

impl State {
    fn new_id(&mut self, kind: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        format!("AAMk{kind}{}/+x==", self.next_id)
    }

    fn add_folder(&mut self, parent: Option<&str>, name: &str, well_known: Option<WellKnown>) -> String {
        let id = self.new_id("F");
        self.folders.push(MFolder { id: id.clone(), parent: parent.map(str::to_string), name: name.to_string(), well_known });
        id
    }

    /// A folder by id or by well-known name.
    fn resolve(&self, key: &str) -> Option<&MFolder> {
        self.folders
            .iter()
            .find(|f| f.id == key)
            .or_else(|| self.folders.iter().find(|f| f.well_known.as_ref().is_some_and(|w| w.graph_name().eq_ignore_ascii_case(key))))
    }

    fn folder_json(&self, f: &MFolder) -> Value {
        let children = self.folders.iter().filter(|c| c.parent.as_deref() == Some(f.id.as_str())).count();
        let total = self.messages.iter().filter(|m| m.folder == f.id).count();
        let unread = self.messages.iter().filter(|m| m.folder == f.id && !m.read).count();
        json!({
            "id": f.id,
            "displayName": f.name,
            "parentFolderId": f.parent.as_deref().unwrap_or(ROOT_ID),
            "childFolderCount": children,
            "totalItemCount": total,
            "unreadItemCount": unread,
            "isHidden": false,
        })
    }
}

fn message_json(m: &MMessage) -> Value {
    json!({
        "id": m.id,
        "parentFolderId": m.folder,
        "isRead": m.read,
        "flag": { "flagStatus": if m.flagged { "flagged" } else { "notFlagged" } },
    })
}

/// A running fake Graph server. Stops accepting connections when dropped.
pub struct MockGraph {
    /// Base URL to hand to [`crate::Client::new`], e.g. `http://127.0.0.1:41234/v1.0`.
    pub base: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl MockGraph {
    /// Starts the server with the seven well-known folders and no messages.
    pub fn start() -> std::io::Result<MockGraph> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let origin = format!("http://{addr}");
        let mut st = State {
            token: DEFAULT_TOKEN.to_string(),
            folders: Vec::new(),
            messages: Vec::new(),
            next_id: 0,
            page_size: 100,
            throttle: 0,
            requests: 0,
            missing_prefer: 0,
            sent: Vec::new(),
        };
        for (wk, name) in [
            (WellKnown::Inbox, "Inbox"),
            (WellKnown::Drafts, "Drafts"),
            (WellKnown::SentItems, "Sent Items"),
            (WellKnown::DeletedItems, "Deleted Items"),
            (WellKnown::Archive, "Archive"),
            (WellKnown::JunkEmail, "Junk Email"),
            (WellKnown::Outbox, "Outbox"),
        ] {
            st.add_folder(None, name, Some(wk));
        }
        let state = Arc::new(Mutex::new(st));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_state, thread_stop, thread_origin) = (Arc::clone(&state), Arc::clone(&stop), origin.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = conn else { continue };
                let (st, origin) = (Arc::clone(&thread_state), thread_origin.clone());
                std::thread::spawn(move || serve(stream, &st, &origin));
            }
        });
        Ok(MockGraph { base: format!("{origin}/v1.0"), state, stop, addr })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Changes the accepted token (a client holding the old one now gets 401).
    pub fn set_token(&self, token: &str) {
        self.lock().token = token.to_string();
    }

    /// Adds a user folder under `parent` (top level when `None`); returns its id.
    pub fn add_folder(&self, parent: Option<&str>, name: &str) -> String {
        self.lock().add_folder(parent, name, None)
    }

    /// Removes a folder, its subfolders and their messages (e.g. a mailbox without Archive).
    pub fn remove_folder(&self, id: &str) {
        let mut st = self.lock();
        let mut doomed = vec![id.to_string()];
        let mut i = 0;
        while let Some(cur) = doomed.get(i).cloned() {
            doomed.extend(st.folders.iter().filter(|f| f.parent.as_deref() == Some(cur.as_str())).map(|f| f.id.clone()));
            i += 1;
        }
        st.folders.retain(|f| !doomed.contains(&f.id));
        st.messages.retain(|m| !doomed.contains(&m.folder));
    }

    /// Id of a well-known folder (empty if it was removed).
    pub fn folder_id(&self, wk: WellKnown) -> String {
        self.lock().folders.iter().find(|f| f.well_known.as_ref() == Some(&wk)).map(|f| f.id.clone()).unwrap_or_default()
    }

    /// Every folder as (id, parent id, display name).
    pub fn folders(&self) -> Vec<(String, Option<String>, String)> {
        self.lock().folders.iter().map(|f| (f.id.clone(), f.parent.clone(), f.name.clone())).collect()
    }

    /// Stores a message; returns its id.
    pub fn add_message(&self, folder_id: &str, mime: &[u8], read: bool, flagged: bool) -> String {
        let mut st = self.lock();
        let id = st.new_id("M");
        st.messages.push(MMessage { id: id.clone(), folder: folder_id.to_string(), read, flagged, mime: mime.to_vec() });
        id
    }

    /// Messages in a folder as (id, read, flagged, MIME), in insertion order.
    pub fn messages(&self, folder_id: &str) -> Vec<(String, bool, bool, Vec<u8>)> {
        self.lock().messages.iter().filter(|m| m.folder == folder_id).map(|m| (m.id.clone(), m.read, m.flagged, m.mime.clone())).collect()
    }

    /// Changes a message server-side (as another client would); false if unknown.
    pub fn set_message_state(&self, id: &str, read: bool, flagged: bool) -> bool {
        let mut st = self.lock();
        match st.messages.iter_mut().find(|m| m.id == id) {
            Some(m) => {
                m.read = read;
                m.flagged = flagged;
                true
            }
            None => false,
        }
    }

    /// Deletes a message server-side; false if unknown.
    pub fn remove_message(&self, id: &str) -> bool {
        let mut st = self.lock();
        let before = st.messages.len();
        st.messages.retain(|m| m.id != id);
        st.messages.len() != before
    }

    /// Decoded MIME of every message received through `sendMail`.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.lock().sent.clone()
    }

    /// Answer the next `n` requests with 429 and `Retry-After: 0`.
    pub fn throttle_next(&self, n: u32) {
        self.lock().throttle = n;
    }

    /// Items per listing page (minimum 1).
    pub fn set_page_size(&self, n: usize) {
        self.lock().page_size = n.max(1);
    }

    /// Requests received so far (throttled and unauthorized ones included).
    pub fn request_count(&self) -> u64 {
        self.lock().requests
    }

    /// Requests that arrived without `Prefer: IdType="ImmutableId"`.
    pub fn requests_missing_immutable_id(&self) -> u64 {
        self.lock().missing_prefer
    }
}

impl Drop for MockGraph {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

struct Resp {
    status: u16,
    headers: Vec<(&'static str, String)>,
    content_type: &'static str,
    body: Vec<u8>,
}

fn json_resp(status: u16, v: &Value) -> Resp {
    Resp { status, headers: Vec::new(), content_type: "application/json", body: v.to_string().into_bytes() }
}

fn error_resp(status: u16, code: &str, message: &str) -> Resp {
    json_resp(status, &json!({ "error": { "code": code, "message": message } }))
}

fn empty_resp(status: u16) -> Resp {
    Resp { status, headers: Vec::new(), content_type: "text/plain", body: Vec::new() }
}

fn serve(mut stream: TcpStream, state: &Mutex<State>, origin: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let resp = match read_request(&mut stream) {
        Some(req) => handle(&req, &mut lock(state), origin),
        None => error_resp(400, "BadRequest", "malformed HTTP request"),
    };
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(chunk.get(..n)?);
    };
    let head = String::from_utf8_lossy(buf.get(..head_end)?).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = buf.get(head_end.saturating_add(4)..)?.to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(chunk.get(..n)?);
    }
    body.truncate(len);
    Some(Request { method, target, headers, body })
}

fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(hex) = s.get(i.saturating_add(1)..i.saturating_add(3))
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One listing page plus `@odata.nextLink` (a `$skip` on the same path) when more remain.
fn page(st: &State, origin: &str, raw_path: &str, query: &str, items: Vec<Value>) -> Resp {
    let skip: usize = query.split('&').find_map(|p| p.strip_prefix("$skip=")).and_then(|v| v.parse().ok()).unwrap_or(0);
    let end = skip.saturating_add(st.page_size).min(items.len());
    let slice: Vec<Value> = items.get(skip.min(end)..end).map(<[Value]>::to_vec).unwrap_or_default();
    let mut out = serde_json::Map::new();
    out.insert("value".into(), Value::Array(slice));
    if end < items.len() {
        let mut params: Vec<&str> = query.split('&').filter(|p| !p.is_empty() && !p.starts_with("$skip=")).collect();
        let skip_param = format!("$skip={end}");
        params.push(&skip_param);
        out.insert("@odata.nextLink".into(), Value::String(format!("{origin}{raw_path}?{}", params.join("&"))));
    }
    json_resp(200, &Value::Object(out))
}

fn handle(req: &Request, st: &mut State, origin: &str) -> Resp {
    st.requests = st.requests.saturating_add(1);
    if req.header("prefer").is_none_or(|p| !p.contains("IdType=\"ImmutableId\"")) {
        st.missing_prefer = st.missing_prefer.saturating_add(1);
    }
    if st.throttle > 0 {
        st.throttle -= 1;
        let mut r = error_resp(429, "TooManyRequests", "Please retry later.");
        r.headers.push(("Retry-After", "0".to_string()));
        return r;
    }
    let expected = format!("Bearer {}", st.token);
    if req.header("authorization") != Some(expected.as_str()) {
        return error_resp(401, "InvalidAuthenticationToken", "Access token is empty or invalid.");
    }
    let (raw_path, query) = req.target.split_once('?').unwrap_or((req.target.as_str(), ""));
    let Some(rest) = raw_path.strip_prefix("/v1.0/") else {
        return error_resp(404, "ResourceNotFound", "unknown path");
    };
    let segs: Vec<String> = rest.split('/').map(pct_decode).collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["me"]) => json_resp(200, &json!({ "displayName": MOCK_NAME, "mail": MOCK_MAIL, "userPrincipalName": MOCK_MAIL })),
        ("GET", ["me", "mailFolders"]) => {
            let items = st.folders.iter().filter(|f| f.parent.is_none()).map(|f| st.folder_json(f)).collect();
            page(st, origin, raw_path, query, items)
        }
        ("POST", ["me", "mailFolders"]) => create_folder(st, None, &body),
        ("GET", ["me", "mailFolders", key]) => match st.resolve(key) {
            Some(f) => json_resp(200, &st.folder_json(f)),
            None => error_resp(404, "ErrorFolderNotFound", "The specified folder could not be found in the store."),
        },
        ("GET", ["me", "mailFolders", key, "childFolders"]) => match st.resolve(key) {
            Some(parent) => {
                let pid = parent.id.clone();
                let items = st.folders.iter().filter(|f| f.parent.as_deref() == Some(pid.as_str())).map(|f| st.folder_json(f)).collect();
                page(st, origin, raw_path, query, items)
            }
            None => error_resp(404, "ErrorFolderNotFound", "The specified folder could not be found in the store."),
        },
        ("POST", ["me", "mailFolders", key, "childFolders"]) => match st.resolve(key).map(|f| f.id.clone()) {
            Some(pid) => create_folder(st, Some(&pid), &body),
            None => error_resp(404, "ErrorFolderNotFound", "The specified folder could not be found in the store."),
        },
        ("GET", ["me", "mailFolders", key, "messages"]) => match st.resolve(key).map(|f| f.id.clone()) {
            Some(fid) => {
                let items = st.messages.iter().filter(|m| m.folder == fid).map(message_json).collect();
                page(st, origin, raw_path, query, items)
            }
            None => error_resp(404, "ErrorFolderNotFound", "The specified folder could not be found in the store."),
        },
        ("GET", ["me", "messages", id]) => match st.messages.iter().find(|m| m.id == *id) {
            Some(m) => json_resp(200, &message_json(m)),
            None => item_not_found(),
        },
        ("GET", ["me", "messages", id, "$value"]) => match st.messages.iter().find(|m| m.id == *id) {
            Some(m) => Resp { status: 200, headers: Vec::new(), content_type: "text/plain", body: m.mime.clone() },
            None => item_not_found(),
        },
        ("PATCH", ["me", "messages", id]) => {
            let Some(m) = st.messages.iter_mut().find(|m| m.id == *id) else { return item_not_found() };
            if let Some(read) = body.get("isRead").and_then(Value::as_bool) {
                m.read = read;
            }
            if let Some(status) = body.get("flag").and_then(|f| f.get("flagStatus")).and_then(Value::as_str) {
                m.flagged = status == "flagged";
            }
            json_resp(200, &message_json(m))
        }
        ("POST", ["me", "messages", id, "move"]) => {
            let dest = body.get("destinationId").and_then(Value::as_str).unwrap_or_default();
            let Some(dest) = st.resolve(dest).map(|f| f.id.clone()) else {
                return error_resp(404, "ErrorFolderNotFound", "The specified folder could not be found in the store.");
            };
            let Some(m) = st.messages.iter_mut().find(|m| m.id == *id) else { return item_not_found() };
            m.folder = dest;
            json_resp(201, &message_json(m))
        }
        ("POST", ["me", "messages", id, "permanentDelete"]) => {
            let before = st.messages.len();
            st.messages.retain(|m| m.id != *id);
            if st.messages.len() == before { item_not_found() } else { empty_resp(204) }
        }
        ("POST", ["me", "sendMail"]) => {
            let text: Vec<u8> = req.body.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            let Ok(mime) = base64::engine::general_purpose::STANDARD.decode(&text) else {
                return error_resp(400, "ErrorMimeContentInvalidBase64String", "Invalid base64 string for MIME content.");
            };
            let sent_id = st.folders.iter().find(|f| f.well_known == Some(WellKnown::SentItems)).map(|f| f.id.clone()).unwrap_or_default();
            let id = st.new_id("M");
            st.messages.push(MMessage { id, folder: sent_id, read: true, flagged: false, mime: mime.clone() });
            st.sent.push(mime);
            empty_resp(202)
        }
        (_, ["me", ..]) => error_resp(405, "MethodNotAllowed", "unsupported request"),
        _ => error_resp(404, "ResourceNotFound", "unknown path"),
    }
}

fn item_not_found() -> Resp {
    error_resp(404, "ErrorItemNotFound", "The specified object was not found in the store.")
}

fn create_folder(st: &mut State, parent: Option<&str>, body: &Value) -> Resp {
    let Some(name) = body.get("displayName").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) else {
        return error_resp(400, "ErrorInvalidRequest", "displayName is required.");
    };
    if st.folders.iter().any(|f| f.parent.as_deref() == parent && f.name.eq_ignore_ascii_case(name)) {
        return error_resp(409, "ErrorFolderExists", "A folder with the specified name already exists.");
    }
    let id = st.add_folder(parent, name, None);
    match st.folders.iter().find(|f| f.id == id) {
        Some(f) => json_resp(201, &st.folder_json(f)),
        None => error_resp(500, "InternalError", "folder vanished"),
    }
}
