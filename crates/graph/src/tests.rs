use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::mock::{DEFAULT_TOKEN, MOCK_MAIL, MOCK_NAME, MockGraph};
use crate::*;

fn mock() -> (MockGraph, Client) {
    let m = MockGraph::start().unwrap();
    let c = Client::new(&m.base, DEFAULT_TOKEN).with_retry(3, Duration::from_millis(50)).with_timeout(Duration::from_secs(20));
    (m, c)
}

/// A raw HTTP server answering each connection with `respond(hit index, request head)`; returns
/// (base URL ending in /v1.0, hit counter).
fn raw_server(respond: impl Fn(usize, &str) -> Vec<u8> + Send + 'static) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1.0", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p);
                }
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break None,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let len: usize =
                head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
            while buf.len() < head_end + 4 + len {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let _ = s.write_all(&respond(n, &head));
            let _ = s.flush();
        }
    });
    (base, hits)
}

fn http_json(body: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

fn raw_client(base: &str) -> Client {
    Client::new(base, "secret-token-xyz").with_retry(2, Duration::from_millis(10)).with_timeout(Duration::from_secs(10))
}

#[test]
fn me_returns_name_and_mail() {
    let (_m, c) = mock();
    assert_eq!(c.me().unwrap(), Me { display_name: MOCK_NAME.into(), email: MOCK_MAIL.into() });
}

#[test]
fn me_falls_back_to_user_principal_name() {
    let (base, _) = raw_server(|_, _| http_json(r#"{"displayName":"X","mail":null,"userPrincipalName":"x@contoso.example"}"#));
    assert_eq!(raw_client(&base).me().unwrap().email, "x@contoso.example");
}

#[test]
fn folders_lists_nested_tree_across_pages_with_well_known() {
    let (m, c) = mock();
    m.set_page_size(2);
    let projects = m.add_folder(None, "Projects");
    let alpha = m.add_folder(Some(&projects), "Alpha");
    let beta = m.add_folder(Some(&projects), "Beta");
    let gamma = m.add_folder(Some(&projects), "Gamma");
    let deep = m.add_folder(Some(&alpha), "Deep");
    let folders = c.folders().unwrap();
    assert_eq!(folders.len(), 7 + 5);
    let find = |id: &str| folders.iter().find(|f| f.id == id).unwrap().clone();
    assert_eq!(find(&projects).parent_id, None);
    assert_eq!(find(&alpha).parent_id.as_deref(), Some(projects.as_str()));
    assert_eq!(find(&beta).display_name, "Beta");
    assert_eq!(find(&gamma).parent_id.as_deref(), Some(projects.as_str()));
    assert_eq!(find(&deep).parent_id.as_deref(), Some(alpha.as_str()));
    for wk in WellKnown::ALL {
        let f = find(&m.folder_id(wk.clone()));
        assert_eq!(f.well_known, Some(wk));
        assert_eq!(f.parent_id, None);
    }
    assert_eq!(find(&projects).well_known, None);
    assert_eq!(m.requests_missing_immutable_id(), 0);
}

#[test]
fn missing_well_known_folder_is_not_an_error() {
    let (m, c) = mock();
    m.remove_folder(&m.folder_id(WellKnown::Archive));
    let folders = c.folders().unwrap();
    assert_eq!(folders.len(), 6);
    assert!(folders.iter().all(|f| f.well_known != Some(WellKnown::Archive)));
    assert!(folders.iter().any(|f| f.well_known == Some(WellKnown::Inbox)));
}

#[test]
fn messages_follow_next_links() {
    let (m, c) = mock();
    m.set_page_size(3);
    let inbox = m.folder_id(WellKnown::Inbox);
    let ids: Vec<String> = (0..10).map(|i| m.add_message(&inbox, b"Subject: x\r\n\r\n", i % 2 == 0, i % 3 == 0)).collect();
    let before = m.request_count();
    let got = c.messages(&inbox).unwrap();
    assert_eq!(m.request_count() - before, 4);
    assert_eq!(got.len(), 10);
    for (i, (s, id)) in got.iter().zip(&ids).enumerate() {
        assert_eq!(&s.id, id);
        assert_eq!(s.is_read, i % 2 == 0);
        assert_eq!(s.flagged, i % 3 == 0);
    }
    assert!(c.messages(&m.folder_id(WellKnown::Drafts)).unwrap().is_empty());
}

#[test]
fn ids_with_reserved_characters_are_percent_encoded() {
    assert_eq!(enc("AA/b+c=d e%"), "AA%2Fb%2Bc%3Dd%20e%25");
    assert_eq!(enc("a-b.c_d~"), "a-b.c_d~");
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let id = m.add_message(&inbox, b"Subject: enc\r\n\r\nbody", false, false);
    assert!(id.contains('/') && id.contains('+') && id.contains('='));
    assert_eq!(c.message_mime(&id).unwrap(), b"Subject: enc\r\n\r\nbody");
    // An id that only differs by an unencoded '/' must not reach the same message.
    assert!(matches!(c.message_mime(&format!("{id}/x")), Err(GraphError::Http { status: 404, .. })));
}

#[test]
fn message_mime_round_trip_binary() {
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let mime: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    let id = m.add_message(&inbox, &mime, true, false);
    assert_eq!(c.message_mime(&id).unwrap(), mime);
}

#[test]
fn set_flags_round_trip() {
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let id = m.add_message(&inbox, b"x", false, false);
    c.set_flags(&id, true, true).unwrap();
    assert!(m.messages(&inbox)[0].1);
    assert!(m.messages(&inbox)[0].2);
    c.set_flags(&id, false, false).unwrap();
    let (_, read, flagged, _) = m.messages(&inbox).remove(0);
    assert!(!read && !flagged);
    let st = c.messages(&inbox).unwrap();
    assert_eq!(st, vec![MessageState { id, is_read: false, flagged: false }]);
}

#[test]
fn move_keeps_id_and_changes_folder() {
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let archive = m.folder_id(WellKnown::Archive);
    let id = m.add_message(&inbox, b"x", false, false);
    assert_eq!(c.move_message(&id, &archive).unwrap(), id);
    assert!(m.messages(&inbox).is_empty());
    assert_eq!(m.messages(&archive)[0].0, id);
    // Well-known names work as destinations too.
    assert_eq!(c.move_message(&id, "deleteditems").unwrap(), id);
    assert_eq!(m.messages(&m.folder_id(WellKnown::DeletedItems)).len(), 1);
}

#[test]
fn create_folder_top_level_and_child() {
    let (m, c) = mock();
    let top = c.create_folder(None, "Receipts").unwrap();
    assert_eq!(top.parent_id, None);
    assert_eq!(top.display_name, "Receipts");
    let child = c.create_folder(Some(&top.id), "2026").unwrap();
    assert_eq!(child.parent_id.as_deref(), Some(top.id.as_str()));
    assert!(m.folders().iter().any(|(id, parent, name)| *id == child.id && parent.as_deref() == Some(top.id.as_str()) && name == "2026"));
    let listed = c.folders().unwrap();
    assert!(listed.contains(&child));
    match c.create_folder(None, "Receipts") {
        Err(GraphError::Http { status: 409, code, .. }) => assert_eq!(code, "ErrorFolderExists"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn delete_permanently_removes() {
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let id = m.add_message(&inbox, b"x", false, false);
    c.delete_permanently(&id).unwrap();
    assert!(m.messages(&inbox).is_empty());
    assert!(matches!(c.delete_permanently(&id), Err(GraphError::Http { status: 404, .. })));
}

#[test]
fn send_mime_stores_copy_in_sent_items() {
    let (m, c) = mock();
    let mime = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: hi\r\n\r\nhello\xff\r\n".to_vec();
    c.send_mime(&mime).unwrap();
    assert_eq!(m.sent(), vec![mime.clone()]);
    let sent = m.messages(&m.folder_id(WellKnown::SentItems));
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1, "sent copy is read");
    assert_eq!(sent[0].3, mime);
    assert_eq!(m.requests_missing_immutable_id(), 0);
}

#[test]
fn expired_token_maps_to_unauthorized() {
    let (m, c) = mock();
    c.me().unwrap();
    m.set_token("rotated");
    let err = c.folders().unwrap_err();
    assert!(matches!(err, GraphError::Unauthorized));
    assert!(!format!("{err} {err:?} {c:?}").contains(DEFAULT_TOKEN));
    assert!(Client::new(&m.base, "rotated").me().is_ok());
}

#[test]
fn throttled_requests_retry_then_succeed() {
    let (m, c) = mock();
    m.throttle_next(2);
    assert!(c.me().is_ok());
    assert_eq!(m.request_count(), 3);
}

#[test]
fn throttling_gives_up_after_bounded_retries() {
    let (m, c) = mock();
    m.throttle_next(100);
    match c.me() {
        Err(GraphError::Http { status: 429, code, .. }) => assert_eq!(code, "TooManyRequests"),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.request_count(), 4, "first try + 3 retries");
}

#[test]
fn retry_after_wait_is_capped() {
    let (base, hits) = raw_server(|n, _| {
        if n == 0 {
            b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 100000\r\nContent-Length: 0\r\n\r\n".to_vec()
        } else {
            http_json(r#"{"mail":"a@b.example"}"#)
        }
    });
    let started = std::time::Instant::now();
    assert_eq!(raw_client(&base).me().unwrap().email, "a@b.example");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn graph_error_json_is_mapped() {
    let (_m, c) = mock();
    match c.message_mime("does-not-exist") {
        Err(GraphError::Http { status, code, message }) => {
            assert_eq!(status, 404);
            assert_eq!(code, "ErrorItemNotFound");
            assert!(message.contains("not found"));
        }
        other => panic!("{other:?}"),
    }
    let (base, _) = raw_server(|_, _| b"HTTP/1.1 500 Internal\r\nContent-Length: 5\r\n\r\noops!".to_vec());
    match raw_client(&base).me() {
        Err(GraphError::Http { status: 500, code, message }) => {
            assert_eq!(code, "");
            assert_eq!(message, "oops!");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn oversized_responses_are_too_large() {
    let (m, c) = mock();
    let inbox = m.folder_id(WellKnown::Inbox);
    let id = m.add_message(&inbox, &vec![b'a'; 5000], false, false);
    let small = Client::new(&m.base, DEFAULT_TOKEN).with_limits(512, 4096);
    assert!(matches!(small.message_mime(&id), Err(GraphError::TooLarge(4096))));
    assert!(matches!(small.folders(), Err(GraphError::TooLarge(512))));
    // Exactly at the cap is fine.
    assert_eq!(Client::new(&m.base, DEFAULT_TOKEN).with_limits(1024, 5000).message_mime(&id).unwrap().len(), 5000);
    drop(c);
    // Default JSON cap against a server that sends 17 MB.
    let (base, _) = raw_server(|_, _| {
        let mut r = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17000000\r\n\r\n".to_vec();
        r.extend(std::iter::repeat_n(b' ', 17_000_000));
        r
    });
    assert!(matches!(raw_client(&base).me(), Err(GraphError::TooLarge(MAX_JSON_BYTES))));
}

#[test]
fn next_link_to_another_host_is_refused() {
    let (other, other_hits) = raw_server(|_, _| http_json(r#"{"value":[]}"#));
    let (base, hits) = raw_server(move |_, _| {
        let page = format!(r#"{{"value":[{{"id":"a","isRead":true}}],"@odata.nextLink":"{other}/me/mailFolders/x/messages?$skip=1"}}"#);
        http_json(&page)
    });
    match raw_client(&base).messages("x") {
        Err(GraphError::BadResponse(msg)) => assert!(msg.contains("another host")),
        other => panic!("{other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(other_hits.load(Ordering::SeqCst), 0, "the token never reached the other host");
    // userinfo tricks and scheme changes are other origins too.
    for evil in ["http://127.0.0.1@evil.example/v1.0/x", "https://127.0.0.1/v1.0/x", "//evil.example/x", "/v1.0/relative"] {
        let (base, hits) = raw_server(move |_, _| http_json(&format!(r#"{{"value":[],"@odata.nextLink":"{evil}"}}"#)));
        assert!(matches!(raw_client(&base).messages("x"), Err(GraphError::BadResponse(_))), "{evil}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn looping_next_link_stops() {
    let (base, hits) = raw_server(|_, head| {
        let host = head.lines().find_map(|l| l.strip_prefix("host: ").or_else(|| l.strip_prefix("Host: "))).unwrap_or("").to_string();
        http_json(&format!(r#"{{"value":[{{"id":"a"}}],"@odata.nextLink":"http://{host}/v1.0/me/mailFolders/x/messages?$skip=5"}}"#))
    });
    match raw_client(&base).messages("x") {
        Err(GraphError::BadResponse(msg)) => assert!(msg.contains("repeats")),
        other => panic!("{other:?}"),
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn endless_unique_next_links_hit_the_page_cap() {
    let (base, hits) = raw_server(|n, head| {
        let host = head.lines().find_map(|l| l.strip_prefix("host: ").or_else(|| l.strip_prefix("Host: "))).unwrap_or("").to_string();
        http_json(&format!(r#"{{"value":[],"@odata.nextLink":"http://{host}/v1.0/me/mailFolders/x/messages?$skip={n}"}}"#))
    });
    assert!(matches!(raw_client(&base).messages("x"), Err(GraphError::BadResponse(_))));
    assert_eq!(hits.load(Ordering::SeqCst), MAX_PAGES);
}

#[test]
fn folder_cycles_are_listed_once() {
    // Every folder claims a child, and every child listing returns the same folder again.
    let (base, _) = raw_server(|_, _| http_json(r#"{"value":[{"id":"loop","displayName":"L","childFolderCount":1}]}"#));
    let folders = raw_client(&base).folders().unwrap();
    assert_eq!(folders.len(), 1);
}

#[test]
fn junk_http_responses_never_panic() {
    let junk: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"garbage\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec(),
        b"HTTP/1.1 abc OK\r\n\r\n".to_vec(),
        b"HTTP/1.1 999 Weird\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nnope\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: -5\r\n\r\n".to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://evil.example/\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 429 Slow\r\nRetry-After: 99999999999999999999999\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 503 Busy\r\nRetry-After: Wed, 21 Oct 2015 07:28:00 GMT\r\nContent-Length: 0\r\n\r\n".to_vec(),
        format!("HTTP/1.1 200 OK\r\nX-Big: {}\r\n\r\n", "a".repeat(200_000)).into_bytes(),
        http_json("not json"),
        http_json("[1,2,3]"),
        http_json(r#"{"value":"nope"}"#),
        http_json(r#"{"value":[{"id":5}]}"#),
        http_json(r#"{"value":[1,null,true]}"#),
        http_json(r#"{"value":[],"@odata.nextLink":42}"#),
        http_json(r#"{"value":[{"id":"a","isRead":"yes","flag":7,"childFolderCount":-1}]}"#),
        http_json(r#"{"error":{"code":5,"message":["x"]}}"#),
        http_json(r#"{"id":null}"#),
        http_json("\u{0}\u{1}\u{ff}"),
        (0..3000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect(),
    ];
    for body in junk {
        let (base, _) = raw_server(move |_, _| body.clone());
        let c = raw_client(&base);
        let _ = c.me();
        let _ = c.folders();
        let _ = c.messages("a/b+c=");
        let _ = c.message_mime("a/b+c=");
        let _ = c.set_flags("x", true, false);
        let _ = c.move_message("x", "y");
        let _ = c.create_folder(Some("p"), "n");
        let _ = c.delete_permanently("x");
        let _ = c.send_mime(b"x");
    }
    // Nothing listening at all.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1.0", l.local_addr().unwrap())
    };
    match raw_client(&dead).me() {
        Err(GraphError::Network(msg)) => assert!(!msg.contains("secret-token-xyz")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_request_sends_bearer_and_immutable_id() {
    let (base, _) = raw_server(|_, head| {
        let lower = head.to_ascii_lowercase();
        if lower.contains("authorization: bearer secret-token-xyz") && lower.contains("prefer: idtype=\"immutableid\"") {
            http_json(r#"{"mail":"ok@example.com","id":"n"}"#)
        } else {
            b"HTTP/1.1 400 Bad\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    });
    let c = raw_client(&base);
    assert!(c.me().is_ok());
    assert!(c.set_flags("x", true, true).is_ok());
    assert!(c.move_message("x", "y").is_ok());
    assert!(c.send_mime(b"x").is_ok());
}
