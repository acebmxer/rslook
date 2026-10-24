#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Blocking Microsoft Graph v1.0 mail client for Microsoft 365 / Outlook.com accounts.
//!
//! Written from Microsoft's public Graph documentation (mail API overview). Every request
//! carries `Authorization: Bearer <token>` and `Prefer: IdType="ImmutableId"`, so message ids
//! survive moves between folders.
//!
//! The server is treated as hostile: every body is size-capped, paging is bounded and loops
//! are detected, `@odata.nextLink` is only followed on the base URL's own scheme and host (the
//! token is never sent anywhere else), redirects are never followed, and the token never
//! appears in an error.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

#[cfg(any(test, feature = "mock"))]
pub mod mock;
#[cfg(test)]
mod tests;

/// Production Graph endpoint.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
/// Largest JSON response accepted.
pub const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
/// Largest raw message (`$value`) accepted.
pub const MAX_MIME_BYTES: usize = 64 * 1024 * 1024;
/// Most pages fetched for one listing (and, plus [`MAX_FOLDERS`], for one folder tree).
pub const MAX_PAGES: usize = 1000;
/// Most items returned by one listing.
pub const MAX_ITEMS: usize = 200_000;
/// Most folders in one mailbox tree.
pub const MAX_FOLDERS: usize = 10_000;
/// Retries after 429 / 503 before giving up.
pub const MAX_RETRIES: u32 = 3;
/// Longest single wait between retries, whatever `Retry-After` says.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

const MAX_ERROR_BODY: usize = 64 * 1024;
const PREFER: &str = "IdType=\"ImmutableId\"";
const PAGE: &str = "$top=100";

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// HTTP 401: the access token is expired or revoked; refresh it and retry.
    #[error("Microsoft Graph: not authorized (access token expired or revoked)")]
    Unauthorized,
    /// Any other non-success status, with Graph's `error.code` / `error.message` when present.
    #[error("Microsoft Graph: HTTP {status} {code}: {message}")]
    Http { status: u16, code: String, message: String },
    #[error("Microsoft Graph: network error: {0}")]
    Network(String),
    #[error("Microsoft Graph: bad response: {0}")]
    BadResponse(String),
    /// A response body exceeded the given cap in bytes.
    #[error("Microsoft Graph: response larger than {0} bytes")]
    TooLarge(usize),
}

/// Well-known mail folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WellKnown {
    Inbox,
    Drafts,
    SentItems,
    DeletedItems,
    Archive,
    JunkEmail,
    Outbox,
}

impl WellKnown {
    pub const ALL: [WellKnown; 7] = [
        WellKnown::Inbox,
        WellKnown::Drafts,
        WellKnown::SentItems,
        WellKnown::DeletedItems,
        WellKnown::Archive,
        WellKnown::JunkEmail,
        WellKnown::Outbox,
    ];

    /// The Graph well-known folder name (usable in place of a folder id).
    pub fn graph_name(&self) -> &'static str {
        match self {
            WellKnown::Inbox => "inbox",
            WellKnown::Drafts => "drafts",
            WellKnown::SentItems => "sentitems",
            WellKnown::DeletedItems => "deleteditems",
            WellKnown::Archive => "archive",
            WellKnown::JunkEmail => "junkemail",
            WellKnown::Outbox => "outbox",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    /// `None` for top-level folders (children of the mailbox root).
    pub parent_id: Option<String>,
    pub display_name: String,
    pub well_known: Option<WellKnown>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageState {
    pub id: String,
    pub is_read: bool,
    pub flagged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub display_name: String,
    /// `mail`, else `userPrincipalName`.
    pub email: String,
}

#[derive(Clone, Copy)]
enum Verb {
    Get,
    Post,
    Patch,
}

/// Blocking Graph client. Cheap to build; one per access token.
pub struct Client {
    base: String,
    origin: Option<(String, String)>,
    token: String,
    agent: ureq::Agent,
    json_max: usize,
    mime_max: usize,
    max_retries: u32,
    max_wait: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("token", &"[redacted]").finish_non_exhaustive()
    }
}

fn make_agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(30)))
        .build();
    ureq::Agent::new_with_config(config)
}

impl Client {
    /// `base` is [`GRAPH_BASE`] in production (a trailing `/` is ignored).
    pub fn new(base: &str, access_token: &str) -> Client {
        let base = base.trim_end_matches('/').to_string();
        Client {
            origin: origin(&base),
            base,
            token: access_token.to_string(),
            agent: make_agent(Duration::from_secs(600)),
            json_max: MAX_JSON_BYTES,
            mime_max: MAX_MIME_BYTES,
            max_retries: MAX_RETRIES,
            max_wait: MAX_RETRY_WAIT,
        }
    }

