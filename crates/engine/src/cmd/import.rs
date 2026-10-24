//! Import commands: the Import/Export dialog (File tab) and importing `.pst`, `.msg` and `.eml`
//! files. A `.pst` lands in a new top-level folder named after the file, with its folder tree
//! below it, as Outlook classic's Import/Export wizard does; a single message goes into the
//! current folder.

use std::path::Path;

use super::{CommandSpec, always, str_param};
use crate::{EngineError, ImportDraft, Session};
use rslook_mail::{FolderId, FolderKind, Store};
use rslook_pst::PstFolder;
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec { id: "import.open", label: "Import/Export", shortcut: None, params: "", undoable: false, enabled: always, run: open },
    CommandSpec {
        id: "import.set",
        label: "Edit Import File",
        shortcut: None,
        params: r#"{"path": string}"#,
        undoable: false,
        enabled: dialog_open,
        run: set,
    },
    CommandSpec { id: "import.run", label: "Import", shortcut: None, params: "", undoable: true, enabled: dialog_open, run: run_dialog },
    CommandSpec { id: "import.cancel", label: "Cancel", shortcut: None, params: "", undoable: false, enabled: dialog_open, run: cancel },
    CommandSpec {
        id: "mail.import",
        label: "Import File",
        shortcut: None,
        params: r#"{"path": "/abs/file.pst|.msg|.eml"}"#,
        undoable: true,
        enabled: always,
        run: import_any,
    },
    CommandSpec {
        id: "mail.import_pst",
        label: "Import Outlook Data File (.pst)",
        shortcut: None,
        params: r#"{"path": "/abs/file.pst"}"#,
        undoable: true,
        enabled: always,
        run: import_pst,
    },
    CommandSpec {
        id: "mail.import_msg",
        label: "Import .msg",
        shortcut: None,
        params: r#"{"path": "/abs/file.msg"}"#,
        undoable: true,
        enabled: always,
        run: import_msg,
    },
];

/// Longest path the dialog accepts (hostile params).
const MAX_PATH: usize = 4096;

fn dialog_open(s: &Session) -> bool {
    s.import_dialog.is_some()
}

fn open(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.import_dialog = Some(ImportDraft::default());
    Ok(())
}

fn set(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    let d = s.import_dialog.as_mut().ok_or(EngineError::Disabled("import dialog is closed"))?;
    if let Some(path) = str_param(p, "path") {
        d.path = path.chars().take(MAX_PATH).collect();
        d.error = None;
    }
    Ok(())
}

fn cancel(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.import_dialog = None;
    Ok(())
}

/// A path as people paste it: surrounding quotes or a `file://` prefix from a file manager.
fn clean_path(p: &str) -> &str {
    let p = p.trim();
    let p = p.strip_prefix('"').and_then(|q| q.strip_suffix('"')).unwrap_or(p);
    let p = p.strip_prefix('\'').and_then(|q| q.strip_suffix('\'')).unwrap_or(p);
    p.strip_prefix("file://").unwrap_or(p)
}

fn run_dialog(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let d = s.import_dialog.clone().ok_or(EngineError::Disabled("import dialog is closed"))?;
    let path = clean_path(&d.path).to_string();
    let result = if path.is_empty() { Err(EngineError::BadParam("Enter the file to import.".into())) } else { import_path(s, &path) };
    match result {
        Ok(()) => {
            s.import_dialog = None;
            Ok(())
        }
        Err(e) => {
            if let Some(d) = s.import_dialog.as_mut() {
                d.error = Some(e.to_string());
            }
            Err(e)
        }
    }
}

fn path_param(p: &Value) -> Result<&str, EngineError> {
    str_param(p, "path").ok_or_else(|| EngineError::BadParam("path required".into()))
}

fn import_any(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    import_path(s, path_param(p)?)
}

fn import_path(s: &mut Session, path: &str) -> Result<(), EngineError> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("pst") => pst(s, path),
        Some("msg") => msg(s, path),
        Some("eml") => super::mail::eml(s, path),
        _ => Err(EngineError::BadParam("Choose a .pst, .msg or .eml file.".into())),
    }
}

fn import_pst(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    pst(s, path_param(p)?)
}

fn import_msg(s: &mut Session, p: &Value) -> Result<(), EngineError> {
    msg(s, path_param(p)?)
}

/// Reads a whole file, refusing files above `cap` before reading them.
pub(super) fn read_capped(path: &str, cap: usize) -> Result<Vec<u8>, EngineError> {
    let meta = std::fs::metadata(path).map_err(|e| EngineError::Io(e.to_string()))?;
    let len = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    if len > cap {
        return Err(EngineError::Mail(rslook_mail::MailError::TooLarge(len)));
    }
    std::fs::read(path).map_err(|e| EngineError::Io(e.to_string()))
}

fn msg(s: &mut Session, path: &str) -> Result<(), EngineError> {
    let bytes = read_capped(path, rslook_msg::MAX_MSG_BYTES)?;
    let m = rslook_msg::parse_msg(&bytes).map_err(|e| EngineError::Import(e.to_string()))?;
    let folder = s.folder;
    let id = s.store.add_message(folder, m)?;
    s.selected = Some(id);
    s.status = "Imported 1 item".into();
    Ok(())
}

/// `base`, or `base (2)`, `base (3)`… if a top-level folder already has that name.
fn free_top_name(store: &Store, base: &str) -> String {
    let taken = |n: &str| store.folders().iter().any(|f| f.parent.is_none() && f.name.eq_ignore_ascii_case(n));
    if !taken(base) {
        return base.to_string();
    }
    (2..=1000).map(|n| format!("{base} ({n})")).find(|n| !taken(n)).unwrap_or_else(|| format!("{base} (imported)"))
}

fn add_tree(store: &mut Store, parent: FolderId, f: PstFolder) -> Result<(), EngineError> {
    for m in f.messages {
        store.add_message(parent, m)?;
    }
    for child in f.children {
        let name = if child.name.trim().is_empty() { "Folder".to_string() } else { child.name.clone() };
        // Custom kind on purpose: an imported "Inbox" must never be taken for the real one
        // (Send/Receive pairs standard folders with the server's).
        let id = store.add_folder(&name, FolderKind::Custom, Some(parent));
        add_tree(store, id, child)?;
    }
    Ok(())
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

fn pst(s: &mut Session, path: &str) -> Result<(), EngineError> {
    let imp = rslook_pst::read_pst(Path::new(path)).map_err(|e| EngineError::Import(e.to_string()))?;
    let stem = Path::new(path).file_stem().and_then(|n| n.to_str()).map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Imported");
    let name = free_top_name(&s.store, stem);
    let count = imp.root.message_count();
    let top = s.store.add_folder(&name, FolderKind::Custom, None);
    add_tree(&mut s.store, top, imp.root)?;
    s.folder = top;
    s.selected = s.visible().first().copied();
    let mut status = format!("Imported {} into {name}", plural(count, "item"));
    if imp.skipped > 0 {
        status.push_str(&format!("; {} skipped (calendar, contacts, tasks and notes can't be imported yet)", plural(imp.skipped, "item")));
    }
    if imp.failed > 0 {
        status.push_str(&format!("; {} could not be read", plural(imp.failed, "damaged item")));
    }
    s.status = status;
    Ok(())
}
