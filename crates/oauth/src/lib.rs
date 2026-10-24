#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Microsoft 365 / Outlook.com sign-in for a desktop public client (no client secret).
//!
//! OAuth 2.0 authorization code flow with PKCE S256 (RFC 7636) and a loopback redirect
//! (RFC 8252), as described by the Microsoft identity platform v2.0 documentation. The
//! token endpoint is reached with a blocking `ureq` POST (form-urlencoded).
//!
//! Tokens are never logged and never put in error messages.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::Digest as _;

mod loopback;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

#[cfg(test)]
mod tests;

/// Largest token endpoint response read before JSON parsing.
const MAX_TOKEN_RESPONSE: u64 = 1024 * 1024;
/// Overall time limit for one token endpoint request.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest server-provided error text kept in a [`OAuthError::Denied`] message.
const MAX_ERROR_TEXT: usize = 400;

/// An OAuth 2.0 authorization server and the app registration used with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
}

impl Provider {
    /// login.microsoftonline.com/{tenant}/oauth2/v2.0/{authorize,token}; tenant usually "common".
    pub fn microsoft(client_id: &str, tenant: &str, scopes: &[&str]) -> Provider {
        let base = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0");
        Provider {
            authorize_url: format!("{base}/authorize"),
            token_url: format!("{base}/token"),
            client_id: client_id.to_string(),
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn scope_param(&self) -> String {
        self.scopes.join(" ")
    }
}

/// Scopes for Microsoft Graph mail access.
pub const GRAPH_SCOPES: &[&str] = &[
    "offline_access",
    "openid",
    "profile",
    "email",
    "https://graph.microsoft.com/User.Read",
    "https://graph.microsoft.com/Mail.ReadWrite",
    "https://graph.microsoft.com/Mail.Send",
];

/// Scopes for IMAP and SMTP with XOAUTH2 against Outlook / Exchange Online.
pub const OUTLOOK_IMAP_SCOPES: &[&str] =
    &["offline_access", "openid", "profile", "email", "https://outlook.office.com/IMAP.AccessAsUser.All", "https://outlook.office.com/SMTP.Send"];

/// Tokens returned by the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    /// Present when the server issued (or rotated) a refresh token; keep the old one otherwise.
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds (0 when the server did not say).
    pub expires_in: u64,
    pub id_token: Option<String>,
}

/// Why a sign-in or refresh failed. Messages are meant for end users.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("Sign-in was cancelled.")]
    Cancelled,
    #[error("Sign-in timed out: the browser did not come back to Rslook in time.")]
    TimedOut,
    /// `error` / `error_description` from the redirect or the token endpoint (e.g. invalid_grant).
    #[error("Sign-in was refused: {0}")]
    Denied(String),
    #[error("Could not reach the sign-in server: {0}")]
    Network(String),
    #[error("The sign-in server sent an unexpected reply: {0}")]
    BadResponse(String),
    #[error("Sign-in failed: {0}")]
    Io(String),
}