    /// Override the 429/503 retry policy: retries after the first try, longest single wait.
    pub fn with_retry(mut self, max_retries: u32, max_wait: Duration) -> Client {
        self.max_retries = max_retries;
        self.max_wait = max_wait;
        self
    }

    /// Override the response caps (JSON, raw MIME) in bytes.
    pub fn with_limits(mut self, json_max: usize, mime_max: usize) -> Client {
        self.json_max = json_max;
        self.mime_max = mime_max;
        self
    }

    /// Override the whole-request timeout (default 10 minutes).
    pub fn with_timeout(mut self, timeout: Duration) -> Client {
        self.agent = make_agent(timeout);
        self
    }

    /// `GET /me`.
    pub fn me(&self) -> Result<Me, GraphError> {
        let v = self.get_json(&format!("{}/me?$select=displayName,mail,userPrincipalName", self.base))?;
        let display_name = str_field(&v, "displayName").unwrap_or_default().to_string();
        let email = str_field(&v, "mail")
            .filter(|s| !s.is_empty())
            .or_else(|| str_field(&v, "userPrincipalName").filter(|s| !s.is_empty()))
            .ok_or_else(|| GraphError::BadResponse("no mail or userPrincipalName".into()))?
            .to_string();
        Ok(Me { display_name, email })
    }

    /// Every mail folder, top-level first then children breadth-first, with well-known folders marked.
    pub fn folders(&self) -> Result<Vec<Folder>, GraphError> {
        let mut pages_left = MAX_PAGES.saturating_add(MAX_FOLDERS);
        let mut out: Vec<Folder> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let top = self.get_paged(format!("{}/me/mailFolders?includeHiddenFolders=false&{PAGE}", self.base), MAX_FOLDERS, &mut pages_left)?;
        push_folders(top, None, &mut out, &mut seen, &mut queue)?;
        while let Some(parent) = queue.pop_front() {
            let url = format!("{}/me/mailFolders/{}/childFolders?includeHiddenFolders=false&{PAGE}", self.base, enc(&parent));
            let kids = self.get_paged(url, MAX_FOLDERS, &mut pages_left)?;
            push_folders(kids, Some(&parent), &mut out, &mut seen, &mut queue)?;
        }
        for wk in WellKnown::ALL {
            let url = format!("{}/me/mailFolders/{}?$select=id", self.base, wk.graph_name());
            let id = match self.get_json(&url) {
                Ok(v) => str_field(&v, "id").map(str::to_string),
                Err(GraphError::Http { status: 404, .. }) => None,
                Err(e) => return Err(e),
            };
            if let Some(id) = id
                && let Some(f) = out.iter_mut().find(|f| f.id == id)
            {
                f.well_known = Some(wk);
            }
        }
        Ok(out)
    }

    /// Id, read and flag state of every message in a folder.
    pub fn messages(&self, folder_id: &str) -> Result<Vec<MessageState>, GraphError> {
        let mut pages_left = MAX_PAGES;
        let url = format!("{}/me/mailFolders/{}/messages?$select=id,isRead,flag&{PAGE}", self.base, enc(folder_id));
        let items = self.get_paged(url, MAX_ITEMS, &mut pages_left)?;
        items.iter().map(message_state).collect()
    }

    /// The message as RFC 5322 bytes.
    pub fn message_mime(&self, id: &str) -> Result<Vec<u8>, GraphError> {
        self.call(Verb::Get, &format!("{}/me/messages/{}/$value", self.base, enc(id)), None, self.mime_max)
    }

    pub fn set_flags(&self, id: &str, read: bool, flagged: bool) -> Result<(), GraphError> {
        let status = if flagged { "flagged" } else { "notFlagged" };
        let body = json!({ "isRead": read, "flag": { "flagStatus": status } });
        self.call_json(Verb::Patch, &format!("{}/me/messages/{}", self.base, enc(id)), &body)?;
        Ok(())
    }

    /// Moves a message; returns its id afterwards (unchanged with immutable ids).
    pub fn move_message(&self, id: &str, destination_folder_id: &str) -> Result<String, GraphError> {
        let body = json!({ "destinationId": destination_folder_id });
        let v = self.call_json(Verb::Post, &format!("{}/me/messages/{}/move", self.base, enc(id)), &body)?;
        str_field(&v, "id").map(str::to_string).ok_or_else(|| GraphError::BadResponse("moved message has no id".into()))
    }

