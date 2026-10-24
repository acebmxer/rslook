//! The loopback redirect receiver (RFC 8252 section 7.3).
//!
//! Every byte that reaches this listener is hostile: any local process or web page can send
//! requests to it. It reads at most [`MAX_REQUEST`] bytes per connection, looks only at the
//! request line, and answers every request with the same small page.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{OAuthError, denied_text, parse_query, query_value};

/// Most bytes read from one connection.
pub(crate) const MAX_REQUEST: usize = 8 * 1024;
/// A connection that has not sent its request line by then is dropped.
const CONN_TIMEOUT: Duration = Duration::from_secs(5);
/// Most connections waiting for their request line at once (oldest dropped first).
const MAX_PENDING: usize = 16;
/// Sleep between polls when nothing happened.
const POLL: Duration = Duration::from_millis(50);

/// What a request to the redirect URI carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Callback {
    Code {
        code: String,
        state: String,
    },
    Error {
        error: String,
        description: Option<String>,
        state: Option<String>,
    },
    /// Anything else (favicon, junk, a bare `/`).
    Ignore,
}

/// Parses the request line of a raw HTTP request. Only `GET /?...` is of interest.
pub(crate) fn parse_request(raw: &[u8]) -> Callback {
    let end = raw.iter().position(|&b| b == b'\n').unwrap_or(raw.len());
    let Some(line) = raw.get(..end).and_then(|l| std::str::from_utf8(l).ok()) else {
        return Callback::Ignore;
    };
    let mut parts = line.trim_end_matches('\r').split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Callback::Ignore;
    };
    if method != "GET" || !version.starts_with("HTTP/") {
        return Callback::Ignore;
    }
    let target = target.split('#').next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return Callback::Ignore;
    }
    let pairs = parse_query(query);
    let state = query_value(&pairs, "state").map(str::to_string);
    if let Some(error) = query_value(&pairs, "error").filter(|e| !e.is_empty()) {
        let description = query_value(&pairs, "error_description").map(str::to_string);
        return Callback::Error { error: error.to_string(), description, state };
    }
    match (query_value(&pairs, "code").filter(|c| !c.is_empty()), state) {
        (Some(code), Some(state)) => Callback::Code { code: code.to_string(), state },
        _ => Callback::Ignore,
    }
}

/// The page every request gets back.
fn page(text: &str) -> String {
    let body = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Rslook</title></head>\
         <body style=\"font-family:sans-serif;margin:3em\"><h2>Rslook</h2><p>{text}</p>\
         <p>You can close this tab and return to Rslook.</p></body></html>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

struct Pending {
    stream: TcpStream,
    buf: Vec<u8>,
    since: Instant,
}

pub(crate) struct Server {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl Server {
    /// Binds 127.0.0.1 on a free port, plus [::1] on the same port when possible (browsers
    /// may resolve `localhost` to IPv6 first).
    pub(crate) fn bind() -> Result<Server, OAuthError> {
        let io = |e: std::io::Error| OAuthError::Io(format!("could not listen for the browser on this computer ({e})"));
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).map_err(io)?;
        let port = v4.local_addr().map_err(io)?.port();
        let mut listeners = vec![v4];
        if let Ok(v6) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
            listeners.push(v6);
        }
        for l in &listeners {
            l.set_nonblocking(true).map_err(io)?;
        }
        Ok(Server { listeners, port })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Waits for one redirect with the matching state and returns its code.
    pub(crate) fn wait_for_code(&self, state: &str, timeout: Duration, cancel: &AtomicBool) -> Result<String, OAuthError> {
        let deadline = Instant::now().checked_add(timeout);
        let mut pending: Vec<Pending> = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(OAuthError::Cancelled);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(OAuthError::TimedOut);
            }
            let mut busy = false;
            for l in &self.listeners {
                match l.accept() {
                    Ok((stream, _)) => {
                        busy = true;
                        if stream.set_nonblocking(true).is_ok() {
                            if pending.len() >= MAX_PENDING {
                                pending.remove(0);
                            }
                            pending.push(Pending { stream, buf: Vec::new(), since: Instant::now() });
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => {}
                }
            }
            let mut i = 0;
            while i < pending.len() {
                let Some(p) = pending.get_mut(i) else { break };
                match poll_request(p) {
                    Poll::Waiting => i += 1,
                    Poll::Drop => {
                        pending.remove(i);
                        busy = true;
                    }
                    Poll::Request => {
                        busy = true;
                        let p = pending.remove(i);
                        if let Some(result) = answer(p, state) {
                            return result;
                        }
                    }
                }
            }
            if !busy {
                std::thread::sleep(POLL);
            }
        }
    }
}

enum Poll {
    Waiting,
    Drop,
    Request,
}

fn poll_request(p: &mut Pending) -> Poll {
    let mut chunk = [0u8; 2048];
    loop {
        if p.buf.contains(&b'\n') {
            return Poll::Request;
        }
        if p.buf.len() >= MAX_REQUEST || p.since.elapsed() > CONN_TIMEOUT {
            return Poll::Drop;
        }
        let room = MAX_REQUEST.saturating_sub(p.buf.len()).min(chunk.len());
        let Some(dst) = chunk.get_mut(..room) else { return Poll::Drop };
        match p.stream.read(dst) {
            Ok(0) => return if p.buf.is_empty() { Poll::Drop } else { Poll::Request },
            Ok(n) => p.buf.extend_from_slice(dst.get(..n).unwrap_or(&[])),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Poll::Waiting,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Poll::Drop,
        }
    }
}

/// Answers one request. `Some` ends the wait (code or error with a fitting state).
fn answer(p: Pending, state: &str) -> Option<Result<String, OAuthError>> {
    let (text, result) = match parse_request(&p.buf) {
        Callback::Code { code, state: s } if s == state => ("Sign-in received.", Some(Ok(code))),
        // A missing state on an error is accepted: at worst a local process ends the attempt.
        Callback::Error { error, description, state: s } if s.as_deref().is_none_or(|s| s == state) => {
            ("Sign-in did not complete.", Some(Err(OAuthError::Denied(denied_text(&error, description.as_deref())))))
        }
        _ => ("Nothing to do here.", None),
    };
    let mut stream = p.stream;
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.write_all(page(text).as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    result
}
