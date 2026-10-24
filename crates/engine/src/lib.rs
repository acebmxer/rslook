#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! The Rslook engine: a [`Session`] over one mailbox, undo, and the command registry that
//! every front end (ribbon, shortcuts, later CLI/MCP) dispatches through by id.

pub mod catalog;
pub mod cmd;
pub mod sample;
pub mod sync;
#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod tests;

pub use cmd::{CommandSpec, commands, find};
pub use rslook_account as account;
pub use rslook_mail as mail;

use rslook_account::{Account, MemorySecrets, SecretStore};
use rslook_mail::{Address, FolderId, FolderKind, MailError, MessageId, Store};
use serde_json::Value;
use std::sync::Arc;
use sync::{Paths, QueuedOp, SyncRequest, SyncState};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown command: {0}")]
    UnknownCommand(String),
    #[error("{0} is not available right now")]
    Disabled(&'static str),
    #[error("bad parameter: {0}")]
    BadParam(String),
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error("io: {0}")]
    Io(String),
    #[error("internal error in {0}")]
    Panic(String),
    #[error("could not import: {0}")]
    Import(String),
}

/// The main window's modules (Outlook's navigation bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Module {
    #[default]
    Mail,
    Calendar,
    People,
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadingPane {
    #[default]
    Right,
    Bottom,
    Off,
}

/// A message being written. Not part of the store until saved or sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// The draft this was opened from (Drafts folder), replaced when saved again.
    pub replaces: Option<MessageId>,
}

/// The account dialog's fields while it is open. The password stays in memory only and goes
/// straight to the OS keychain on save.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AccountDraft {
    pub name: String,
    pub email: String,
    pub username: String,
    pub password: String,
    pub imap_host: String,
    pub imap_port: String,
    pub imap_security: rslook_account::Security,
    pub smtp_host: String,
    pub smtp_port: String,
    pub smtp_security: rslook_account::Security,
    /// Editing the existing account (Account Settings) rather than adding one.
    pub editing: bool,
    /// Why the last save was refused, shown in the dialog.
    pub error: Option<String>,
}

/// The Import/Export dialog while it is open: the file to import and why the last try failed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ImportDraft {
    pub path: String,
    pub error: Option<String>,
}

impl std::fmt::Debug for AccountDraft {
    // Never print the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountDraft").field("email", &self.email).field("editing", &self.editing).finish_non_exhaustive()
    }
}

const UNDO_DEPTH: usize = 100;

pub struct Session {
    pub store: Store,
    pub module: Module,
    pub folder: FolderId,
    pub selected: Option<MessageId>,
    pub search: String,
    pub reading_pane: ReadingPane,
    pub folder_pane: bool,
    pub compose: Option<Draft>,
    pub account_dialog: Option<AccountDraft>,
    pub import_dialog: Option<ImportDraft>,
    /// Last command's outcome for the status bar.
    pub status: String,
    /// Work Offline: no connections, changes wait in the queue.
    pub work_offline: bool,
    pub sync_state: SyncState,
    pub last_sync: Option<chrono::DateTime<chrono::Utc>>,
    /// Store or queue changed since the cache was last written.
    pub dirty: bool,
    account: Option<Account>,
    secrets: Arc<dyn SecretStore>,
    paths: Option<Paths>,
    queue: Vec<QueuedOp>,
    next_op: u64,
    requests: Vec<SyncRequest>,
    next_job_id: u64,
    undo: Vec<Store>,
    redo: Vec<Store>,
}