    pub fn create_folder(&self, parent_id: Option<&str>, name: &str) -> Result<Folder, GraphError> {
        let url = match parent_id {
            Some(p) => format!("{}/me/mailFolders/{}/childFolders", self.base, enc(p)),
            None => format!("{}/me/mailFolders", self.base),
        };
        let v = self.call_json(Verb::Post, &url, &json!({ "displayName": name }))?;
        let id = str_field(&v, "id").ok_or_else(|| GraphError::BadResponse("new folder has no id".into()))?;
        Ok(Folder {
            id: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            display_name: str_field(&v, "displayName").unwrap_or(name).to_string(),
            well_known: None,
        })
    }

    pub fn delete_permanently(&self, id: &str) -> Result<(), GraphError> {
        self.call(Verb::Post, &format!("{}/me/messages/{}/permanentDelete", self.base, enc(id)), None, self.json_max)?;
        Ok(())
    }

    /// Sends a complete RFC 5322 message; Graph saves a copy to Sent Items.
    pub fn send_mime(&self, mime: &[u8]) -> Result<(), GraphError> {
        let body = base64::engine::general_purpose::STANDARD.encode(mime);
        self.call(Verb::Post, &format!("{}/me/sendMail", self.base), Some(("text/plain", body.as_bytes())), self.json_max)?;
        Ok(())
    }

    // ---- plumbing ----

    fn get_json(&self, url: &str) -> Result<Value, GraphError> {
        parse_json(&self.call(Verb::Get, url, None, self.json_max)?)
    }

    fn call_json(&self, verb: Verb, url: &str, body: &Value) -> Result<Value, GraphError> {
        let bytes = serde_json::to_vec(body).map_err(|e| GraphError::BadResponse(e.to_string()))?;
        let resp = self.call(verb, url, Some(("application/json", &bytes)), self.json_max)?;
        if resp.iter().all(u8::is_ascii_whitespace) { Ok(Value::Null) } else { parse_json(&resp) }
    }

    /// Follows `@odata.nextLink` from `first`, collecting every `value` entry.
    fn get_paged(&self, first: String, max_items: usize, pages_left: &mut usize) -> Result<Vec<Value>, GraphError> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        let mut url = first;
        seen.insert(url.clone());
        loop {
            if *pages_left == 0 {
                return Err(GraphError::BadResponse("too many pages".into()));
            }
            *pages_left -= 1;
            let Value::Object(mut page) = self.get_json(&url)? else {
                return Err(GraphError::BadResponse("page is not a JSON object".into()));
            };
            let Some(Value::Array(values)) = page.remove("value") else {
                return Err(GraphError::BadResponse("page has no value array".into()));
            };
            if items.len().saturating_add(values.len()) > max_items {
                return Err(GraphError::BadResponse(format!("more than {max_items} items")));
            }
            items.extend(values);
            match page.remove("@odata.nextLink") {
                None | Some(Value::Null) => return Ok(items),
                Some(Value::String(next)) => {
                    let next_origin = origin(&next);
                    if next_origin.is_none() || next_origin != self.origin {
                        return Err(GraphError::BadResponse("nextLink points to another host; not followed".into()));
                    }
                    if !seen.insert(next.clone()) {
                        return Err(GraphError::BadResponse("nextLink repeats an earlier page".into()));
                    }
                    url = next;
                }
                Some(_) => return Err(GraphError::BadResponse("nextLink is not a string".into())),
            }
        }
    }

    /// One request with 429/503 retries; returns the success body (capped at `limit`).
    fn call(&self, verb: Verb, url: &str, body: Option<(&str, &[u8])>, limit: usize) -> Result<Vec<u8>, GraphError> {
        let mut attempt: u32 = 0;
        loop {
            let resp = self.send_once(verb, url, body).map_err(|e| GraphError::Network(self.scrub(e.to_string())))?;
            let status = resp.status().as_u16();
            if (status == 429 || status == 503) && attempt < self.max_retries {
                attempt = attempt.saturating_add(1);
                let backoff = Duration::from_secs(1u64 << attempt.min(5));
                let wait = retry_after(&resp).unwrap_or(backoff).min(self.max_wait);
                drop(resp);
                std::thread::sleep(wait);
                continue;
            }
            if (200..300).contains(&status) {
                return self.read_body(resp, limit);
            }
            if status == 401 {
                return Err(GraphError::Unauthorized);
            }
            let text = self.read_body(resp, MAX_ERROR_BODY).unwrap_or_default();
            return Err(self.http_error(status, &text));
        }
    }

    fn send_once(&self, verb: Verb, url: &str, body: Option<(&str, &[u8])>) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let auth = format!("Bearer {}", self.token);
        match verb {
            Verb::Get => {
                self.agent.get(url).header("Authorization", auth.as_str()).header("Prefer", PREFER).header("Accept", "application/json").call()
            }
            Verb::Post | Verb::Patch => {
                let rb = if matches!(verb, Verb::Post) { self.agent.post(url) } else { self.agent.patch(url) };
                let rb = rb.header("Authorization", auth.as_str()).header("Prefer", PREFER).header("Accept", "application/json");
                match body {
                    Some((content_type, bytes)) => rb.header("Content-Type", content_type).send(bytes),
                    None => rb.send_empty(),
                }
            }
        }
    }

    fn read_body(&self, mut resp: ureq::http::Response<ureq::Body>, limit: usize) -> Result<Vec<u8>, GraphError> {
        // One byte over the cap tells "exactly the cap" apart from "too large".
        let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        match resp.body_mut().with_config().limit(cap).read_to_vec() {
            Ok(v) if v.len() > limit => Err(GraphError::TooLarge(limit)),
            Ok(v) => Ok(v),
            Err(ureq::Error::BodyExceedsLimit(_)) => Err(GraphError::TooLarge(limit)),
            Err(e) => Err(GraphError::Network(self.scrub(e.to_string()))),
        }
    }

    fn http_error(&self, status: u16, body: &[u8]) -> GraphError {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let err = parsed.as_ref().and_then(|v| v.get("error"));
        let code: String = err.and_then(|e| str_field(e, "code")).unwrap_or_default().chars().take(200).collect();
        let message: String = match err.and_then(|e| str_field(e, "message")) {
            Some(m) => m.chars().take(2000).collect(),
            None => String::from_utf8_lossy(body).chars().take(300).collect(),
        };
        GraphError::Http { status, code: self.scrub(code), message: self.scrub(message) }
    }

    /// Belt and braces: never let the token leak into an error string.
    fn scrub(&self, s: String) -> String {
        if self.token.is_empty() || !s.contains(&self.token) { s } else { s.replace(&self.token, "[redacted]") }
    }
}

