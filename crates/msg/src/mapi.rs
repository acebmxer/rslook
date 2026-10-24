//! MAPI property sets ([MS-OXPROPS]) and how a message's properties become a [`NewMessage`].
//!
//! Shared by the `.msg` reader in this crate and the `.pst` reader in `rslook-pst`, so both
//! formats map subject, sender, recipients, dates and flags the same way.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rslook_mail::{Address, Attachment, Importance, NewMessage};

/// Property ids ([MS-OXPROPS]) the import reads.
pub mod id {
    pub const IMPORTANCE: u16 = 0x0017;
    pub const MESSAGE_CLASS: u16 = 0x001A;
    pub const SUBJECT: u16 = 0x0037;
    pub const CLIENT_SUBMIT_TIME: u16 = 0x0039;
    pub const SENT_REPRESENTING_NAME: u16 = 0x0042;
    pub const SENT_REPRESENTING_EMAIL_ADDRESS: u16 = 0x0065;
    pub const SENDER_NAME: u16 = 0x0C1A;
    pub const RECIPIENT_TYPE: u16 = 0x0C15;
    pub const SENDER_EMAIL_ADDRESS: u16 = 0x0C1F;
    pub const DISPLAY_TO: u16 = 0x0E04;
    pub const DISPLAY_CC: u16 = 0x0E03;
    pub const MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
    pub const MESSAGE_FLAGS: u16 = 0x0E07;
    pub const ATTACH_SIZE: u16 = 0x0E20;
    pub const BODY: u16 = 0x1000;
    pub const HTML: u16 = 0x1013;
    pub const FLAG_STATUS: u16 = 0x1090;
    pub const DISPLAY_NAME: u16 = 0x3001;
    pub const EMAIL_ADDRESS: u16 = 0x3003;
    pub const CREATION_TIME: u16 = 0x3007;
    pub const ATTACH_DATA: u16 = 0x3701;
    pub const ATTACH_FILENAME: u16 = 0x3704;
    pub const ATTACH_LONG_FILENAME: u16 = 0x3707;
    pub const SMTP_ADDRESS: u16 = 0x39FE;
    pub const INTERNET_CPID: u16 = 0x3FDE;
    pub const MESSAGE_CODEPAGE: u16 = 0x3FFD;
    pub const SENDER_SMTP_ADDRESS: u16 = 0x5D01;
    pub const SENT_REPRESENTING_SMTP_ADDRESS: u16 = 0x5D02;
    pub const ATTACHMENT_HIDDEN: u16 = 0x7FFE;
    pub const CONTAINER_CLASS: u16 = 0x3613;
}

/// `PidTagMessageFlags` bit: the message has been read.
const MSGFLAG_READ: i32 = 0x1;
/// `PidTagFlagStatus`: flagged for follow-up.
const FOLLOWUP_FLAGGED: i32 = 2;

/// One property value, reduced to what the import needs.
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Int32(i32),
    Int64(i64),
    Bool(bool),
    /// FILETIME: 100-nanosecond intervals since 1601-01-01 UTC.
    Time(i64),
    String(String),
    Binary(Vec<u8>),
}

/// The properties of one object (message, recipient row or attachment).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Props(pub HashMap<u16, PropValue>);

impl Props {
    pub fn insert(&mut self, id: u16, v: PropValue) {
        self.0.insert(id, v);
    }

    pub fn get(&self, id: u16) -> Option<&PropValue> {
        self.0.get(&id)
    }

