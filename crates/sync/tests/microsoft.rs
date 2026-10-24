//! Microsoft accounts end to end through the real worker code, against in-process fake
//! Microsoft sign-in and Graph servers (no network; runs in plain `cargo test` and CI).

use rslook_engine::Session;
use rslook_engine::account::{Account, AccountKind, MemorySecrets, MicrosoftSettings, MsBackend, SecretStore};
use rslook_engine::mail::{Address, FolderKind};
use rslook_engine::sync::{JobResult, SignInJob, SyncRequest, SyncState};
use rslook_graph::WellKnown;
use rslook_graph::mock::{DEFAULT_TOKEN, MOCK_MAIL, MOCK_NAME, MockGraph};
use rslook_oauth::mock::{CLIENT_ID, MockAuth};
use rslook_sync::{NetOptions, run_job, sign_in};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

struct World {
    auth: MockAuth,
    graph: MockGraph,
}

impl World {
    fn new() -> World {
        let auth = MockAuth::start().unwrap();
        let graph = MockGraph::start().unwrap();
        // The access tokens the fake sign-in server hands out are the ones the fake Graph accepts.
        auth.set_access_token(DEFAULT_TOKEN);
        World { auth, graph }
    }

    fn settings(&self) -> MicrosoftSettings {
        MicrosoftSettings {
            backend: MsBackend::Graph,
            client_id: CLIENT_ID.into(),
            auth_base: self.auth.base.clone(),
            graph_base: self.graph.base.clone(),
            ..Default::default()
        }
    }

    fn session(&self) -> (Session, Arc<MemorySecrets>) {
        let account = Account {
            name: MOCK_NAME.into(),
            email: MOCK_MAIL.into(),
            username: MOCK_MAIL.into(),
            kind: AccountKind::Microsoft,
            microsoft: self.settings(),
            ..Default::default()
        };
        let secrets = Arc::new(MemorySecrets::default());
        secrets.set(&account.key(), &self.auth.issue_refresh_token()).unwrap();
        let mut s = Session::empty(Address::default());
        s.set_secrets(secrets.clone());
        s.open_account(account, None);
        (s, secrets)
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn send_receive(s: &mut Session) -> JobResult {
    s.request(SyncRequest::All);
    let job = s.next_job().expect("a job");
    let result = rt().block_on(run_job(&job, &NetOptions::default()));
    s.apply_result(result.clone());
    result
}

fn run(s: &mut Session, id: &str, p: Value) {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn mime(subject: &str) -> Vec<u8> {
    format!("From: Bob <bob@example.test>\r\nTo: {MOCK_MAIL}\r\nSubject: {subject}\r\nDate: Tue, 20 Oct 2026 09:30:00 +0000\r\n\r\nSynthetic {subject}\r\n")
        .into_bytes()
}

fn id_of(s: &Session, subject: &str) -> u64 {
    s.store.messages().iter().find(|m| m.subject == subject).map(|m| m.id).unwrap_or_else(|| panic!("no local message {subject}"))
}

fn select(s: &mut Session, subject: &str) {
    let id = id_of(s, subject);
    run(s, "message.select", json!({ "id": id }));
}

fn count(s: &Session, kind: FolderKind) -> usize {
    s.store.folder_of_kind(kind).map(|f| s.store.total_count(f)).unwrap_or(0)
}

fn subjects(w: &World, folder: &str) -> Vec<(String, bool, bool)> {
    let mut v: Vec<(String, bool, bool)> = w
        .graph
        .messages(folder)
        .into_iter()
        .map(|(_, read, flagged, bytes)| (rslook_engine::mail::parse::parse_rfc5322(&bytes).unwrap().subject, read, flagged))
        .collect();
    v.sort();
    v
}

#[test]
fn graph_account_receives_changes_sends_and_deletes() {
    let w = World::new();
    let inbox = w.graph.folder_id(WellKnown::Inbox);
    let projects = w.graph.add_folder(None, "Projects");
    let y2026 = w.graph.add_folder(Some(&projects), "2026");
    w.graph.add_message(&inbox, &mime("Hello"), false, false);
    w.graph.add_message(&inbox, &mime("Report"), true, true);
    w.graph.add_message(&y2026, &mime("Plan"), false, false);
    w.graph.set_page_size(1); // every list goes through paging
    let (mut s, secrets) = w.session();
    let first_token = secrets.get(MOCK_MAIL).unwrap();

    // Receive.
    let r = send_receive(&mut s);
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert_eq!(s.sync_state, SyncState::Idle);
    assert_eq!(count(&s, FolderKind::Inbox), 2);
    let report = s.store.message(id_of(&s, "Report")).unwrap().clone();
    assert!(report.read && report.flagged);
    let plan_folder = s.store.message(id_of(&s, "Plan")).unwrap().folder;
    let plan_folder = s.store.folder(plan_folder).unwrap().clone();
    assert_eq!(plan_folder.name, "2026");
    assert_eq!(s.store.folder(plan_folder.parent.unwrap()).unwrap().name, "Projects", "the folder tree comes across");
    assert!(
        s.store.folder_of_kind(FolderKind::Outbox).is_some_and(|f| s.store.folder(f).unwrap().remote.is_none()),
        "the server Outbox is not mirrored"
    );
    assert_ne!(secrets.get(MOCK_MAIL).unwrap(), first_token, "the new refresh token is kept");
    assert_eq!(w.graph.requests_missing_immutable_id(), 0);

    // Change things locally: read + flag Hello, archive Report, send a message.
    select(&mut s, "Hello");
    run(&mut s, "mail.mark_read", json!({}));
    run(&mut s, "mail.toggle_flag", json!({}));
    select(&mut s, "Report");
    run(&mut s, "mail.archive", json!({}));
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "Bob <bob@example.test>", "subject": "Sent through Graph", "body": "Hello Bob"}));
    run(&mut s, "compose.send", json!({}));

