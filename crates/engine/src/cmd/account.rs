//! Account commands: the Add Account / Account Settings dialog and removing the account.

use super::{CommandSpec, str_param};
use crate::{AccountDraft, EngineError, Session};
use rslook_account::{Account, Security, Server};
use rslook_mail::{Address, Store};
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec { id: "account.add", label: "Add Account", shortcut: None, params: "", undoable: false, enabled: no_account, run: add },
    CommandSpec {
        id: "account.settings",
        label: "Account Settings",
        shortcut: None,
        params: "",
        undoable: false,
        enabled: has_account,
        run: settings,
    },
    CommandSpec {
        id: "account.set",
        label: "Edit Account Fields",
        shortcut: None,
        params: r#"{"name"?, "email"?, "username"?, "password"?, "imap_host"?, "imap_port"?, "smtp_host"?, "smtp_port"?: string, "imap_security"?, "smtp_security"?: "tls|starttls|none"}"#,
        undoable: false,
        enabled: dialog_open,
        run: set,
    },
    CommandSpec { id: "account.save", label: "Save Account", shortcut: None, params: "", undoable: false, enabled: dialog_open, run: save },
    CommandSpec { id: "account.cancel", label: "Cancel", shortcut: None, params: "", undoable: false, enabled: dialog_open, run: cancel },
    CommandSpec { id: "account.remove", label: "Remove Account", shortcut: None, params: "", undoable: false, enabled: has_account, run: remove },
];

fn no_account(s: &Session) -> bool {
    s.account.is_none()
}

pub(crate) fn has_account(s: &Session) -> bool {
    s.account.is_some()
}

fn dialog_open(s: &Session) -> bool {
    s.account_dialog.is_some()
}

/// Caps on dialog fields (hostile params).
const MAX_FIELD: usize = 512;

fn add(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.account_dialog = Some(AccountDraft {
        imap_port: "993".into(),
        imap_security: Security::Tls,
        smtp_port: "587".into(),
        smtp_security: Security::StartTls,
        ..Default::default()
    });
    Ok(())
}

fn settings(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let a = s.account.clone().ok_or(EngineError::Disabled("no account"))?;
    s.account_dialog = Some(AccountDraft {
        name: a.name,
        email: a.email,
        username: a.username,
        password: String::new(),
        imap_host: a.imap.host,
        imap_port: a.imap.port.to_string(),
        imap_security: a.imap.security,
        smtp_host: a.smtp.host,
        smtp_port: a.smtp.port.to_string(),
        smtp_security: a.smtp.security,
        editing: true,
        error: None,
    });
    Ok(())
}

fn set(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let d = s.account_dialog.as_mut().ok_or(EngineError::Disabled("account dialog is closed"))?;
    for (key, field) in [
        ("name", &mut d.name),
        ("email", &mut d.email),
        ("username", &mut d.username),
        ("password", &mut d.password),
        ("imap_host", &mut d.imap_host),
        ("imap_port", &mut d.imap_port),
        ("smtp_host", &mut d.smtp_host),
        ("smtp_port", &mut d.smtp_port),
    ] {
        if let Some(v) = str_param(p, key) {
            if v.len() > MAX_FIELD {
                return Err(EngineError::BadParam(format!("{key} too long")));
            }
            *field = v.to_string();
        } else if let Some(n) = p.get(key).and_then(Value::as_u64) {
            *field = n.to_string();
        }
    }
    for (key, field) in [("imap_security", &mut d.imap_security), ("smtp_security", &mut d.smtp_security)] {
        if let Some(v) = str_param(p, key) {
            *field = Security::parse(v).ok_or_else(|| EngineError::BadParam(format!("{key} must be tls, starttls or none")))?;
        }
    }
    Ok(())
}

fn port(text: &str, what: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(format!("The {what} port must be a number from 1 to 65535.")),
    }
}

fn draft_to_account(d: &AccountDraft) -> Result<Account, String> {
    let a = Account {
        name: d.name.trim().to_string(),
        email: d.email.trim().to_string(),
        username: if d.username.trim().is_empty() { d.email.trim().to_string() } else { d.username.trim().to_string() },
        imap: Server { host: d.imap_host.trim().to_string(), port: port(&d.imap_port, "incoming (IMAP)")?, security: d.imap_security },
        smtp: Server { host: d.smtp_host.trim().to_string(), port: port(&d.smtp_port, "outgoing (SMTP)")?, security: d.smtp_security },
    };
    a.validate().map_err(|e| e.to_string())?;
    Ok(a)
}

fn refuse(s: &mut Session, msg: String) -> Result<(), EngineError> {
    if let Some(d) = s.account_dialog.as_mut() {
        d.error = Some(msg.clone());
    }
    Err(EngineError::BadParam(msg))
}

fn save(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let d = s.account_dialog.clone().ok_or(EngineError::Disabled("account dialog is closed"))?;
    let account = match draft_to_account(&d) {
        Ok(a) => a,
        Err(m) => return refuse(s, m),
    };
    let old = s.account.clone();
    let same_mailbox = old.as_ref().is_some_and(|o| o.key() == account.key());
    if d.password.is_empty() && !same_mailbox {
        return refuse(s, "Enter the password.".into());
    }
    if !d.password.is_empty()
        && let Err(e) = s.secrets.set(&account.key(), &d.password)
    {
        return refuse(s, e.to_string());
    }
    if let Some(paths) = s.paths.clone()
        && let Err(e) = rslook_account::save_accounts(&paths.accounts_file(), std::slice::from_ref(&account))
    {
        return refuse(s, e.to_string());
    }
    if let Some(o) = old.as_ref().filter(|_| !same_mailbox) {
        // A different address is a different mailbox: forget the old one's password.
        let _ = s.secrets.delete(&o.key());
    }
    if same_mailbox {
        s.store.account = Address::new(account.name.clone(), account.email.clone());
        s.account = Some(account);
    } else {
        let cached = match s.paths.as_ref().map(|p| crate::sync::load_cache(&p.cache_file(&account))) {
            Some(Ok(c)) => c,
            _ => None,
        };
        s.open_account(account, cached);
    }
    s.account_dialog = None;
    s.dirty = true;
    s.status = "Account saved".into();
    s.request(crate::sync::SyncRequest::All);
    Ok(())
}

fn cancel(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.account_dialog = None;
    Ok(())
}

fn remove(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let a = s.account.clone().ok_or(EngineError::Disabled("no account"))?;
    s.secrets.delete(&a.key()).map_err(|e| EngineError::Io(e.to_string()))?;
    if let Some(paths) = s.paths.clone() {
        rslook_account::save_accounts(&paths.accounts_file(), &[]).map_err(|e| EngineError::Io(e.to_string()))?;
        match std::fs::remove_file(paths.cache_file(&a)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(EngineError::Io(e.to_string())),
        }
    }
    let store = Store::new(Address::new("", "Local mailbox"));
    s.open_account(a, Some((store, Vec::new(), 1)));
    s.account = None;
    s.account_dialog = None;
    s.work_offline = false;
    s.dirty = false;
    s.status = "Account removed".into();
    Ok(())
}
