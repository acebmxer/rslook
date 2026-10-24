//! Mail commands: respond, delete, archive, move, tags.

use super::{CommandSpec, always, has_selection, str_param, u64_param};
use crate::{Draft, EngineError, Session};
use rslook_mail::{Address, FolderKind, Message};
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec { id: "mail.new", label: "New Email", shortcut: Some("Ctrl+N"), params: "", undoable: false, enabled: always, run: new_mail },
    CommandSpec { id: "mail.reply", label: "Reply", shortcut: Some("Ctrl+R"), params: "", undoable: false, enabled: has_selection, run: reply },
    CommandSpec {
        id: "mail.reply_all",
        label: "Reply All",
        shortcut: Some("Ctrl+Shift+R"),
        params: "",
        undoable: false,
        enabled: has_selection,
        run: reply_all,
    },
    CommandSpec { id: "mail.forward", label: "Forward", shortcut: Some("Ctrl+F"), params: "", undoable: false, enabled: has_selection, run: forward },
    CommandSpec { id: "mail.delete", label: "Delete", shortcut: Some("Delete"), params: "", undoable: true, enabled: has_selection, run: delete },
    CommandSpec {
        id: "mail.archive",
        label: "Archive",
        shortcut: Some("Backspace"),
        params: "",
        undoable: true,
        enabled: has_selection,
        run: archive,
    },
    CommandSpec {
        id: "mail.move",
        label: "Move",
        shortcut: Some("Ctrl+Shift+V"),
        params: r#"{"folder": <folder id>} or {"kind": "inbox|drafts|sent|deleted|archive|junk|outbox"}"#,
        undoable: true,
        enabled: has_selection,
        run: move_to,
    },
    CommandSpec {
        id: "mail.mark_read",
        label: "Mark as Read",
        shortcut: Some("Ctrl+Q"),
        params: "",
        undoable: true,
        enabled: has_selection,
        run: mark_read,
    },
    CommandSpec {
        id: "mail.mark_unread",
        label: "Mark as Unread",
        shortcut: Some("Ctrl+U"),
        params: "",
        undoable: true,
        enabled: has_selection,
        run: mark_unread,
    },
    CommandSpec {
        id: "mail.toggle_flag",
        label: "Follow Up",
        shortcut: Some("Insert"),
        params: "",
        undoable: true,
        enabled: has_selection,
        run: toggle_flag,
    },
    CommandSpec { id: "mail.junk", label: "Junk", shortcut: None, params: "", undoable: true, enabled: has_selection, run: junk },
    CommandSpec {
        id: "mail.import_eml",
        label: "Import .eml",
        shortcut: None,
        params: r#"{"path": "/abs/file.eml"}"#,
        undoable: true,
        enabled: always,
        run: import_eml,
    },
];

fn selected(s: &Session) -> Result<Message, EngineError> {
    s.selected.and_then(|id| s.store.message(id)).cloned().ok_or(EngineError::Disabled("no message selected"))
}

fn new_mail(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.compose = Some(Draft::default());
    Ok(())
}

/// Adds `prefix` unless the subject already starts with it (case-insensitive).
fn prefixed(prefix: &str, subject: &str) -> String {
    let lower = subject.trim_start().to_ascii_lowercase();
    if lower.starts_with(&prefix.to_ascii_lowercase()) { subject.to_string() } else { format!("{prefix} {subject}") }
}

/// The quoted original below the reply, as Outlook classic lays out a plain-text reply header.
fn quoted(m: &Message) -> String {
    let to: Vec<String> = m.to.iter().map(Address::full).collect();
    let mut s = format!(
        "\n\n________________________________\nFrom: {}\nSent: {}\nTo: {}\n",
        m.from.full(),
        m.date.with_timezone(&chrono::Local).format("%A, %B %-d, %Y %-I:%M %p"),
        to.join("; ")
    );
    if !m.cc.is_empty() {
        let cc: Vec<String> = m.cc.iter().map(Address::full).collect();
        s.push_str(&format!("Cc: {}\n", cc.join("; ")));
    }
    s.push_str(&format!("Subject: {}\n\n{}", m.subject, m.body));
    s
}

fn respond(s: &mut Session, all: bool) -> Result<(), EngineError> {
    let m = selected(s)?;
    let me = s.store.account.email.to_ascii_lowercase();
    let mut cc = String::new();
    if all {
        let others: Vec<String> =
            m.to.iter().chain(m.cc.iter()).filter(|a| a.email.to_ascii_lowercase() != me && a.email != m.from.email).map(Address::full).collect();
        cc = others.join("; ");
    }
    s.compose = Some(Draft { to: m.from.full(), cc, subject: prefixed("RE:", &m.subject), body: quoted(&m), replaces: None });
    Ok(())
}

fn reply(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    respond(s, false)
}

fn reply_all(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    respond(s, true)
}

fn forward(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let m = selected(s)?;
    s.compose = Some(Draft { subject: prefixed("FW:", &m.subject), body: quoted(&m), ..Default::default() });
    Ok(())
}

fn remove_selected(s: &mut Session, f: impl FnOnce(&mut Session, u64) -> Result<(), EngineError>) -> Result<(), EngineError> {
    let id = selected(s)?.id;
    let before = s.visible();
    f(s, id)?;
    s.select_after_removal(id, &before);
    Ok(())
}

fn delete(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    remove_selected(s, |s, id| Ok(s.store.delete_message(id)?))
}

fn move_to_kind(s: &mut Session, kind: FolderKind) -> Result<(), EngineError> {
    let folder = s.store.folder_of_kind(kind).ok_or_else(|| EngineError::BadParam(format!("no {} folder", kind.default_name())))?;
    remove_selected(s, |s, id| Ok(s.store.move_message(id, folder)?))
}

fn archive(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    move_to_kind(s, FolderKind::Archive)
}

fn junk(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    move_to_kind(s, FolderKind::Junk)
}

fn move_to(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    if let Some(kind) = str_param(p, "kind") {
        let kind = FolderKind::parse(kind).ok_or_else(|| EngineError::BadParam(format!("unknown folder kind {kind:?}")))?;
        return move_to_kind(s, kind);
    }
    let folder = u64_param(p, "folder").ok_or_else(|| EngineError::BadParam("folder or kind required".into()))?;
    if s.store.folder(folder).is_none() {
        return Err(EngineError::BadParam(format!("no folder {folder}")));
    }
    remove_selected(s, |s, id| Ok(s.store.move_message(id, folder)?))
}

fn set_read(s: &mut Session, read: bool) -> Result<(), EngineError> {
    let id = selected(s)?.id;
    s.store.message_mut(id)?.read = read;
    Ok(())
}

fn mark_read(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    set_read(s, true)
}

fn mark_unread(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    set_read(s, false)
}

fn toggle_flag(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let id = selected(s)?.id;
    let m = s.store.message_mut(id)?;
    m.flagged = !m.flagged;
    Ok(())
}

fn import_eml(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let path = str_param(p, "path").ok_or_else(|| EngineError::BadParam("path required".into()))?;
    eml(s, path)
}

/// Imports one `.eml` file into the current folder.
pub(super) fn eml(s: &mut Session, path: &str) -> Result<(), EngineError> {
    let bytes = super::import::read_capped(path, rslook_mail::parse::MAX_MESSAGE_BYTES)?;
    let msg = rslook_mail::parse::parse_rfc5322(&bytes)?;
    let folder = s.folder;
    let id = s.store.add_message(folder, msg)?;
    s.selected = Some(id);
    s.status = "Imported 1 item".into();
    Ok(())
}
