//! End-to-end Send/Receive against real IMAP/SMTP servers with synthetic mail only.
//!
//! These tests are ignored by default. `scripts/mail-test.sh` starts throwaway GreenMail,
//! Dovecot and Mailpit containers with test users and a private TLS certificate, then runs
//! `cargo test -p rslook-sync --test mail_servers -- --ignored --test-threads=1`.
//!
//! Environment: `RSLOOK_GM_HOST` (GreenMail, default localhost), `RSLOOK_DC_HOST` (Dovecot,
//! default localhost), `RSLOOK_MP_HOST` (Mailpit, default localhost), `RSLOOK_TEST_CA` (PEM
//! file of the test CA; the server certificate names all three hosts). GreenMail test ports:
//! IMAP 3143, SMTP 3025, IMAPS 3993, SMTPS 3465; users flow, offline, tls, bob, oauth (password
//! `pw-<name>`, domain example.test; GreenMail takes the password as the XOAUTH2 token). Dovecot IMAP with STARTTLS on 31143 and Mailpit SMTP with
//! STARTTLS on 1025 (web API on 8025), user start / pw-start.

use rslook_engine::Session;
use rslook_engine::account::{Account, Login, MemorySecrets, SecretStore, Security, Server};
use rslook_engine::mail::{Address, FolderKind};
use rslook_engine::sync::{JobResult, SyncRequest, SyncState};
use rslook_imap::TlsOptions;
use rslook_sync::{NetOptions, run_job};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::Arc;

fn env_host(var: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| "localhost".into())
}

fn host() -> String {
    env_host("RSLOOK_GM_HOST")
}

