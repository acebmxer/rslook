//! Account sync glue: the offline change queue, sync jobs handed to a background worker, merging
//! the worker's results into the store, and the on-disk cache.
//!
//! The engine never touches the network. Commands change the local store; [`Session::run`]
//! turns changes to server messages into [`QueuedOp`]s. A front end takes a [`Job`] with
//! [`Session::next_job`], runs it (see the `rslook-sync` crate) and hands the [`JobResult`] back
//! to [`Session::apply_result`]. The queue is part of the cache, so offline changes survive a
//! restart and replay on the next Send/Receive.

use crate::{EngineError, Session};
use chrono::{DateTime, Utc};
use rslook_account::{Account, AccountKind, MicrosoftSettings, MsBackend};
use rslook_mail::{Address, FolderId, FolderKind, MessageId, NewMessage, RemoteRef, Store};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub type OpId = u64;

/// One change made locally to a server message, waiting to be replayed on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedOp {
    pub id: OpId,
    pub target: RemoteRef,
    pub kind: OpKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    /// Set \Seen and \Flagged to these values.
    Flags { read: bool, flagged: bool },
    /// Move to another mailbox, creating it first when `create` is set.
    Move { to: String, create: bool },
    /// Remove for good (STORE \Deleted + EXPUNGE).
    Delete,
}

/// What a front end asked to sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRequest {
    /// Send/Receive All Folders: replay changes, send the Outbox, pull every folder.
    All,
    /// Update Folder: replay changes and pull one folder.
    Folder(FolderId),
    /// Send All: send the Outbox only.
    SendAll,
}

/// Which mailboxes a job pulls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pull {
    None,
    All,
    Mailbox(String),
}

/// A message waiting in the Outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub id: MessageId,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub body: String,
    pub date: DateTime<Utc>,
}

/// UIDs the cache already holds for a mailbox, so the worker only downloads new messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownMailbox {
    pub uid_validity: u32,
    pub uids: HashSet<u32>,
    /// Server ids (Graph) the cache knows, with the number each was given.
    pub keys: HashMap<String, u32>,
}

/// Everything a worker needs for one Send/Receive, with no reference back to the session.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub account: Account,
    /// The keychain secret: the password, or for a Microsoft account the OAuth refresh token.
    pub password: String,
    pub ops: Vec<QueuedOp>,
    pub outbox: Vec<OutgoingMessage>,
    /// Where to save a copy of sent mail, and whether that mailbox must be created first.
    pub sent_mailbox: Option<(String, bool)>,
    pub pull: Pull,
    pub known: HashMap<String, KnownMailbox>,
}

