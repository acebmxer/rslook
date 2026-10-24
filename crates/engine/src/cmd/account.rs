//! Account commands: the Add Account / Account Settings dialog and removing the account.

use super::{CommandSpec, str_param};
use crate::{AccountDraft, EngineError, Session};
use rslook_account::{Account, AccountKind, MS_IMAP, MS_SMTP, MicrosoftSettings, MsBackend, Security, Server};
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
        params: r#"{"name"?, "email"?, "username"?, "password"?, "imap_host"?, "imap_port"?, "smtp_host"?, "smtp_port"?, "client_id"?: string, "imap_security"?, "smtp_security"?: "tls|starttls|none", "kind"?: "imap|microsoft", "backend"?: "graph|imap"}"#,
        undoable: false,
        enabled: dialog_open,
        run: set,
    },
    CommandSpec {
        id: "account.sign_in",
        label: "Sign in with Microsoft",
        shortcut: None,
        params: "",
        undoable: false,
        enabled: can_sign_in,
        run: sign_in,
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

fn can_sign_in(s: &Session) -> bool {
    s.account_dialog.as_ref().is_some_and(|d| d.kind == AccountKind::Microsoft && !d.signing_in)
}

fn sign_in(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    match s.start_sign_in() {
        Ok(()) => {
            s.status = "Finish signing in to Microsoft in your web browser.".into();
            Ok(())
        }
        Err(m) => refuse(s, m),
    }
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
    let signed_in = (a.kind == AccountKind::Microsoft).then(|| (a.name.clone(), a.email.clone()));
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
        kind: a.kind,
        ms_backend: a.microsoft.backend,
        client_id: a.microsoft.client_id.clone(),
        signed_in,
        editing: true,
        ..Default::default()
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
    if let Some(v) = str_param(p, "client_id") {
        if v.len() > MAX_FIELD {
            return Err(EngineError::BadParam("client_id too long".into()));
        }
        d.client_id = v.to_string();
    }
    let mut changed_how = false;
    if let Some(v) = str_param(p, "kind") {
        let kind = match v {
            "imap" => AccountKind::Imap,
            "microsoft" => AccountKind::Microsoft,
            _ => return Err(EngineError::BadParam("kind must be imap or microsoft".into())),
        };
        changed_how |= kind != d.kind;
        d.kind = kind;
    }
    if let Some(v) = str_param(p, "backend") {
        let b = MsBackend::parse(v).ok_or_else(|| EngineError::BadParam("backend must be graph or imap".into()))?;
        changed_how |= b != d.ms_backend;
        d.ms_backend = b;
    }
    if changed_how && !d.signing_in && !d.editing {
        // A sign-in grants access for one way in (Graph or IMAP); a different choice needs a new one.
        d.signed_in = None;
        d.oauth_token.clear();
    }
    Ok(())
}

fn port(text: &str, what: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(format!("The {what} port must be a number from 1 to 65535.")),
    }
}

fn draft_to_account(d: &AccountDraft, old: Option<&Account>) -> Result<Account, String> {
    if d.kind == AccountKind::Microsoft {
        let Some((_, email)) = &d.signed_in else { return Err("Sign in with Microsoft first.".into()) };
        let keep = old.filter(|o| o.kind == AccountKind::Microsoft).map(|o| o.microsoft.clone()).unwrap_or_default();
        let a = Account {
            name: d.name.trim().to_string(),
            email: email.clone(),
            username: email.clone(),
            imap: Server { host: MS_IMAP.0.into(), port: MS_IMAP.1, security: Security::Tls },
            smtp: Server { host: MS_SMTP.0.into(), port: MS_SMTP.1, security: Security::StartTls },
            kind: AccountKind::Microsoft,
            microsoft: MicrosoftSettings { backend: d.ms_backend, client_id: d.client_id.trim().to_string(), ..keep },
        };
        a.validate().map_err(|e| e.to_string())?;
        return Ok(a);
    }
    let a = Account {
        name: d.name.trim().to_string(),
        email: d.email.trim().to_string(),
        username: if d.username.trim().is_empty() { d.email.trim().to_string() } else { d.username.trim().to_string() },
        imap: Server { host: d.imap_host.trim().to_string(), port: port(&d.imap_port, "incoming (IMAP)")?, security: d.imap_security },
        smtp: Server { host: d.smtp_host.trim().to_string(), port: port(&d.smtp_port, "outgoing (SMTP)")?, security: d.smtp_security },
        ..Default::default()
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
    let old = s.account.clone();
    let account = match draft_to_account(&d, old.as_ref()) {
        Ok(a) => a,
        Err(m) => return refuse(s, m),
    };
    // Same address, same kind and same way in: the cache and the saved secret still fit.
    let same_mailbox =
        old.as_ref().is_some_and(|o| o.key() == account.key() && o.kind == account.kind && o.microsoft.backend == account.microsoft.backend);
    let secret = if account.kind == AccountKind::Microsoft { &d.oauth_token } else { &d.password };
    if secret.is_empty() && !same_mailbox {
        let ask = if account.kind == AccountKind::Microsoft { "Sign in with Microsoft first." } else { "Enter the password." };
        return refuse(s, ask.into());
    }
    if !secret.is_empty()
        && let Err(e) = s.secrets.set(&account.key(), secret)
    {
        return refuse(s, e.to_string());
    }
    if let Some(paths) = s.paths.clone()
        && let Err(e) = rslook_account::save_accounts(&paths.accounts_file(), std::slice::from_ref(&account))
    {
        return refuse(s, e.to_string());
    }
    if let Some(o) = old.as_ref().filter(|o| !same_mailbox && o.key() != account.key()) {
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
    s.cancel_sign_in();
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
