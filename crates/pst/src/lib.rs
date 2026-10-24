#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Read-only `.pst` import ([MS-PST]) on top of Microsoft's MIT-licensed `outlook-pst` crate.
//!
//! [`read_pst`] walks the IPM subtree (the folders a user sees) and turns every mail item into a
//! [`NewMessage`] through the MAPI mapping in `rslook-msg`, so `.pst` and `.msg` import agree.
//! Calendar, contact, task and note folders and items are skipped and counted.

#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;
#[cfg(any(test, feature = "fixtures"))]
pub mod sample;

use std::collections::HashSet;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::rc::Rc;

use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{AnsiAttachment, Attachment, AttachmentData, UnicodeAttachment};
use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::message::{AnsiMessage, Message, UnicodeMessage};
use outlook_pst::messaging::store::{AnsiStore, EntryId, Store, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::{AnsiPstFile, UnicodePstFile};
use rslook_mail::NewMessage;
use rslook_msg::mapi::{self, MapiMessage, PropValue, Props, id};

#[derive(Debug, thiserror::Error)]
pub enum PstError {
    #[error("not an Outlook data file (.pst), or it is damaged: {0}")]
    Open(String),
    #[error("the .pst file is damaged: {0}")]
    Damaged(String),
    #[error("the .pst file holds more than {0} items; Rslook can't import that many at once yet")]
    TooMany(usize),
    #[error("the .pst file holds more than {} MB of message text; Rslook can't import that much at once yet", .0 / (1024 * 1024))]
    TooLarge(usize),
}

/// One imported folder and its mail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PstFolder {
    pub name: String,
    pub messages: Vec<NewMessage>,
    pub children: Vec<PstFolder>,
}

impl PstFolder {
    /// Messages in this folder and every folder below it.
    pub fn message_count(&self) -> usize {
        self.children.iter().fold(self.messages.len(), |n, c| n.saturating_add(c.message_count()))
    }
}

/// What [`read_pst`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PstImport {
    /// The IPM subtree: its own messages (rare) and the top-level folders as children.
    pub root: PstFolder,
    /// The data file's own display name.
    pub store_name: String,
    /// Calendar items, contacts, tasks and notes left out.
    pub skipped: usize,
    /// Items or folders that could not be read.
    pub failed: usize,
}

/// The whole mailbox lives in memory, so very large files are refused rather than half-loaded.
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_TEXT_BYTES: usize = 2 << 30;
/// Deeper folder trees are treated as damage (a hostile file could nest forever).
const MAX_DEPTH: usize = 64;

/// Reads a `.pst` (Unicode or ANSI) from disk.
pub fn read_pst(path: &Path) -> Result<PstImport, PstError> {
    // `outlook-pst` is not ours; a panic on a hostile file must not take the app down.
    match catch_unwind(AssertUnwindSafe(|| read_inner(path))) {
        Ok(r) => r,
        Err(_) => Err(PstError::Damaged("unreadable structure".into())),
    }
}

fn read_inner(path: &Path) -> Result<PstImport, PstError> {
    match UnicodePstFile::open(path) {
        Ok(pst) => {
            let store = UnicodeStore::read(Rc::new(pst)).map_err(|e| PstError::Open(e.to_string()))?;
            Walker::new(Unicode(store)).run()
        }
        Err(unicode_err) => match AnsiPstFile::open(path) {
            Ok(pst) => {
                let store = AnsiStore::read(Rc::new(pst)).map_err(|e| PstError::Open(e.to_string()))?;
                Walker::new(Ansi(store)).run()
            }
            Err(_) => Err(PstError::Open(unicode_err.to_string())),
        },
    }
}

/// The parts of the reader that differ between Unicode and ANSI files: opening a message with
/// its concrete type, which is what reading its attachments needs.
trait Backend {
    type Msg: Message + 'static;
    fn store(&self) -> &dyn Store;
    fn message(&self, entry: &EntryId) -> io::Result<Rc<Self::Msg>>;
    fn attachment(&self, m: &Rc<Self::Msg>, node: NodeId) -> io::Result<Rc<dyn Attachment>>;
}

struct Unicode(Rc<UnicodeStore>);
struct Ansi(Rc<AnsiStore>);