/// A server mailbox as listed by the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Raw IMAP name (used in commands), or the Graph folder id.
    pub name: String,
    /// The parent folder's `name`, for servers that say it directly (Graph); IMAP folders
    /// derive it from the delimiter.
    pub parent: Option<String>,
    /// Decoded last path component, for display.
    pub display: String,
    pub delimiter: Option<String>,
    /// Standard folder this mailbox is, from its special-use attribute or a well-known name.
    pub kind: Option<FolderKind>,
    /// True when `kind` came from a special-use attribute (RFC 6154) rather than the name.
    pub special_use: bool,
    pub selectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMessage {
    pub uid: u32,
    /// The server's own id, for servers with string ids (Graph).
    pub key: Option<String>,
    pub read: bool,
    pub flagged: bool,
    /// Parsed content, present for messages the cache did not know.
    pub content: Option<NewMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxContents {
    pub mailbox: String,
    pub uid_validity: u32,
    pub messages: Vec<RemoteMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentItem {
    pub id: MessageId,
    /// The mailbox a copy was appended to, if that worked.
    pub appended_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub done_ops: Vec<OpId>,
    pub failed_ops: Vec<(OpId, String)>,
    pub sent: Vec<SentItem>,
    pub send_failed: Vec<(MessageId, String)>,
    pub folders: Option<Vec<RemoteFolder>>,
    pub contents: Vec<MailboxContents>,
    /// The job stopped part way (connection lost after some mail was sent). What is in the
    /// report did happen; anything else is retried next time.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobResult {
    pub job: u64,
    /// `Err` when the job could not run at all (no connection, login refused).
    pub outcome: Result<Report, String>,
    /// A new keychain secret to keep (Microsoft may hand out a new refresh token on each use).
    pub new_secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Idle,
    Busy {
        job: u64,
        what: String,
    },
    Error(String),
}

/// Where settings and the offline cache live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    /// The platform's per-user folders: `$XDG_CONFIG_HOME/rslook` and `$XDG_DATA_HOME/rslook`
    /// (falling back to `~/.config` and `~/.local/share`) on Linux, `%APPDATA%\Rslook` on Windows.
    pub fn platform() -> Option<Paths> {
        if cfg!(windows) {
            let base = PathBuf::from(std::env::var_os("APPDATA")?).join("Rslook");
            return Some(Paths { config: base.clone(), data: base });
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| -> Option<PathBuf> {
            match std::env::var_os(var).map(PathBuf::from) {
                Some(p) if p.is_absolute() => Some(p),
                _ => home.as_ref().map(|h| h.join(fallback)),
            }
        };
        Some(Paths { config: xdg("XDG_CONFIG_HOME", ".config")?.join("rslook"), data: xdg("XDG_DATA_HOME", ".local/share")?.join("rslook") })
    }

    pub fn accounts_file(&self) -> PathBuf {
        self.config.join("accounts.json")
    }

    /// One cache file per account, named after its address (unsafe characters replaced). A
    /// Microsoft account gets its own file per backend: Graph and IMAP name messages differently.
    pub fn cache_file(&self, account: &Account) -> PathBuf {
        let name: String =
            account.key().chars().take(120).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_') { c } else { '_' }).collect();
        let prefix = match (account.kind, account.microsoft.backend) {
            (AccountKind::Imap, _) => "cache",
            (AccountKind::Microsoft, MsBackend::Graph) => "cache-graph",
            (AccountKind::Microsoft, MsBackend::Imap) => "cache-ms-imap",
        };
        self.data.join(format!("{prefix}-{name}.json"))
    }
}

/// The offline cache: the mailbox plus changes not yet on the server.
#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    version: u32,
    store: Store,
    queue: Vec<QueuedOp>,
    next_op: u64,
}

/// Cache files bigger than this are refused rather than read into memory.
const MAX_CACHE_BYTES: u64 = 4 << 30;

/// Reads a cache file. `Ok(None)` when there is none yet.
pub fn load_cache(path: &Path) -> Result<Option<(Store, Vec<QueuedOp>, u64)>, EngineError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(EngineError::Io(e.to_string())),
    };
    if meta.len() > MAX_CACHE_BYTES {
        return Err(EngineError::Io("offline cache file is too large".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| EngineError::Io(e.to_string()))?;
    let cache: Cache = serde_json::from_slice(&bytes).map_err(|e| EngineError::Io(format!("offline cache is damaged: {e}")))?;
    Ok(Some((cache.store, cache.queue, cache.next_op)))
}

/// A message's server state before a command ran, for working out what the command changed.
pub(crate) type RemoteSnapshot = HashMap<MessageId, (FolderId, bool, bool, RemoteRef)>;

pub(crate) fn snapshot(store: &Store) -> RemoteSnapshot {
    store
        .messages()
        .iter()
        .filter_map(|m| m.remote.as_ref().filter(|r| r.uid != 0).map(|r| (m.id, (m.folder, m.read, m.flagged, r.clone()))))
        .collect()
}

/// Standard folders Rslook creates on the server the first time something goes there.
fn creatable(kind: FolderKind) -> bool {
    matches!(kind, FolderKind::Sent | FolderKind::Drafts | FolderKind::Deleted | FolderKind::Archive | FolderKind::Junk)
}

/// A Microsoft sign-in for the front end to run (browser + loopback redirect, see `rslook-sync`).
#[derive(Debug, Clone)]
pub struct SignInJob {
    pub id: u64,
    pub settings: MicrosoftSettings,
    /// Set when the user cancels; the sign-in stops waiting for the browser.
    pub cancel: Arc<AtomicBool>,
}

/// Who signed in, and the refresh token to keep in the keychain.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedIn {
    pub name: String,
    pub email: String,
    pub refresh_token: String,
}

impl std::fmt::Debug for SignedIn {
    // Never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedIn").field("name", &self.name).field("email", &self.email).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct SignInResult {
    pub id: u64,
    pub outcome: Result<SignedIn, String>,
}

impl Session {
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    /// Changes not yet replayed on the server.
    pub fn queue(&self) -> &[QueuedOp] {
        &self.queue
    }

    pub fn busy(&self) -> bool {
        matches!(self.sync_state, SyncState::Busy { .. })
    }

