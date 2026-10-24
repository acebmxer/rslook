//! Tests for accounts, the offline change queue and merging Send/Receive results.

use crate::account::{Account, MemorySecrets, SecretStore, Security, Server};
use crate::sync::*;
use crate::{Session, sample};
use rslook_mail::{Address, FolderKind, NewMessage};
use serde_json::{Value, json};
use std::sync::Arc;

fn run(s: &mut Session, id: &str, p: Value) {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn account() -> Account {
    Account {
        name: "Test User".into(),
        email: "test@example.test".into(),
        username: "test".into(),
        imap: Server { host: "imap.example.test".into(), port: 993, security: Security::Tls },
        smtp: Server { host: "smtp.example.test".into(), port: 587, security: Security::StartTls },
    }
}

fn msg(subject: &str) -> NewMessage {
    NewMessage { from: Address::new("Sam", "sam@example.test"), subject: subject.into(), body: format!("{subject} body"), ..Default::default() }
}

fn rmsg(uid: u32, read: bool, content: Option<&str>) -> RemoteMessage {
    RemoteMessage { uid, read, flagged: false, content: content.map(msg) }
}

fn folder(name: &str, kind: Option<FolderKind>, special: bool) -> RemoteFolder {
    RemoteFolder { name: name.into(), display: name.into(), delimiter: Some("/".into()), kind, special_use: special, selectable: true }
}

fn server_folders() -> Vec<RemoteFolder> {
    vec![
        folder("INBOX", Some(FolderKind::Inbox), true),
        folder("Sent", Some(FolderKind::Sent), true),
        folder("Trash", Some(FolderKind::Deleted), false),
        folder("Projects", None, false),
        folder("Projects/2026", None, false),
    ]
}

fn secrets() -> Arc<MemorySecrets> {
    let s = Arc::new(MemorySecrets::default());
    let _ = s.set("test@example.test", "pw");
    s
}

/// Runs one Send/Receive round trip with a canned report.
fn sync(s: &mut Session, request: SyncRequest, report: Report) -> Job {
    s.request(request);
    let job = s.next_job().expect("a job");
    s.apply_result(JobResult { job: job.id, outcome: Ok(report) });
    job
}

/// A session with the test account after a first sync: three Inbox messages (UID 1..3).
fn synced() -> Session {
    let mut s = Session::empty(Address::default());
    s.set_secrets(secrets());
    s.open_account(account(), None);
    sync(
        &mut s,
        SyncRequest::All,
        Report {
            folders: Some(server_folders()),
            contents: vec![
                MailboxContents {
                    mailbox: "INBOX".into(),
                    uid_validity: 7,
                    messages: vec![rmsg(1, false, Some("one")), rmsg(2, true, Some("two")), rmsg(3, false, Some("three"))],
                },
                MailboxContents { mailbox: "Trash".into(), uid_validity: 9, messages: vec![] },
            ],
            ..Default::default()
        },
    );
    s
}

fn id_of(s: &Session, subject: &str) -> u64 {
    s.store.messages().iter().find(|m| m.subject == subject).map(|m| m.id).unwrap_or_else(|| panic!("no message {subject}"))
}

fn kinds(s: &Session) -> Vec<OpKind> {
    s.queue().iter().map(|op| op.kind.clone()).collect()
}

#[test]
fn first_sync_maps_folders_and_downloads_messages() {
    let s = synced();
    let inbox = s.store.folder_of_kind(FolderKind::Inbox).unwrap();
    assert_eq!(s.store.folder(inbox).unwrap().remote.as_deref(), Some("INBOX"));
    assert_eq!(s.store.total_count(inbox), 3);
    assert_eq!(s.store.unread_count(inbox), 2);
    let deleted = s.store.folder_of_kind(FolderKind::Deleted).unwrap();
    assert_eq!(s.store.folder(deleted).unwrap().remote.as_deref(), Some("Trash"));
    let projects = s.store.folder_by_remote("Projects").unwrap();
    let sub = s.store.folder_by_remote("Projects/2026").unwrap();
    assert_eq!(s.store.folder(sub).unwrap().parent, Some(projects));
    assert_eq!(s.store.folder(projects).unwrap().kind, FolderKind::Custom);
    assert!(s.store.folder(s.store.folder_of_kind(FolderKind::Outbox).unwrap()).unwrap().remote.is_none());
    assert_eq!(s.sync_state, SyncState::Idle);
    assert!(s.last_sync.is_some());
}

#[test]
fn flag_changes_queue_one_op_per_message() {
    let mut s = synced();
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    run(&mut s, "mail.toggle_flag", json!({}));
    assert_eq!(kinds(&s), vec![OpKind::Flags { read: true, flagged: true }]);
    assert_eq!(s.queue()[0].target.uid, 1);
    run(&mut s, "mail.mark_unread", json!({}));
    assert_eq!(kinds(&s), vec![OpKind::Flags { read: false, flagged: true }]);
}

#[test]
fn delete_queues_a_move_and_undo_takes_it_back() {
    let mut s = synced();
    let two = id_of(&s, "two");
    run(&mut s, "message.select", json!({"id": two}));
    run(&mut s, "mail.delete", json!({}));
    assert_eq!(kinds(&s), vec![OpKind::Move { to: "Trash".into(), create: false }]);
    run(&mut s, "edit.undo", json!({}));
    assert!(s.queue().is_empty(), "back where the server has it: {:?}", s.queue());
}

#[test]
fn permanent_delete_replaces_the_move_and_undo_restores_it() {
    let mut s = synced();
    let two = id_of(&s, "two");
    run(&mut s, "message.select", json!({"id": two}));
    run(&mut s, "mail.delete", json!({}));
    run(&mut s, "folder.select", json!({"kind": "deleted"}));
    run(&mut s, "message.select", json!({"id": two}));
    run(&mut s, "mail.delete", json!({}));
    assert_eq!(kinds(&s), vec![OpKind::Delete]);
    run(&mut s, "edit.undo", json!({}));
    assert_eq!(kinds(&s), vec![OpKind::Move { to: "Trash".into(), create: false }]);
}

#[test]
fn moving_to_a_folder_the_server_lacks_creates_it() {
    let mut s = synced();
    let three = id_of(&s, "three");
    run(&mut s, "message.select", json!({"id": three}));
    run(&mut s, "mail.archive", json!({}));
    let read_op = OpKind::Flags { read: true, flagged: false };
    assert_eq!(kinds(&s), vec![read_op, OpKind::Move { to: "Archive".into(), create: true }]);
    let archive = s.store.folder_of_kind(FolderKind::Archive).unwrap();
    assert_eq!(s.store.folder(archive).unwrap().remote.as_deref(), Some("Archive"));
}

#[test]
fn outbox_is_local_only() {
    let mut s = synced();
    let one = id_of(&s, "one");
    let outbox = s.store.folder_of_kind(FolderKind::Outbox).unwrap();
    run(&mut s, "message.select", json!({"id": one}));
    let before = s.queue().len();
    run(&mut s, "mail.move", json!({"folder": outbox}));
    assert_eq!(s.queue().len(), before, "no server move for the Outbox");
}

#[test]
fn jobs_carry_known_uids_queued_ops_and_the_password() {
    let mut s = synced();
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    s.request(SyncRequest::All);
    let job = s.next_job().unwrap();
    assert_eq!(job.password, "pw");
    assert_eq!(job.pull, Pull::All);
    assert_eq!(job.ops.len(), 1);
    let known = job.known.get("INBOX").unwrap();
    assert_eq!(known.uid_validity, 7);
    assert_eq!(known.uids.len(), 3);
    assert!(s.busy());
    assert!(s.next_job().is_none(), "one job at a time");
}

#[test]
fn merge_applies_server_changes_and_finishes_ops() {
    let mut s = synced();
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    let op = s.queue()[0].id;
    sync(
        &mut s,
        SyncRequest::All,
        Report {
            done_ops: vec![op],
            folders: Some(server_folders()),
            contents: vec![MailboxContents {
                mailbox: "INBOX".into(),
                uid_validity: 7,
                // 2 deleted elsewhere, 3 read elsewhere, 4 is new.
                messages: vec![rmsg(1, true, None), rmsg(3, true, None), rmsg(4, false, Some("four"))],
            }],
            ..Default::default()
        },
    );
    assert!(s.queue().is_empty());
    let inbox = s.store.folder_of_kind(FolderKind::Inbox).unwrap();
    let subjects: Vec<&str> = s.store.messages_in(inbox, "").iter().map(|m| m.subject.as_str()).collect();
    assert_eq!(subjects.len(), 3, "{subjects:?}");
    assert!(!subjects.contains(&"two"));
    assert!(s.store.message(id_of(&s, "three")).unwrap().read);
    assert!(!s.can_undo(), "undo steps would point at replaced server state");
}

#[test]
fn merge_keeps_local_changes_still_waiting_in_the_queue() {
    let mut s = synced();
    s.request(SyncRequest::All);
    let job = s.next_job().unwrap();
    // The user marks a message read while the job runs.
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    s.apply_result(JobResult {
        job: job.id,
        outcome: Ok(Report {
            contents: vec![MailboxContents {
                mailbox: "INBOX".into(),
                uid_validity: 7,
                messages: vec![rmsg(1, false, None), rmsg(2, true, None), rmsg(3, false, None)],
            }],
            ..Default::default()
        }),
    });
    assert!(s.store.message(one).unwrap().read);
    assert_eq!(s.queue().len(), 1);
}

#[test]
fn a_new_uid_validity_downloads_the_folder_again() {
    let mut s = synced();
    sync(
        &mut s,
        SyncRequest::All,
        Report {
            contents: vec![MailboxContents { mailbox: "INBOX".into(), uid_validity: 8, messages: vec![rmsg(1, false, Some("again"))] }],
            ..Default::default()
        },
    );
    let inbox = s.store.folder_of_kind(FolderKind::Inbox).unwrap();
    let all = s.store.messages_in(inbox, "");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].subject, "again");
    assert_eq!(all[0].remote.as_ref().unwrap().uid_validity, 8);
}

