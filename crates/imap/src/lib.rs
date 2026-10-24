#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! IMAP client for Rslook (RFC 9051, works with IMAP4rev1 servers): connect over implicit TLS,
//! STARTTLS or plain TCP, log in, list folders, fetch flags and messages, change flags, move,
//! delete and append. Certificates are checked against the operating system's trust store.

pub mod mutf7;

use async_imap::types::{Fetch, Flag, NameAttribute};
use futures::TryStreamExt;
use rslook_account::{Login, Security, Server};
use rslook_mail::FolderKind;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};

#[derive(Debug, thiserror::Error)]
pub enum ImapError {
    #[error("could not connect to {0}")]
    Connect(String),
    #[error("the connection to the server was lost ({0})")]
    Lost(String),
    #[error("secure connection failed: {0}")]
    Tls(String),
    #[error("the server refused the sign-in ({0})")]
    Auth(String),
    #[error("server error: {0}")]
    Protocol(String),
    #[error("the server did not answer in time ({0})")]
    Timeout(&'static str),
}

impl ImapError {
    /// The connection is unusable (as opposed to the server refusing one command).
    pub fn is_fatal(&self) -> bool {
        !matches!(self, ImapError::Protocol(_))
    }
}

impl From<async_imap::error::Error> for ImapError {
    fn from(e: async_imap::error::Error) -> Self {
        use async_imap::error::Error as E;
        match e {
            E::Io(e) => ImapError::Lost(e.to_string()),
            E::ConnectionLost => ImapError::Lost("closed by the server".into()),
            e => ImapError::Protocol(e.to_string()),
        }
    }
}

/// Extra trust anchors on top of the operating system's (used by tests with a private CA).
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    pub extra_roots_pem: Vec<u8>,
}

/// TLS client settings: OS trust store plus `extra`, ring crypto, TLS 1.2+.
pub fn tls_config(opts: &TlsOptions) -> Result<Arc<rustls::ClientConfig>, ImapError> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
    if !opts.extra_roots_pem.is_empty() {
        for cert in CertificateDer::pem_slice_iter(&opts.extra_roots_pem) {
            let cert = cert.map_err(|e| ImapError::Tls(format!("bad extra root certificate: {e}")))?;
            roots.add(cert).map_err(|e| ImapError::Tls(e.to_string()))?;
        }
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| ImapError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send + Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Debug> Io for T {}
type Stream = Box<dyn Io>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// Messages fetched per FETCH command.
const FETCH_CHUNK: usize = 25;

async fn timed<T, E: Into<ImapError>>(what: &'static str, f: impl Future<Output = Result<T, E>>) -> Result<T, ImapError> {
    match tokio::time::timeout(COMMAND_TIMEOUT, f).await {
        Ok(r) => r.map_err(Into::into),
        Err(_) => Err(ImapError::Timeout(what)),
    }
}

async fn tls_wrap(tcp: TcpStream, host: &str, opts: &TlsOptions) -> Result<Stream, ImapError> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| ImapError::Tls(format!("bad server name {host}: {e}")))?;
    let connector = tokio_rustls::TlsConnector::from(tls_config(opts)?);
    match tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, tcp)).await {
        Ok(Ok(s)) => Ok(Box::new(s)),
        Ok(Err(e)) => Err(ImapError::Tls(e.to_string())),
        Err(_) => Err(ImapError::Timeout("TLS handshake")),
    }
}

async fn greeting<T: AsyncRead + AsyncWrite + Unpin + Send + Debug>(client: &mut async_imap::Client<T>) -> Result<(), ImapError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, client.read_response()).await {
        Ok(Ok(Some(_))) => Ok(()),
        Ok(Ok(None)) => Err(ImapError::Protocol("connection closed before the greeting".into())),
        Ok(Err(e)) => Err(ImapError::Protocol(e.to_string())),
        Err(_) => Err(ImapError::Timeout("greeting")),
    }
}

/// A server mailbox from LIST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFolder {
    pub name: String,
    pub display: String,
    pub delimiter: Option<String>,
    pub kind: Option<FolderKind>,
    pub special_use: bool,
    pub selectable: bool,
}

/// Flags and size of one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageState {
    pub uid: u32,
    pub read: bool,
    pub flagged: bool,
    pub size: u32,
}