    /// Asks for a sync. Ignored without an account or while working offline.
    pub fn request(&mut self, r: SyncRequest) {
        if self.account.is_some() && !self.work_offline && !self.requests.contains(&r) {
            self.requests.push(r);
        }
    }

    pub fn has_requests(&self) -> bool {
        !self.requests.is_empty()
    }

    /// Starts a Microsoft sign-in for the open account dialog.
    pub(crate) fn start_sign_in(&mut self) -> Result<(), String> {
        let Some(d) = self.account_dialog.as_ref() else { return Err("The account dialog is closed.".into()) };
        let (backend, client_id) = (d.ms_backend, d.client_id.trim().to_string());
        let settings = MicrosoftSettings { backend, client_id, ..self.ms_overrides() };
        settings.validate().map_err(|e| e.to_string())?;
        self.next_sign_in_id = self.next_sign_in_id.saturating_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(d) = self.account_dialog.as_mut() {
            d.signing_in = true;
            d.error = None;
        }
        self.sign_in = Some((self.next_sign_in_id, cancel.clone()));
        self.sign_in_requests.push(SignInJob { id: self.next_sign_in_id, settings, cancel });
        Ok(())
    }

    /// Test endpoints of the current Microsoft account, kept when its dialog signs in again.
    fn ms_overrides(&self) -> MicrosoftSettings {
        match &self.account {
            Some(a) if a.kind == AccountKind::Microsoft => MicrosoftSettings {
                auth_base: a.microsoft.auth_base.clone(),
                graph_base: a.microsoft.graph_base.clone(),
                tenant: a.microsoft.tenant.clone(),
                ..Default::default()
            },
            _ => MicrosoftSettings::default(),
        }
    }

    pub(crate) fn cancel_sign_in(&mut self) {
        if let Some((_, cancel)) = self.sign_in.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.sign_in_requests.clear();
        if let Some(d) = self.account_dialog.as_mut() {
            d.signing_in = false;
        }
    }

    /// The next sign-in for the front end to run.
    pub fn next_sign_in(&mut self) -> Option<SignInJob> {
        self.sign_in_requests.pop()
    }

    /// Takes a finished sign-in into the account dialog (stale or cancelled ones are dropped).
    pub fn apply_sign_in(&mut self, r: SignInResult) {
        if !self.sign_in.as_ref().is_some_and(|(id, _)| *id == r.id) {
            return;
        }
        self.sign_in = None;
        let Some(d) = self.account_dialog.as_mut() else { return };
        d.signing_in = false;
        match r.outcome {
            Ok(who) => {
                if d.name.trim().is_empty() {
                    d.name = who.name.clone();
                }
                d.email = who.email.clone();
                d.username = who.email.clone();
                d.oauth_token = who.refresh_token;
                d.signed_in = Some((who.name, who.email));
                d.error = None;
            }
            Err(e) => d.error = Some(e),
        }
    }

    fn push_op(&mut self, target: RemoteRef, kind: OpKind) {
        let id = self.next_op;
        self.next_op = self.next_op.saturating_add(1);
        self.queue.push(QueuedOp { id, target, kind });
    }

    /// The server mailbox for a local folder, assigning the standard name for a standard folder
    /// the server doesn't have yet. `None` for folders that only exist locally (Outbox).
    fn mailbox_for(&mut self, folder: FolderId) -> Option<(String, bool)> {
        let f = self.store.folder(folder)?;
        if let Some(r) = &f.remote {
            return Some((r.clone(), false));
        }
        if !creatable(f.kind) {
            return None;
        }
        let name = f.kind.default_name().to_string();
        if let Ok(f) = self.store.folder_mut(folder) {
            f.remote = Some(name.clone());
        }
        Some((name, true))
    }

    fn queue_flags(&mut self, target: RemoteRef, read: bool, flagged: bool) {
        // Only the newest flag state matters.
        self.queue.retain(|op| !(op.target == target && matches!(op.kind, OpKind::Flags { .. })));
        self.push_op(target, OpKind::Flags { read, flagged });
    }

    fn queue_move(&mut self, target: RemoteRef, folder: FolderId) {
        self.queue.retain(|op| !(op.target == target && matches!(op.kind, OpKind::Move { .. })));
        match self.mailbox_for(folder) {
            Some((to, _)) if to == target.mailbox => {} // back where the server has it
            Some((to, create)) => self.push_op(target, OpKind::Move { to, create }),
            None => self.status = "This folder is only on this computer; the server copy stays where it was.".into(),
        }
    }

