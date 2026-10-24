#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Mail account settings (IMAP and SMTP servers, user name) and the OS keychain that holds the
//! password. An [`Account`] has no password field on purpose: settings files never contain one.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("{0}")]
    Invalid(String),
    #[error("could not read or write account settings: {0}")]
    Io(String),
    #[error("account settings file is damaged: {0}")]
    Format(String),
    #[error("OS keychain: {0}")]
    Keychain(String),
    #[error("no password saved for {0}")]
    NoPassword(String),
}

/// How the connection to a server is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Security {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    #[default]
    Tls,
    /// Plain connection upgraded with STARTTLS before login (IMAP 143, SMTP 587).
    StartTls,
    /// No encryption. Passwords cross the network in the clear; for local test servers only.
    None,
}

impl Security {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "tls" | "ssl" | "ssl/tls" => Some(Security::Tls),
            "starttls" => Some(Security::StartTls),
            "none" => Some(Security::None),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
            Security::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Security::Tls => "SSL/TLS",
            Security::StartTls => "STARTTLS",
            Security::None => "None (unencrypted)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// What kind of account this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccountKind {
    /// Any IMAP + SMTP server, signed in with a password.
    #[default]
    Imap,
    /// Microsoft 365 or Outlook.com, signed in through Microsoft (OAuth 2.0); the keychain holds
    /// the refresh token instead of a password.
    Microsoft,
}

/// How a Microsoft account reaches its mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MsBackend {
    /// Microsoft Graph (the default).
    #[default]
    Graph,
    /// IMAP + SMTP with OAuth (XOAUTH2), for tenants or accounts where Graph is not wanted.
    Imap,
}

impl MsBackend {
    pub fn id(self) -> &'static str {
        match self {
            MsBackend::Graph => "graph",
            MsBackend::Imap => "imap",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "graph" => Some(MsBackend::Graph),
            "imap" => Some(MsBackend::Imap),
            _ => None,
        }
    }
}

/// Rslook's own application (client) ID in the Microsoft identity platform. Empty until the
/// app registration exists; then people must enter their own ID in Account Settings.
pub const BUILTIN_CLIENT_ID: &str = "";

/// Microsoft's IMAP and SMTP servers for the IMAP backend.
pub const MS_IMAP: (&str, u16) = ("outlook.office365.com", 993);
pub const MS_SMTP: (&str, u16) = ("smtp.office365.com", 587);

/// Settings that only Microsoft accounts use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MicrosoftSettings {
    #[serde(default)]
    pub backend: MsBackend,
    /// Application (client) ID; empty uses [`BUILTIN_CLIENT_ID`].
    #[serde(default)]
    pub client_id: String,
    /// Directory (tenant); empty is `common` (work, school and personal accounts).
    #[serde(default)]
    pub tenant: String,
    /// Test override for the sign-in server (`<base>/authorize`, `<base>/token`). Empty in real use.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_base: String,
    /// Test override for the Graph base URL. Empty in real use.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub graph_base: String,
}

impl MicrosoftSettings {
    pub fn client_id(&self) -> &str {
        let own = self.client_id.trim();
        if own.is_empty() { BUILTIN_CLIENT_ID } else { own }
    }

    pub fn tenant(&self) -> &str {
        let t = self.tenant.trim();
        if t.is_empty() { "common" } else { t }
    }

