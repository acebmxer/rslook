//! Modified UTF-7 mailbox names (RFC 3501 §5.1.3, kept by RFC 9051 for servers that don't
//! announce UTF8=ACCEPT). Only decoding is needed: Rslook creates ASCII names.

/// Base64 alphabet with `,` in place of `/`.
fn value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b',' => 63,
        _ => return None,
    } as u32)
}

/// Decodes a mailbox name for display. Anything malformed is kept as written.
pub fn decode(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < bytes.len() {
        let Some(&b) = bytes.get(i) else { break };
        if b != b'&' {
            // Copy the next char whole (names may already be UTF-8).
            let ch = name.get(i..).and_then(|s| s.chars().next());
            match ch {
                Some(c) => {
                    out.push(c);
                    i += c.len_utf8();
                }
                None => break,
            }
            continue;
        }
        let Some(end) = bytes.get(i + 1..).and_then(|rest| rest.iter().position(|&c| c == b'-')).map(|p| i + 1 + p) else {
            out.push_str(name.get(i..).unwrap_or(""));
            break;
        };
        let run = bytes.get(i + 1..end).unwrap_or(&[]);
        if run.is_empty() {
            out.push('&');
        } else {
            match decode_run(run) {
                Some(s) => out.push_str(&s),
                None => out.push_str(name.get(i..=end).unwrap_or("")),
            }
        }
        i = end + 1;
    }
    out
}

fn decode_run(run: &[u8]) -> Option<String> {
    let mut bits: u32 = 0;
    let mut nbits = 0u32;
    let mut units = Vec::new();
    for &c in run {
        bits = (bits << 6) | value(c)?;
        nbits += 6;
        if nbits >= 16 {
            nbits -= 16;
            units.push(((bits >> nbits) & 0xffff) as u16);
        }
        bits &= (1 << nbits) - 1;
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decodes_rfc_examples() {
        assert_eq!(decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-"), "~peter/mail/台北/日本語");
        assert_eq!(decode("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode("A&-B"), "A&B");
        assert_eq!(decode("INBOX"), "INBOX");
    }

    #[test]
    fn malformed_names_never_panic() {
        for s in ["&", "&abc", "&!!-", "&-&-", "é&AOk-", "&////-", "&AAAAAAAAAAAAAAAAAAAA-"] {
            let _ = decode(s);
        }
    }
}