    fn queue_delete(&mut self, target: RemoteRef) {
        // Expunged where the server has it, so an earlier move no longer applies.
        self.queue.retain(|op| !(op.target == target && matches!(op.kind, OpKind::Move { .. })));
        self.push_op(target, OpKind::Delete);
    }

    /// Turns what the last command did to server messages into queued operations.
    pub(crate) fn queue_changes(&mut self, before: &RemoteSnapshot) {
        enum Change {
            Delete(RemoteRef),
            Flags(RemoteRef, bool, bool),
            Move(RemoteRef, FolderId),
        }
        let mut changes = Vec::new();
        for (id, (folder, read, flagged, target)) in before {
            match self.store.message(*id) {
                None => changes.push(Change::Delete(target.clone())),
                Some(m) => {
                    if (m.read, m.flagged) != (*read, *flagged) {
                        changes.push(Change::Flags(target.clone(), m.read, m.flagged));
                    }
                    if m.folder != *folder {
                        changes.push(Change::Move(target.clone(), m.folder));
                    }
                }
            }
        }
        // Messages that came back (undo of a permanent delete): drop the pending delete, and
        // move them back on the server if they are no longer where the server has them.
        let back: Vec<(RemoteRef, FolderId)> = self
            .store
            .messages()
            .iter()
            .filter(|m| !before.contains_key(&m.id))
            .filter_map(|m| m.remote.as_ref().filter(|r| r.uid != 0).map(|r| (r.clone(), m.folder)))
            .collect();
        for (target, folder) in back {
            let had_delete = self.queue.iter().any(|op| op.target == target && op.kind == OpKind::Delete);
            if had_delete {
                self.queue.retain(|op| !(op.target == target && op.kind == OpKind::Delete));
                let here = self.store.folder(folder).and_then(|f| f.remote.clone());
                if here.as_deref() != Some(target.mailbox.as_str()) {
                    self.queue_move(target, folder);
                }
            }
        }
        for change in changes {
            match change {
                Change::Delete(target) => self.queue_delete(target),
                Change::Flags(target, read, flagged) => self.queue_flags(target, read, flagged),
                Change::Move(target, folder) => self.queue_move(target, folder),
            }
        }
    }

    /// Builds the next job from pending requests, or `None` when there is nothing to do, a job
    /// is already running, or the session is offline. Takes the password from the keychain.
    pub fn next_job(&mut self) -> Option<Job> {
        if self.busy() || self.work_offline || self.requests.is_empty() {
            return None;
        }
        let account = self.account.clone()?;
        let requests = std::mem::take(&mut self.requests);
        let password = match self.secrets.get(&account.key()) {
            Ok(p) => p,
            Err(rslook_account::AccountError::NoPassword(_)) if account.kind == AccountKind::Microsoft => {
                self.sync_state = SyncState::Error("Not signed in to Microsoft. Open Account Settings and sign in again.".into());
                return None;
            }
            Err(e) => {
                self.sync_state = SyncState::Error(e.to_string());
                return None;
            }
        };
        let all = requests.contains(&SyncRequest::All);
        let folder = requests.iter().find_map(|r| match r {
            SyncRequest::Folder(f) => Some(*f),
            _ => None,
        });
        let send = all || requests.contains(&SyncRequest::SendAll);
        let pull = if all {
            Pull::All
        } else if let Some(f) = folder {
            match self.store.folder(f).and_then(|f| f.remote.clone()) {
                Some(m) => Pull::Mailbox(m),
                None => Pull::None,
            }
        } else {
            Pull::None
        };
        let replay = all || folder.is_some();
        let outbox: Vec<OutgoingMessage> = if send {
            let out = self.store.folder_of_kind(FolderKind::Outbox);
            self.store
                .messages()
                .iter()
                .filter(|m| Some(m.folder) == out)
                .map(|m| OutgoingMessage {
                    id: m.id,
                    from: m.from.clone(),
                    to: m.to.clone(),
                    cc: m.cc.clone(),
                    subject: m.subject.clone(),
                    body: m.body.clone(),
                    date: m.date,
                })
                .collect()
        } else {
            Vec::new()
        };
        let sent_mailbox = if outbox.is_empty() { None } else { self.store.folder_of_kind(FolderKind::Sent).and_then(|f| self.mailbox_for(f)) };
        if !replay && outbox.is_empty() && pull == Pull::None {
            self.status = if send { "Nothing to send.".into() } else { "This folder is only on this computer.".into() };
            return None;
        }
        let mut known: HashMap<String, KnownMailbox> = HashMap::new();
        for r in self.store.messages().iter().filter_map(|m| m.remote.as_ref()).filter(|r| r.uid != 0) {
            let k = known.entry(r.mailbox.clone()).or_insert_with(|| KnownMailbox { uid_validity: r.uid_validity, ..Default::default() });
            if k.uid_validity == r.uid_validity {
                k.uids.insert(r.uid);
                if let Some(key) = &r.key {
                    k.keys.insert(key.clone(), r.uid);
                }
            }
        }
        self.next_job_id = self.next_job_id.saturating_add(1);
        let id = self.next_job_id;
        let what = match (&pull, outbox.is_empty()) {
            (Pull::None, false) => "Sending…",
            (Pull::Mailbox(_), _) => "Updating folder…",
            _ => "Sending and receiving…",
        };
        self.sync_state = SyncState::Busy { job: id, what: what.into() };
        Some(Job { id, account, password, ops: if replay { self.queue.clone() } else { Vec::new() }, outbox, sent_mailbox, pull, known })
    }

