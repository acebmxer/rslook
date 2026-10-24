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
