//! Navigation and view commands.

use super::{CommandSpec, always, str_param, u64_param};
use crate::{EngineError, Module, ReadingPane, Session};
use rslook_mail::FolderKind;
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "folder.select",
        label: "Go to Folder",
        shortcut: Some("Ctrl+Y"),
        params: r#"{"folder": <id>} or {"kind": "inbox|…"}"#,
        undoable: false,
        enabled: always,
        run: select_folder,
    },
    CommandSpec {
        id: "message.select",
        label: "Select Message",
        shortcut: None,
        params: r#"{"id": <message id>}"#,
        undoable: false,
        enabled: always,
        run: select_message,
    },
    CommandSpec { id: "message.next", label: "Next Item", shortcut: Some("Ctrl+."), params: "", undoable: false, enabled: always, run: next },
    CommandSpec {
        id: "message.previous",
        label: "Previous Item",
        shortcut: Some("Ctrl+,"),
        params: "",
        undoable: false,
        enabled: always,
        run: previous,
    },
    CommandSpec {
        id: "search.set",
        label: "Search",
        shortcut: Some("Ctrl+E"),
        params: r#"{"text": string}"#,
        undoable: false,
        enabled: always,
        run: search,
    },
    CommandSpec {
        id: "view.reading_pane",
        label: "Reading Pane",
        shortcut: None,
        params: r#"{"position": "right|bottom|off"}"#,
        undoable: false,
        enabled: always,
        run: reading_pane,
    },
    CommandSpec { id: "view.folder_pane", label: "Folder Pane", shortcut: None, params: "", undoable: false, enabled: always, run: folder_pane },
    CommandSpec {
        id: "module.mail",
        label: "Mail",
        shortcut: Some("Ctrl+1"),
        params: "",
        undoable: false,
        enabled: always,
        run: |s, _| go(s, Module::Mail),
    },
    CommandSpec {
        id: "module.calendar",
        label: "Calendar",
        shortcut: Some("Ctrl+2"),
        params: "",
        undoable: false,
        enabled: always,
        run: |s, _| go(s, Module::Calendar),
    },
    CommandSpec {
        id: "module.people",
        label: "People",
        shortcut: Some("Ctrl+3"),
        params: "",
        undoable: false,
        enabled: always,
        run: |s, _| go(s, Module::People),
    },
    CommandSpec {
        id: "module.tasks",
        label: "Tasks",
        shortcut: Some("Ctrl+4"),
        params: "",
        undoable: false,
        enabled: always,
        run: |s, _| go(s, Module::Tasks),
    },
];

fn go(s: &mut Session, m: Module) -> Result<(), EngineError> {
    s.module = m;
    Ok(())
}

fn select_folder(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let folder = match (u64_param(p, "folder"), str_param(p, "kind")) {
        (Some(id), _) => s.store.folder(id).map(|f| f.id),
        (None, Some(k)) => FolderKind::parse(k).and_then(|k| s.store.folder_of_kind(k)),
        _ => None,
    }
    .ok_or_else(|| EngineError::BadParam("unknown folder".into()))?;
    s.folder = folder;
    s.module = Module::Mail;
    s.selected = s.visible().first().copied();
    Ok(())
}

/// Selecting a message opens it in the reading pane and marks it read (Outlook's default
/// "mark as read when selection changes").
fn open(s: &mut Session, id: u64) -> Result<(), EngineError> {
    s.selected = Some(id);
    s.store.message_mut(id)?.read = true;
    Ok(())
}

fn select_message(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let id = u64_param(p, "id").ok_or_else(|| EngineError::BadParam("id required".into()))?;
    if !s.visible().contains(&id) {
        return Err(EngineError::BadParam(format!("message {id} is not in this view")));
    }
    open(s, id)
}

fn step(s: &mut Session, forward: bool) -> Result<(), EngineError> {
    let v = s.visible();
    let pos = s.selected.and_then(|id| v.iter().position(|&m| m == id));
    let target = match (pos, forward) {
        (None, _) => v.first(),
        (Some(i), true) => v.get(i.saturating_add(1)),
        (Some(i), false) => i.checked_sub(1).and_then(|j| v.get(j)),
    };
    match target.copied() {
        Some(id) => open(s, id),
        None => Ok(()),
    }
}

fn next(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    step(s, true)
}

fn previous(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    step(s, false)
}

fn search(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let t = str_param(p, "text").unwrap_or("");
    s.search = t.chars().take(1000).collect();
    let v = s.visible();
    if !s.selected.is_some_and(|id| v.contains(&id)) {
        s.selected = v.first().copied();
    }
    Ok(())
}

fn reading_pane(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    s.reading_pane = match str_param(p, "position") {
        Some("right") => ReadingPane::Right,
        Some("bottom") => ReadingPane::Bottom,
        Some("off") => ReadingPane::Off,
        other => return Err(EngineError::BadParam(format!("position must be right, bottom or off, not {other:?}"))),
    };
    Ok(())
}

fn folder_pane(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.folder_pane = !s.folder_pane;
    Ok(())
}