    /// Merges a finished job into the store.
    pub fn apply_result(&mut self, result: JobResult) {
        if !matches!(&self.sync_state, SyncState::Busy { job, .. } if *job == result.job) {
            return;
        }
        if let (Some(secret), Some(account)) = (&result.new_secret, &self.account)
            && let Err(e) = self.secrets.set(&account.key(), secret)
        {
            log::warn!("could not keep the new sign-in token: {e}");
        }
        let report = match result.outcome {
            Ok(r) => r,
            Err(e) => {
                self.sync_state = SyncState::Error(e);
                return;
            }
        };
        self.sync_state = SyncState::Idle;
        self.last_sync = Some(Utc::now());
        let finished: HashSet<OpId> = report.done_ops.iter().copied().chain(report.failed_ops.iter().map(|(id, _)| *id)).collect();
        self.queue.retain(|op| !finished.contains(&op.id));
        self.apply_sent(&report);
        if let Some(folders) = &report.folders {
            self.apply_folders(folders);
        }
        for c in &report.contents {
            self.apply_contents(c);
        }
        // Undo steps hold server references the merge may have replaced.
        self.undo.clear();
        self.redo.clear();
        self.dirty = true;
        self.fix_selection();
        let mut notes = Vec::new();
        if let Some((_, e)) = report.send_failed.first() {
            notes.push(format!("{} message(s) could not be sent: {e}", report.send_failed.len()));
        }
        if let Some((_, e)) = report.failed_ops.first() {
            notes.push(format!("{} change(s) could not be made on the server: {e}", report.failed_ops.len()));
        }
        self.status = if notes.is_empty() {
            match report.sent.len() {
                0 => "Send/Receive complete".to_string(),
                n => format!("Send/Receive complete, {n} sent"),
            }
        } else {
            notes.join(" ")
        };
        if let Some(e) = report.error {
            self.sync_state = SyncState::Error(e);
        }
    }

    fn apply_sent(&mut self, report: &Report) {
        let Some(sent) = self.store.folder_of_kind(FolderKind::Sent) else { return };
        for item in &report.sent {
            let folder = item.appended_to.as_deref().and_then(|m| self.store.folder_by_remote(m)).unwrap_or(sent);
            if self.store.move_message(item.id, folder).is_ok()
                && let (Some(mailbox), Ok(m)) = (&item.appended_to, self.store.message_mut(item.id))
            {
                // On the server now; the next pull of that mailbox swaps in the server copy.
                m.remote = Some(RemoteRef { mailbox: mailbox.clone(), uid_validity: 0, uid: 0, key: None });
            }
        }
    }

