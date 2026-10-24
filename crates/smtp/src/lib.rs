#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! SMTP submission for Rslook (RFC 5321, RFC 6409): builds the RFC 5322 message and hands it to
//! the server over implicit TLS, STARTTLS or plain TCP. The same bytes are returned so the caller
//! can append the exact sent message to the Sent folder.

use chrono::{DateTime, Utc};
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use rslook_account::{Security, Server};
use rslook_mail::Address;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum SmtpError {
    #[error("{0}")]
    Build(String),
    #[error("secure connection failed: {0}")]
    Tls(String),
    #[error("the server did not accept the message: {0}")]
    Send(String),
}

/// One message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub body: String,
    pub date: DateTime<Utc>,
}

fn mailbox(a: &Address) -> Result<Mailbox, SmtpError> {
    let email = a.email.trim().parse().map_err(|_| SmtpError::Build(format!("\"{}\" is not a valid email address.", a.email.trim())))?;
    let name = a.name.trim();
    let name = (!name.is_empty() && !name.contains(['\r', '\n'])).then(|| name.to_string());
    Ok(Mailbox::new(name, email))
}

/// Builds the RFC 5322 message (plain text, UTF-8, with Date and Message-ID).
pub fn build(o: &Outgoing) -> Result<Message, SmtpError> {
    if o.to.is_empty() && o.cc.is_empty() {
        return Err(SmtpError::Build("There is no recipient.".into()));
    }
    let subject: String = o.subject.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let mut b = Message::builder()
        .from(mailbox(&o.from)?)
        .subject(subject)
        .date(o.date.into())
        .message_id(None)
        .user_agent(format!("Rslook {}", env!("CARGO_PKG_VERSION")));
    for a in &o.to {
        b = b.to(mailbox(a)?);
    }
    for a in &o.cc {
        b = b.cc(mailbox(a)?);
    }
    b.header(ContentType::TEXT_PLAIN).body(o.body.clone()).map_err(|e| SmtpError::Build(e.to_string()))
}

/// Sends a message and returns the exact bytes sent.
pub async fn send(server: &Server, user: &str, password: &str, extra_roots_pem: &[u8], o: &Outgoing) -> Result<Vec<u8>, SmtpError> {
    let message = build(o)?;
    let host = server.host.trim();
    let tls_params = || -> Result<TlsParameters, SmtpError> {
        let mut b = TlsParameters::builder(host.to_string());
        if !extra_roots_pem.is_empty() {
            let cert = Certificate::from_pem(extra_roots_pem).map_err(|e| SmtpError::Tls(e.to_string()))?;
            b = b.add_root_certificate(cert);
        }
        b.build_rustls().map_err(|e| SmtpError::Tls(e.to_string()))
    };
    let tls = match server.security {
        Security::Tls => Tls::Wrapper(tls_params()?),
        Security::StartTls => Tls::Required(tls_params()?),
        Security::None => Tls::None,
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host).port(server.port).tls(tls).timeout(Some(Duration::from_secs(60)));
    if !user.is_empty() {
        builder = builder.credentials(Credentials::new(user.to_string(), password.to_string()));
    }
    let transport = builder.build();
    let bytes = message.formatted();
    transport.send_raw(message.envelope(), &bytes).await.map_err(|e| SmtpError::Send(e.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outgoing() -> Outgoing {
        Outgoing {
            from: Address::new("Test Sender", "sender@example.test"),
            to: vec![Address::new("Ann", "ann@example.test")],
            cc: vec![Address::new("", "bob@example.test")],
            subject: "Café\r\nBcc: evil@example.test".into(),
            body: "Hello\nthere".into(),
            date: DateTime::from_timestamp(1_790_000_000, 0).unwrap_or_default(),
        }
    }

    #[test]
    fn builds_a_message_our_own_parser_reads_back() {
        let bytes = build(&outgoing()).unwrap().formatted();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Message-ID:"), "{text}");
        assert!(!text.contains("\r\nBcc:"), "subject line break must not start a header: {text}");
        let m = rslook_mail::parse::parse_rfc5322(&bytes).unwrap();
        assert_eq!(m.from, Address::new("Test Sender", "sender@example.test"));
        assert_eq!(m.to[0].email, "ann@example.test");
        assert_eq!(m.cc[0].email, "bob@example.test");
        assert!(m.subject.starts_with("Café"));
        assert!(m.body.contains("Hello"));
        assert_eq!(m.date.map(|d| d.timestamp()), Some(1_790_000_000));
    }

    #[test]
    fn bad_addresses_and_no_recipients_are_refused() {
        assert!(build(&Outgoing { to: vec![Address::new("", "not an address")], ..outgoing() }).is_err());
        assert!(build(&Outgoing { to: vec![], cc: vec![], ..outgoing() }).is_err());
        assert!(build(&Outgoing { from: Address::new("", ""), ..outgoing() }).is_err());
    }
}
