use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::loopback::{Callback, MAX_REQUEST, parse_request};
use crate::mock::{CLIENT_ID, MockAuth, http_get, raw_exchange};
use crate::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

const WAIT: Duration = Duration::from_secs(20);

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

/// The value of query parameter `key` in `url`.
fn param(url: &str, key: &str) -> String {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    query_value(&parse_query(q), key).unwrap_or("").to_string()
}

#[test]
fn sign_in_full_flow_with_mock() {
    let mock = MockAuth::start().unwrap();
    mock.set_identity("Ada Example", "ada@example.test");
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let browser = mock.browser();
    let seen2 = seen.clone();
    let open = move |url: &str| {
        *seen2.lock().unwrap() = url.to_string();
        browser(url)
    };
    let t = sign_in(&mock.provider(GRAPH_SCOPES), &open, WAIT, &no_cancel()).unwrap();
    assert!(mock.access_token_valid(&t.access_token));
    assert!(t.refresh_token.is_some());
    assert_eq!(t.expires_in, 3600);
    let id = t.id_token.as_deref().unwrap();
    assert_eq!(id_token_identity(id), Some(("Ada Example".into(), "ada@example.test".into())));
    assert_eq!(mock.token_requests(), 1);

    let url = seen.lock().unwrap().clone();
    assert!(url.starts_with(&format!("{}/authorize?", mock.base)));
    assert_eq!(param(&url, "client_id"), CLIENT_ID);
    assert_eq!(param(&url, "response_type"), "code");
    assert_eq!(param(&url, "code_challenge_method"), "S256");
    assert_eq!(param(&url, "prompt"), "select_account");
    assert_eq!(param(&url, "scope"), GRAPH_SCOPES.join(" "));
    assert_eq!(param(&url, "state").len(), 43);
    assert_eq!(param(&url, "code_challenge").len(), 43);
    let redirect = param(&url, "redirect_uri");
    let port: u16 = redirect.strip_prefix("http://localhost:").unwrap().parse().unwrap();
    assert!(port > 0);
}

