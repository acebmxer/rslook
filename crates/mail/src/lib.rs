#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Mail model: folders, messages, addresses and an in-memory store.
//!
//! The store is a plain value (`Clone`) so the engine can snapshot it for undo.

pub mod parse;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type FolderId = u64;
pub type MessageId = u64;

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("no such folder: {0}")]
    NoFolder(FolderId),
    #[error("no such message: {0}")]
    NoMessage(MessageId),
    #[error("could not parse message: {0}")]
    Parse(String),
    #[error("message too large ({0} bytes)")]
    TooLarge(usize),
}

/// Well-known folders every mailbox has, plus user folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderKind {
    Inbox,
    Drafts,
    Sent,
    Deleted,
    Archive,
    Junk,
    Outbox,
    Custom,
}

impl FolderKind {
    pub fn default_name(self) -> &'static str {
        match self {
            FolderKind::Inbox => "Inbox",
            FolderKind::Drafts => "Drafts",
            FolderKind::Sent => "Sent Items",
            FolderKind::Deleted => "Deleted Items",
            FolderKind::Archive => "Archive",
            FolderKind::Junk => "Junk Email",
            FolderKind::Outbox => "Outbox",
            FolderKind::Custom => "Folder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "inbox" => FolderKind::Inbox,
            "drafts" => FolderKind::Drafts,
            "sent" => FolderKind::Sent,
            "deleted" => FolderKind::Deleted,
            "archive" => FolderKind::Archive,
            "junk" => FolderKind::Junk,
            "outbox" => FolderKind::Outbox,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub kind: FolderKind,
    pub parent: Option<FolderId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }

    /// The name if there is one, otherwise the address.
    pub fn display(&self) -> &str {
        if self.name.trim().is_empty() { &self.email } else { &self.name }
    }

    /// `Name <email>` form for headers and the compose fields.
    pub fn full(&self) -> String {
        if self.name.trim().is_empty() { self.email.clone() } else { format!("{} <{}>", self.name, self.email) }
    }

    /// Parses a `;` or `,` separated recipient line (`Ann <ann@x.org>; bob@y.org`).
    pub fn parse_list(s: &str) -> Vec<Address> {
        s.split([';', ','])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match (p.find('<'), p.rfind('>')) {
                (Some(a), Some(b)) if a < b => {
                    let name = p.get(..a).unwrap_or("").trim().trim_matches('"').to_string();
                    let email = p.get(a + 1..b).unwrap_or("").trim().to_string();
                    Address { name, email }
                }
                _ => Address { name: String::new(), email: p.to_string() },
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub folder: FolderId,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: DateTime<Utc>,
    pub body: String,
    pub read: bool,
    pub flagged: bool,
    pub importance: Importance,
    pub attachments: Vec<Attachment>,
}

impl Message {
    /// The start of the body with whitespace collapsed, for the message list preview.
    pub fn preview(&self, max_chars: usize) -> String {
        let mut out = String::new();
        for (n, word) in self.body.split_whitespace().enumerate() {
            if out.chars().count() >= max_chars {
                break;
            }
            if n > 0 {
                out.push(' ');
            }
            out.push_str(word);
        }
        out.chars().take(max_chars).collect()
    }
}

/// A message before it gets an id and a folder (parsed, composed or sample).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessage {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: Option<DateTime<Utc>>,
    pub body: String,
    pub read: bool,
    pub flagged: bool,
    pub importance: Importance,
    pub attachments: Vec<Attachment>,
}

/// One mailbox: folders and messages, in memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub account: Address,
    folders: Vec<Folder>,
    messages: Vec<Message>,
    next_id: u64,
}

