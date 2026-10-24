//! Microsoft account dialog: sign-in hand-off to the front end, save, cancel.

use crate::sample::sample_session;
use crate::sync::{SignInResult, SignedIn};
use crate::{Session, sync::Paths};
use rslook_account::{AccountKind, MemorySecrets, MsBackend, SecretStore};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn run(s: &mut Session, id: &str, p: Value) {
    s.run(id, &p).unwrap_or_else(|e| panic!("{id}: {e}"));
}

fn who() -> SignedIn {
    SignedIn { name: "Avery Test".into(), email: "avery@example.test".into(), refresh_token: "refresh-1".into() }
}

fn session() -> (Session, Arc<MemorySecrets>) {
    let mut s = sample_session();
    let secrets = Arc::new(MemorySecrets::default());
    s.set_secrets(secrets.clone());
    (s, secrets)
}

#[test]
fn sign_in_then_save_keeps_the_refresh_token_in_the_keychain() {
    let (mut s, secrets) = session();
    run(&mut s, "account.add", json!({}));
    assert!(s.run("account.sign_in", &json!({})).is_err(), "only for Microsoft accounts");
    run(&mut s, "account.set", json!({"kind": "microsoft", "client_id": "11111111-2222-3333-4444-555555555555"}));
    assert!(s.run("account.save", &json!({})).is_err(), "must sign in first");
    run(&mut s, "account.sign_in", json!({}));
    assert!(s.account_dialog.as_ref().unwrap().signing_in);
    assert!(s.run("account.sign_in", &json!({})).is_err(), "one sign-in at a time");
    let job = s.next_sign_in().expect("a sign-in for the front end");
    assert_eq!(job.settings.client_id, "11111111-2222-3333-4444-555555555555");
    assert_eq!(job.settings.backend, MsBackend::Graph);
    assert!(s.next_sign_in().is_none());

    // A result for an older sign-in is ignored.
    s.apply_sign_in(SignInResult { id: job.id + 7, outcome: Ok(who()) });
    assert!(s.account_dialog.as_ref().unwrap().signed_in.is_none());
    s.apply_sign_in(SignInResult { id: job.id, outcome: Ok(who()) });
    let d = s.account_dialog.clone().unwrap();
    assert!(!d.signing_in);
    assert_eq!(d.signed_in, Some(("Avery Test".into(), "avery@example.test".into())));
    assert_eq!(d.name, "Avery Test");

    run(&mut s, "account.save", json!({}));
    let a = s.account().unwrap().clone();
    assert_eq!((a.kind, a.email.as_str(), a.username.as_str()), (AccountKind::Microsoft, "avery@example.test", "avery@example.test"));
    assert_eq!(secrets.get("avery@example.test").unwrap(), "refresh-1");
    assert!(s.has_requests(), "saving asks for a first Send/Receive");
    let job = s.next_job().unwrap();
    assert_eq!(job.password, "refresh-1");
}

#[test]
fn a_failed_or_cancelled_sign_in_is_shown_and_cancel_stops_the_browser_wait() {
    let (mut s, _) = session();
    run(&mut s, "account.add", json!({}));
    run(&mut s, "account.set", json!({"kind": "microsoft", "client_id": "abc"}));
    run(&mut s, "account.sign_in", json!({}));
    let job = s.next_sign_in().unwrap();
    s.apply_sign_in(SignInResult { id: job.id, outcome: Err("access_denied: the user said no".into()) });
    let d = s.account_dialog.clone().unwrap();
    assert!(!d.signing_in && d.signed_in.is_none());
    assert!(d.error.unwrap().contains("the user said no"));

    run(&mut s, "account.sign_in", json!({}));
    let job = s.next_sign_in().unwrap();
    run(&mut s, "account.cancel", json!({}));
    assert!(job.cancel.load(Ordering::Relaxed), "the waiting sign-in is told to stop");
    assert!(s.account_dialog.is_none());
}

#[test]
fn sign_in_needs_a_valid_client_id_and_switching_backend_needs_a_new_sign_in() {
    let (mut s, _) = session();
    run(&mut s, "account.add", json!({}));
    run(&mut s, "account.set", json!({"kind": "microsoft", "client_id": ""}));
    assert!(s.run("account.sign_in", &json!({})).is_err());
    assert!(s.account_dialog.as_ref().unwrap().error.as_deref().unwrap().contains("client) ID"));
    run(&mut s, "account.set", json!({"client_id": "bad id/with slash"}));
    assert!(s.run("account.sign_in", &json!({})).is_err());
    run(&mut s, "account.set", json!({"client_id": "abc"}));
    run(&mut s, "account.sign_in", json!({}));
    let job = s.next_sign_in().unwrap();
    s.apply_sign_in(SignInResult { id: job.id, outcome: Ok(who()) });
    run(&mut s, "account.set", json!({"backend": "imap"}));
    let d = s.account_dialog.clone().unwrap();
    assert!(d.signed_in.is_none() && d.oauth_token.is_empty(), "IMAP needs its own permission");
    assert!(s.run("account.set", &json!({"backend": "ews"})).is_err());
}

#[test]
fn microsoft_backends_get_their_own_cache_files() {
    let paths = Paths { config: "/c".into(), data: "/d".into() };
    let mut a = rslook_account::Account { email: "a@x.org".into(), ..Default::default() };
    let imap = paths.cache_file(&a);
    a.kind = AccountKind::Microsoft;
    let graph = paths.cache_file(&a);
    a.microsoft.backend = MsBackend::Imap;
    let ms_imap = paths.cache_file(&a);
    assert_eq!(imap, std::path::PathBuf::from("/d/cache-a@x.org.json"), "existing IMAP caches keep their name");
    assert!(imap != graph && graph != ms_imap && imap != ms_imap);
}

#[test]
fn a_new_refresh_token_from_a_job_replaces_the_saved_one() {
    let (mut s, secrets) = session();
    run(&mut s, "account.add", json!({}));
    run(&mut s, "account.set", json!({"kind": "microsoft", "client_id": "abc"}));
    run(&mut s, "account.sign_in", json!({}));
    let job = s.next_sign_in().unwrap();
    s.apply_sign_in(SignInResult { id: job.id, outcome: Ok(who()) });
    run(&mut s, "account.save", json!({}));
    let job = s.next_job().unwrap();
    s.apply_result(crate::sync::JobResult { job: job.id, outcome: Ok(Default::default()), new_secret: Some("refresh-2".into()) });
    assert_eq!(secrets.get("avery@example.test").unwrap(), "refresh-2");
}
