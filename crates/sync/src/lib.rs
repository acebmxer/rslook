#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! The Send/Receive worker. [`run_job`] carries out one engine [`Job`]: send the Outbox, save
//! copies to Sent, replay queued changes, then pull folders. IMAP accounts use SMTP + IMAP with a
//! password; Microsoft accounts first trade the saved refresh token for an access token, then use
//! Microsoft Graph ([`graph`]) or IMAP + SMTP with XOAUTH2. [`Worker`] runs jobs, and Microsoft
//! sign-ins ([`sign_in`]), on background threads so the window never waits on the network.

mod graph;

use futures::FutureExt;
use rslook_engine::account::{AccountKind, Login, MicrosoftSettings, MsBackend};
use rslook_engine::mail::{NewMessage, parse};
use rslook_engine::sync::{
    Job, JobResult, MailboxContents, OpKind, Pull, QueuedOp, RemoteFolder, RemoteMessage, Report, SentItem, SignInJob, SignInResult, SignedIn,
};
use rslook_imap::{Conn, ImapError, TlsOptions};
use rslook_oauth::{OAuthError, Provider};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// Network settings that are not part of the account (tests add a private CA here).
#[derive(Debug, Clone, Default)]
pub struct NetOptions {
    pub extra_roots_pem: Vec<u8>,
}

pub(crate) fn outgoing(o: &rslook_engine::sync::OutgoingMessage) -> rslook_smtp::Outgoing {
    rslook_smtp::Outgoing { from: o.from.clone(), to: o.to.clone(), cc: o.cc.clone(), subject: o.subject.clone(), body: o.body.clone(), date: o.date }
}

/// Replay order: flags first (they address the message where it is), then moves, then deletes.
pub(crate) fn phase(op: &QueuedOp) -> u8 {
    match op.kind {
        OpKind::Flags { .. } => 0,
        OpKind::Move { .. } => 1,
        OpKind::Delete => 2,
    }
}

/// How long an interactive sign-in waits for the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The sign-in server and permissions for a Microsoft account's backend.
pub fn provider(ms: &MicrosoftSettings) -> Provider {
    let scopes = match ms.backend {
        MsBackend::Graph => rslook_oauth::GRAPH_SCOPES,
        MsBackend::Imap => rslook_oauth::OUTLOOK_IMAP_SCOPES,
    };
    let base = ms.auth_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Provider::microsoft(ms.client_id(), ms.tenant(), scopes);
    }
    Provider {
        authorize_url: format!("{base}/authorize"),
        token_url: format!("{base}/token"),
        client_id: ms.client_id().to_string(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    }
}

/// What the user sees when Microsoft refuses the saved sign-in.
const SIGN_IN_AGAIN: &str = "Your Microsoft sign-in has expired or was withdrawn. Open Account Settings (File tab) and sign in again.";

fn oauth_message(e: &OAuthError) -> String {
    match e {
        OAuthError::Denied(_) => SIGN_IN_AGAIN.to_string(),
        e => e.to_string(),
    }
}

/// Runs an interactive Microsoft sign-in: opens the browser on Microsoft's page and waits for it
/// to come back. Blocking; run it on its own thread.
pub fn sign_in(job: &SignInJob, open_browser: &dyn Fn(&str) -> Result<(), String>) -> SignInResult {
    let outcome = (|| -> Result<SignedIn, String> {
        let tokens = rslook_oauth::sign_in(&provider(&job.settings), open_browser, SIGN_IN_TIMEOUT, &job.cancel).map_err(|e| e.to_string())?;
        let refresh_token =
            tokens.refresh_token.clone().ok_or("Microsoft did not allow Rslook to stay signed in (no refresh token was returned).")?;
        let (mut name, mut email) = tokens.id_token.as_deref().and_then(rslook_oauth::id_token_identity).unwrap_or_default();
        if job.settings.backend == MsBackend::Graph {
            let me = graph::client(&job.settings, &tokens.access_token).me().map_err(|e| e.to_string())?;
            if !me.email.trim().is_empty() {
                email = me.email;
            }
            if !me.display_name.trim().is_empty() {
                name = me.display_name;
            }
        }
        if !email.contains('@') {
            return Err("Microsoft did not say which email address you signed in with.".into());
        }
        Ok(SignedIn { name, email, refresh_token })
    })();
    SignInResult { id: job.id, outcome }
}

