#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! The Send/Receive worker. [`run_job`] carries out one engine [`Job`]: send the Outbox over
//! SMTP, save copies to Sent, replay queued changes, then pull folders over IMAP. [`Worker`] runs
//! jobs on a background thread so the window never waits on the network.

use futures::FutureExt;
use rslook_engine::mail::{NewMessage, parse};
use rslook_engine::sync::{Job, JobResult, MailboxContents, OpKind, Pull, QueuedOp, RemoteFolder, RemoteMessage, Report, SentItem};
use rslook_imap::{Conn, ImapError, TlsOptions};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

/// Network settings that are not part of the account (tests add a private CA here).
#[derive(Debug, Clone, Default)]
pub struct NetOptions {
    pub extra_roots_pem: Vec<u8>,
}

fn outgoing(o: &rslook_engine::sync::OutgoingMessage) -> rslook_smtp::Outgoing {
    rslook_smtp::Outgoing { from: o.from.clone(), to: o.to.clone(), cc: o.cc.clone(), subject: o.subject.clone(), body: o.body.clone(), date: o.date }
}

/// Replay order: flags first (they address the message where it is), then moves, then deletes.
fn phase(op: &QueuedOp) -> u8 {
    match op.kind {
        OpKind::Flags { .. } => 0,
        OpKind::Move { .. } => 1,
        OpKind::Delete => 2,
    }
}

/// Runs one job. `Err` only when nothing at all happened (e.g. the server could not be
/// reached before anything was sent); partial progress comes back as a report with `error`.
pub async fn run_job(job: &Job, net: &NetOptions) -> JobResult {
    let outcome = run(job, net).await;
    JobResult { job: job.id, outcome }
}

async fn run(job: &Job, net: &NetOptions) -> Result<Report, String> {
    let a = &job.account;
    let mut report = Report::default();

    // 1. Send the Outbox.
    let mut sent_bytes = Vec::new();
    for o in &job.outbox {
        match rslook_smtp::send(&a.smtp, &a.username, &job.password, &net.extra_roots_pem, &outgoing(o)).await {
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
    let mut conn = match rslook_imap::connect(&a.imap, &a.username, &job.password, &tls).await {
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

fn unreadable(header_only: bool) -> NewMessage {
    NewMessage { subject: "(message could not be read)".into(), body: if header_only { too_big() } else { String::new() }, ..Default::default() }
}

fn too_big() -> String {
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
    let messages = states.iter().map(|s| RemoteMessage { uid: s.uid, read: s.read, flagged: s.flagged, content: content.remove(&s.uid) }).collect();
    Ok(MailboxContents { mailbox: mailbox.to_string(), uid_validity, messages })
}

/// Runs jobs on a background thread with its own async runtime.
pub struct Worker {
    jobs: mpsc::Sender<Job>,
    results: mpsc::Receiver<JobResult>,
}

impl Worker {
    /// Starts the worker. `wake` is called after each job (e.g. to repaint the window).
    pub fn spawn(net: NetOptions, wake: Box<dyn Fn() + Send>) -> Result<Worker, String> {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (res_tx, res_rx) = mpsc::channel::<JobResult>();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("rslook-sync".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let id = job.id;
                    let result = runtime
                        .block_on(std::panic::AssertUnwindSafe(run_job(&job, &net)).catch_unwind())
                        .unwrap_or_else(|_| JobResult { job: id, outcome: Err("internal error in Send/Receive".into()) });
                    if res_tx.send(result).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Worker { jobs: job_tx, results: res_rx })
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