fn push_folders(
    items: Vec<Value>,
    parent: Option<&str>,
    out: &mut Vec<Folder>,
    seen: &mut HashSet<String>,
    queue: &mut VecDeque<String>,
) -> Result<(), GraphError> {
    for v in items {
        let id = str_field(&v, "id").ok_or_else(|| GraphError::BadResponse("folder has no id".into()))?;
        if !seen.insert(id.to_string()) {
            continue; // A folder listed twice (or a cycle) is taken once.
        }
        if out.len() >= MAX_FOLDERS {
            return Err(GraphError::BadResponse(format!("more than {MAX_FOLDERS} folders")));
        }
        if v.get("childFolderCount").and_then(Value::as_u64).unwrap_or(0) > 0 {
            queue.push_back(id.to_string());
        }
        out.push(Folder {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            display_name: str_field(&v, "displayName").unwrap_or_default().to_string(),
            well_known: None,
        });
    }
    Ok(())
}

fn message_state(v: &Value) -> Result<MessageState, GraphError> {
    let id = str_field(v, "id").ok_or_else(|| GraphError::BadResponse("message has no id".into()))?;
    let flagged = v.get("flag").and_then(|f| str_field(f, "flagStatus")) == Some("flagged");
    Ok(MessageState { id: id.to_string(), is_read: v.get("isRead").and_then(Value::as_bool).unwrap_or(false), flagged })
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn parse_json(bytes: &[u8]) -> Result<Value, GraphError> {
    serde_json::from_slice(bytes).map_err(|e| GraphError::BadResponse(format!("invalid JSON: {e}")))
}

fn retry_after(resp: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    let secs: u64 = resp.headers().get("retry-after")?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

/// Lower-cased (scheme, authority) of an absolute URL.
fn origin(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end)?;
    if scheme.is_empty() || authority.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), authority.to_ascii_lowercase()))
}

/// Percent-encodes everything but RFC 3986 unreserved characters, for one URL path segment.
pub(crate) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_mul(3));
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
    out
}

fn hex_digit(n: u8) -> char {
    char::from(if n < 10 { b'0' + n } else { b'A' + (n - 10) })
}