/// A downloaded message (whole, or just its header when it is over the size cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedMessage {
    pub uid: u32,
    pub read: bool,
    pub flagged: bool,
    pub bytes: Vec<u8>,
    pub header_only: bool,
}

/// An authenticated connection.
pub struct Conn {
    session: async_imap::Session<Stream>,
    can_move: bool,
    uidplus: bool,
}

/// Connects and logs in with a password.
pub async fn connect(server: &Server, user: &str, password: &str, opts: &TlsOptions) -> Result<Conn, ImapError> {
    connect_with(server, user, &Login::Password(password.to_string()), opts).await
}

/// SASL XOAUTH2 sends its whole response with the command (SASL-IR, RFC 4959, which Microsoft
/// and Google support and some servers require). A failure comes back as a challenge holding a
/// JSON error, answered with an empty response so the server ends with NO.
struct XOAuth2Failure;

impl async_imap::Authenticator for XOAuth2Failure {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> String {
        String::new()
    }
}

/// Connects and logs in with a password (LOGIN) or an OAuth access token (AUTHENTICATE XOAUTH2).
pub async fn connect_with(server: &Server, user: &str, login: &Login, opts: &TlsOptions) -> Result<Conn, ImapError> {
    let host = server.host.trim();
    let addr = format!("{host}:{}", server.port);
    let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, server.port))).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(ImapError::Connect(format!("{addr}: {e}"))),
        Err(_) => return Err(ImapError::Timeout("connect")),
    };
    let client = match server.security {
        Security::Tls => {
            let mut c = async_imap::Client::new(tls_wrap(tcp, host, opts).await?);
            greeting(&mut c).await?;
            c
        }
        Security::StartTls => {
            let mut plain = async_imap::Client::new(tcp);
            greeting(&mut plain).await?;
            timed("STARTTLS", plain.run_command_and_check_ok("STARTTLS", None)).await?;
            let tcp = plain.into_inner();
            // Nothing read before the upgrade is trusted: start a fresh client on the TLS stream.
            async_imap::Client::new(tls_wrap(tcp, host, opts).await?)
        }
        Security::None => {
            let mut c = async_imap::Client::new(Box::new(tcp) as Stream);
            greeting(&mut c).await?;
            c
        }
    };
    let logged_in = match login {
        Login::Password(password) => tokio::time::timeout(COMMAND_TIMEOUT, client.login(user, password)).await,
        Login::Bearer(token) => {
            use base64::Engine as _;
            let initial = base64::engine::general_purpose::STANDARD.encode(rslook_oauth::xoauth2(user, token));
            tokio::time::timeout(COMMAND_TIMEOUT, client.authenticate(format!("XOAUTH2 {initial}"), XOAuth2Failure)).await
        }
    };
    let mut session = match logged_in {
        Ok(Ok(s)) => s,
        Ok(Err((e, _))) => return Err(ImapError::Auth(e.to_string())),
        Err(_) => return Err(ImapError::Timeout("login")),
    };
    let caps = timed("CAPABILITY", session.capabilities()).await?;
    let can_move = caps.has_str("MOVE");
    let uidplus = caps.has_str("UIDPLUS");
    Ok(Conn { session, can_move, uidplus })
}

