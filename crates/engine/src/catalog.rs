//! Outlook classic feature catalog: the ribbon's features (by their public names) mapped to the
//! command ids that implement them. Parity is the share of entries with a registered command.
//!
//! Feature names and grouping come from black-box observation of the Outlook classic ribbon
//! (Microsoft 365, Windows). Only names are recorded here, never Microsoft text or images.

pub struct Feature {
    pub tab: &'static str,
    pub group: &'static str,
    pub name: &'static str,
    pub command: Option<&'static str>,
}

const fn f(tab: &'static str, group: &'static str, name: &'static str, command: Option<&'static str>) -> Feature {
    Feature { tab, group, name, command }
}

pub const FEATURES: &[Feature] = &[
    // Home
    f("Home", "New", "New Email", Some("mail.new")),
    f("Home", "New", "New Items", None),
    f("Home", "Delete", "Ignore", None),
    f("Home", "Delete", "Clean Up", None),
    f("Home", "Delete", "Junk", Some("mail.junk")),
    f("Home", "Delete", "Delete", Some("mail.delete")),
    f("Home", "Delete", "Archive", Some("mail.archive")),
    f("Home", "Respond", "Reply", Some("mail.reply")),
    f("Home", "Respond", "Reply All", Some("mail.reply_all")),
    f("Home", "Respond", "Forward", Some("mail.forward")),
    f("Home", "Respond", "Meeting", None),
    f("Home", "Quick Steps", "Quick Steps", None),
    f("Home", "Move", "Move", Some("mail.move")),
    f("Home", "Move", "Rules", None),
    f("Home", "Tags", "Unread/Read", Some("mail.mark_unread")),
    f("Home", "Tags", "Categorize", None),
    f("Home", "Tags", "Follow Up", Some("mail.toggle_flag")),
    f("Home", "Find", "Search People", None),
    f("Home", "Find", "Address Book", None),
    f("Home", "Find", "Filter Email", None),
    f("Home", "Find", "Search", Some("search.set")),
    // Send / Receive
    f("Send / Receive", "Send & Receive", "Send/Receive All Folders", None),
    f("Send / Receive", "Send & Receive", "Update Folder", None),
    f("Send / Receive", "Send & Receive", "Send All", None),
    f("Send / Receive", "Server", "Download Headers", None),
    f("Send / Receive", "Preferences", "Work Offline", None),
    // Folder
    f("Folder", "New", "New Folder", None),
    f("Folder", "Actions", "Rename Folder", None),
    f("Folder", "Actions", "Delete Folder", None),
    f("Folder", "Actions", "Move Folder", None),
    f("Folder", "Clean Up", "Mark All as Read", None),
    f("Folder", "Clean Up", "Empty Folder", None),
    f("Folder", "Favorites", "Show in Favorites", None),
    f("Folder", "Properties", "Folder Properties", None),
    // View
    f("View", "Current View", "Change View", None),
    f("View", "Messages", "Show as Conversations", None),
    f("View", "Arrangement", "Arrange By", None),
    f("View", "Arrangement", "Reverse Sort", None),
    f("View", "Layout", "Folder Pane", Some("view.folder_pane")),
    f("View", "Layout", "Reading Pane", Some("view.reading_pane")),
    f("View", "Layout", "To-Do Bar", None),
    f("View", "Window", "Reminders Window", None),
    // File (Backstage)
    f("File", "Info", "Add Account", None),
    f("File", "Open & Export", "Import/Export", Some("mail.import_eml")),
    f("File", "Open & Export", "Open Outlook Data File", None),
    f("File", "Print", "Print", None),
    f("File", "Options", "Options", None),
    // Message window
    f("Message", "Basic Text", "Format Text", None),
    f("Message", "Include", "Attach File", None),
    f("Message", "Send", "Send", Some("compose.send")),
    f("Message", "Save", "Save", Some("compose.save_draft")),
    // Navigation bar
    f("Navigation", "Modules", "Mail", Some("module.mail")),
    f("Navigation", "Modules", "Calendar", Some("module.calendar")),
    f("Navigation", "Modules", "People", Some("module.people")),
    f("Navigation", "Modules", "Tasks", Some("module.tasks")),
];

/// (implemented, total).
pub fn parity() -> (usize, usize) {
    let done = FEATURES.iter().filter(|f| f.command.is_some_and(|c| crate::find(c).is_some())).count();
    (done, FEATURES.len())
}