/// Interactive sign-in: binds 127.0.0.1:0, opens the authorize URL in the browser, waits for
/// one redirect carrying the matching state, then exchanges the code for tokens.
///
/// Returns [`OAuthError::Cancelled`] soon after `cancel` becomes true and
/// [`OAuthError::TimedOut`] after `timeout`. The final token request itself is bounded by
/// its own 30 s limit and does not watch `cancel`.
pub fn sign_in(
    p: &Provider,
    open_browser: &dyn Fn(&str) -> Result<(), String>,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Tokens, OAuthError> {
    let server = loopback::Server::bind()?;
    let redirect_uri = format!("http://localhost:{}", server.port());
    let state = random_token(32)?;
    let verifier = random_token(64)?;
    let url = authorize_url(p, &redirect_uri, &state, &pkce_challenge(&verifier));
    open_browser(&url).map_err(|e| OAuthError::Io(format!("could not open the web browser ({e})")))?;
    let code = server.wait_for_code(&state, timeout, cancel)?;
    drop(server);
    let scope = p.scope_param();
    let form = [
        ("client_id", p.client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier.as_str()),
        ("scope", scope.as_str()),
    ];
    token_request(&p.token_url, &form)
}

/// Refresh-token grant. Microsoft may rotate the refresh token: callers keep the new one when present.
pub fn refresh(p: &Provider, refresh_token: &str) -> Result<Tokens, OAuthError> {
    let scope = p.scope_param();
    let form = [("client_id", p.client_id.as_str()), ("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("scope", scope.as_str())];
    token_request(&p.token_url, &form)
}

/// Display name and email from an id_token's payload (claims `name`, then `email` or
/// `preferred_username`). Not a security check: the token came straight from the token
/// endpoint over TLS. The name is empty when the claim is missing; `None` on anything
/// malformed or when neither `email` nor `preferred_username` is present.
pub fn id_token_identity(id_token: &str) -> Option<(String, String)> {
    const MAX_ID_TOKEN: usize = 64 * 1024;
    if id_token.len() > MAX_ID_TOKEN {
        return None;
    }
    let mut parts = id_token.split('.');
    let (_header, payload, _sig) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let claims = claims.as_object()?;
    let text = |k: &str| claims.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let email = text("email").or_else(|| text("preferred_username"))?;
    let name = text("name").unwrap_or("");
    Some((name.to_string(), email.to_string()))
}

/// SASL XOAUTH2 initial response for IMAP/SMTP (raw, not base64).
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// `n` random bytes as base64url without padding (only RFC 3986 unreserved characters).
pub(crate) fn random_token(n: usize) -> Result<String, OAuthError> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| OAuthError::Io(format!("no secure random numbers available ({e})")))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

/// PKCE S256: BASE64URL(SHA256(ASCII(code_verifier))).
pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn authorize_url(p: &Provider, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let scope = p.scope_param();
    let params = [
        ("client_id", p.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", scope.as_str()),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("prompt", "select_account"),
    ];
    let sep = if p.authorize_url.contains('?') { '&' } else { '?' };
    format!("{}{sep}{}", p.authorize_url, encode_pairs(&params))
}

/// `k=v&k=v` with every value percent-encoded.
pub(crate) fn encode_pairs(pairs: &[(&str, &str)]) -> String {
    let v: Vec<String> = pairs.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect();
    v.join("&")
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn percent_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// Decodes `application/x-www-form-urlencoded` text (`+` is a space). A `%` not followed by
/// two hex digits is kept literally. `None` when the result is not UTF-8.
pub(crate) fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|c| char::from(*c).to_digit(16));
        match b {
            b'+' => {
                out.push(b' ');
                i = i.saturating_add(1);
            }
            b'%' => match (hex(i.saturating_add(1)), hex(i.saturating_add(2))) {
                (Some(h), Some(l)) => {
                    out.push(u8::try_from(h * 16 + l).ok()?);
                    i = i.saturating_add(3);
                }
                _ => {
                    out.push(b'%');
                    i = i.saturating_add(1);
                }
            },
            _ => {
                out.push(b);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Splits a query string into decoded pairs (pairs that fail to decode are skipped).
pub(crate) fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

/// First value for `key` in decoded pairs.
pub(crate) fn query_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Keeps at most [`MAX_ERROR_TEXT`] characters of server-provided text, without control characters.
fn clip(s: &str) -> String {
    let cleaned: String = s.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    match cleaned.char_indices().nth(MAX_ERROR_TEXT) {
        Some((i, _)) => format!("{}…", cleaned.get(..i).unwrap_or("")),
        None => cleaned,
    }
}

/// `error` plus `error_description` as one readable line.
pub(crate) fn denied_text(error: &str, description: Option<&str>) -> String {
    match description.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => clip(&format!("{error}: {d}")),
        None => clip(error),
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn token_request(url: &str, form: &[(&str, &str)]) -> Result<Tokens, OAuthError> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(TOKEN_TIMEOUT)).max_redirects(0).build().into();
    let resp =
        agent.post(url).header("Accept", "application/json").send_form(form.iter().copied()).map_err(|e| OAuthError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp.into_body().into_with_config().limit(MAX_TOKEN_RESPONSE).read_to_string().map_err(|e| match e {
        ureq::Error::BodyExceedsLimit(_) => OAuthError::BadResponse("the reply was too large".into()),
        other => OAuthError::Network(other.to_string()),
    })?;
    parse_token_response(status, &body)
}

/// Maps a token endpoint reply (status + body) to tokens or an error.
pub(crate) fn parse_token_response(status: u16, body: &str) -> Result<Tokens, OAuthError> {
    let parsed: Option<TokenResponse> = serde_json::from_str(body).ok();
    if let Some(error) = parsed.as_ref().and_then(|r| r.error.as_deref()).filter(|e| !e.trim().is_empty()) {
        let desc = parsed.as_ref().and_then(|r| r.error_description.as_deref());
        return Err(OAuthError::Denied(denied_text(error, desc)));
    }
    if !(200..300).contains(&status) {
        return Err(OAuthError::BadResponse(format!("HTTP status {status}")));
    }
    let r = parsed.ok_or_else(|| OAuthError::BadResponse("the reply was not valid JSON".into()))?;
    let access_token = r.access_token.filter(|t| !t.is_empty()).ok_or_else(|| OAuthError::BadResponse("no access token in the reply".into()))?;
    let expires_in = match r.expires_in {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };
    Ok(Tokens { access_token, refresh_token: r.refresh_token.filter(|t| !t.is_empty()), expires_in, id_token: r.id_token.filter(|t| !t.is_empty()) })
}