/// Quotes a string for an IMAP command (RFC 9051 quoted string).
fn quote(s: &str) -> Result<String, ImapError> {
    if s.contains(['\r', '\n', '\0']) {
        return Err(ImapError::Protocol("mailbox name contains a line break".into()));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Standard folder from a special-use attribute (RFC 6154) or a well-known name.
fn classify(name: &str, delimiter: Option<&str>, attrs: &[NameAttribute<'_>]) -> (Option<FolderKind>, bool) {
    if name.eq_ignore_ascii_case("INBOX") {
        return (Some(FolderKind::Inbox), true);
    }
    for a in attrs {
        let kind = match a {
            NameAttribute::Sent => FolderKind::Sent,
            NameAttribute::Drafts => FolderKind::Drafts,
            NameAttribute::Trash => FolderKind::Deleted,
            NameAttribute::Junk => FolderKind::Junk,
            NameAttribute::Archive => FolderKind::Archive,
            _ => continue,
        };
        return (Some(kind), true);
    }
    // Names only count at the top level or directly under INBOX (servers that nest everything).
    let leaf = match delimiter.filter(|d| !d.is_empty()) {
        Some(d) => match name.split_once(d) {
            None => name,
            Some((top, rest)) if top.eq_ignore_ascii_case("INBOX") && !rest.contains(d) => rest,
            Some(_) => return (None, false),
        },
        None => name,
    };
    let kind = match leaf.to_ascii_lowercase().as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" | "deleted items" | "deleted messages" | "deleted" => FolderKind::Deleted,
        "junk" | "spam" | "junk email" | "junk e-mail" => FolderKind::Junk,
        "archive" | "archives" => FolderKind::Archive,
        _ => return (None, false),
    };
    (Some(kind), false)
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

fn flags_of(f: &Fetch) -> (bool, bool) {
    let mut read = false;
    let mut flagged = false;
    for flag in f.flags() {
        match flag {
            Flag::Seen => read = true,
            Flag::Flagged => flagged = true,
            _ => {}
        }
    }
    (read, flagged)
}

impl Conn {
    /// Every mailbox (LIST "" "*").
    pub async fn list(&mut self) -> Result<Vec<ListedFolder>, ImapError> {
        let names: Vec<async_imap::types::Name> = timed("LIST", async {
            let stream = self.session.list(Some(""), Some("*")).await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
        Ok(names
            .iter()
            .map(|n| {
                let attrs = n.attributes();
                let delimiter = n.delimiter().map(str::to_string);
                let (kind, special_use) = classify(n.name(), n.delimiter(), attrs);
                let leaf = match n.delimiter().filter(|d| !d.is_empty()) {
                    Some(d) => n.name().rsplit(d).next().unwrap_or(n.name()),
                    None => n.name(),
                };
                let selectable = !attrs.iter().any(|a| match a {
                    NameAttribute::NoSelect => true,
                    NameAttribute::Extension(e) => e.eq_ignore_ascii_case("\\NonExistent"),
                    _ => false,
                });
                ListedFolder { name: n.name().to_string(), display: mutf7::decode(leaf), delimiter, kind, special_use, selectable }
            })
            .collect())
    }

    /// Creates a mailbox unless it already exists.
    pub async fn ensure_mailbox(&mut self, name: &str) -> Result<(), ImapError> {
        let pattern = quote(name)?;
        let found: Vec<async_imap::types::Name> = timed("LIST", async {
            let stream = self.session.list(Some(""), Some(&pattern)).await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
        if found.iter().any(|n| n.name() == name) {
            return Ok(());
        }
        timed("CREATE", self.session.create(name)).await
    }

    /// Selects a mailbox; returns (UIDVALIDITY, message count).
    pub async fn select(&mut self, name: &str) -> Result<(u32, u32), ImapError> {
        let mb = timed("SELECT", self.session.select(name)).await?;
        Ok((mb.uid_validity.unwrap_or(0), mb.exists))
    }

    /// UID, flags and size of every message in the selected mailbox.
    pub async fn states(&mut self, exists: u32) -> Result<Vec<MessageState>, ImapError> {
        if exists == 0 {
            return Ok(Vec::new());
        }
        let fetches: Vec<Fetch> = timed("FETCH", async {
            let stream = self.session.uid_fetch("1:*", "(UID FLAGS RFC822.SIZE)").await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                let (read, flagged) = flags_of(f);
                f.uid.map(|uid| MessageState { uid, read, flagged, size: f.size.unwrap_or(0) })
            })
            .collect())
    }

    /// Downloads messages without marking them read. `header_only` UIDs get just the header.
    pub async fn fetch(&mut self, uids: &[u32], header_only: bool) -> Result<Vec<FetchedMessage>, ImapError> {
        let query = if header_only { "(UID FLAGS BODY.PEEK[HEADER])" } else { "(UID FLAGS BODY.PEEK[])" };
        let mut out = Vec::with_capacity(uids.len());
        for chunk in uids.chunks(FETCH_CHUNK) {
            let set = uid_set(chunk);
            let fetches: Vec<Fetch> = timed("FETCH", async {
                let stream = self.session.uid_fetch(&set, query).await?;
                stream.try_collect::<Vec<_>>().await
            })
            .await?;
            for f in &fetches {
                let Some(uid) = f.uid else { continue };
                let bytes = if header_only { f.header() } else { f.body() };
                let Some(bytes) = bytes else { continue };
                let (read, flagged) = flags_of(f);
                out.push(FetchedMessage { uid, read, flagged, bytes: bytes.to_vec(), header_only });
            }
        }
        Ok(out)
    }

    async fn store(&mut self, uid: u32, query: &str) -> Result<(), ImapError> {
        let set = uid.to_string();
        timed("STORE", async {
            let stream = self.session.uid_store(&set, query).await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
        Ok(())
    }

    /// Sets \Seen and \Flagged on a message in the selected mailbox.
    pub async fn set_flags(&mut self, uid: u32, read: bool, flagged: bool) -> Result<(), ImapError> {
        let mut add = Vec::new();
        let mut remove = Vec::new();
        for (on, flag) in [(read, "\\Seen"), (flagged, "\\Flagged")] {
            if on { add.push(flag) } else { remove.push(flag) }
        }
        if !add.is_empty() {
            self.store(uid, &format!("+FLAGS.SILENT ({})", add.join(" "))).await?;
        }
        if !remove.is_empty() {
            self.store(uid, &format!("-FLAGS.SILENT ({})", remove.join(" "))).await?;
        }
        Ok(())
    }

    /// Expunges one message: UID EXPUNGE with UIDPLUS, otherwise a plain EXPUNGE (which also
    /// removes any other message already marked \Deleted in that mailbox).
    async fn expunge_uid(&mut self, uid: u32) -> Result<(), ImapError> {
        if self.uidplus {
            let set = uid.to_string();
            timed("EXPUNGE", async {
                let stream = self.session.uid_expunge(&set).await?;
                stream.try_collect::<Vec<_>>().await
            })
            .await?;
        } else {
            timed("EXPUNGE", async {
                let stream = self.session.expunge().await?;
                stream.try_collect::<Vec<_>>().await
            })
            .await?;
        }
        Ok(())
    }

    /// Moves a message from the selected mailbox (MOVE, or COPY + delete without it).
    pub async fn move_to(&mut self, uid: u32, to: &str) -> Result<(), ImapError> {
        let set = uid.to_string();
        if self.can_move {
            return timed("MOVE", self.session.uid_mv(&set, to)).await;
        }
        timed("COPY", self.session.uid_copy(&set, to)).await?;
        self.store(uid, "+FLAGS.SILENT (\\Deleted)").await?;
        self.expunge_uid(uid).await
    }

    /// Removes a message from the selected mailbox for good.
    pub async fn delete(&mut self, uid: u32) -> Result<(), ImapError> {
        self.store(uid, "+FLAGS.SILENT (\\Deleted)").await?;
        self.expunge_uid(uid).await
    }

    /// Appends a message (e.g. a copy of sent mail) with the given flags, like `(\Seen)`.
    pub async fn append(&mut self, mailbox: &str, flags: &str, bytes: &[u8]) -> Result<(), ImapError> {
        timed("APPEND", self.session.append(mailbox, Some(flags), None, bytes)).await
    }

    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.session.logout()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_escapes_and_refuses_line_breaks() {
        assert_eq!(quote(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert!(quote("a\r\nb").is_err());
    }

    #[test]
    fn classifies_standard_folders() {
        assert_eq!(classify("INBOX", Some("/"), &[]), (Some(FolderKind::Inbox), true));
        assert_eq!(classify("Whatever", Some("/"), &[NameAttribute::Sent]), (Some(FolderKind::Sent), true));
        assert_eq!(classify("Trash", Some("/"), &[]), (Some(FolderKind::Deleted), false));
        assert_eq!(classify("INBOX.Sent", Some("."), &[]), (Some(FolderKind::Sent), false));
        assert_eq!(classify("Projects/Sent", Some("/"), &[]), (None, false));
        assert_eq!(classify("Receipts", None, &[]), (None, false));
    }

    #[test]
    fn uid_sets() {
        assert_eq!(uid_set(&[1, 5, 9]), "1,5,9");
    }

    #[test]
    fn tls_config_builds_and_rejects_bad_extra_roots() {
        assert!(tls_config(&TlsOptions::default()).is_ok());
        let junk = TlsOptions { extra_roots_pem: b"-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----\n".to_vec() };
        assert!(tls_config(&junk).is_err());
    }
}
