#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! `.msg` import ([MS-OXMSG]): an Outlook item saved as a compound file ([MS-CFB]) through the
//! generic `cfb` container crate, plus the MAPI property mapping shared with `.pst` import.
//!
//! Layout read here (MS-OXMSG 2.2 / 2.4): every object storage holds a
//! `__properties_version1.0` stream of 16-byte entries (tag, flags, 8-byte value) after a header
//! (32 bytes for the message, 8 for recipients and attachments). Fixed-size values sit in the
//! entry; variable-size ones (strings, binary) live in `__substg1.0_<id><type>` streams.
//! Recipients are `__recip_version1.0_#XXXXXXXX` storages, attachments
//! `__attach_version1.0_#XXXXXXXX`.

#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;
pub mod mapi;

use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};

use mapi::{MapiMessage, PropValue, Props, id};
use rslook_mail::NewMessage;

#[derive(Debug, thiserror::Error)]
pub enum MsgError {
    #[error("not an Outlook .msg file: {0}")]
    NotMsg(String),
    #[error("the .msg file is damaged: {0}")]
    Damaged(String),
    #[error("file too large ({0} bytes)")]
    TooLarge(usize),
    #[error("not an email ({0}); calendar items, contacts and tasks can't be imported yet")]
    NotMail(String),
}

/// Larger `.msg` files are refused (hostile input); same cap as a single `.eml`.
pub const MAX_MSG_BYTES: usize = rslook_mail::parse::MAX_MESSAGE_BYTES;
const MAX_RECIPIENTS: usize = 10_000;
const MAX_ATTACHMENTS: usize = 1_000;

/// Property types ([MS-OXCDATA] 2.11.1) the reader understands.
pub(crate) mod ptype {
    pub const INT16: u16 = 0x0002;
    pub const INT32: u16 = 0x0003;
    pub const BOOL: u16 = 0x000B;
    pub const INT64: u16 = 0x0014;
    pub const STRING8: u16 = 0x001E;
    pub const UNICODE: u16 = 0x001F;
    pub const TIME: u16 = 0x0040;
    pub const BINARY: u16 = 0x0102;
}

pub(crate) const PROPS_STREAM: &str = "__properties_version1.0";
pub(crate) const RECIP_PREFIX: &str = "__recip_version1.0_#";
pub(crate) const ATTACH_PREFIX: &str = "__attach_version1.0_#";
pub(crate) const MESSAGE_HEADER: usize = 32;
pub(crate) const CHILD_HEADER: usize = 8;

pub(crate) fn substg(id: u16, ty: u16) -> String {
    format!("__substg1.0_{id:04X}{ty:04X}")
}

type Cfb<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;

/// Parses a `.msg` file into Rslook's mail model. Refuses calendar items, contacts and tasks.
pub fn parse_msg(bytes: &[u8]) -> Result<NewMessage, MsgError> {
    let m = read_msg(bytes)?;
    let class = m.props.str(id::MESSAGE_CLASS).unwrap_or_default();
    if !mapi::is_mail_class(class) {
        return Err(MsgError::NotMail(class.to_string()));
    }
    Ok(mapi::to_new_message(&m))
}

/// Reads the raw properties, recipients and attachments of a `.msg` file.
pub fn read_msg(bytes: &[u8]) -> Result<MapiMessage, MsgError> {
    if bytes.len() > MAX_MSG_BYTES {
        return Err(MsgError::TooLarge(bytes.len()));
    }
    // The container crate is not ours; a panic on a hostile file must not take the app down.
    match catch_unwind(AssertUnwindSafe(|| read_inner(bytes))) {
        Ok(r) => r,
        Err(_) => Err(MsgError::Damaged("unreadable compound file".into())),
    }
}

fn read_inner(bytes: &[u8]) -> Result<MapiMessage, MsgError> {
    let mut cf = cfb::CompoundFile::open(Cursor::new(bytes)).map_err(|e| MsgError::NotMsg(e.to_string()))?;
    if !cf.is_stream(format!("/{PROPS_STREAM}")) {
        return Err(MsgError::NotMsg("no property stream".into()));
    }
    let raw = read_props(&mut cf, "/", MESSAGE_HEADER)?;
    let codepage = raw.props.int(id::MESSAGE_CODEPAGE).or_else(|| raw.props.int(id::INTERNET_CPID));
    let props = raw.finish(codepage);

    let mut children: Vec<String> = cf.read_root_storage().filter(|e| e.is_storage()).map(|e| e.name().to_string()).collect();
    children.sort();
    let mut recipients = Vec::new();
    let mut attachments = Vec::new();
    for name in children {
        if name.starts_with(RECIP_PREFIX) && recipients.len() < MAX_RECIPIENTS {
            recipients.push(read_props(&mut cf, &format!("/{name}/"), CHILD_HEADER)?.finish(codepage));
        } else if name.starts_with(ATTACH_PREFIX) && attachments.len() < MAX_ATTACHMENTS {
            attachments.push(read_props(&mut cf, &format!("/{name}/"), CHILD_HEADER)?.finish(codepage));
        }
    }
    Ok(MapiMessage { props, recipients, attachments })
}

