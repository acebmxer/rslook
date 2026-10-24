//! A small synthetic mailbox as a `.pst`, for tests, the engine's import tests and the
//! `make_sample` example. Every name and address is made up.

use crate::fixture::{FixtureError, Folder, Message, Props, Value, write_pst};
use rslook_msg::mapi::id;

/// 2026-10-20 09:30 UTC as a FILETIME.
pub const SAMPLE_TIME: i64 = 134_369_622_000_000_000;

fn u(s: &str) -> Value {
    Value::Unicode(s.to_string())
}

fn recipient(kind: i32, name: &str, email: &str) -> Props {
    vec![(id::RECIPIENT_TYPE, Value::Int32(kind)), (id::DISPLAY_NAME, u(name)), (id::EMAIL_ADDRESS, u(email)), (id::SMTP_ADDRESS, u(email))]
}

/// A mail item. `flags` is `PidTagMessageFlags` (1 = read).
pub fn mail(subject: &str, from: (&str, &str), body: &str, flags: i32) -> Message {
    Message {
        props: vec![
            (id::MESSAGE_CLASS, u("IPM.Note")),
            (id::SUBJECT, u(subject)),
            (id::SENDER_NAME, u(from.0)),
            (id::SENDER_EMAIL_ADDRESS, u(from.1)),
            (id::BODY, u(body)),
            (id::MESSAGE_FLAGS, Value::Int32(flags)),
            (id::MESSAGE_DELIVERY_TIME, Value::Time(SAMPLE_TIME)),
        ],
        recipients: vec![recipient(1, "Sam Doe", "sam@example.org")],
        attachments: Vec::new(),
    }
}

/// Inbox (3 mails, one with an attachment and a Cc, one flagged and important, plus a
/// contact item that must be skipped), Projects > 2026 (1 mail) and a Calendar folder (1
/// appointment, skipped).
pub fn sample_folders() -> Vec<Folder> {
    let mut report = mail("Quarterly report", ("Rosa Park", "rosa@example.org"), "Numbers attached.", 1);
    report.recipients.push(recipient(2, "Team", "team@example.org"));
    report.attachments.push(vec![
        (id::ATTACH_LONG_FILENAME, u("quarterly-report.csv")),
        (id::ATTACH_FILENAME, u("QUARTE~1.CSV")),
        (0x3705, Value::Int32(1)),
        (id::ATTACH_DATA, Value::Binary(b"quarter,total\nQ3,42\n".to_vec())),
    ]);
    let mut urgent = mail("Server move tonight", ("Ops Desk", "ops@example.org"), "Please log off by 9 pm.", 0);
    urgent.props.push((id::IMPORTANCE, Value::Int32(2)));
    urgent.props.push((id::FLAG_STATUS, Value::Int32(2)));
    let lunch = mail("Lunch?", ("Lee Chan", "lee@example.org"), "Noon at the café?", 0);
    let contact = Message { props: vec![(id::MESSAGE_CLASS, u("IPM.Contact")), (id::SUBJECT, u("Sam Doe"))], ..Default::default() };
    let plan = mail("2026 plan", ("Rosa Park", "rosa@example.org"), "Draft attached next week.", 1);
    let meeting = Message { props: vec![(id::MESSAGE_CLASS, u("IPM.Appointment")), (id::SUBJECT, u("Stand-up"))], ..Default::default() };
    vec![
        Folder { name: "Inbox".into(), messages: vec![report, urgent, lunch, contact], ..Default::default() },
        Folder {
            name: "Projects".into(),
            children: vec![Folder { name: "2026".into(), messages: vec![plan], ..Default::default() }],
            ..Default::default()
        },
        Folder { name: "Calendar".into(), props: vec![(id::CONTAINER_CLASS, u("IPF.Appointment"))], messages: vec![meeting], ..Default::default() },
    ]
}

/// [`sample_folders`] as `.pst` bytes.
pub fn sample_pst() -> Result<Vec<u8>, FixtureError> {
    write_pst(&sample_folders())
}