#[test]
fn folders_gone_from_the_server_go_locally_but_standard_ones_stay() {
    let mut s = synced();
    let only_inbox = vec![folder("INBOX", Some(FolderKind::Inbox), true)];
    sync(&mut s, SyncRequest::All, Report { folders: Some(only_inbox), ..Default::default() });
    assert!(s.store.folder_by_remote("Projects").is_none());
    let deleted = s.store.folder_of_kind(FolderKind::Deleted).unwrap();
    assert!(s.store.folder(deleted).unwrap().remote.is_none());
}

#[test]
fn sending_with_an_account_goes_through_the_outbox() {
    let mut s = synced();
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "ann@example.test", "subject": "Hello", "body": "Hi"}));
    run(&mut s, "compose.send", json!({}));
    let outbox = s.store.folder_of_kind(FolderKind::Outbox).unwrap();
    let sent = s.store.folder_of_kind(FolderKind::Sent).unwrap();
    assert_eq!(s.store.total_count(outbox), 1);
    s.request(SyncRequest::SendAll);
    let job = s.next_job().unwrap();
    assert_eq!(job.outbox.len(), 1);
    assert_eq!(job.outbox[0].to[0].email, "ann@example.test");
    assert_eq!(job.sent_mailbox, Some(("Sent".to_string(), false)));
    assert_eq!(job.pull, Pull::None);
    assert!(job.ops.is_empty(), "Send All doesn't replay changes");
    let id = job.outbox[0].id;
    s.apply_result(JobResult {
        job: job.id,
        outcome: Ok(Report { sent: vec![SentItem { id, appended_to: Some("Sent".into()) }], ..Default::default() }),
    });
    assert_eq!(s.store.total_count(outbox), 0);
    assert_eq!(s.store.total_count(sent), 1);
    // The next pull of Sent swaps in the server copy instead of adding a second one.
    sync(
        &mut s,
        SyncRequest::All,
        Report {
            contents: vec![MailboxContents { mailbox: "Sent".into(), uid_validity: 3, messages: vec![rmsg(1, true, Some("Hello"))] }],
            ..Default::default()
        },
    );
    assert_eq!(s.store.total_count(sent), 1);
    assert_eq!(s.store.messages_in(sent, "")[0].remote.as_ref().unwrap().uid, 1);
}

