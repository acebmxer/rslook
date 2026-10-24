#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! The Rslook engine: a [`Session`] over one mailbox, undo, and the command registry that
//! every front end (ribbon, shortcuts, later CLI/MCP) dispatches through by id.

pub mod catalog;
pub mod cmd;
pub mod sample;
#[cfg(test)]
mod tests;

pub use cmd::{CommandSpec, commands, find};
pub use rslook_mail as mail;

use rslook_mail::{Address, FolderId, FolderKind, MailError, MessageId, Store};
use serde_json::Value;

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
    /// Last command's outcome for the status bar.
    pub status: String,
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
            status: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        };
        s.selected = s.visible().first().copied();
        s
    }

    pub fn empty(account: Address) -> Self {
        Self::new(Store::new(account))
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