impl Backend for Unicode {
    type Msg = UnicodeMessage;
    fn store(&self) -> &dyn Store {
        self.0.as_ref()
    }
    fn message(&self, entry: &EntryId) -> io::Result<Rc<UnicodeMessage>> {
        UnicodeMessage::read(self.0.clone(), entry, None)
    }
    fn attachment(&self, m: &Rc<UnicodeMessage>, node: NodeId) -> io::Result<Rc<dyn Attachment>> {
        Ok(UnicodeAttachment::read(m.clone(), node, None)?)
    }
}

impl Backend for Ansi {
    type Msg = AnsiMessage;
    fn store(&self) -> &dyn Store {
        self.0.as_ref()
    }
    fn message(&self, entry: &EntryId) -> io::Result<Rc<AnsiMessage>> {
        AnsiMessage::read(self.0.clone(), entry, None)
    }
    fn attachment(&self, m: &Rc<AnsiMessage>, node: NodeId) -> io::Result<Rc<dyn Attachment>> {
        Ok(AnsiAttachment::read(m.clone(), node, None)?)
    }
}

/// Property values before 8-bit strings are decoded with the item's code page.
#[derive(Default)]
struct RawProps {
    props: Props,
    string8: Vec<(u16, Vec<u8>)>,
}

impl RawProps {
    fn add(&mut self, pid: u16, v: &PropertyValue) {
        let v = match v {
            PropertyValue::Integer16(n) => PropValue::Int32(i32::from(*n)),
            PropertyValue::Integer32(n) => PropValue::Int32(*n),
            PropertyValue::Integer64(n) => PropValue::Int64(*n),
            PropertyValue::Boolean(b) => PropValue::Bool(*b),
            PropertyValue::Time(t) => PropValue::Time(*t),
            PropertyValue::Unicode(s) => PropValue::String(String::from_utf16_lossy(s.buffer()).trim_end_matches('\0').to_string()),
            PropertyValue::String8(s) => {
                self.string8.push((pid, s.buffer().to_vec()));
                return;
            }
            PropertyValue::Binary(b) => PropValue::Binary(b.buffer().to_vec()),
            _ => return,
        };
        self.props.insert(pid, v);
    }

    /// Decodes the 8-bit strings; the code page comes from `codepage` or, failing that, from
    /// the properties themselves.
    fn finish(mut self, codepage: Option<i64>) -> Props {
        let cp = codepage.or_else(|| page_of(&self.props));
        for (pid, b) in self.string8 {
            self.props.insert(pid, PropValue::String(rslook_msg::decode_8bit(&b, cp)));
        }
        self.props
    }
}

fn page_of(p: &Props) -> Option<i64> {
    p.int(id::MESSAGE_CODEPAGE).or_else(|| p.int(id::INTERNET_CPID))
}

/// Every row of a table as properties, keyed by the row's node id.
fn table_rows(table: &dyn TableContext, cp: Option<i64>) -> io::Result<Vec<(NodeId, Props)>> {
    let ctx = table.context();
    let mut out = Vec::new();
    for row in table.rows_matrix() {
        let mut raw = RawProps::default();
        for (col, value) in ctx.columns().iter().zip(row.columns(ctx)?) {
            if let Some(value) = value
                && let Ok(v) = table.read_column(&value, col.prop_type())
            {
                raw.add(col.prop_id(), &v);
            }
        }
        out.push((NodeId::from(u32::from(row.id())), raw.finish(cp)));
    }
    Ok(out)
}

struct Walker<B: Backend> {
    b: B,
    visited: HashSet<u32>,
    items: usize,
    text_bytes: usize,
    skipped: usize,
    failed: usize,
}

impl<B: Backend> Walker<B> {
    fn new(b: B) -> Self {
        Walker { b, visited: HashSet::new(), items: 0, text_bytes: 0, skipped: 0, failed: 0 }
    }

    fn fail(&mut self) {
        self.failed = self.failed.saturating_add(1);
    }

    /// Starts at the IPM subtree. (`Store::root_hierarchy_table` is avoided on purpose: in
    /// outlook-pst 1.2.0 it locks the file twice and hangs.)
    fn run(mut self) -> Result<PstImport, PstError> {
        let props = self.b.store().properties();
        let store_name = props.display_name().unwrap_or_default();
        let ipm = props.ipm_sub_tree_entry_id().map_err(|e| PstError::Open(e.to_string()))?;
        let folder = self.b.store().open_folder(&ipm).map_err(|e| PstError::Open(e.to_string()))?;
        self.visited.insert(u32::from(ipm.node_id()));
        let root = self.folder(folder.as_ref(), 0)?;
        Ok(PstImport { root, store_name, skipped: self.skipped, failed: self.failed })
    }