#[test]
fn a_failed_send_stays_in_the_outbox() {
    let mut s = synced();
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "ann@example.test"}));
    run(&mut s, "compose.send", json!({}));
    s.request(SyncRequest::SendAll);
    let job = s.next_job().unwrap();
    let id = job.outbox[0].id;
    s.apply_result(JobResult { job: job.id, outcome: Ok(Report { send_failed: vec![(id, "relay denied".into())], ..Default::default() }) });
    let outbox = s.store.folder_of_kind(FolderKind::Outbox).unwrap();
    assert_eq!(s.store.total_count(outbox), 1);
    assert!(s.status.contains("relay denied"), "{}", s.status);
}

#[test]
fn a_job_that_could_not_connect_keeps_the_queue() {
    let mut s = synced();
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    s.request(SyncRequest::All);
    let job = s.next_job().unwrap();
    s.apply_result(JobResult { job: job.id, outcome: Err("could not connect".into()) });
    assert_eq!(s.sync_state, SyncState::Error("could not connect".into()));
    assert_eq!(s.queue().len(), 1);
    // A stale result for another job is ignored.
    s.apply_result(JobResult { job: 999, outcome: Ok(Report::default()) });
    assert_eq!(s.queue().len(), 1);
}

#[test]
fn partial_failure_still_records_what_was_sent() {
    let mut s = synced();
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "ann@example.test"}));
    run(&mut s, "compose.send", json!({}));
    s.request(SyncRequest::All);
    let job = s.next_job().unwrap();
    let id = job.outbox[0].id;
    s.apply_result(JobResult {
        job: job.id,
        outcome: Ok(Report { sent: vec![SentItem { id, appended_to: None }], error: Some("connection lost".into()), ..Default::default() }),
    });
    let outbox = s.store.folder_of_kind(FolderKind::Outbox).unwrap();
    assert_eq!(s.store.total_count(outbox), 0, "sent mail must not be sent twice");
    assert_eq!(s.sync_state, SyncState::Error("connection lost".into()));
}