#[test]
fn code_is_single_use_and_pkce_is_checked_server_side() {
    let mock = MockAuth::start().unwrap();
    let p = mock.provider(OUTLOOK_IMAP_SCOPES);
    let verifier = random_token(64).unwrap();
    let redirect = "http://localhost:1";
    let auth = authorize_url(&p, redirect, "st", &pkce_challenge(&verifier));
    let get_code = || {
        let r = http_get(&auth).unwrap();
        assert_eq!(r.status, 302);
        let loc = r.location.unwrap();
        assert_eq!(param(&loc, "state"), "st");
        param(&loc, "code")
    };
    let exchange = |code: &str, verifier: &str, redirect: &str, client: &str| {
        let scope = p.scope_param();
        let form = [
            ("client_id", client),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
            ("scope", scope.as_str()),
        ];
        token_request(&p.token_url, &form)
    };
    let wrong = random_token(64).unwrap();
    let code = get_code();
    match exchange(&code, &wrong, redirect, CLIENT_ID) {
        Err(OAuthError::Denied(m)) => assert!(m.starts_with("invalid_grant"), "{m}"),
        other => panic!("{other:?}"),
    }
    // The failed attempt consumed the code.
    assert!(matches!(exchange(&code, &verifier, redirect, CLIENT_ID), Err(OAuthError::Denied(_))));
    let code = get_code();
    assert!(matches!(exchange(&code, &verifier, "http://localhost:2", CLIENT_ID), Err(OAuthError::Denied(_))));
    let code = get_code();
    assert!(matches!(exchange(&code, &verifier, redirect, "someone-else"), Err(OAuthError::Denied(_))));
    let code = get_code();
    assert!(exchange(&code, &verifier, redirect, CLIENT_ID).is_ok());
    match exchange(&code, &verifier, redirect, CLIENT_ID) {
        Err(OAuthError::Denied(m)) => assert!(m.starts_with("invalid_grant"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn wrong_state_favicon_and_junk_are_ignored() {
    let mock = MockAuth::start().unwrap();
    let real = Arc::new(mock.browser());
    let checks = Arc::new(AtomicBool::new(false));
    let checks2 = checks.clone();
    let open = move |url: &str| {
        let url = url.to_string();
        let real = real.clone();
        let checks = checks2.clone();
        std::thread::spawn(move || {
            let redirect = param(&url, "redirect_uri");
            let addr = format!("127.0.0.1:{}", redirect.rsplit(':').next().unwrap());
            let fav = http_get(&format!("{redirect}/favicon.ico")).unwrap();
            let bad = http_get(&format!("{redirect}/?code=mock-code-999&state=not-the-state")).unwrap();
            let nostate = http_get(&format!("{redirect}/?code=mock-code-999")).unwrap();
            let errbad = http_get(&format!("{redirect}/?error=access_denied&state=nope")).unwrap();
            let post = raw_exchange(&addr, b"POST /?code=x&state=y HTTP/1.1\r\n\r\n");
            let big = raw_exchange(&addr, &vec![b'A'; MAX_REQUEST + 100]);
            let junk = raw_exchange(&addr, b"\xff\xfe\x00\r\n\r\n");
            let ok = [&fav, &bad, &nostate, &errbad].iter().all(|r| r.status == 200 && r.body.contains("close this tab"));
            let ok = ok && String::from_utf8_lossy(&post).contains("close this tab") && String::from_utf8_lossy(&junk).contains("close this tab");
            let ok = ok && big.is_empty();
            checks.store(ok, Ordering::SeqCst);
            let _ = real(&url);
        });
        Ok(())
    };
    let t = sign_in(&mock.provider(GRAPH_SCOPES), &open, WAIT, &no_cancel()).unwrap();
    assert!(mock.access_token_valid(&t.access_token));
    assert!(checks.load(Ordering::SeqCst));
    assert_eq!(mock.token_requests(), 1);
}

#[test]
fn error_redirect_gives_denied() {
    let mock = MockAuth::start().unwrap();
    mock.deny_next("access_denied", "The user declined the consent.");
    let r = sign_in(&mock.provider(GRAPH_SCOPES), &mock.browser(), WAIT, &no_cancel());
    match r {
        Err(e @ OAuthError::Denied(_)) => {
            let m = e.to_string();
            assert!(m.contains("access_denied: The user declined the consent."), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(mock.token_requests(), 0);
}

#[test]
fn browser_failure_is_reported() {
    let mock = MockAuth::start().unwrap();
    let r = sign_in(&mock.provider(GRAPH_SCOPES), &|_: &str| Err("no browser".to_string()), WAIT, &no_cancel());
    assert!(matches!(r, Err(OAuthError::Io(m)) if m.contains("no browser")));
}

#[test]
fn cancel_flag_gives_cancelled_quickly() {
    let mock = MockAuth::start().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    let r = sign_in(&mock.provider(GRAPH_SCOPES), &|_: &str| Ok(()), Duration::from_secs(60), &cancel);
    assert!(matches!(r, Err(OAuthError::Cancelled)), "{r:?}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn timeout_gives_timed_out() {
    let mock = MockAuth::start().unwrap();
    let start = Instant::now();
    let r = sign_in(&mock.provider(GRAPH_SCOPES), &|_: &str| Ok(()), Duration::from_millis(300), &no_cancel());
    assert!(matches!(r, Err(OAuthError::TimedOut)), "{r:?}");
    let e = start.elapsed();
    assert!(e >= Duration::from_millis(300) && e < Duration::from_secs(3), "{e:?}");
}

#[test]
fn refresh_rotates_the_refresh_token() {
    let mock = MockAuth::start().unwrap();
    let p = mock.provider(GRAPH_SCOPES);
    let t = sign_in(&p, &mock.browser(), WAIT, &no_cancel()).unwrap();
    let rt1 = t.refresh_token.unwrap();
    let t2 = refresh(&p, &rt1).unwrap();
    let rt2 = t2.refresh_token.unwrap();
    assert_ne!(rt1, rt2);
    assert_ne!(t.access_token, t2.access_token);
    assert!(mock.access_token_valid(&t2.access_token));
    assert!(matches!(refresh(&p, &rt1), Err(OAuthError::Denied(m)) if m.starts_with("invalid_grant")));
    assert!(refresh(&p, &rt2).is_ok());
}

#[test]
fn revoked_refresh_gives_denied() {
    let mock = MockAuth::start().unwrap();
    let p = mock.provider(OUTLOOK_IMAP_SCOPES);
    let rt = mock.issue_refresh_token();
    let other = mock.issue_refresh_token();
    mock.revoke_refresh_token(&rt);
    assert!(matches!(refresh(&p, &rt), Err(OAuthError::Denied(m)) if m.starts_with("invalid_grant")));
    let t = refresh(&p, &other).unwrap();
    mock.revoke_all();
    assert!(!mock.access_token_valid(&t.access_token));
    assert!(matches!(refresh(&p, &t.refresh_token.unwrap()), Err(OAuthError::Denied(_))));
    assert!(matches!(refresh(&p, "never-issued"), Err(OAuthError::Denied(_))));
    let wrong_client = Provider { client_id: "other".into(), ..p.clone() };
    let rt = mock.issue_refresh_token();
    assert!(matches!(refresh(&wrong_client, &rt), Err(OAuthError::Denied(_))));
}

#[test]
fn fixed_access_token_for_code_and_refresh_grants() {
    let mock = MockAuth::start().unwrap();
    mock.set_access_token("fixed-bearer");
    mock.set_expires_in(60);
    let p = mock.provider(GRAPH_SCOPES);
    let t = sign_in(&p, &mock.browser(), WAIT, &no_cancel()).unwrap();
    assert_eq!(t.access_token, "fixed-bearer");
    assert_eq!(t.expires_in, 60);
    let t2 = refresh(&p, &mock.issue_refresh_token()).unwrap();
    assert_eq!(t2.access_token, "fixed-bearer");
    assert!(mock.access_token_valid("fixed-bearer"));
}

#[test]
fn id_token_identity_good_and_junk() {
    let enc = |s: &str| URL_SAFE_NO_PAD.encode(s);
    let tok = |claims: &str| format!("{}.{}.sig", enc(r#"{"alg":"RS256"}"#), enc(claims));
    assert_eq!(id_token_identity(&tok(r#"{"name":"Bo","email":"bo@x.test"}"#)), Some(("Bo".into(), "bo@x.test".into())));
    assert_eq!(id_token_identity(&tok(r#"{"name":"Bo","preferred_username":"bo@work.test"}"#)), Some(("Bo".into(), "bo@work.test".into())));
    assert_eq!(id_token_identity(&tok(r#"{"email":"bo@x.test"}"#)), Some((String::new(), "bo@x.test".into())));
    // Padded payloads are tolerated.
    let padded = format!("h.{}==.s", enc(r#"{"email":"p@x.test"}"#));
    assert_eq!(id_token_identity(&padded).map(|x| x.1), Some("p@x.test".into()));
    for junk in [
        "",
        ".",
        "..",
        "a.b",
        "a.b.c.d",
        "a.!!!.c",
        &tok("not json"),
        &tok("[1,2]"),
        &tok(r#"{"name":"Bo"}"#),
        &tok(r#"{"email":42}"#),
        &tok(r#"{"email":"  "}"#),
        &format!("a.{}.c", "A".repeat(70_000)),
        "\u{0}.\u{ffff}.é",
    ] {
        assert_eq!(id_token_identity(junk), None, "{junk:.40}");
    }
}

#[test]
fn xoauth2_exact_bytes() {
    assert_eq!(xoauth2("someone@example.test", "abc.def"), "user=someone@example.test\x01auth=Bearer abc.def\x01\x01");
    assert_eq!(xoauth2("u", "t").as_bytes(), b"user=u\x01auth=Bearer t\x01\x01");
}

#[test]
fn provider_microsoft_urls_and_scopes() {
    let p = Provider::microsoft("00000000-1111", "common", GRAPH_SCOPES);
    assert_eq!(p.authorize_url, "https://login.microsoftonline.com/common/oauth2/v2.0/authorize");
    assert_eq!(p.token_url, "https://login.microsoftonline.com/common/oauth2/v2.0/token");
    assert_eq!(p.client_id, "00000000-1111");
    assert_eq!(p.scopes, GRAPH_SCOPES.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = Provider::microsoft("id", "consumers", OUTLOOK_IMAP_SCOPES);
    assert!(c.token_url.starts_with("https://login.microsoftonline.com/consumers/"));
    assert!(c.scopes.iter().any(|s| s == "https://outlook.office.com/IMAP.AccessAsUser.All"));
    let url = authorize_url(&p, "http://localhost:5000", "s t", "ch");
    assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=00000000-1111&response_type=code&"));
    assert!(url.contains("&redirect_uri=http%3A%2F%2Flocalhost%3A5000&"));
    assert!(url.contains("&scope=offline_access%20openid%20profile%20email%20https%3A%2F%2Fgraph.microsoft.com%2FUser.Read%20"));
    assert!(url.contains("&state=s%20t&code_challenge=ch&code_challenge_method=S256&prompt=select_account"));
}

#[test]
fn pkce_verifier_and_challenge() {
    // RFC 7636 appendix B.
    assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let v = random_token(64).unwrap();
    assert!((43..=128).contains(&v.len()));
    assert!(v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')));
    assert_ne!(v, random_token(64).unwrap());
}

#[test]
fn token_response_mapping() {
    let ok = parse_token_response(200, r#"{"access_token":"a","refresh_token":"r","expires_in":"3599","id_token":"i"}"#).unwrap();
    assert_eq!(ok, Tokens { access_token: "a".into(), refresh_token: Some("r".into()), expires_in: 3599, id_token: Some("i".into()) });
    let bare = parse_token_response(200, r#"{"access_token":"a","refresh_token":""}"#).unwrap();
    assert_eq!((bare.refresh_token, bare.expires_in), (None, 0));
    match parse_token_response(400, r#"{"error":"invalid_grant","error_description":"AADSTS70000: expired\r\nline"}"#) {
        Err(OAuthError::Denied(m)) => assert_eq!(m, "invalid_grant: AADSTS70000: expired  line"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(parse_token_response(500, "<html>oops</html>"), Err(OAuthError::BadResponse(_))));
    assert!(matches!(parse_token_response(200, "not json"), Err(OAuthError::BadResponse(_))));
    assert!(matches!(parse_token_response(200, r#"{"token_type":"Bearer"}"#), Err(OAuthError::BadResponse(_))));
    let long = format!(r#"{{"error":"x","error_description":"{}"}}"#, "é".repeat(5000));
    match parse_token_response(400, &long) {
        Err(OAuthError::Denied(m)) => assert!(m.chars().count() <= MAX_ERROR_TEXT + 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn token_response_size_is_capped() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            // Read the whole request (form body included) so closing never resets the socket.
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(6).any(|w| w == b"scope=") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            std::thread::sleep(Duration::from_millis(50));
            let body = format!(r#"{{"access_token":"{}"}}"#, "a".repeat(2 * 1024 * 1024));
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        }
    });
    let p = Provider { token_url: format!("http://127.0.0.1:{port}/token"), ..Provider::microsoft("c", "common", GRAPH_SCOPES) };
    let r = refresh(&p, "rt");
    assert!(matches!(r, Err(OAuthError::BadResponse(_))), "{r:?}");
}

#[test]
fn unreachable_token_endpoint_is_network_error() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let p = Provider { token_url: format!("http://127.0.0.1:{port}/token"), ..Provider::microsoft("c", "common", GRAPH_SCOPES) };
    match refresh(&p, "secret-refresh-token") {
        Err(e @ OAuthError::Network(_)) => assert!(!e.to_string().contains("secret-refresh-token")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn parse_request_cases() {
    let code = |c: &str, s: &str| Callback::Code { code: c.into(), state: s.into() };
    assert_eq!(parse_request(b"GET /?code=a%2Bb&state=x HTTP/1.1\r\nHost: h\r\n\r\n"), code("a+b", "x"));
    assert_eq!(parse_request(b"GET /?state=x&code=c+d HTTP/1.1\n"), code("c d", "x"));
    assert_eq!(parse_request(b"GET /?code=a&state=x&code=b HTTP/1.0\r\n"), code("a", "x"));
    assert_eq!(
        parse_request(b"GET /?error=access_denied&error_description=No%20way HTTP/1.1\r\n"),
        Callback::Error { error: "access_denied".into(), description: Some("No way".into()), state: None }
    );
    for ignored in [
        &b"GET /favicon.ico HTTP/1.1\r\n"[..],
        b"GET / HTTP/1.1\r\n",
        b"GET /?code=a HTTP/1.1\r\n",
        b"GET /?code=&state=x HTTP/1.1\r\n",
        b"POST /?code=a&state=x HTTP/1.1\r\n",
        b"GET /?code=a&state=x\r\n",
        b"GET  /?code=a&state=x HTTP/1.1\r\n",
        b"GET /x?code=a&state=x HTTP/1.1\r\n",
        b"GET /?code=%ff&state=x HTTP/1.1\r\n",
        b"",
        b"\xff\xfe\xfd",
    ] {
        assert_eq!(parse_request(ignored), Callback::Ignore, "{}", String::from_utf8_lossy(ignored));
    }
}

#[test]
fn percent_codec_round_trip_and_junk() {
    for s in ["", "a b+c", "é/?&=%", "\u{0}\u{7f}~-._"] {
        assert_eq!(percent_decode(&percent_encode(s)).as_deref(), Some(s));
    }
    assert_eq!(percent_decode("%").as_deref(), Some("%"));
    assert_eq!(percent_decode("%4").as_deref(), Some("%4"));
    assert_eq!(percent_decode("%zz%41").as_deref(), Some("%zzA"));
    assert_eq!(percent_decode("%C3"), None);
}

/// Deterministic xorshift for the fuzz loops.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn fuzz_loopback_request_parser_never_panics() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let seed = b"GET /?code=mock-code-1&state=abc%20def&error_description=x%C3%A9 HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let alphabet = b"GET /?&=%+#\r\n HTTP/1.1codestateerror0123456789abcdefABCDEF\xff\xc3\xa9\x00";
    for i in 0..20_000 {
        let mut buf: Vec<u8> = if i % 2 == 0 { seed.to_vec() } else { Vec::new() };
        if i % 2 == 0 {
            for _ in 0..rng.below(8) + 1 {
                let len = buf.len();
                match rng.below(4) {
                    0 if len > 0 => buf[rng.below(len)] = rng.next() as u8,
                    1 if len > 0 => {
                        buf.remove(rng.below(len));
                    }
                    2 => buf.insert(rng.below(len + 1), alphabet[rng.below(alphabet.len())]),
                    _ => buf.truncate(rng.below(len + 1)),
                }
            }
        } else {
            for _ in 0..rng.below(300) {
                let b = if rng.below(3) == 0 { rng.next() as u8 } else { alphabet[rng.below(alphabet.len())] };
                buf.push(b);
            }
        }
        let _ = parse_request(&buf);
        let s = String::from_utf8_lossy(&buf);
        let _ = parse_query(&s);
        let _ = percent_decode(&s);
        let _ = id_token_identity(&s);
        let _ = parse_token_response(400, &s);
    }
}