/// Properties of one storage; 8-bit strings wait for the message code page.
struct RawProps {
    props: Props,
    string8: Vec<(u16, Vec<u8>)>,
}

impl RawProps {
    fn finish(mut self, codepage: Option<i64>) -> Props {
        for (pid, b) in self.string8 {
            self.props.insert(pid, PropValue::String(decode_8bit(&b, codepage)));
        }
        self.props
    }
}

fn read_stream(cf: &mut Cfb<'_>, path: &str) -> Result<Vec<u8>, MsgError> {
    let mut s = cf.open_stream(path).map_err(|e| MsgError::Damaged(e.to_string()))?;
    let mut out = Vec::new();
    // A stream can't be bigger than the file; the cap guards against a lying length.
    (&mut s).take(MAX_MSG_BYTES as u64).read_to_end(&mut out).map_err(|e| MsgError::Damaged(e.to_string()))?;
    Ok(out)
}

fn read_props(cf: &mut Cfb<'_>, dir: &str, header: usize) -> Result<RawProps, MsgError> {
    let table = read_stream(cf, &format!("{dir}{PROPS_STREAM}"))?;
    let entries = table.get(header..).unwrap_or_default();
    let mut raw = RawProps { props: Props::default(), string8: Vec::new() };
    let is_attachment = dir.contains(ATTACH_PREFIX);
    for e in entries.as_chunks::<16>().0 {
        let tag = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
        let ty = (tag & 0xFFFF) as u16;
        let pid = (tag >> 16) as u16;
        let v8 = [e[8], e[9], e[10], e[11], e[12], e[13], e[14], e[15]];
        let i32v = i32::from_le_bytes([v8[0], v8[1], v8[2], v8[3]]);
        let i64v = i64::from_le_bytes(v8);
        let v = match ty {
            ptype::INT16 => PropValue::Int32(i32::from(i16::from_le_bytes([v8[0], v8[1]]))),
            ptype::INT32 => PropValue::Int32(i32v),
            ptype::BOOL => PropValue::Bool(v8[0] != 0),
            ptype::INT64 => PropValue::Int64(i64v),
            ptype::TIME => PropValue::Time(i64v),
            ptype::UNICODE | ptype::STRING8 | ptype::BINARY => {
                let path = format!("{dir}{}", substg(pid, ty));
                if is_attachment && pid == id::ATTACH_DATA && ty == ptype::BINARY {
                    // Only the size is kept; don't load the file into memory for it.
                    let len = cf.entry(&path).map(|e| e.len()).unwrap_or(0);
                    raw.props.insert(id::ATTACH_SIZE, PropValue::Int64(i64::try_from(len).unwrap_or(i64::MAX)));
                    continue;
                }
                let Ok(b) = read_stream(cf, &path) else { continue };
                match ty {
                    ptype::UNICODE => PropValue::String(decode_utf16(&b)),
                    ptype::STRING8 => {
                        raw.string8.push((pid, b));
                        continue;
                    }
                    _ => PropValue::Binary(b),
                }
            }
            _ => continue,
        };
        raw.props.insert(pid, v);
    }
    Ok(raw)
}

pub(crate) fn decode_utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
}

