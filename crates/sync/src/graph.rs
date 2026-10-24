//! Send/Receive over Microsoft Graph. Same job and report as the IMAP path: Graph folder ids
//! stand in for mailbox names and each message's Graph id rides along in `key`, with a number
//! per folder standing in for the IMAP UID (kept stable through the ids the cache already knows).

use crate::{outgoing, phase, too_big, unreadable};
use rslook_engine::account::MicrosoftSettings;
use rslook_engine::mail::{FolderKind, NewMessage, parse};
use rslook_engine::sync::{Job, MailboxContents, OpKind, Pull, QueuedOp, RemoteFolder, RemoteMessage, Report, SentItem};
use rslook_graph::{Client, GraphError, WellKnown};
use std::collections::HashMap;

/// Graph has no UIDVALIDITY; immutable ids never change meaning, so one fixed value serves.
const UID_VALIDITY: u32 = 1;

pub(crate) fn client(ms: &MicrosoftSettings, token: &str) -> Client {
    let base = ms.graph_base.trim().trim_end_matches('/');
    Client::new(if base.is_empty() { rslook_graph::GRAPH_BASE } else { base }, token)
}

/// The connection or the sign-in is gone, so carrying on is pointless.
fn fatal(e: &GraphError) -> bool {
    matches!(e, GraphError::Unauthorized | GraphError::Network(_))
}

fn message(e: &GraphError) -> String {
    match e {
        GraphError::Unauthorized => "Microsoft refused the sign-in. Open Account Settings (File tab) and sign in again.".into(),
        e => e.to_string(),
    }
}

fn kind(w: &WellKnown) -> Option<FolderKind> {
    Some(match w {
        WellKnown::Inbox => FolderKind::Inbox,
        WellKnown::Drafts => FolderKind::Drafts,
        WellKnown::SentItems => FolderKind::Sent,
        WellKnown::DeletedItems => FolderKind::Deleted,
        WellKnown::Archive => FolderKind::Archive,
        WellKnown::JunkEmail => FolderKind::Junk,
        // The server's Outbox is never mirrored: Rslook's Outbox is what Send All sends.
        WellKnown::Outbox => return None,
    })
}

pub(crate) fn run(job: &Job, token: &str) -> Result<Report, String> {
    let g = client(&job.account.microsoft, token);
    let mut report = Report::default();
    match work(&g, job, &mut report) {
        Ok(()) => Ok(report),
        // Nothing happened at all: report it as a failed Send/Receive.
        Err(e) if report == Report::default() => Err(message(&e)),
        Err(e) => {
            report.error = Some(message(&e));
            Ok(report)
        }
    }
}

fn work(g: &Client, job: &Job, report: &mut Report) -> Result<(), GraphError> {
    // 1. Send the Outbox. Graph files the sent copy in Sent Items itself.
    let sent_folder = job.sent_mailbox.as_ref().filter(|(_, create)| !create).map(|(m, _)| m.clone());
    for o in &job.outbox {
        let built = match rslook_smtp::build(&outgoing(o)) {
            Ok(m) => m.formatted(),
            Err(e) => {
                report.send_failed.push((o.id, e.to_string()));
                continue;
            }
        };
        match g.send_mime(&built) {
            Ok(()) => report.sent.push(SentItem { id: o.id, appended_to: sent_folder.clone() }),
            Err(e) if fatal(&e) => return Err(e),
            Err(e) => report.send_failed.push((o.id, message(&e))),
        }
    }

    // 2. Replay offline changes.
    let mut ops: Vec<&QueuedOp> = job.ops.iter().collect();
    ops.sort_by_key(|op| phase(op));
    let mut created: HashMap<String, String> = HashMap::new();
    for op in ops {
        match replay(g, op, &mut created) {
            Ok(()) => report.done_ops.push(op.id),
            Err(e) if fatal(&e) => return Err(e),
            Err(e) => report.failed_ops.push((op.id, message(&e))),
        }
    }

    // 3. Pull.
    let folders: Vec<String> = match &job.pull {
        Pull::None => Vec::new(),
        Pull::Mailbox(id) => vec![id.clone()],
        Pull::All => {
            let listed = g.folders()?;
            let ids: std::collections::HashSet<&str> = listed.iter().map(|f| f.id.as_str()).collect();
            let folders: Vec<RemoteFolder> = listed
                .iter()
                .filter(|f| f.well_known != Some(WellKnown::Outbox))
                .map(|f| RemoteFolder {
                    name: f.id.clone(),
                    // Top-level folders name the hidden root as parent; that is no folder here.
                    parent: f.parent_id.clone().filter(|p| ids.contains(p.as_str())),
                    display: f.display_name.clone(),
                    delimiter: None,
                    kind: f.well_known.as_ref().and_then(kind),
                    special_use: f.well_known.as_ref().and_then(kind).is_some(),
                    selectable: true,
                })
                .collect();
            let names = folders.iter().map(|f| f.name.clone()).collect();
            report.folders = Some(folders);
            names
        }
    };
    for id in folders {
        match pull(g, job, &id) {
            Ok(c) => report.contents.push(c),
            Err(e) if fatal(&e) => return Err(e),
            Err(e) => log::warn!("could not read a Graph folder: {e}"),
        }
    }
    Ok(())
}