/// Runs one job. `Err` only when nothing at all happened (e.g. the server could not be
/// reached before anything was sent); partial progress comes back as a report with `error`.
pub async fn run_job(job: &Job, net: &NetOptions) -> JobResult {
    let a = &job.account;
    if a.kind == AccountKind::Imap {
        let outcome = run(job, net, &a.username, &Login::Password(job.password.clone())).await;
        return JobResult { job: job.id, outcome, new_secret: None };
    }
    let tokens = match rslook_oauth::refresh(&provider(&a.microsoft), &job.password) {
        Ok(t) => t,
        Err(e) => return JobResult { job: job.id, outcome: Err(oauth_message(&e)), new_secret: None },
    };
    // Microsoft may hand out a new refresh token; the old one keeps working for a while, but the
    // new one is the one to keep.
    let new_secret = tokens.refresh_token.clone().filter(|t| !t.is_empty() && *t != job.password);
    let outcome = match a.microsoft.backend {
        MsBackend::Graph => graph::run(job, &tokens.access_token),
        MsBackend::Imap => run(job, net, &a.email, &Login::Bearer(tokens.access_token.clone())).await,
    };
    JobResult { job: job.id, outcome, new_secret }
}

async fn run(job: &Job, net: &NetOptions, user: &str, login: &Login) -> Result<Report, String> {
    let a = &job.account;
    let mut report = Report::default();

    // 1. Send the Outbox.
    let mut sent_bytes = Vec::new();
    for o in &job.outbox {
        match rslook_smtp::send_with(&a.smtp, user, login, &net.extra_roots_pem, &outgoing(o)).await {
            Ok(bytes) => sent_bytes.push((o.id, bytes)),
            Err(e) => report.send_failed.push((o.id, e.to_string())),
        }
    }
    let need_imap = !job.ops.is_empty() || job.pull != Pull::None || (!sent_bytes.is_empty() && job.sent_mailbox.is_some());
    if !need_imap {
        report.sent = sent_bytes.into_iter().map(|(id, _)| SentItem { id, appended_to: None }).collect();
        return Ok(report);
    }

    let tls = TlsOptions { extra_roots_pem: net.extra_roots_pem.clone() };
    let mut conn = match rslook_imap::connect_with(&a.imap, user, login, &tls).await {
        Ok(c) => c,
        Err(e) => {
            if sent_bytes.is_empty() && report.send_failed.is_empty() {
                return Err(e.to_string());
            }
            report.sent = sent_bytes.into_iter().map(|(id, _)| SentItem { id, appended_to: None }).collect();
            report.error = Some(e.to_string());
            return Ok(report);
        }
    };
    let result = imap_part(&mut conn, job, sent_bytes, &mut report).await;
    conn.logout().await;
    if let Err(e) = result {
        report.error = Some(e.to_string());
    }
    Ok(report)
}