    /// The client ID and tenant go into sign-in URLs: present, short, and URL-safe.
    pub fn validate(&self) -> Result<(), AccountError> {
        let id = self.client_id();
        if id.is_empty() {
            return Err(AccountError::Invalid("Enter the application (client) ID registered for Rslook in Microsoft Entra.".into()));
        }
        let ok = |s: &str| s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
        if !ok(id) || !ok(self.tenant()) {
            return Err(AccountError::Invalid("The application (client) ID or tenant has characters that can't be in one.".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Account {
    /// Display name used in From.
    pub name: String,
    pub email: String,
    /// Login name for both servers.
    pub username: String,
    pub imap: Server,
    pub smtp: Server,
    #[serde(default)]
    pub kind: AccountKind,
    #[serde(default)]
    pub microsoft: MicrosoftSettings,
}

/// How a worker logs in to IMAP or SMTP.
#[derive(Clone, PartialEq, Eq)]
pub enum Login {
    Password(String),
    /// An OAuth 2.0 access token, sent with SASL XOAUTH2.
    Bearer(String),
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret.
        f.write_str(match self {
            Login::Password(_) => "Login::Password(..)",
            Login::Bearer(_) => "Login::Bearer(..)",
        })
    }
}

/// Longest host name (RFC 1035) and the caps we put on the other fields.
const MAX_HOST: usize = 253;
const MAX_FIELD: usize = 320;

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= MAX_HOST
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        && !h.starts_with('-')
}

impl Account {
    /// The keychain entry name. One entry per account, keyed by address.
    pub fn key(&self) -> String {
        self.email.trim().to_ascii_lowercase()
    }

    /// Checks every field the way the account dialog needs (messages are shown to the user).
    pub fn validate(&self) -> Result<(), AccountError> {
        let bad = |m: &str| Err(AccountError::Invalid(m.to_string()));
        let email = self.email.trim();
        let at = email.find('@');
        if email.is_empty() || email.len() > MAX_FIELD || !at.is_some_and(|i| i > 0 && i + 1 < email.len()) || email.contains(char::is_whitespace) {
            return bad("Enter a valid email address, like name@example.com.");
        }
        if self.name.len() > MAX_FIELD || self.name.contains(['\r', '\n']) {
            return bad("The name is too long or has a line break in it.");
        }
        if self.username.trim().is_empty() || self.username.len() > MAX_FIELD || self.username.contains(['\r', '\n']) {
            return bad("Enter the user name for the mail server.");
        }
        if self.kind == AccountKind::Microsoft {
            self.microsoft.validate()?;
        }
        for (what, s) in [("incoming (IMAP)", &self.imap), ("outgoing (SMTP)", &self.smtp)] {
            if !valid_host(s.host.trim()) {
                return Err(AccountError::Invalid(format!("Enter the {what} server name.")));
            }
            if s.port == 0 {
                return Err(AccountError::Invalid(format!("Enter the {what} server port.")));
            }
        }
        Ok(())
    }
}

/// The accounts file. Today Rslook holds one account; the list leaves room for more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsFile {
    pub version: u32,
    pub accounts: Vec<Account>,
}

/// Accounts files bigger than this are refused (hostile input).
const MAX_ACCOUNTS_FILE: u64 = 1 << 20;

/// Reads the accounts file; a missing file is no accounts.
pub fn load_accounts(path: &Path) -> Result<Vec<Account>, AccountError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AccountError::Io(e.to_string())),
    };
    if meta.len() > MAX_ACCOUNTS_FILE {
        return Err(AccountError::Format("file is too large".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| AccountError::Io(e.to_string()))?;
    let file: AccountsFile = serde_json::from_slice(&bytes).map_err(|e| AccountError::Format(e.to_string()))?;
    Ok(file.accounts)
}

/// Writes the accounts file atomically (temporary file, then rename).
pub fn save_accounts(path: &Path, accounts: &[Account]) -> Result<(), AccountError> {
    let file = AccountsFile { version: 1, accounts: accounts.to_vec() };
    let json = serde_json::to_vec_pretty(&file).map_err(|e| AccountError::Format(e.to_string()))?;
    write_atomic(path, &json).map_err(|e| AccountError::Io(e.to_string()))
}

/// Writes `bytes` to a sibling temporary file and renames it over `path`, so a crash never
/// leaves a half-written file. On Unix the file is readable by its owner only (it may hold mail).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

/// Where passwords live. The app uses [`OsKeychain`]; tests use [`MemorySecrets`].
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<String, AccountError>;
    fn set(&self, key: &str, secret: &str) -> Result<(), AccountError>;
    fn delete(&self, key: &str) -> Result<(), AccountError>;
}

/// The operating system's keychain: Secret Service on Linux (GNOME Keyring, KWallet),
/// Credential Manager on Windows.
pub struct OsKeychain;

const SERVICE: &str = "rslook";

impl OsKeychain {
    fn entry(key: &str) -> Result<keyring::Entry, AccountError> {
        keyring::Entry::new(SERVICE, key).map_err(|e| match e {
            keyring::Error::NoDefaultStore => AccountError::Keychain(
                "no keychain service is running (on Linux, start GNOME Keyring or KWallet so Rslook can store the password)".into(),
            ),
            e => AccountError::Keychain(e.to_string()),
        })
    }
}

