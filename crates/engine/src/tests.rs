use crate::sample::sample_session;
use crate::{Module, ReadingPane, Session, catalog, commands};
use rslook_mail::FolderKind;
use serde_json::{Value, json};

fn run(s: &mut Session, id: &str, p: Value) {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn selected_subject(s: &Session) -> String {
    s.store.message(s.selected.unwrap()).unwrap().subject.clone()
}

#[test]
fn sample_opens_on_inbox_with_newest_selected() {
    let s = sample_session();
    assert_eq!(s.store.folder(s.folder).unwrap().kind, FolderKind::Inbox);
    assert_eq!(selected_subject(&s), "Q4 roadmap review — slides attached");
}

#[test]
fn ids_are_unique_and_shortcuts_unique() {
    let cmds = commands();
    let mut ids: Vec<_> = cmds.iter().map(|c| c.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), cmds.len());
    let mut keys: Vec<_> = cmds.iter().filter_map(|c| c.shortcut).collect();
    keys.sort();
    let n = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), n, "duplicate shortcut");
}

#[test]
fn delete_selects_next_and_undo_restores() {
    let mut s = sample_session();
    let first = s.selected.unwrap();
    let second = s.visible()[1];
    run(&mut s, "mail.delete", json!({}));
    assert_eq!(s.selected, Some(second));
    let deleted = s.store.folder_of_kind(FolderKind::Deleted).unwrap();
    assert_eq!(s.store.message(first).unwrap().folder, deleted);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.store.message(first).unwrap().folder, s.folder);
    run(&mut s, "edit.redo", json!({}));
    assert_eq!(s.store.message(first).unwrap().folder, deleted);
}

#[test]
fn reply_reply_all_forward_prefixes() {
    let mut s = sample_session();
    run(&mut s, "mail.reply", json!({}));
    let d = s.compose.clone().unwrap();
    assert_eq!(d.subject, "RE: Q4 roadmap review — slides attached");
    assert!(d.to.contains("priya@lumen.example"));
    assert!(d.body.contains("From: Priya Natarajan <priya@lumen.example>"));
    run(&mut s, "compose.discard", json!({}));
    run(&mut s, "mail.forward", json!({}));
    assert_eq!(s.compose.as_ref().unwrap().subject, "FW: Q4 roadmap review — slides attached");
    assert!(s.compose.as_ref().unwrap().to.is_empty());
}

#[test]
fn reply_does_not_double_prefix() {
    let mut s = sample_session();
    run(&mut s, "search.set", json!({"text": "offline mode"}));
    run(&mut s, "mail.reply", json!({}));
    assert_eq!(s.compose.unwrap().subject, "Re: offline mode design");
}

#[test]
fn send_requires_recipient_then_files_in_sent() {
    let mut s = sample_session();
    run(&mut s, "mail.new", json!({}));
    assert!(s.run("compose.send", &json!({})).is_err());
    assert!(s.compose.is_some(), "a refused send keeps the draft open");
    run(&mut s, "compose.set", json!({"to": "Sam <sam@x.example>", "subject": "Hi", "body": "Hello"}));
    let sent = s.store.folder_of_kind(FolderKind::Sent).unwrap();
    let before = s.store.total_count(sent);
    run(&mut s, "compose.send", json!({}));
    assert!(s.compose.is_none());
    assert_eq!(s.store.total_count(sent), before + 1);
    assert_eq!(s.store.messages_in(sent, "")[0].to[0].email, "sam@x.example");
}

#[test]
fn saving_a_draft_twice_keeps_one_copy() {
    let mut s = sample_session();
    let drafts = s.store.folder_of_kind(FolderKind::Drafts).unwrap();
    let deleted = s.store.folder_of_kind(FolderKind::Deleted).unwrap();
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"subject": "one"}));
    run(&mut s, "compose.save_draft", json!({}));
    run(&mut s, "compose.set", json!({"subject": "two"}));
    run(&mut s, "compose.save_draft", json!({}));
    assert_eq!(s.store.total_count(drafts), 1);
    assert_eq!(s.store.messages_in(drafts, "")[0].subject, "two");
    assert_eq!(s.store.total_count(deleted), 0);
}

#[test]
fn navigation_marks_read_and_tags_toggle() {
    let mut s = sample_session();
    run(&mut s, "message.next", json!({}));
    let id = s.selected.unwrap();
    assert!(s.store.message(id).unwrap().read);
    run(&mut s, "mail.mark_unread", json!({}));
    assert!(!s.store.message(id).unwrap().read);
    let flagged = s.store.message(id).unwrap().flagged;
    run(&mut s, "mail.toggle_flag", json!({}));
    assert_eq!(s.store.message(id).unwrap().flagged, !flagged);
}

#[test]
fn folders_views_and_modules() {
    let mut s = sample_session();
    run(&mut s, "folder.select", json!({"kind": "sent"}));
    assert_eq!(selected_subject(&s), "Re: Lunch Thursday?");
    run(&mut s, "mail.move", json!({"kind": "archive"}));
    run(&mut s, "folder.select", json!({"kind": "archive"}));
    assert_eq!(s.visible().len(), 1);
    run(&mut s, "view.reading_pane", json!({"position": "bottom"}));
    assert_eq!(s.reading_pane, ReadingPane::Bottom);
    run(&mut s, "module.calendar", json!({}));
    assert_eq!(s.module, Module::Calendar);
}