    let r = send_receive(&mut s);
    let report = r.outcome.expect("job ran");
    assert!(report.failed_ops.is_empty() && report.send_failed.is_empty() && report.error.is_none(), "{report:?}");
    assert!(s.queue().is_empty());
    assert_eq!(count(&s, FolderKind::Outbox), 0);
    assert_eq!(subjects(&w, &inbox), vec![("Hello".to_string(), true, true)]);
    assert_eq!(subjects(&w, &w.graph.folder_id(WellKnown::Archive)), vec![("Report".to_string(), true, true)]);
    assert_eq!(w.graph.sent().len(), 1);
    assert!(String::from_utf8_lossy(&w.graph.sent()[0]).contains("Sent through Graph"));
    // Locally the archived and sent copies are the server's, not duplicates.
    assert_eq!(count(&s, FolderKind::Archive), 1);
    assert_eq!(count(&s, FolderKind::Sent), 1);

    // Delete Hello: to Deleted Items, then for good.
    run(&mut s, "folder.select", json!({"kind": "inbox"}));
    select(&mut s, "Hello");
    run(&mut s, "mail.delete", json!({}));
    assert!(send_receive(&mut s).outcome.is_ok());
    let deleted = w.graph.folder_id(WellKnown::DeletedItems);
    assert_eq!(subjects(&w, &deleted).len(), 1);
    run(&mut s, "folder.select", json!({"kind": "deleted"}));
    select(&mut s, "Hello");
    run(&mut s, "mail.delete", json!({}));
    assert!(send_receive(&mut s).outcome.is_ok());
    assert!(subjects(&w, &deleted).is_empty());
    assert_eq!(count(&s, FolderKind::Deleted), 0);

    // Mail that arrives later and changes made elsewhere come in on the next Send/Receive.
    let later = w.graph.add_message(&inbox, &mime("Later"), false, false);
    w.graph.set_message_state(&later, true, false);
    assert!(send_receive(&mut s).outcome.is_ok());
    assert!(s.store.message(id_of(&s, "Later")).unwrap().read);
    assert_eq!(count(&s, FolderKind::Inbox), 1);
}

#[test]
fn a_withdrawn_sign_in_asks_to_sign_in_again_and_changes_nothing() {
    let w = World::new();
    w.graph.add_message(&w.graph.folder_id(WellKnown::Inbox), &mime("Hello"), false, false);
    let (mut s, _) = w.session();
    w.auth.revoke_all();
    let r = send_receive(&mut s);
    let e = r.outcome.expect_err("refused");
    assert!(e.contains("sign in again"), "{e}");
    assert!(matches!(&s.sync_state, SyncState::Error(m) if m.contains("sign in again")));
    assert_eq!(count(&s, FolderKind::Inbox), 0);
}

#[test]
fn graph_refusing_the_token_is_reported_without_touching_the_cache() {
    let w = World::new();
    w.graph.add_message(&w.graph.folder_id(WellKnown::Inbox), &mime("Hello"), false, false);
    let (mut s, _) = w.session();
    assert!(send_receive(&mut s).outcome.is_ok());
    w.graph.set_token("a-different-token");
    let e = send_receive(&mut s).outcome.expect_err("refused");
    assert!(e.contains("sign in again"), "{e}");
    assert_eq!(count(&s, FolderKind::Inbox), 1, "the cached mail stays");
}

#[test]
fn throttling_is_waited_out() {
    let w = World::new();
    w.graph.add_message(&w.graph.folder_id(WellKnown::Inbox), &mime("Hello"), false, false);
    let (mut s, _) = w.session();
    w.graph.throttle_next(2);
    assert!(send_receive(&mut s).outcome.is_ok());
    assert_eq!(count(&s, FolderKind::Inbox), 1);
}

#[test]
fn sign_in_through_the_browser_finds_the_mailbox_address() {
    let w = World::new();
    w.auth.set_identity("Someone Else", "not-the-mailbox@example.test");
    let job = SignInJob { id: 3, settings: w.settings(), cancel: Arc::new(AtomicBool::new(false)) };
    let browser = w.auth.browser();
    let r = sign_in(&job, &browser);
    assert_eq!(r.id, 3);
    let who = r.outcome.expect("signed in");
    // Graph's own answer wins over the sign-in token's claims.
    assert_eq!((who.name.as_str(), who.email.as_str()), (MOCK_NAME, MOCK_MAIL));
    assert!(!who.refresh_token.is_empty());
    assert!(rslook_oauth::refresh(&w.auth.provider(rslook_oauth::GRAPH_SCOPES), &who.refresh_token).is_ok());

    // On the IMAP backend there is no Graph call: the address comes from the sign-in.
    let job = SignInJob { id: 4, settings: MicrosoftSettings { backend: MsBackend::Imap, ..w.settings() }, cancel: Arc::new(AtomicBool::new(false)) };
    let who = sign_in(&job, &browser).outcome.expect("signed in");
    assert_eq!(who.email, "not-the-mailbox@example.test");

    // The user saying no comes back as an error.
    w.auth.deny_next("access_denied", "The user declined.");
    let job = SignInJob { id: 5, settings: w.settings(), cancel: Arc::new(AtomicBool::new(false)) };
    assert!(sign_in(&job, &browser).outcome.is_err());
}