async fn imap_part(conn: &mut Conn, job: &Job, sent_bytes: Vec<(u64, Vec<u8>)>, report: &mut Report) -> Result<(), ImapError> {
    // 2. Copies of sent mail.
    let mut sent_ok = None;
    if let Some((mailbox, create)) = &job.sent_mailbox
        && !sent_bytes.is_empty()
    {
        sent_ok = Some(mailbox.clone());
        if *create && let Err(e) = conn.ensure_mailbox(mailbox).await {
            if e.is_fatal() {
                report.sent = sent_bytes.iter().map(|(id, _)| SentItem { id: *id, appended_to: None }).collect();
                return Err(e);
            }
            log::warn!("could not create {mailbox}: {e}");
            sent_ok = None;
        }
    }
    let mut fatal = None;
    for (id, bytes) in &sent_bytes {
        let appended_to = match (&sent_ok, &fatal) {
            (Some(mailbox), None) => match conn.append(mailbox, "(\\Seen)", bytes).await {
                Ok(()) => Some(mailbox.clone()),
                Err(e) => {
                    log::warn!("could not save a copy to {mailbox}: {e}");
                    if e.is_fatal() {
                        fatal = Some(e);
                    }
                    None
                }
            },
            _ => None,
        };
        report.sent.push(SentItem { id: *id, appended_to });
    }
    if let Some(e) = fatal {
        return Err(e);
    }

    // 3. Replay offline changes.
    let mut ops: Vec<&QueuedOp> = job.ops.iter().collect();
    ops.sort_by(|x, y| (phase(x), &x.target.mailbox).cmp(&(phase(y), &y.target.mailbox)));
    let mut selected: Option<(String, u32)> = None;
    let mut created: HashSet<String> = HashSet::new();
    for op in ops {
        let r = replay(conn, op, &mut selected, &mut created).await;
        match r {
            Ok(()) => report.done_ops.push(op.id),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => report.failed_ops.push((op.id, e.to_string())),
        }
    }

    // 4. Pull.
    let mailboxes: Vec<String> = match &job.pull {
        Pull::None => Vec::new(),
        Pull::Mailbox(m) => vec![m.clone()],
        Pull::All => {
            let listed = conn.list().await?;
            let folders: Vec<RemoteFolder> = listed
                .into_iter()
                .map(|f| RemoteFolder {
                    name: f.name,
                    parent: None,
                    display: f.display,
                    delimiter: f.delimiter,
                    kind: f.kind,
                    special_use: f.special_use,
                    selectable: f.selectable,
                })
                .collect();
            let names = folders.iter().filter(|f| f.selectable).map(|f| f.name.clone()).collect();
            report.folders = Some(folders);
            names
        }
    };
    for mailbox in mailboxes {
        match pull_mailbox(conn, job, &mailbox).await {
            Ok(c) => report.contents.push(c),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => log::warn!("could not read {mailbox}: {e}"),
        }
    }
    Ok(())
}

async fn replay(conn: &mut Conn, op: &QueuedOp, selected: &mut Option<(String, u32)>, created: &mut HashSet<String>) -> Result<(), ImapError> {
    let t = &op.target;
    let uid_validity = match selected {
        Some((m, uv)) if *m == t.mailbox => *uv,
        _ => {
            let (uv, _) = conn.select(&t.mailbox).await?;
            *selected = Some((t.mailbox.clone(), uv));
            uv
        }
    };
    if uid_validity != t.uid_validity {
        return Err(ImapError::Protocol(format!("the folder {} changed on the server", t.mailbox)));
    }
    match &op.kind {
        OpKind::Flags { read, flagged } => conn.set_flags(t.uid, *read, *flagged).await,
        OpKind::Move { to, create } => {
            if *create && !created.contains(to) {
                conn.ensure_mailbox(to).await?;
                created.insert(to.clone());
            }
            conn.move_to(t.uid, to).await
        }
        OpKind::Delete => conn.delete(t.uid).await,
    }
}

pub(crate) fn unreadable(header_only: bool) -> NewMessage {
    NewMessage { subject: "(message could not be read)".into(), body: if header_only { too_big() } else { String::new() }, ..Default::default() }
}

pub(crate) fn too_big() -> String {
    format!("This message is larger than {} MB, so only its header was downloaded.", parse::MAX_MESSAGE_BYTES / (1024 * 1024))
}