fn ca() -> Vec<u8> {
    let path = std::env::var("RSLOOK_TEST_CA").expect("RSLOOK_TEST_CA must name the test CA certificate");
    std::fs::read(path).expect("read test CA")
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn plain_account(user: &str) -> Account {
    Account {
        name: format!("Test {user}"),
        email: format!("{user}@example.test"),
        username: user.into(),
        imap: Server { host: host(), port: 3143, security: Security::None },
        smtp: Server { host: host(), port: 3025, security: Security::None },
        ..Default::default()
    }
}

fn session_for(account: Account, password: &str) -> Session {
    let secrets = Arc::new(MemorySecrets::default());
    secrets.set(&account.key(), password).unwrap();
    let mut s = Session::empty(Address::default());
    s.set_secrets(secrets);
    s.open_account(account, None);
    s
}

/// One Send/Receive through the real worker code.
fn send_receive(s: &mut Session, r: SyncRequest, net: &NetOptions) -> JobResult {
    s.request(r);
    let job = s.next_job().expect("a job");
    let result = rt().block_on(run_job(&job, net));
    s.apply_result(result.clone());
    result
}

fn run(s: &mut Session, id: &str, p: Value) {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn send_mail(from: &str, to: &str, subject: &str) {
    let o = rslook_smtp::Outgoing {
        from: Address::new("Seeder", format!("{from}@example.test")),
        to: vec![Address::new("", format!("{to}@example.test"))],
        cc: vec![],
        subject: subject.into(),
        body: format!("Synthetic test mail: {subject}"),
        date: chrono::Utc::now(),
    };
    let server = Server { host: host(), port: 3025, security: Security::None };
    rt().block_on(rslook_smtp::send(&server, from, &format!("pw-{from}"), &[], &o)).unwrap();
}

/// What the server holds, read with a fresh IMAP connection: subject -> (read, flagged).
fn server_state(user: &str, mailbox: &str) -> Vec<(String, bool, bool)> {
    rt().block_on(async {
        let server = Server { host: host(), port: 3143, security: Security::None };
        let mut c = rslook_imap::connect(&server, user, &format!("pw-{user}"), &TlsOptions::default()).await.unwrap();
        let (_, exists) = c.select(mailbox).await.unwrap();
        let states = c.states(exists).await.unwrap();
        let uids: Vec<u32> = states.iter().map(|s| s.uid).collect();
        let fetched = c.fetch(&uids, true).await.unwrap();
        c.logout().await;
        let mut out: Vec<(String, bool, bool)> = fetched
            .iter()
            .map(|f| {
                let m = rslook_engine::mail::parse::parse_rfc5322(&f.bytes).unwrap();
                (m.subject, f.read, f.flagged)
            })
            .collect();
        out.sort();
        out
    })
}

fn mailboxes(user: &str) -> Vec<String> {
    rt().block_on(async {
        let server = Server { host: host(), port: 3143, security: Security::None };
        let mut c = rslook_imap::connect(&server, user, &format!("pw-{user}"), &TlsOptions::default()).await.unwrap();
        let names = c.list().await.unwrap().into_iter().map(|f| f.name).collect();
        c.logout().await;
        names
    })
}

fn id_of(s: &Session, subject: &str) -> u64 {
    s.store.messages().iter().find(|m| m.subject == subject).map(|m| m.id).unwrap_or_else(|| panic!("no local message {subject}"))
}

fn count(s: &Session, kind: FolderKind) -> usize {
    s.store.total_count(s.store.folder_of_kind(kind).unwrap())
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn receive_change_send_and_delete_round_trip() {
    let net = NetOptions::default();
    send_mail("bob", "flow", "First test message");
    send_mail("bob", "flow", "Second test message");
    let mut s = session_for(plain_account("flow"), "pw-flow");

    // Receive.
    let r = send_receive(&mut s, SyncRequest::All, &net);
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert_eq!(s.sync_state, SyncState::Idle);
    assert_eq!(count(&s, FolderKind::Inbox), 2);
    let first = id_of(&s, "First test message");
    assert!(!s.store.message(first).unwrap().read, "fetching must not mark mail read");

    // Change things locally: read + flag one, delete the other, send a reply.
    run(&mut s, "message.select", json!({"id": first}));
    run(&mut s, "mail.toggle_flag", json!({}));
    let second = id_of(&s, "Second test message");
    run(&mut s, "message.select", json!({"id": second}));
    run(&mut s, "mail.delete", json!({}));
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "Bob <bob@example.test>", "subject": "Reply from Rslook", "body": "Hello Bob"}));
    run(&mut s, "compose.send", json!({}));
    assert_eq!(count(&s, FolderKind::Outbox), 1);
    // Read+flag on the first; read (by selecting it) and move on the second.
    assert_eq!(s.queue().len(), 3, "{:?}", s.queue());

    let r = send_receive(&mut s, SyncRequest::All, &net);
    let report = r.outcome.expect("job ran");
    assert!(report.failed_ops.is_empty() && report.send_failed.is_empty() && report.error.is_none(), "{report:?}");
    assert!(s.queue().is_empty());
    assert_eq!(count(&s, FolderKind::Outbox), 0);

    // The server agrees.
    assert_eq!(server_state("flow", "INBOX"), vec![("First test message".to_string(), true, true)]);
    let boxes = mailboxes("flow");
    assert!(boxes.contains(&"Deleted Items".to_string()) && boxes.contains(&"Sent Items".to_string()), "{boxes:?}");
    assert_eq!(server_state("flow", "Deleted Items"), vec![("Second test message".to_string(), true, false)]);
    assert_eq!(server_state("flow", "Sent Items"), vec![("Reply from Rslook".to_string(), true, false)]);
    assert!(server_state("bob", "INBOX").iter().any(|(s, _, _)| s == "Reply from Rslook"));
    // Locally, the moved and sent copies are the server's, not duplicates.
    assert_eq!(count(&s, FolderKind::Deleted), 1);
    assert_eq!(count(&s, FolderKind::Sent), 1);

    // Delete for good from Deleted Items.
    let second = id_of(&s, "Second test message");
    run(&mut s, "folder.select", json!({"kind": "deleted"}));
    run(&mut s, "message.select", json!({"id": second}));
    run(&mut s, "mail.delete", json!({}));
    let r = send_receive(&mut s, SyncRequest::All, &net);
    assert!(r.outcome.is_ok());
    assert!(server_state("flow", "Deleted Items").is_empty());
    assert_eq!(count(&s, FolderKind::Deleted), 0);

    // Mail arriving later shows up with Update Folder on the Inbox.
    send_mail("bob", "flow", "Third test message");
    run(&mut s, "folder.select", json!({"kind": "inbox"}));
    let inbox = s.folder;
    send_receive(&mut s, SyncRequest::Folder(inbox), &net);
    assert_eq!(count(&s, FolderKind::Inbox), 2);
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn offline_changes_survive_a_restart_and_replay() {
    let net = NetOptions::default();
    send_mail("bob", "offline", "Offline test message");
    let dir = std::env::temp_dir().join(format!("rslook-gm-offline-{}", std::process::id()));
    let paths = rslook_engine::sync::Paths { config: dir.join("config"), data: dir.join("data") };
    let account = plain_account("offline");
    let mut s = session_for(account.clone(), "pw-offline");
    s.set_paths(paths.clone());
    send_receive(&mut s, SyncRequest::All, &net);

    run(&mut s, "sendrecv.work_offline", json!({}));
    let id = id_of(&s, "Offline test message");
    run(&mut s, "message.select", json!({"id": id}));
    run(&mut s, "mail.toggle_flag", json!({}));
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "bob@example.test", "subject": "Written offline"}));
    run(&mut s, "compose.send", json!({}));
    s.save_cache().unwrap();
    drop(s);

    // "Restart": a new session from the cache, still offline, then back online.
    let cached = rslook_engine::sync::load_cache(&paths.cache_file(&account)).unwrap();
    let mut s = session_for(account, "pw-offline");
    s.open_account(plain_account("offline"), cached);
    assert_eq!(s.queue().len(), 1);
    assert_eq!(count(&s, FolderKind::Outbox), 1);
    assert_eq!(server_state("offline", "INBOX"), vec![("Offline test message".to_string(), false, false)]);
    let r = send_receive(&mut s, SyncRequest::All, &net);
    assert!(r.outcome.as_ref().is_ok_and(|r| r.error.is_none()), "{:?}", r.outcome);
    assert_eq!(server_state("offline", "INBOX"), vec![("Offline test message".to_string(), true, true)]);
    assert!(server_state("bob", "INBOX").iter().any(|(s, _, _)| s == "Written offline"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn a_wrong_password_is_refused_and_nothing_changes() {
    let mut s = session_for(plain_account("flow"), "not-the-password");
    let r = send_receive(&mut s, SyncRequest::All, &NetOptions::default());
    let e = r.outcome.expect_err("login must fail");
    assert!(e.contains("refused"), "{e}");
    assert!(matches!(s.sync_state, SyncState::Error(_)));
}

fn tls_account(user: &str) -> Account {
    Account {
        name: "TLS".into(),
        email: format!("{user}@example.test"),
        username: user.into(),
        imap: Server { host: host(), port: 3993, security: Security::Tls },
        smtp: Server { host: host(), port: 3465, security: Security::Tls },
        ..Default::default()
    }
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn implicit_tls_works_with_a_trusted_certificate() {
    send_mail("bob", "tls", "Over TLS");
    let net = NetOptions { extra_roots_pem: ca() };
    let mut s = session_for(tls_account("tls"), "pw-tls");
    let r = send_receive(&mut s, SyncRequest::All, &net);
    assert!(r.outcome.as_ref().is_ok_and(|r| r.error.is_none()), "{:?}", r.outcome);
    assert_eq!(count(&s, FolderKind::Inbox), 1);
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "bob@example.test", "subject": "Sent over TLS"}));
    run(&mut s, "compose.send", json!({}));
    let r = send_receive(&mut s, SyncRequest::SendAll, &net);
    assert!(r.outcome.as_ref().is_ok_and(|r| r.send_failed.is_empty()), "{:?}", r.outcome);
    assert!(server_state("bob", "INBOX").iter().any(|(s, _, _)| s == "Sent over TLS"));
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn an_untrusted_certificate_is_refused() {
    // Same server, but the test CA is not trusted: the connection must not go ahead.
    let mut s = session_for(tls_account("tls"), "pw-tls");
    let r = send_receive(&mut s, SyncRequest::All, &NetOptions::default());
    let e = r.outcome.expect_err("TLS must fail");
    assert!(e.contains("secure connection failed"), "{e}");

    // SMTP refuses it too.
    let o = rslook_smtp::Outgoing {
        from: Address::new("", "tls@example.test"),
        to: vec![Address::new("", "bob@example.test")],
        cc: vec![],
        subject: "must not arrive".into(),
        body: String::new(),
        date: chrono::Utc::now(),
    };
    let err = rt().block_on(rslook_smtp::send(&tls_account("tls").smtp, "tls", "pw-tls", &[], &o)).expect_err("untrusted SMTP TLS");
    assert!(err.to_string().to_ascii_lowercase().contains("cert") || err.to_string().contains("secure"), "{err}");
}

/// Subjects Mailpit has received, from its web API.
fn mailpit_subjects() -> String {
    let mut s = std::net::TcpStream::connect((env_host("RSLOOK_MP_HOST"), 8025)).unwrap();
    write!(s, "GET /api/v1/messages HTTP/1.0\r\nHost: mailpit\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn starttls_account() -> Account {
    Account {
        name: "Start TLS".into(),
        email: "start@example.test".into(),
        username: "start".into(),
        imap: Server { host: env_host("RSLOOK_DC_HOST"), port: 31143, security: Security::StartTls },
        smtp: Server { host: env_host("RSLOOK_MP_HOST"), port: 1025, security: Security::StartTls },
        ..Default::default()
    }
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn starttls_imap_and_smtp_work_with_a_trusted_certificate() {
    let net = NetOptions { extra_roots_pem: ca() };
    let mut s = session_for(starttls_account(), "pw-start");
    let r = send_receive(&mut s, SyncRequest::All, &net);
    assert!(r.outcome.as_ref().is_ok_and(|r| r.error.is_none() && r.folders.is_some()), "{:?}", r.outcome);
    run(&mut s, "mail.new", json!({}));
    run(&mut s, "compose.set", json!({"to": "someone@example.test", "subject": "Sent with STARTTLS"}));
    run(&mut s, "compose.send", json!({}));
    let r = send_receive(&mut s, SyncRequest::All, &net);
    let report = r.outcome.expect("job ran");
    assert!(report.send_failed.is_empty() && report.error.is_none(), "{report:?}");
    assert!(mailpit_subjects().contains("Sent with STARTTLS"));
    // The copy went to Dovecot over the STARTTLS IMAP connection and came back on the pull.
    assert_eq!(count(&s, FolderKind::Sent), 1);
    assert!(s.store.messages_in(s.store.folder_of_kind(FolderKind::Sent).unwrap(), "")[0].remote.as_ref().is_some_and(|r| r.uid != 0));
}

#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn starttls_with_an_untrusted_certificate_is_refused() {
    let mut s = session_for(starttls_account(), "pw-start");
    let r = send_receive(&mut s, SyncRequest::All, &NetOptions::default());
    let e = r.outcome.expect_err("STARTTLS must fail");
    assert!(e.contains("secure connection failed"), "{e}");
    let o = rslook_smtp::Outgoing {
        from: Address::new("", "start@example.test"),
        to: vec![Address::new("", "someone@example.test")],
        cc: vec![],
        subject: "must not arrive".into(),
        body: String::new(),
        date: chrono::Utc::now(),
    };
    assert!(rt().block_on(rslook_smtp::send(&starttls_account().smtp, "start", "pw-start", &[], &o)).is_err());
    assert!(!mailpit_subjects().contains("must not arrive"));
}

/// Microsoft accounts on the IMAP backend sign in with SASL XOAUTH2 on both IMAP and SMTP.
#[test]
#[ignore = "needs the test mail servers (scripts/mail-test.sh)"]
fn xoauth2_sign_in_works_for_imap_and_smtp_and_a_bad_token_is_refused() {
    let imap = Server { host: host(), port: 3143, security: Security::None };
    let smtp = Server { host: host(), port: 3025, security: Security::None };
    let user = "oauth";
    let o = rslook_smtp::Outgoing {
        from: Address::new("OAuth", "oauth@example.test"),
        to: vec![Address::new("", "oauth@example.test")],
        cc: vec![],
        subject: "sent with XOAUTH2".into(),
        body: "Synthetic test mail.".into(),
        date: chrono::Utc::now(),
    };
    rt().block_on(async {
        let good = Login::Bearer("pw-oauth".into());
        rslook_smtp::send_with(&smtp, user, &good, &[], &o).await.expect("SMTP AUTH XOAUTH2");
        let mut c = rslook_imap::connect_with(&imap, user, &good, &TlsOptions::default()).await.expect("IMAP AUTHENTICATE XOAUTH2");
        let (_, exists) = c.select("INBOX").await.unwrap();
        assert!(exists >= 1, "the XOAUTH2-sent message arrived");
        c.logout().await;
        let bad = Login::Bearer("expired-token".into());
        assert!(rslook_imap::connect_with(&imap, user, &bad, &TlsOptions::default()).await.is_err());
        assert!(rslook_smtp::send_with(&smtp, user, &bad, &[], &o).await.is_err());
    });
}
