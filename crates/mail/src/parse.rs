//! RFC 5322 / MIME parsing into [`NewMessage`] through `mail-parser`.

use mail_parser::MimeHeaders;

use crate::{Address, Attachment, Importance, MailError, NewMessage};
use chrono::DateTime;

/// Larger messages are refused rather than buffered (hostile input).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

fn addrs(a: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    a.map(|a| {
        a.iter()
            .map(|x| Address { name: x.name().unwrap_or_default().to_string(), email: x.address().unwrap_or_default().to_string() })
            .collect()
    })
    .unwrap_or_default()
}

/// Parses one RFC 5322 message (an `.eml` file or an IMAP `BODY[]`).
pub fn parse_rfc5322(bytes: &[u8]) -> Result<NewMessage, MailError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(MailError::TooLarge(bytes.len()));
    }
    let msg = mail_parser::MessageParser::default().parse(bytes).ok_or_else(|| MailError::Parse("not an RFC 5322 message".into()))?;
    let from = addrs(msg.from()).into_iter().next().unwrap_or_default();
    let date = msg.date().and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0));
    let body = msg.body_text(0).map(|b| b.into_owned()).unwrap_or_default();
    let importance = match msg.header_raw("Importance").map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if s == "high" => Importance::High,
        Some(s) if s == "low" => Importance::Low,
        _ => match msg.header_raw("X-Priority").and_then(|s| s.trim().chars().next()) {
            Some('1') | Some('2') => Importance::High,
            Some('4') | Some('5') => Importance::Low,
            _ => Importance::Normal,
        },
    };
    let attachments = msg
        .attachments()
        .map(|p| Attachment { name: p.attachment_name().unwrap_or("attachment").to_string(), size: p.contents().len() })
        .collect();
    Ok(NewMessage {
        from,
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        subject: msg.subject().unwrap_or_default().to_string(),
        date,
        body,
        read: false,
        flagged: false,
        importance,
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EML: &[u8] = b"From: Rosa Park <rosa@example.org>\r\n\
To: Me <me@example.org>, Team <team@example.org>\r\n\
Subject: =?utf-8?q?Caf=C3=A9_plans?=\r\n\
Date: Tue, 20 Oct 2026 09:30:00 -0400\r\n\
Importance: High\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See you at noon.\r\n\
--b\r\n\
Content-Type: text/plain; name=\"menu.txt\"\r\n\
Content-Disposition: attachment; filename=\"menu.txt\"\r\n\
\r\n\
soup\r\n\
--b--\r\n";

    #[test]
    fn parses_headers_body_and_attachments() {
        let m = parse_rfc5322(EML).unwrap();
        assert_eq!(m.from, Address::new("Rosa Park", "rosa@example.org"));
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.subject, "Café plans");
        assert_eq!(m.importance, Importance::High);
        assert!(m.body.contains("See you at noon."));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].name, "menu.txt");
        assert_eq!(m.date.unwrap().to_rfc3339(), "2026-10-20T13:30:00+00:00");
    }

    #[test]
    fn junk_never_panics() {
        for b in [&b""[..], b"\xff\xfe", b"From:\r\n\r\n", b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n"] {
            let _ = parse_rfc5322(b);
        }
    }
}
