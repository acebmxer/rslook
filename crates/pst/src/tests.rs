use super::*;
use crate::fixture::{Folder as FxFolder, Message as FxMessage, Value, write_pst};
use crate::sample::{SAMPLE_TIME, mail, sample_folders, sample_pst};
use rslook_mail::{Address, Attachment, Importance};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A temp file removed when dropped.
struct Temp(PathBuf);

impl Temp {
    fn new(bytes: &[u8]) -> Temp {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("rslook-pst-test-{}-{n}.pst", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        Temp(p)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn find<'a>(f: &'a PstFolder, name: &str) -> &'a PstFolder {
    f.children
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no folder {name} in {:?}", f.children.iter().map(|c| &c.name).collect::<Vec<_>>()))
}

#[test]
fn imports_folders_mail_recipients_and_attachments() {
    let file = Temp::new(&sample_pst().unwrap());
    let imp = read_pst(&file.0).unwrap();
    assert_eq!(imp.failed, 0);
    assert_eq!(imp.skipped, 2, "the contact and the appointment");
    let names: Vec<&str> = imp.root.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Inbox", "Projects"], "the Calendar folder is skipped");
    assert_eq!(imp.root.message_count(), 4);

    let inbox = find(&imp.root, "Inbox");
    assert_eq!(inbox.messages.len(), 3);
    let report = inbox.messages.iter().find(|m| m.subject == "Quarterly report").unwrap();
    assert_eq!(report.from, Address::new("Rosa Park", "rosa@example.org"));
    assert_eq!(report.to, vec![Address::new("Sam Doe", "sam@example.org")]);
    assert_eq!(report.cc, vec![Address::new("Team", "team@example.org")]);
    assert_eq!(report.body, "Numbers attached.");
    assert!(report.read);
    assert_eq!(report.date, mapi::filetime_to_utc(SAMPLE_TIME));
    assert_eq!(report.attachments, vec![Attachment { name: "quarterly-report.csv".into(), size: 20 }]);

    let urgent = inbox.messages.iter().find(|m| m.subject == "Server move tonight").unwrap();
    assert!(urgent.flagged && !urgent.read);
    assert_eq!(urgent.importance, Importance::High);
    assert!(inbox.messages.iter().any(|m| m.body == "Noon at the café?"));

    let y2026 = find(find(&imp.root, "Projects"), "2026");
    assert_eq!(y2026.messages.len(), 1);
    assert_eq!(y2026.messages[0].subject, "2026 plan");
}

#[test]
fn large_folders_and_bodies_come_through_whole() {
    let body = "lorem ipsum ".repeat(600);
    let mut messages: Vec<FxMessage> = (0..150).map(|n| mail(&format!("Item {n}"), ("Bulk", "bulk@example.org"), "short", 0)).collect();
    messages.push(mail("Long one", ("Bulk", "bulk@example.org"), &body, 0));
    let file = Temp::new(&write_pst(&[FxFolder { name: "Bulk".into(), messages, ..Default::default() }]).unwrap());
    let imp = read_pst(&file.0).unwrap();
    let bulk = find(&imp.root, "Bulk");
    assert_eq!(bulk.messages.len(), 151);
    assert_eq!(bulk.messages.iter().find(|m| m.subject == "Long one").unwrap().body, body);
}

#[test]
fn html_only_bodies_become_text() {
    let mut m = mail("Newsletter", ("News", "news@example.org"), "", 0);
    m.props.retain(|(pid, _)| *pid != id::BODY);
    m.props.push((id::HTML, Value::Binary(b"<html><body><p>Big <i>news</i></p></body></html>".to_vec())));
    let file = Temp::new(&write_pst(&[FxFolder { name: "News".into(), messages: vec![m], ..Default::default() }]).unwrap());
    let imp = read_pst(&file.0).unwrap();
    assert!(find(&imp.root, "News").messages[0].body.contains("Big news"));
}

#[test]
fn not_a_pst_is_an_error() {
    for b in [&b""[..], b"!BDN", b"From: x\r\n\r\nhi", &[0u8; 8192]] {
        let file = Temp::new(b);
        assert!(read_pst(&file.0).is_err());
    }
    assert!(read_pst(Path::new("/nonexistent/rslook/none.pst")).is_err());
}

#[test]
fn damaged_files_never_panic() {
    let good = write_pst(&sample_folders()).unwrap();
    // Truncations and byte flips across the whole file: an error or a (partial) import, never a
    // panic or a hang.
    for cut in (0..good.len()).step_by(good.len() / 40 + 1) {
        let file = Temp::new(&good[..cut]);
        let _ = read_pst(&file.0);
    }
    for pos in (0..good.len()).step_by(good.len() / 300 + 1) {
        let mut bad = good.clone();
        bad[pos] ^= 0xA5;
        let file = Temp::new(&bad);
        let _ = read_pst(&file.0);
    }
}
