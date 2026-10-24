//! Compose window commands. Without an account a sent message goes straight to Sent Items; with
//! one it waits in the Outbox for Send/Receive.

use super::{CommandSpec, composing, str_param};
use crate::{EngineError, Session};
use rslook_mail::{Address, FolderKind, NewMessage};
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "compose.set",
        label: "Edit Message Fields",
        shortcut: None,
        params: r#"{"to"?, "cc"?, "subject"?, "body"?: string}"#,
        undoable: false,
        enabled: composing,
        run: set,
    },
    CommandSpec { id: "compose.send", label: "Send", shortcut: Some("Ctrl+Enter"), params: "", undoable: true, enabled: composing, run: send },
    CommandSpec {
        id: "compose.save_draft",
        label: "Save",
        shortcut: Some("Ctrl+S"),
        params: "",
        undoable: true,
        enabled: composing,
        run: save_draft,
    },
    CommandSpec { id: "compose.discard", label: "Discard", shortcut: Some("Escape"), params: "", undoable: false, enabled: composing, run: discard },
];

/// Caps on composed fields (hostile params).
const MAX_FIELD: usize = 1 << 20;

fn set(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let d = s.compose.as_mut().ok_or(EngineError::Disabled("not composing"))?;
    for (key, field) in [("to", &mut d.to), ("cc", &mut d.cc), ("subject", &mut d.subject), ("body", &mut d.body)] {
        if let Some(v) = str_param(p, key) {
            if v.len() > MAX_FIELD {
                return Err(EngineError::BadParam(format!("{key} too long")));
            }
            *field = v.to_string();
        }
    }
    Ok(())
}

fn to_message(s: &Session, read: bool) -> Result<NewMessage, EngineError> {
    let d = s.compose.as_ref().ok_or(EngineError::Disabled("not composing"))?;
    Ok(NewMessage {
        from: s.store.account.clone(),
        to: Address::parse_list(&d.to),
        cc: Address::parse_list(&d.cc),
        subject: d.subject.clone(),
        date: Some(chrono::Utc::now()),
        body: d.body.clone(),
        read,
        ..Default::default()
    })
}

fn file_into(s: &mut Session, kind: FolderKind, read: bool) -> Result<(), EngineError> {
    let msg = to_message(s, read)?;
    let folder = s.store.folder_of_kind(kind).ok_or_else(|| EngineError::BadParam(format!("no {} folder", kind.default_name())))?;
    if let Some(old) = s.compose.as_ref().and_then(|d| d.replaces) {
        // A saved draft is replaced, not sent to Deleted Items.
        if s.store.message(old).is_some() {
            s.store.purge_message(old)?;
        }
    }
    s.store.add_message(folder, msg)?;
    Ok(())
}

fn send(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let to_empty = s.compose.as_ref().is_none_or(|d| Address::parse_list(&d.to).is_empty() && Address::parse_list(&d.cc).is_empty());
    if to_empty {
        return Err(EngineError::BadParam("There must be at least one name or contact group in the To or Cc box.".into()));
    }
    if s.account().is_some() {
        // With an account, sent mail waits in the Outbox until Send/Receive hands it to SMTP.
        file_into(s, FolderKind::Outbox, true)?;
        s.compose = None;
        s.request(crate::sync::SyncRequest::SendAll);
        s.status = if s.work_offline { "Working offline: the message waits in the Outbox".into() } else { "Sending…".into() };
        return Ok(());
    }
    file_into(s, FolderKind::Sent, true)?;
    s.compose = None;
    s.status = "Message sent".into();
    Ok(())
}

fn save_draft(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    file_into(s, FolderKind::Drafts, true)?;
    // Keep composing; later saves replace the stored draft.
    let drafts = s.store.folder_of_kind(FolderKind::Drafts).unwrap_or(0);
    let newest = s.store.messages_in(drafts, "").first().map(|m| m.id);
    if let Some(d) = s.compose.as_mut() {
        d.replaces = newest;
    }
    s.status = "Draft saved".into();
    Ok(())
}

fn discard(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.compose = None;
    Ok(())
}