#[test]
fn work_offline_holds_requests_and_going_online_syncs() {
    let mut s = synced();
    run(&mut s, "sendrecv.work_offline", json!({}));
    assert!(s.run("sendrecv.all", &json!({})).is_err());
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "ann@example.test"}));
    run(&mut s, "compose.send", json!({}));
    assert!(!s.has_requests());
    assert!(s.next_job().is_none());
    run(&mut s, "sendrecv.work_offline", json!({}));
    let job = s.next_job().unwrap();
    assert_eq!(job.pull, Pull::All);
    assert_eq!(job.outbox.len(), 1);
}

#[test]
fn update_folder_pulls_only_the_current_folder() {
    let mut s = synced();
    run(&mut s, "sendrecv.update_folder", json!({}));
    let job = s.next_job().unwrap();
    assert_eq!(job.pull, Pull::Mailbox("INBOX".into()));
    assert!(job.outbox.is_empty());
}

#[test]
fn a_missing_password_is_reported_not_sent() {
    let mut s = synced();
    s.set_secrets(Arc::new(MemorySecrets::default()));
    s.request(SyncRequest::All);
    assert!(s.next_job().is_none());
    assert!(matches!(&s.sync_state, SyncState::Error(e) if e.contains("password")));
}

fn temp_paths(tag: &str) -> Paths {
    let base = std::env::temp_dir().join(format!("rslook-{tag}-{}", std::process::id()));
    Paths { config: base.join("config"), data: base.join("data") }
}