impl Session {
    pub fn new(store: Store) -> Self {
        let folder = store.folder_of_kind(FolderKind::Inbox).or_else(|| store.folders().first().map(|f| f.id)).unwrap_or(0);
        let mut s = Session {
            store,
            module: Module::Mail,
            folder,
            selected: None,
            search: String::new(),
            reading_pane: ReadingPane::Right,
            folder_pane: true,
            compose: None,
            account_dialog: None,
            import_dialog: None,
            status: String::new(),
            work_offline: false,
            sync_state: SyncState::Idle,
            last_sync: None,
            dirty: false,
            account: None,
            secrets: Arc::new(MemorySecrets::default()),
            paths: None,
            queue: Vec::new(),
            next_op: 1,
            requests: Vec::new(),
            next_job_id: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        s.selected = s.visible().first().copied();
        s
    }

    pub fn empty(account: Address) -> Self {
        Self::new(Store::new(account))
    }

    /// Where passwords are kept (the OS keychain in the app; in-memory by default).
    pub fn set_secrets(&mut self, secrets: Arc<dyn SecretStore>) {
        self.secrets = secrets;
    }

    /// Where settings and the offline cache are written. Without paths nothing is saved.
    pub fn set_paths(&mut self, paths: Paths) {
        self.paths = Some(paths);
    }

    pub fn paths(&self) -> Option<&Paths> {
        self.paths.as_ref()
    }

    /// Opens an account: its cached mailbox and queued changes, or an empty mailbox for it.
    pub fn open_account(&mut self, account: Account, cached: Option<(Store, Vec<QueuedOp>, u64)>) {
        let (store, queue, next_op) =
            cached.unwrap_or_else(|| (Store::new(Address::new(account.name.clone(), account.email.clone())), Vec::new(), 1));
        self.store = store;
        self.store.account = Address::new(account.name.clone(), account.email.clone());
        self.queue = queue;
        self.next_op = next_op.max(self.queue.iter().map(|op| op.id.saturating_add(1)).max().unwrap_or(1));
        self.account = Some(account);
        self.undo.clear();
        self.redo.clear();
        self.requests.clear();
        self.sync_state = SyncState::Idle;
        self.compose = None;
        self.folder = self.store.folder_of_kind(FolderKind::Inbox).unwrap_or(0);
        self.selected = self.visible().first().copied();
    }

    /// Ids of the messages shown in the list, in display order.
    pub fn visible(&self) -> Vec<MessageId> {
        self.store.messages_in(self.folder, &self.search).iter().map(|m| m.id).collect()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Runs a command by id. Mutating commands snapshot the store for undo; a panic inside a
    /// command is caught and the store restored (never-crash safety net).
    pub fn run(&mut self, id: &str, params: &Value) -> Result<(), EngineError> {
        let spec = cmd::find(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        if !(spec.enabled)(self) {
            return Err(EngineError::Disabled(spec.id));
        }
        let before = spec.undoable.then(|| self.store.clone());
        // Server messages before the command, to queue what it changes for the server.
        let remote_before = self.account.is_some().then(|| sync::snapshot(&self.store));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.run)(self, params)))
            .unwrap_or_else(|_| Err(EngineError::Panic(spec.id.to_string())));
        match (&result, before) {
            (Ok(()), Some(snapshot)) if snapshot != self.store => {
                self.undo.push(snapshot);
                if self.undo.len() > UNDO_DEPTH {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
            (Err(_), Some(snapshot)) => self.store = snapshot,
            _ => {}
        }
        if let (Ok(()), Some(remote_before)) = (&result, remote_before) {
            self.queue_changes(&remote_before);
            self.dirty = true;
        }
        self.fix_selection();
        if let Err(e) = &result {
            self.status = e.to_string();
        }
        result
    }

    fn undo_step(&mut self) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                let cur = std::mem::replace(&mut self.store, prev);
                self.redo.push(cur);
                true
            }
            None => false,
        }
    }

    fn redo_step(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                let cur = std::mem::replace(&mut self.store, next);
                self.undo.push(cur);
                true
            }
            None => false,
        }
    }

    /// Keeps the folder and selection pointing at things that exist and are visible.
    fn fix_selection(&mut self) {
        if self.store.folder(self.folder).is_none() {
            self.folder = self.store.folder_of_kind(FolderKind::Inbox).unwrap_or(0);
        }
        let visible = self.visible();
        if !self.selected.is_some_and(|s| visible.contains(&s)) {
            self.selected = None;
        }
    }

    /// After removing the selected message, select the one that took its place (Outlook moves
    /// to the next item down the list).
    fn select_after_removal(&mut self, removed: MessageId, old_visible: &[MessageId]) {
        let pos = old_visible.iter().position(|&m| m == removed).unwrap_or(0);
        let now = self.visible();
        self.selected = now.get(pos).or_else(|| now.last()).copied();
    }
}