impl Store {
    /// A mailbox with the standard folders, in Outlook's folder-pane order.
    pub fn new(account: Address) -> Self {
        let mut s = Store { account, folders: Vec::new(), messages: Vec::new(), next_id: 1 };
        for kind in [
            FolderKind::Inbox,
            FolderKind::Drafts,
            FolderKind::Sent,
            FolderKind::Deleted,
            FolderKind::Archive,
            FolderKind::Junk,
            FolderKind::Outbox,
        ] {
            s.add_folder(kind.default_name(), kind, None);
        }
        s
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub fn add_folder(&mut self, name: &str, kind: FolderKind, parent: Option<FolderId>) -> FolderId {
        let id = self.fresh_id();
        self.folders.push(Folder { id, name: name.to_string(), kind, parent });
        id
    }

    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    pub fn folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn folder_of_kind(&self, kind: FolderKind) -> Option<FolderId> {
        self.folders.iter().find(|f| f.kind == kind).map(|f| f.id)
    }

    pub fn add_message(&mut self, folder: FolderId, m: NewMessage) -> Result<MessageId, MailError> {
        if self.folder(folder).is_none() {
            return Err(MailError::NoFolder(folder));
        }
        let id = self.fresh_id();
        self.messages.push(Message {
            id,
            folder,
            from: m.from,
            to: m.to,
            cc: m.cc,
            subject: m.subject,
            date: m.date.unwrap_or_else(Utc::now),
            body: m.body,
            read: m.read,
            flagged: m.flagged,
            importance: m.importance,
            attachments: m.attachments,
        });
        Ok(id)
    }

    pub fn message(&self, id: MessageId) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    pub fn message_mut(&mut self, id: MessageId) -> Result<&mut Message, MailError> {
        self.messages.iter_mut().find(|m| m.id == id).ok_or(MailError::NoMessage(id))
    }

    /// Messages in a folder, newest first, optionally filtered by a case-insensitive search.
    pub fn messages_in(&self, folder: FolderId, search: &str) -> Vec<&Message> {
        let q = search.trim().to_lowercase();
        let mut v: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.folder == folder)
            .filter(|m| {
                q.is_empty()
                    || m.subject.to_lowercase().contains(&q)
                    || m.body.to_lowercase().contains(&q)
                    || m.from.name.to_lowercase().contains(&q)
                    || m.from.email.to_lowercase().contains(&q)
            })
            .collect();
        v.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
        v
    }

    pub fn unread_count(&self, folder: FolderId) -> usize {
        self.messages.iter().filter(|m| m.folder == folder && !m.read).count()
    }

    pub fn total_count(&self, folder: FolderId) -> usize {
        self.messages.iter().filter(|m| m.folder == folder).count()
    }

    pub fn move_message(&mut self, id: MessageId, folder: FolderId) -> Result<(), MailError> {
        if self.folder(folder).is_none() {
            return Err(MailError::NoFolder(folder));
        }
        self.message_mut(id)?.folder = folder;
        Ok(())
    }

    /// Removes a message for good (e.g. a draft replaced by a newer save).
    pub fn purge_message(&mut self, id: MessageId) -> Result<(), MailError> {
        let before = self.messages.len();
        self.messages.retain(|m| m.id != id);
        if self.messages.len() == before { Err(MailError::NoMessage(id)) } else { Ok(()) }
    }

    /// Delete as Outlook does: to Deleted Items, or permanently when already there.
    pub fn delete_message(&mut self, id: MessageId) -> Result<(), MailError> {
        let deleted = self.folder_of_kind(FolderKind::Deleted);
        let current = self.message(id).map(|m| m.folder).ok_or(MailError::NoMessage(id))?;
        match deleted {
            Some(d) if d != current => self.move_message(id, d),
            _ => {
                self.messages.retain(|m| m.id != id);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(Address::new("Me", "me@example.org"))
    }

    #[test]
    fn standard_folders_exist() {
        let s = store();
        for k in [FolderKind::Inbox, FolderKind::Sent, FolderKind::Deleted, FolderKind::Drafts] {
            assert!(s.folder_of_kind(k).is_some());
        }
    }

    #[test]
    fn delete_moves_then_purges() {
        let mut s = store();
        let inbox = s.folder_of_kind(FolderKind::Inbox).unwrap();
        let id = s.add_message(inbox, NewMessage { subject: "x".into(), ..Default::default() }).unwrap();
        s.delete_message(id).unwrap();
        assert_eq!(s.message(id).unwrap().folder, s.folder_of_kind(FolderKind::Deleted).unwrap());
        s.delete_message(id).unwrap();
        assert!(s.message(id).is_none());
    }

    #[test]
    fn parse_recipient_list() {
        let v = Address::parse_list("Ann Lee <ann@x.org>; bob@y.org, \"C D\" <c@z.org>");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], Address::new("Ann Lee", "ann@x.org"));
        assert_eq!(v[1].email, "bob@y.org");
        assert_eq!(v[2].name, "C D");
    }

    #[test]
    fn hostile_recipient_lists_never_panic() {
        for s in ["<", ">", "><", "a<b", "é<ü>", ";;;", "<>", "\"<\">"] {
            let _ = Address::parse_list(s);
        }
    }
}