fn replay(g: &Client, op: &QueuedOp, created: &mut HashMap<String, String>) -> Result<(), GraphError> {
    let Some(id) = op.target.key.as_deref() else {
        return Err(GraphError::BadResponse("this message has no Microsoft Graph id".into()));
    };
    match &op.kind {
        OpKind::Flags { read, flagged } => g.set_flags(id, *read, *flagged),
        OpKind::Move { to, create } => {
            // A standard folder the server did not have yet arrives as a name to create.
            let dest = if *create {
                match created.get(to) {
                    Some(d) => d.clone(),
                    None => {
                        let f = g.create_folder(None, to)?;
                        created.insert(to.clone(), f.id.clone());
                        f.id
                    }
                }
            } else {
                to.clone()
            };
            g.move_message(id, &dest).map(|_| ())
        }
        OpKind::Delete => match g.delete_permanently(id) {
            // Already gone on the server is what was wanted.
            Err(GraphError::Http { status: 404, .. }) => Ok(()),
            r => r,
        },
    }
}

fn pull(g: &Client, job: &Job, folder: &str) -> Result<MailboxContents, GraphError> {
    let states = g.messages(folder)?;
    let known = job.known.get(folder).filter(|k| k.uid_validity == UID_VALIDITY);
    let mut keys: HashMap<String, u32> = known.map(|k| k.keys.clone()).unwrap_or_default();
    let mut next = known.and_then(|k| k.uids.iter().max().copied()).unwrap_or(0);
    let mut messages = Vec::with_capacity(states.len());
    for s in states {
        let (uid, is_new) = match keys.get(&s.id) {
            Some(uid) => (*uid, false),
            None => {
                next = next.saturating_add(1);
                keys.insert(s.id.clone(), next);
                (next, true)
            }
        };
        let content = if is_new { Some(fetch(g, &s.id)?) } else { None };
        messages.push(RemoteMessage { uid, key: Some(s.id), read: s.is_read, flagged: s.flagged, content });
    }
    Ok(MailboxContents { mailbox: folder.to_string(), uid_validity: UID_VALIDITY, messages })
}

fn fetch(g: &Client, id: &str) -> Result<NewMessage, GraphError> {
    match g.message_mime(id) {
        Ok(bytes) => Ok(parse::parse_rfc5322(&bytes).unwrap_or_else(|_| unreadable(false))),
        Err(GraphError::TooLarge(_)) => {
            let mut m = unreadable(true);
            m.body = too_big();
            Ok(m)
        }
        Err(e) if fatal(&e) => Err(e),
        Err(e) => {
            log::warn!("could not download a message: {e}");
            Ok(unreadable(false))
        }
    }
}