    /// A non-empty string property.
    pub fn str(&self, id: u16) -> Option<&str> {
        match self.0.get(&id) {
            Some(PropValue::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn int(&self, id: u16) -> Option<i64> {
        match self.0.get(&id) {
            Some(PropValue::Int32(v)) => Some(i64::from(*v)),
            Some(PropValue::Int64(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn bool(&self, id: u16) -> Option<bool> {
        match self.0.get(&id) {
            Some(PropValue::Bool(b)) => Some(*b),
            Some(PropValue::Int32(v)) => Some(*v != 0),
            _ => None,
        }
    }

    pub fn time(&self, id: u16) -> Option<DateTime<Utc>> {
        match self.0.get(&id) {
            Some(PropValue::Time(ft)) => filetime_to_utc(*ft),
            _ => None,
        }
    }

    pub fn binary(&self, id: u16) -> Option<&[u8]> {
        match self.0.get(&id) {
            Some(PropValue::Binary(b)) => Some(b.as_slice()),
            _ => None,
        }
    }
}

/// A message read from a `.msg` or `.pst`: its properties, recipient rows and attachments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapiMessage {
    pub props: Props,
    pub recipients: Vec<Props>,
    pub attachments: Vec<Props>,
}

/// Seconds between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_DIFF: i64 = 11_644_473_600;

pub fn filetime_to_utc(ft: i64) -> Option<DateTime<Utc>> {
    if ft <= 0 {
        return None;
    }
    let secs = (ft / 10_000_000).checked_sub(FILETIME_UNIX_DIFF)?;
    let nanos = u32::try_from((ft % 10_000_000).checked_mul(100)?).ok()?;
    DateTime::from_timestamp(secs, nanos)
}

/// The inverse of [`filetime_to_utc`], for fixtures and tests.
pub fn utc_to_filetime(t: DateTime<Utc>) -> i64 {
    t.timestamp().saturating_add(FILETIME_UNIX_DIFF).saturating_mul(10_000_000).saturating_add(i64::from(t.timestamp_subsec_nanos() / 100))
}

/// Item classes that are not mail (calendar, contacts, tasks, notes, journal). Rslook has no
/// home for them yet, so the import skips them.
pub fn is_mail_class(class: &str) -> bool {
    let mut parts = class.split('.');
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default().to_ascii_lowercase();
    !(first.eq_ignore_ascii_case("IPM") && ["appointment", "contact", "distlist", "task", "stickynote", "activity"].contains(&second.as_str()))
}

/// Folder container classes that hold mail (none set counts as mail, as in older files).
pub fn is_mail_container(class: Option<&str>) -> bool {
    match class {
        None => true,
        Some(c) => {
            let c = c.trim().to_ascii_lowercase();
            c.is_empty() || c == "ipf" || c.starts_with("ipf.note") || c.starts_with("ipf.imap")
        }
    }
}

fn address(name: Option<&str>, email: Option<&str>) -> Address {
    Address::new(name.unwrap_or_default().trim(), email.unwrap_or_default().trim())
}

/// An address that is an SMTP address (not an Exchange distinguished name).
fn smtp(p: &Props, id: u16) -> Option<&str> {
    p.str(id).filter(|s| s.contains('@'))
}

fn sender(p: &Props) -> Address {
    let name = p.str(id::SENT_REPRESENTING_NAME).or_else(|| p.str(id::SENDER_NAME));
    let email = smtp(p, id::SENT_REPRESENTING_SMTP_ADDRESS)
        .or_else(|| smtp(p, id::SENDER_SMTP_ADDRESS))
        .or_else(|| smtp(p, id::SENT_REPRESENTING_EMAIL_ADDRESS))
        .or_else(|| smtp(p, id::SENDER_EMAIL_ADDRESS))
        .or_else(|| p.str(id::SENDER_EMAIL_ADDRESS));
    address(name, email)
}

fn body(p: &Props) -> String {
    if let Some(b) = p.str(id::BODY) {
        return b.to_string();
    }
    let html = match p.get(id::HTML) {
        Some(PropValue::Binary(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(PropValue::String(s)) => s.clone(),
        _ => return String::new(),
    };
    mail_parser::decoders::html::html_to_text(&html)
}

fn attachment(p: &Props) -> Option<Attachment> {
    if p.bool(id::ATTACHMENT_HIDDEN) == Some(true) {
        return None;
    }
    let name = p.str(id::ATTACH_LONG_FILENAME).or_else(|| p.str(id::ATTACH_FILENAME)).or_else(|| p.str(id::DISPLAY_NAME)).unwrap_or("attachment");
    let size = match p.binary(id::ATTACH_DATA) {
        Some(d) => d.len(),
        None => p.int(id::ATTACH_SIZE).and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
    };
    Some(Attachment { name: name.to_string(), size })
}

/// Maps a message's properties onto Rslook's mail model.
pub fn to_new_message(m: &MapiMessage) -> NewMessage {
    let p = &m.props;
    let mut to = Vec::new();
    let mut cc = Vec::new();
    for r in &m.recipients {
        let a = address(r.str(id::DISPLAY_NAME), smtp(r, id::SMTP_ADDRESS).or_else(|| r.str(id::EMAIL_ADDRESS)));
        if a.name.is_empty() && a.email.is_empty() {
            continue;
        }
        match r.int(id::RECIPIENT_TYPE) {
            Some(2) => cc.push(a),
            Some(3) => {} // Bcc: Rslook's model has no Bcc line yet.
            _ => to.push(a),
        }
    }
    // Files without a recipient table still carry the display lines.
    if m.recipients.is_empty() {
        to = p.str(id::DISPLAY_TO).map(Address::parse_list).unwrap_or_default();
        cc = p.str(id::DISPLAY_CC).map(Address::parse_list).unwrap_or_default();
    }
    let importance = match p.int(id::IMPORTANCE) {
        Some(0) => Importance::Low,
        Some(2) => Importance::High,
        _ => Importance::Normal,
    };
    NewMessage {
        from: sender(p),
        to,
        cc,
        subject: p.str(id::SUBJECT).unwrap_or_default().to_string(),
        date: p.time(id::MESSAGE_DELIVERY_TIME).or_else(|| p.time(id::CLIENT_SUBMIT_TIME)).or_else(|| p.time(id::CREATION_TIME)),
        body: body(p),
        read: p.int(id::MESSAGE_FLAGS).is_some_and(|f| f & i64::from(MSGFLAG_READ) != 0),
        flagged: p.int(id::FLAG_STATUS) == Some(i64::from(FOLLOWUP_FLAGGED)),
        importance,
        attachments: m.attachments.iter().filter_map(attachment).collect(),
        remote: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> PropValue {
        PropValue::String(v.into())
    }

    #[test]
    fn maps_sender_recipients_flags_and_dates() {
        let mut m = MapiMessage::default();
        m.props.insert(id::SUBJECT, s("Quarterly numbers"));
        m.props.insert(id::SENDER_NAME, s("Rosa Park"));
        m.props.insert(id::SENDER_EMAIL_ADDRESS, s("/o=Org/ou=Exchange/cn=Recipients/cn=rosa"));
        m.props.insert(id::SENDER_SMTP_ADDRESS, s("rosa@example.org"));
        m.props.insert(id::BODY, s("See attached."));
        m.props.insert(id::MESSAGE_FLAGS, PropValue::Int32(1));
        m.props.insert(id::FLAG_STATUS, PropValue::Int32(2));
        m.props.insert(id::IMPORTANCE, PropValue::Int32(2));
        let t = DateTime::from_timestamp(1_792_000_000, 0).unwrap();
        m.props.insert(id::MESSAGE_DELIVERY_TIME, PropValue::Time(utc_to_filetime(t)));
        let mut to = Props::default();
        to.insert(id::RECIPIENT_TYPE, PropValue::Int32(1));
        to.insert(id::DISPLAY_NAME, s("Me"));
        to.insert(id::EMAIL_ADDRESS, s("me@example.org"));
        let mut cc = Props::default();
        cc.insert(id::RECIPIENT_TYPE, PropValue::Int32(2));
        cc.insert(id::DISPLAY_NAME, s("Team"));
        cc.insert(id::SMTP_ADDRESS, s("team@example.org"));
        let mut bcc = Props::default();
        bcc.insert(id::RECIPIENT_TYPE, PropValue::Int32(3));
        bcc.insert(id::EMAIL_ADDRESS, s("hidden@example.org"));
        m.recipients = vec![to, cc, bcc];
        let mut a = Props::default();
        a.insert(id::ATTACH_LONG_FILENAME, s("numbers.csv"));
        a.insert(id::ATTACH_DATA, PropValue::Binary(b"a,b\n1,2\n".to_vec()));
        let mut hidden = Props::default();
        hidden.insert(id::ATTACH_FILENAME, s("logo.png"));
        hidden.insert(id::ATTACHMENT_HIDDEN, PropValue::Bool(true));
        m.attachments = vec![a, hidden];

        let n = to_new_message(&m);
        assert_eq!(n.subject, "Quarterly numbers");
        assert_eq!(n.from, Address::new("Rosa Park", "rosa@example.org"));
        assert_eq!(n.to, vec![Address::new("Me", "me@example.org")]);
        assert_eq!(n.cc, vec![Address::new("Team", "team@example.org")]);
        assert_eq!(n.body, "See attached.");
        assert!(n.read && n.flagged);
        assert_eq!(n.importance, Importance::High);
        assert_eq!(n.date, Some(t));
        assert_eq!(n.attachments, vec![Attachment { name: "numbers.csv".into(), size: 8 }]);
    }

    #[test]
    fn html_only_body_becomes_text_and_display_lines_fill_recipients() {
        let mut m = MapiMessage::default();
        m.props.insert(id::HTML, PropValue::Binary(b"<html><body><p>Hello <b>there</b></p></body></html>".to_vec()));
        m.props.insert(id::DISPLAY_TO, s("Ann Lee; Bob"));
        let n = to_new_message(&m);
        assert!(n.body.contains("Hello there"), "{:?}", n.body);
        assert_eq!(n.to.len(), 2);
        assert!(!n.read);
        assert_eq!(n.date, None);
    }

    #[test]
    fn classes() {
        for c in ["IPM.Note", "", "IPM.Note.SMIME", "REPORT.IPM.Note.NDR", "IPM.Schedule.Meeting.Request", "IPM.Post"] {
            assert!(is_mail_class(c), "{c}");
        }
        for c in ["IPM.Appointment", "ipm.contact", "IPM.DistList", "IPM.Task", "IPM.StickyNote", "IPM.Activity"] {
            assert!(!is_mail_class(c), "{c}");
        }
        assert!(is_mail_container(None));
        assert!(is_mail_container(Some("IPF.Note")));
        assert!(!is_mail_container(Some("IPF.Appointment")));
        assert!(!is_mail_container(Some("IPF.Contact")));
    }

    #[test]
    fn filetimes() {
        assert_eq!(filetime_to_utc(0), None);
        assert_eq!(filetime_to_utc(-5), None);
        assert_eq!(filetime_to_utc(116_444_736_000_000_000), DateTime::from_timestamp(0, 0));
        let _ = filetime_to_utc(i64::MAX);
        let _ = utc_to_filetime(DateTime::<Utc>::MAX_UTC);
    }
}
