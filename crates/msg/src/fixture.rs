//! Synthetic `.msg` writer for tests and examples, written from [MS-OXMSG] (never used by the app).

use std::io::{Cursor, Write};

use crate::mapi::{MapiMessage, PropValue, Props};
use crate::{ATTACH_PREFIX, CHILD_HEADER, MESSAGE_HEADER, MsgError, PROPS_STREAM, RECIP_PREFIX, ptype, substg};

/// PROPATTR_READABLE | PROPATTR_WRITABLE.
const FLAGS: u32 = 0x2 | 0x4;

fn io(e: std::io::Error) -> MsgError {
    MsgError::Damaged(e.to_string())
}

/// Encodes characters below U+0100 one byte each (equal to Windows-1252 for letters like é).
fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
}

fn write_props(cf: &mut cfb::CompoundFile<Cursor<Vec<u8>>>, dir: &str, header: Vec<u8>, props: &Props, ansi: bool) -> Result<(), MsgError> {
    let mut table = header;
    let mut ids: Vec<&u16> = props.0.keys().collect();
    ids.sort();
    for pid in ids {
        let Some(v) = props.get(*pid) else { continue };
        let (ty, inline, stream): (u16, [u8; 8], Option<Vec<u8>>) = match v {
            PropValue::Int32(n) => (ptype::INT32, i64::from(*n).to_le_bytes(), None),
            PropValue::Int64(n) => (ptype::INT64, n.to_le_bytes(), None),
            PropValue::Bool(b) => (ptype::BOOL, i64::from(*b).to_le_bytes(), None),
            PropValue::Time(t) => (ptype::TIME, t.to_le_bytes(), None),
            PropValue::String(s) if ansi => {
                let b = latin1(s);
                (ptype::STRING8, u64::try_from(b.len().saturating_add(1)).unwrap_or(0).to_le_bytes(), Some(b))
            }
            PropValue::String(s) => {
                let b: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
                (ptype::UNICODE, u64::try_from(b.len().saturating_add(2)).unwrap_or(0).to_le_bytes(), Some(b))
            }
            PropValue::Binary(b) => (ptype::BINARY, u64::try_from(b.len()).unwrap_or(0).to_le_bytes(), Some(b.clone())),
        };
        let tag = (u32::from(*pid) << 16) | u32::from(ty);
        table.extend_from_slice(&tag.to_le_bytes());
        table.extend_from_slice(&FLAGS.to_le_bytes());
        table.extend_from_slice(&inline);
        if let Some(b) = stream {
            cf.create_stream(format!("{dir}{}", substg(*pid, ty))).map_err(io)?.write_all(&b).map_err(io)?;
        }
    }
    cf.create_stream(format!("{dir}{PROPS_STREAM}")).map_err(io)?.write_all(&table).map_err(io)?;
    Ok(())
}

/// A `.msg` holding `m`. With `ansi`, strings are written as 8-bit `PtypString8`.
pub fn write_msg(m: &MapiMessage, ansi: bool) -> Result<Vec<u8>, MsgError> {
    let mut cf = cfb::CompoundFile::create(Cursor::new(Vec::new())).map_err(io)?;
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes();
    let mut header = vec![0u8; 8];
    header.extend_from_slice(&count(m.recipients.len()));
    header.extend_from_slice(&count(m.attachments.len()));
    header.extend_from_slice(&count(m.recipients.len()));
    header.extend_from_slice(&count(m.attachments.len()));
    header.resize(MESSAGE_HEADER, 0);
    write_props(&mut cf, "/", header, &m.props, ansi)?;
    cf.create_storage("/__nameid_version1.0").map_err(io)?;
    for (prefix, list) in [(RECIP_PREFIX, &m.recipients), (ATTACH_PREFIX, &m.attachments)] {
        for (n, p) in list.iter().enumerate() {
            let dir = format!("/{prefix}{n:08X}");
            cf.create_storage(&dir).map_err(io)?;
            write_props(&mut cf, &format!("{dir}/"), vec![0u8; CHILD_HEADER], p, ansi)?;
        }
    }
    cf.flush().map_err(io)?;
    Ok(cf.into_inner().into_inner())
}