    fn folder(&mut self, folder: &dyn Folder, depth: usize) -> Result<PstFolder, PstError> {
        let mut out = PstFolder { name: folder.properties().display_name().unwrap_or_default(), ..Default::default() };
        // Every folder has both tables ([MS-PST] 2.4.4); `outlook-pst` returns `None` when one
        // can't be read, so a missing table counts as damage.
        match folder.contents_table().map(|t| table_rows(t.as_ref(), None)) {
            Some(Ok(rows)) => {
                for (node, _) in rows {
                    self.message(node, &mut out)?;
                }
            }
            _ => self.fail(),
        }
        let Some(Ok(rows)) = folder.hierarchy_table().map(|t| table_rows(t.as_ref(), None)) else {
            self.fail();
            return Ok(out);
        };
        for (node, row) in rows {
            // A folder listed twice (or inside itself) is damage, not a reason to loop.
            if depth >= MAX_DEPTH || !self.visited.insert(u32::from(node)) {
                self.fail();
                continue;
            }
            let Ok(child) = self.b.store().properties().make_entry_id(node).and_then(|e| self.b.store().open_folder(&e)) else {
                self.fail();
                continue;
            };
            let class = match child.properties().get(id::CONTAINER_CLASS) {
                Some(PropertyValue::Unicode(s)) => Some(String::from_utf16_lossy(s.buffer())),
                Some(PropertyValue::String8(s)) => Some(String::from_utf8_lossy(s.buffer()).into_owned()),
                _ => row.str(id::CONTAINER_CLASS).map(str::to_string),
            };
            if !mapi::is_mail_container(class.as_deref().map(|c| c.trim_end_matches('\0'))) {
                self.skipped = self.skipped.saturating_add(child.contents_table().map(|t| t.rows_matrix().count()).unwrap_or(0));
                continue;
            }
            out.children.push(self.folder(child.as_ref(), depth.saturating_add(1))?);
        }
        Ok(out)
    }

    fn message(&mut self, node: NodeId, out: &mut PstFolder) -> Result<(), PstError> {
        let Ok(m) = self.b.store().properties().make_entry_id(node).and_then(|e| self.b.message(&e)) else {
            self.fail();
            return Ok(());
        };
        let mut raw = RawProps::default();
        for (pid, v) in m.properties().iter() {
            raw.add(*pid, v);
        }
        let props = raw.finish(None);
        if !mapi::is_mail_class(props.str(id::MESSAGE_CLASS).unwrap_or_default()) {
            self.skipped = self.skipped.saturating_add(1);
            return Ok(());
        }
        self.items = self.items.saturating_add(1);
        if self.items > MAX_ITEMS {
            return Err(PstError::TooMany(MAX_ITEMS));
        }
        let cp = page_of(&props);
        let recipients = m.recipient_table().and_then(|t| table_rows(t.as_ref(), cp).ok()).unwrap_or_default();
        let rows = m.attachment_table().and_then(|t| table_rows(t.as_ref(), cp).ok()).unwrap_or_default();
        let attachments = rows.into_iter().map(|(node, row)| self.attachment(&m, node, row, cp)).collect();
        let recipients = recipients.into_iter().map(|(_, p)| p).collect();
        let msg = mapi::to_new_message(&MapiMessage { props, recipients, attachments });
        self.text_bytes = self.text_bytes.saturating_add(msg.body.len()).saturating_add(msg.subject.len());
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err(PstError::TooLarge(MAX_TEXT_BYTES));
        }
        out.messages.push(msg);
        Ok(())
    }

    /// The attachment's own properties (long file name, hidden flag, real size), falling back to
    /// its row in the attachment table when the attachment object can't be read.
    fn attachment(&self, m: &Rc<B::Msg>, node: NodeId, row: Props, cp: Option<i64>) -> Props {
        let Ok(a) = self.b.attachment(m, node) else { return row };
        let mut raw = RawProps::default();
        for (pid, v) in a.properties().iter() {
            if *pid != id::ATTACH_DATA {
                raw.add(*pid, v);
            }
        }
        let mut p = raw.finish(cp);
        if let Some(AttachmentData::Binary(b)) = a.data() {
            p.insert(id::ATTACH_SIZE, PropValue::Int64(i64::try_from(b.buffer().len()).unwrap_or(i64::MAX)));
        }
        for (k, v) in row.0 {
            p.0.entry(k).or_insert(v);
        }
        p
    }
}

#[cfg(test)]
mod tests;