#[test]
fn import_eml_from_disk() {
    let dir = std::env::temp_dir().join(format!("rslook-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.eml");
    std::fs::write(&path, "From: Q <q@x.example>\r\nSubject: imported\r\n\r\nbody\r\n").unwrap();
    let mut s = sample_session();
    run(&mut s, "mail.import_eml", json!({"path": path.to_str().unwrap()}));
    assert_eq!(selected_subject(&s), "imported");
    std::fs::remove_dir_all(&dir).ok();
}

fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rslook-import-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn import_pst_adds_a_folder_tree_named_after_the_file_and_undoes() {
    let path = temp_file("old-archive.pst", &rslook_pst::sample::sample_pst().unwrap());
    let mut s = sample_session();
    let before = s.store.clone();
    run(&mut s, "mail.import", json!({"path": path.to_str().unwrap()}));
    let top = s.store.folder(s.folder).unwrap().clone();
    assert_eq!((top.name.as_str(), top.parent, top.kind), ("old-archive", None, FolderKind::Custom));
    let child = |parent, name: &str| s.store.folders().iter().find(|f| f.parent == Some(parent) && f.name == name).map(|f| f.id);
    let inbox = child(top.id, "Inbox").unwrap();
    assert_eq!(s.store.folder(inbox).unwrap().kind, FolderKind::Custom, "an imported Inbox is not the real Inbox");
    assert_eq!(s.store.total_count(inbox), 3);
    assert_eq!(s.store.unread_count(inbox), 2);
    let y2026 = child(child(top.id, "Projects").unwrap(), "2026").unwrap();
    assert_eq!(s.store.total_count(y2026), 1);
    assert!(child(top.id, "Calendar").is_none());
    assert!(s.status.contains("Imported 4 items into old-archive"), "{}", s.status);
    assert!(s.status.contains("2 items skipped"), "{}", s.status);
    // The real Inbox is untouched.
    let real = s.store.folder_of_kind(FolderKind::Inbox).unwrap();
    assert_eq!(s.store.total_count(real), before.total_count(real));

    // A second import of a file with the same name gets its own folder.
    run(&mut s, "mail.import_pst", json!({"path": path.to_str().unwrap()}));
    assert_eq!(s.store.folder(s.folder).unwrap().name, "old-archive (2)");

    run(&mut s, "edit.undo", json!({}));
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(s.store, before);
    std::fs::remove_file(&path).ok();
}

#[test]
fn import_msg_into_current_folder() {
    use rslook_msg::mapi::{MapiMessage, PropValue, id};
    let mut m = MapiMessage::default();
    m.props.insert(id::MESSAGE_CLASS, PropValue::String("IPM.Note".into()));
    m.props.insert(id::SUBJECT, PropValue::String("from a msg file".into()));
    let path = temp_file("saved.msg", &rslook_msg::fixture::write_msg(&m, false).unwrap());
    let mut s = sample_session();
    run(&mut s, "mail.import", json!({"path": path.to_str().unwrap()}));
    assert_eq!(selected_subject(&s), "from a msg file");
    assert_eq!(s.store.message(s.selected.unwrap()).unwrap().folder, s.folder);
    std::fs::remove_file(&path).ok();
}

#[test]
fn import_dialog_reports_errors_and_closes_on_success() {
    let path = temp_file("dialog.pst", &rslook_pst::sample::sample_pst().unwrap());
    let junk = temp_file("junk.pst", b"not a pst at all");
    let mut s = sample_session();
    run(&mut s, "import.open", json!({}));
    assert!(s.run("import.run", &json!({})).is_err(), "empty path");
    assert!(s.import_dialog.as_ref().unwrap().error.is_some());
    run(&mut s, "import.set", json!({"path": "/tmp/notes.txt"}));
    assert!(s.import_dialog.as_ref().unwrap().error.is_none(), "editing clears the error");
    assert!(s.run("import.run", &json!({})).is_err(), "wrong extension");
    let before = s.store.clone();
    run(&mut s, "import.set", json!({"path": junk.to_str().unwrap()}));
    assert!(s.run("import.run", &json!({})).is_err(), "damaged file");
    assert_eq!(s.store, before, "a failed import adds nothing");
    assert!(s.import_dialog.as_ref().unwrap().error.as_deref().unwrap().contains("could not import"));
    // Quotes from a pasted path are accepted.
    run(&mut s, "import.set", json!({"path": format!("\"{}\"", path.to_str().unwrap())}));
    run(&mut s, "import.run", json!({}));
    assert!(s.import_dialog.is_none());
    assert_eq!(s.store.folder(s.folder).unwrap().name, "dialog");
    run(&mut s, "import.open", json!({}));
    run(&mut s, "import.cancel", json!({}));
    assert!(s.import_dialog.is_none());
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&junk).ok();
}

#[test]
fn hostile_params_never_panic() {
    let junk = [
        json!(null),
        json!({}),
        json!([1, 2]),
        json!({"id": -1, "folder": u64::MAX, "kind": "nope", "position": 7, "text": "\u{0}é", "path": "/nonexistent/\u{0}"}),
        json!({"id": 1e300, "folder": "x", "to": 5, "body": {"a": 1}}),
    ];
    for c in commands() {
        for p in &junk {
            let mut s = sample_session();
            let _ = s.run("mail.new", &json!({}));
            let _ = s.run(c.id, p);
            let mut e = Session::empty(Default::default());
            let _ = e.run(c.id, p);
        }
    }
}

#[test]
fn parity_floor() {
    let (done, total) = catalog::parity();
    assert!(total > 0);
    assert!(done >= 20, "parity fell to {done}/{total}");
    for f in catalog::FEATURES {
        if let Some(c) = f.command {
            assert!(crate::find(c).is_some(), "catalog names unknown command {c}");
        }
    }
}