    fn apply_folders(&mut self, folders: &[RemoteFolder]) {
        let names: HashSet<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        // Local folders whose mailbox is gone: user folders go, standard folders stay local.
        let gone: Vec<(FolderId, FolderKind)> =
            self.store.folders().iter().filter(|f| f.remote.as_deref().is_some_and(|r| !names.contains(r))).map(|f| (f.id, f.kind)).collect();
        for (id, kind) in gone {
            if kind == FolderKind::Custom {
                let _ = self.store.remove_folder(id);
            } else if let Ok(f) = self.store.folder_mut(id) {
                f.remote = None;
            }
        }
        // Parents before children: by depth when the server names parents (Graph), else by name
        // length (an IMAP child's name contains its parent's).
        let parent_of: HashMap<&str, &str> = folders.iter().filter_map(|f| f.parent.as_deref().map(|p| (f.name.as_str(), p))).collect();
        let depth = |name: &str| {
            let mut d = 0usize;
            let mut cur = name;
            while let Some(p) = parent_of.get(cur) {
                d = d.saturating_add(1);
                cur = p;
                if d > folders.len() {
                    break; // a cycle: treat as deep, never loop
                }
            }
            d
        };
        let mut ordered: Vec<&RemoteFolder> = folders.iter().collect();
        ordered.sort_by_key(|f| (!f.special_use, depth(&f.name), f.name.len()));
        for rf in ordered {
            if self.store.folder_by_remote(&rf.name).is_some() {
                continue;
            }
            if let Some(kind) = rf.kind
                && let Some(local) = self.store.folders().iter().find(|f| f.kind == kind && f.remote.is_none()).map(|f| f.id)
            {
                if let Ok(f) = self.store.folder_mut(local) {
                    f.remote = Some(rf.name.clone());
                }
                continue;
            }
            let parent = match &rf.parent {
                Some(p) => self.store.folder_by_remote(p),
                None => rf
                    .delimiter
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .and_then(|d| rf.name.rsplit_once(d))
                    .and_then(|(p, _)| self.store.folder_by_remote(p)),
            };
            let id = self.store.add_folder(&rf.display, FolderKind::Custom, parent);
            if let Ok(f) = self.store.folder_mut(id) {
                f.remote = Some(rf.name.clone());
            }
        }
    }

    fn apply_contents(&mut self, c: &MailboxContents) {
        let Some(folder) = self.store.folder_by_remote(&c.mailbox) else { return };
        let on_server: HashMap<u32, &RemoteMessage> = c.messages.iter().map(|m| (m.uid, m)).collect();
        let pending: HashSet<RemoteRef> = self.queue.iter().map(|op| op.target.clone()).collect();
        let mut purge = Vec::new();
        let mut updates = Vec::new();
        for m in self.store.messages() {
            let Some(r) = &m.remote else { continue };
            if m.folder != folder || pending.contains(r) {
                continue;
            }
            let current = r.mailbox == c.mailbox && r.uid_validity == c.uid_validity && r.uid != 0;
            match on_server.get(&r.uid) {
                Some(s) if current => updates.push((m.id, s.read, s.flagged)),
                _ => purge.push(m.id),
            }
        }
        for id in purge {
            let _ = self.store.purge_message(id);
        }
        for (id, read, flagged) in updates {
            if let Ok(m) = self.store.message_mut(id) {
                m.read = read;
                m.flagged = flagged;
            }
        }
        let referenced: HashSet<u32> = self
            .store
            .messages()
            .iter()
            .filter_map(|m| m.remote.as_ref())
            .filter(|r| r.mailbox == c.mailbox && r.uid_validity == c.uid_validity)
            .map(|r| r.uid)
            .collect();
        for rm in &c.messages {
            if referenced.contains(&rm.uid) {
                continue;
            }
            if let Some(content) = &rm.content {
                let mut nm = content.clone();
                nm.read = rm.read;
                nm.flagged = rm.flagged;
                nm.remote = Some(RemoteRef { mailbox: c.mailbox.clone(), uid_validity: c.uid_validity, uid: rm.uid, key: rm.key.clone() });
                let _ = self.store.add_message(folder, nm);
            }
        }
    }

    /// Writes the offline cache when something changed. No-op without an account or paths.
    pub fn save_cache(&mut self) -> Result<(), EngineError> {
        if !self.dirty {
            return Ok(());
        }
        let (Some(paths), Some(account)) = (&self.paths, &self.account) else {
            self.dirty = false;
            return Ok(());
        };
        let path = paths.cache_file(account);
        #[derive(Serialize)]
        struct CacheRef<'a> {
            version: u32,
            store: &'a Store,
            queue: &'a [QueuedOp],
            next_op: u64,
        }
        let json = serde_json::to_vec(&CacheRef { version: 1, store: &self.store, queue: &self.queue, next_op: self.next_op })
            .map_err(|e| EngineError::Io(e.to_string()))?;
        rslook_account::write_atomic(&path, &json).map_err(|e| EngineError::Io(e.to_string()))?;
        self.dirty = false;
        Ok(())
    }
}