impl SecretStore for OsKeychain {
    fn get(&self, key: &str) -> Result<String, AccountError> {
        Self::entry(key)?.get_password().map_err(|e| match e {
            keyring::Error::NoEntry => AccountError::NoPassword(key.to_string()),
            e => AccountError::Keychain(e.to_string()),
        })
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), AccountError> {
        Self::entry(key)?.set_password(secret).map_err(|e| AccountError::Keychain(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), AccountError> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AccountError::Keychain(e.to_string())),
        }
    }
}

/// In-memory secrets for tests and for sessions with no keychain.
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<String, AccountError> {
        let map = self.0.lock().map_err(|_| AccountError::Keychain("lock poisoned".into()))?;
        map.get(key).cloned().ok_or_else(|| AccountError::NoPassword(key.to_string()))
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), AccountError> {
        let mut map = self.0.lock().map_err(|_| AccountError::Keychain("lock poisoned".into()))?;
        map.insert(key.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), AccountError> {
        let mut map = self.0.lock().map_err(|_| AccountError::Keychain("lock poisoned".into()))?;
        map.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            name: "Test User".into(),
            email: "Test@Example.org".into(),
            username: "test".into(),
            imap: Server { host: "imap.example.org".into(), port: 993, security: Security::Tls },
            smtp: Server { host: "smtp.example.org".into(), port: 587, security: Security::StartTls },
            ..Default::default()
        }
    }

    #[test]
    fn microsoft_accounts_need_a_client_id_and_old_files_still_load() {
        let mut a = Account { kind: AccountKind::Microsoft, ..account() };
        if BUILTIN_CLIENT_ID.is_empty() {
            assert!(a.validate().is_err(), "no built-in ID yet, so one must be entered");
        }
        a.microsoft.client_id = "00000000-1111-2222-3333-444444444444".into();
        assert!(a.validate().is_ok());
        assert_eq!(a.microsoft.tenant(), "common");
        a.microsoft.tenant = "x/../y".into();
        assert!(a.validate().is_err());
        // Files written before Microsoft accounts existed have no kind or microsoft fields.
        let old = r#"{"name":"A","email":"a@x.org","username":"a","imap":{"host":"i.x.org","port":993,"security":"Tls"},"smtp":{"host":"s.x.org","port":587,"security":"StartTls"}}"#;
        let a: Account = serde_json::from_str(old).unwrap();
        assert_eq!(a.kind, AccountKind::Imap);
        assert_eq!(format!("{:?}", Login::Bearer("secret-token".into())), "Login::Bearer(..)");
    }

    #[test]
    fn valid_account_passes_and_bad_fields_fail() {
        assert!(account().validate().is_ok());
        for bad in [
            Account { email: "nope".into(), ..account() },
            Account { email: "@x.org".into(), ..account() },
            Account { email: "a@".into(), ..account() },
            Account { email: "a b@x.org".into(), ..account() },
            Account { username: " ".into(), ..account() },
            Account { name: "a\r\nBcc: x".into(), ..account() },
            Account { imap: Server { host: "".into(), ..account().imap }, ..account() },
            Account { smtp: Server { host: "bad host".into(), ..account().smtp }, ..account() },
            Account { smtp: Server { port: 0, ..account().smtp }, ..account() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn key_is_lowercased_address() {
        assert_eq!(account().key(), "test@example.org");
    }

    #[test]
    fn settings_file_round_trips_and_never_holds_a_password() {
        let dir = std::env::temp_dir().join(format!("rslook-acct-{}", std::process::id()));
        let path = dir.join("accounts.json");
        save_accounts(&path, &[account()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(load_accounts(&path).unwrap(), vec![account()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_no_accounts_and_junk_is_an_error() {
        let dir = std::env::temp_dir().join(format!("rslook-acct-junk-{}", std::process::id()));
        assert!(load_accounts(&dir.join("none.json")).unwrap().is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("junk.json");
        std::fs::write(&p, b"\xff{not json").unwrap();
        assert!(load_accounts(&p).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn memory_secrets() {
        let s = MemorySecrets::default();
        assert!(matches!(s.get("a"), Err(AccountError::NoPassword(_))));
        s.set("a", "pw").unwrap();
        assert_eq!(s.get("a").unwrap(), "pw");
        s.delete("a").unwrap();
        assert!(s.get("a").is_err());
    }
}
