//! Writes a synthetic `.pst` and `.msg` (made-up names and addresses) for trying Import/Export:
//! `cargo run -p rslook-pst --features fixtures --example make_sample -- /some/dir`

use rslook_msg::mapi::{MapiMessage, PropValue, Props, id, utc_to_filetime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir)?;
    let pst = dir.join("synthetic-archive.pst");
    std::fs::write(&pst, rslook_pst::sample::sample_pst()?)?;

    let s = |v: &str| PropValue::String(v.into());
    let mut m = MapiMessage::default();
    m.props.insert(id::MESSAGE_CLASS, s("IPM.Note"));
    m.props.insert(id::SUBJECT, s("Saved message from a .msg file"));
    m.props.insert(id::SENDER_NAME, s("Lee Chan"));
    m.props.insert(id::SENDER_SMTP_ADDRESS, s("lee@example.org"));
    m.props.insert(id::BODY, s("This message was saved as a .msg file.\r\n"));
    m.props.insert(id::MESSAGE_DELIVERY_TIME, PropValue::Time(utc_to_filetime(chrono::Utc::now())));
    let mut to = Props::default();
    to.insert(id::RECIPIENT_TYPE, PropValue::Int32(1));
    to.insert(id::DISPLAY_NAME, s("Sam Doe"));
    to.insert(id::SMTP_ADDRESS, s("sam@example.org"));
    m.recipients.push(to);
    let msg = dir.join("synthetic-message.msg");
    std::fs::write(&msg, rslook_msg::fixture::write_msg(&m, false)?)?;

    println!("{}\n{}", pst.display(), msg.display());
    Ok(())
}