#[test]
fn account_dialog_saves_settings_and_keychain_but_no_password_on_disk() {
    let mut s = sample::sample_session();
    let keys = Arc::new(MemorySecrets::default());
    s.set_secrets(keys.clone());
    let paths = temp_paths("acct");
    s.set_paths(paths.clone());
    run(&mut s, "account.add", json!({}));
    run(
        &mut s,
        "account.set",
        json!({"name": "Test User", "email": "test@example.test", "username": "test", "password": "s3cret-pw",
               "imap_host": "imap.example.test", "smtp_host": "smtp.example.test"}),
    );
    run(&mut s, "account.save", json!({}));
    assert!(s.account_dialog.is_none());
    assert_eq!(s.account().unwrap().imap.port, 993);
    assert_eq!(s.account().unwrap().smtp.security, Security::StartTls);
    assert_eq!(keys.get("test@example.test").unwrap(), "s3cret-pw");
    let file = std::fs::read_to_string(paths.accounts_file()).unwrap();
    assert!(file.contains("imap.example.test") && !file.contains("s3cret-pw"));
    // The sample mail is gone: this is the new account's (empty) mailbox, waiting for a sync.
    let inbox = s.store.folder_of_kind(FolderKind::Inbox).unwrap();
    assert_eq!(s.store.total_count(inbox), 0);
    assert!(s.has_requests());
    // Editing without retyping the password keeps it.
    run(&mut s, "account.settings", json!({}));
    run(&mut s, "account.set", json!({"name": "Renamed"}));
    run(&mut s, "account.save", json!({}));
    assert_eq!(s.store.account.name, "Renamed");
    assert_eq!(keys.get("test@example.test").unwrap(), "s3cret-pw");
    // Remove forgets the password and the files.
    s.dirty = true;
    s.save_cache().unwrap();
    assert!(paths.cache_file(&account()).exists());
    run(&mut s, "account.remove", json!({}));
    assert!(s.account().is_none());
    assert!(keys.get("test@example.test").is_err());
    assert!(!paths.cache_file(&account()).exists());
    std::fs::remove_dir_all(paths.config.parent().unwrap()).ok();
}

#[test]
fn account_dialog_refuses_bad_input_and_says_why() {
    let mut s = Session::empty(Address::default());
    run(&mut s, "account.add", json!({}));
    run(&mut s, "account.set", json!({"email": "test@example.test", "imap_host": "imap.example.test", "smtp_host": "smtp.example.test"}));
    assert!(s.run("account.save", &json!({})).is_err());
    assert_eq!(s.account_dialog.as_ref().unwrap().error.as_deref(), Some("Enter the password."));
    run(&mut s, "account.set", json!({"password": "x", "imap_port": "99999"}));
    assert!(s.run("account.save", &json!({})).is_err());
    assert!(s.account_dialog.as_ref().unwrap().error.as_deref().unwrap().contains("port"));
    assert!(s.run("account.set", &json!({"smtp_security": "bogus"})).is_err());
    assert!(s.account().is_none());
}

#[test]
fn offline_cache_round_trips_with_the_queue() {
    let mut s = synced();
    let paths = temp_paths("cache");
    s.set_paths(paths.clone());
    let one = id_of(&s, "one");
    run(&mut s, "message.select", json!({"id": one}));
    s.save_cache().unwrap();
    let (store, queue, next_op) = load_cache(&paths.cache_file(&account())).unwrap().unwrap();
    assert_eq!(store, s.store);
    assert_eq!(queue, s.queue());
    let mut again = Session::empty(Address::default());
    again.open_account(account(), Some((store, queue, next_op)));
    assert_eq!(again.queue().len(), 1);
    assert!(load_cache(&paths.data.join("missing.json")).unwrap().is_none());
    std::fs::write(paths.data.join("junk.json"), b"{nope").unwrap();
    assert!(load_cache(&paths.data.join("junk.json")).is_err());
    std::fs::remove_dir_all(paths.config.parent().unwrap()).ok();
}

#[test]
fn cache_file_names_are_safe() {
    let p = temp_paths("names");
    let a = Account { email: "../../etc/x@y".into(), ..account() };
    let name = p.cache_file(&a);
    assert_eq!(name.parent(), Some(p.data.as_path()));
    assert!(!name.file_name().unwrap().to_string_lossy().contains('/'));
}