async fn pull_mailbox(conn: &mut Conn, job: &Job, mailbox: &str) -> Result<MailboxContents, ImapError> {
    let (uid_validity, exists) = conn.select(mailbox).await?;
    let states = conn.states(exists).await?;
    let known: HashSet<u32> = job.known.get(mailbox).filter(|k| k.uid_validity == uid_validity).map(|k| k.uids.clone()).unwrap_or_default();
    let max = u32::try_from(parse::MAX_MESSAGE_BYTES).unwrap_or(u32::MAX);
    let (big, small): (Vec<&rslook_imap::MessageState>, Vec<&rslook_imap::MessageState>) =
        states.iter().filter(|s| !known.contains(&s.uid)).partition(|s| s.size > max);
    let mut content: HashMap<u32, NewMessage> = HashMap::new();
    for (uids, header_only) in [(small.iter().map(|s| s.uid).collect::<Vec<_>>(), false), (big.iter().map(|s| s.uid).collect(), true)] {
        if uids.is_empty() {
            continue;
        }
        for f in conn.fetch(&uids, header_only).await? {
            let mut m = parse::parse_rfc5322(&f.bytes).unwrap_or_else(|_| unreadable(header_only));
            if header_only {
                m.body = too_big();
            }
            content.insert(f.uid, m);
        }
    }
    let messages =
        states.iter().map(|s| RemoteMessage { uid: s.uid, key: None, read: s.read, flagged: s.flagged, content: content.remove(&s.uid) }).collect();
    Ok(MailboxContents { mailbox: mailbox.to_string(), uid_validity, messages })
}

/// Runs jobs on a background thread with its own async runtime, and sign-ins on threads of
/// their own (they wait minutes for the browser).
pub struct Worker {
    jobs: mpsc::Sender<Job>,
    results: mpsc::Receiver<JobResult>,
    sign_in_tx: mpsc::Sender<SignInResult>,
    sign_ins: mpsc::Receiver<SignInResult>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Worker {
    /// Starts the worker. `wake` is called after each job or sign-in (e.g. to repaint the window).
    pub fn spawn(net: NetOptions, wake: Box<dyn Fn() + Send + Sync>) -> Result<Worker, String> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        let job_wake = wake.clone();
        let (sign_in_tx, sign_ins) = mpsc::channel::<SignInResult>();
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, res_rx) = mpsc::channel::<JobResult>();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("rslook-sync".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let id = job.id;
                    let result = runtime.block_on(std::panic::AssertUnwindSafe(run_job(&job, &net)).catch_unwind()).unwrap_or_else(|_| JobResult {
                        job: id,
                        outcome: Err("internal error in Send/Receive".into()),
                        new_secret: None,
                    });
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    job_wake();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Worker { jobs: job_tx, results: res_rx, sign_in_tx, sign_ins, wake })
    }

    /// Runs a Microsoft sign-in in the system web browser on its own thread.
    pub fn sign_in(&self, job: SignInJob) {
        let tx = self.sign_in_tx.clone();
        let wake = self.wake.clone();
        let id = job.id;
        let spawned = std::thread::Builder::new().name("rslook-sign-in".into()).spawn(move || {
            let browser = |url: &str| webbrowser::open(url).map_err(|e| format!("could not open the web browser: {e}"));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sign_in(&job, &browser)))
                .unwrap_or_else(|_| SignInResult { id, outcome: Err("internal error while signing in".into()) });
            let _ = tx.send(result);
            wake();
        });
        if let Err(e) = spawned {
            let _ = self.sign_in_tx.send(SignInResult { id, outcome: Err(e.to_string()) });
        }
    }

    /// Finished sign-ins since the last call.
    pub fn poll_sign_ins(&self) -> Vec<SignInResult> {
        self.sign_ins.try_iter().collect()
    }

    /// Hands a job to the worker. `false` if the worker has stopped.
    pub fn submit(&self, job: Job) -> bool {
        self.jobs.send(job).is_ok()
    }

    /// Finished jobs since the last call.
    pub fn poll(&self) -> Vec<JobResult> {
        self.results.try_iter().collect()
    }
}