/// Decodes an 8-bit string in a Windows code page ([MS-OXCDATA] PtypString8).
pub fn decode_8bit(b: &[u8], codepage: Option<i64>) -> String {
    let b = b.split(|&c| c == 0).next().unwrap_or_default();
    let name = match codepage {
        Some(65001) => return String::from_utf8_lossy(b).into_owned(),
        Some(20127) => "us-ascii".to_string(),
        Some(28591) => "iso-8859-1".to_string(),
        Some(932) => "shift_jis".to_string(),
        Some(936) => "gb2312".to_string(),
        Some(949) => "euc-kr".to_string(),
        Some(950) => "big5".to_string(),
        Some(cp @ (874 | 1250..=1258)) => format!("windows-{cp}"),
        _ => "windows-1252".to_string(),
    };
    match mail_parser::decoders::charsets::map::charset_decoder(name.as_bytes()) {
        Some(decode) => decode(b),
        None => String::from_utf8_lossy(b).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::write_msg;
    use rslook_mail::{Address, Importance};

    fn s(v: &str) -> PropValue {
        PropValue::String(v.into())
    }

    fn sample() -> MapiMessage {
        let mut m = MapiMessage::default();
        m.props.insert(id::MESSAGE_CLASS, s("IPM.Note"));
        m.props.insert(id::SUBJECT, s("Café plans"));
        m.props.insert(id::SENDER_NAME, s("Rosa Park"));
        m.props.insert(id::SENDER_SMTP_ADDRESS, s("rosa@example.org"));
        m.props.insert(id::BODY, s("See you at noon.\r\n"));
        m.props.insert(id::IMPORTANCE, PropValue::Int32(0));
        m.props.insert(id::MESSAGE_FLAGS, PropValue::Int32(1));
        m.props
            .insert(id::MESSAGE_DELIVERY_TIME, PropValue::Time(mapi::utc_to_filetime(chrono::DateTime::from_timestamp(1_792_000_000, 0).unwrap())));
        for (ty, name, email) in [(1, "Me", "me@example.org"), (2, "Team", "team@example.org")] {
            let mut r = Props::default();
            r.insert(id::RECIPIENT_TYPE, PropValue::Int32(ty));
            r.insert(id::DISPLAY_NAME, s(name));
            r.insert(id::SMTP_ADDRESS, s(email));
            m.recipients.push(r);
        }
        let mut a = Props::default();
        a.insert(id::ATTACH_LONG_FILENAME, s("menu.txt"));
        a.insert(id::ATTACH_DATA, PropValue::Binary(b"soup".to_vec()));
        m.attachments.push(a);
        m
    }

    #[test]
    fn round_trips_a_synthetic_msg() {
        let bytes = write_msg(&sample(), false).unwrap();
        let n = parse_msg(&bytes).unwrap();
        assert_eq!(n.subject, "Café plans");
        assert_eq!(n.from, Address::new("Rosa Park", "rosa@example.org"));
        assert_eq!(n.to, vec![Address::new("Me", "me@example.org")]);
        assert_eq!(n.cc, vec![Address::new("Team", "team@example.org")]);
        assert_eq!(n.body, "See you at noon.\r\n");
        assert_eq!(n.importance, Importance::Low);
        assert!(n.read);
        assert_eq!(n.date.unwrap().timestamp(), 1_792_000_000);
        assert_eq!(n.attachments.len(), 1);
        assert_eq!(n.attachments[0].name, "menu.txt");
        assert_eq!(n.attachments[0].size, 4);
    }

    #[test]
    fn reads_8bit_strings_in_the_message_code_page() {
        let mut m = sample();
        m.props.insert(id::MESSAGE_CODEPAGE, PropValue::Int32(1252));
        let bytes = write_msg(&m, true).unwrap();
        let n = parse_msg(&bytes).unwrap();
        assert_eq!(n.subject, "Café plans");
        assert_eq!(n.cc[0].email, "team@example.org");
    }

    #[test]
    fn refuses_non_mail_items() {
        let mut m = sample();
        m.props.insert(id::MESSAGE_CLASS, s("IPM.Contact"));
        let bytes = write_msg(&m, false).unwrap();
        assert!(matches!(parse_msg(&bytes), Err(MsgError::NotMail(c)) if c == "IPM.Contact"));
    }

    #[test]
    fn junk_and_damaged_files_never_panic() {
        for b in [&b""[..], b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1", b"From: x\r\n\r\nhi", &[0u8; 4096]] {
            assert!(parse_msg(b).is_err());
        }
        let good = write_msg(&sample(), false).unwrap();
        // Truncations and byte flips all over the file: errors or a message, never a panic.
        for cut in (0..good.len()).step_by(97) {
            let _ = parse_msg(&good[..cut]);
        }
        for pos in (0..good.len()).step_by(13) {
            let mut bad = good.clone();
            bad[pos] ^= 0xA5;
            let _ = parse_msg(&bad);
        }
    }

    #[test]
    fn oversized_input_is_refused_before_parsing() {
        let big = vec![0u8; MAX_MSG_BYTES + 1];
        assert!(matches!(read_msg(&big), Err(MsgError::TooLarge(_))));
    }
}
