// SPDX-License-Identifier: CC0-1.0

//! `ListTunnels` serves a continuation token only when it is one THIS store minted.
//!
//! The rule under test: the position a page cursor names travels INSIDE the token —
//! nothing is kept on the side to look up — so a listing that checks only the
//! token's SHAPE honours any value of that shape and a caller can name a position
//! the listing never offered (past the newest tunnel, which restarts the walk;
//! before the oldest, which empties the page). What this service answers for such a
//! value is the modelled validation refusal, so a `nextToken` is accepted only when
//! it is this service's own rendering of a cursor authenticated under the secret the
//! store drew from the system's randomness and never reveals, and every other string
//! is refused with HTTP 400 `ValidationException` and no page at all.
//!
//! Every case is a row in a table, each row derived from a token this service really
//! minted over the wire. The rows differ along what the rule generalises over:
//!
//! * the FORM of the string — re-cased, zero-stripped, over-padded, truncated,
//!   re-prefixed, non-hex, hand-written (`list_tunnels_refuses_every_non_canonical_next_token`);
//! * the CONTENT behind a minted token's exact shape — the cursor's sequence field
//!   rewritten to a position the listing never issued, its scope field rewritten,
//!   single bytes flipped in the cursor and in the authenticator, an invented
//!   authenticator, a truncated and an extended token, another store's secret, and
//!   this store's secret over another listing's scope
//!   (`list_tunnels_refuses_a_token_this_store_did_not_authenticate`).
//!
//! Which listing a token belongs to is the subject of
//! `list_tunnels_page_token_scope.rs`; every row here is signed for one Region.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotsecuretunneling_iotsecuredtunneling::state::{
    encode_next_token, service_state, PageTokenKey, TunnelListing, AUTHENTICATOR_BYTES,
    NEXT_TOKEN_OTHER_LISTING_MESSAGE, NEXT_TOKEN_UNISSUED_MESSAGE,
};
use service_com_amazonaws_iotsecuretunneling_iotsecuredtunneling::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "eu-west-1";

/// The prefix this service renders every page token with.
const PREFIX: &str = "tnl-";

/// A router carrying only this service, over the engine the test also inspects.
fn mount(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// The secret THIS engine's store authenticates its page cursors under, so a row can
/// mint a cursor the way the store mints one — the case only the scope can refuse.
fn store_key(engine: &Arc<engine::InMemoryEngine>) -> PageTokenKey {
    service_state(&**engine)
        .expect("the IoT Secure Tunneling store is mounted on this engine")
        .lock()
        .page_token_key()
}

/// Call one operation the way an SDK does and return `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, input: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(input).expect("serialize the request body");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("IoTSecuredTunneling.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/IoTSecuredTunneling/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Whether `token` satisfies `NextToken`'s modelled pattern
/// `^[a-zA-Z0-9_=-]{1,4096}$` (from
/// `smithy_query(query="operation", operation="ListTunnels")`), so a row is
/// known to reach the handler instead of stopping at the boundary validator.
fn is_modelled_next_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4096
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_=-".contains(c))
}

/// Open `count` tunnels and return their ids in the order they were opened.
async fn open_tunnels(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let input = json!({
            "destinationConfig": {"thingName": format!("thing-{index}"), "services": ["SSH"]}
        });
        let (status, opened) = call(router, "OpenTunnel", &input).await;
        assert_eq!(status, 200, "OpenTunnel #{index} answered {opened:?}");
        ids.push(
            opened["tunnelId"]
                .as_str()
                .unwrap_or_else(|| panic!("OpenTunnel #{index} reported no tunnelId: {opened:?}"))
                .to_string(),
        );
    }
    ids
}

/// The token a listing of page size `page_size` hands back, or `None` when the
/// listing fits in one page.
async fn minted_token(router: &MultiServiceRouter, page_size: i64) -> Option<String> {
    let (status, listed) = call(router, "ListTunnels", &json!({"maxResults": page_size})).await;
    assert_eq!(
        status, 200,
        "page size {page_size}: ListTunnels answered {listed:?}"
    );
    listed
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The ids a listing resumed with `token` reports, newest first.
async fn page_after(router: &MultiServiceRouter, token: &str) -> Vec<String> {
    let (status, listed) = call(router, "ListTunnels", &json!({"nextToken": token})).await;
    assert_eq!(
        status, 200,
        "token {token:?}: a token this service minted must be accepted, answered {listed:?}"
    );
    listed["tunnelSummaries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|summary| summary["tunnelId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The bytes a minted token carries: its hex digits, decoded.
fn token_bytes(token: &str) -> Vec<u8> {
    let digits = token
        .strip_prefix(PREFIX)
        .unwrap_or_else(|| panic!("minted token {token:?} carries no {PREFIX:?} prefix"));
    ::hex::decode(digits).unwrap_or_else(|_| panic!("minted token {token:?} is not hex"))
}

/// `bytes` rendered the way this service renders a token: the prefix, then the bytes
/// in lowercase hex.
fn render(bytes: &[u8]) -> String {
    format!("{PREFIX}{}", ::hex::encode(bytes))
}

/// The cursor a minted token carries, as the string it was authenticated over.
fn cursor_of(token: &str) -> String {
    let bytes = token_bytes(token);
    assert!(
        bytes.len() > AUTHENTICATOR_BYTES,
        "minted token {token:?}: expected an authenticator and a cursor, got {} bytes",
        bytes.len()
    );
    String::from_utf8(bytes[AUTHENTICATOR_BYTES..].to_vec())
        .unwrap_or_else(|_| panic!("minted token {token:?}: its cursor must be UTF-8"))
}

/// Every token this service mints is its own canonical rendering — the `tnl-`
/// prefix, then an authenticator over the cursor and the cursor itself, in
/// lowercase hex — and it resumes that listing exactly where its page stopped.
#[tokio::test]
async fn a_minted_next_token_is_canonical_and_resumes_the_listing() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = mount(&engine);
    let key = store_key(&engine);

    // Eleven tunnels, so the listing pages at several positions.
    let opened = open_tunnels(&router, 11).await;
    let newest_first: Vec<String> = opened.iter().rev().cloned().collect();

    // Rows: each page size up to and past the one that pages, so the token is
    // minted at several positions in the listing.
    for page_size in 1..=4_i64 {
        let token = minted_token(&router, page_size)
            .await
            .unwrap_or_else(|| panic!("page size {page_size}: the listing minted no nextToken"));
        let digits = token.strip_prefix(PREFIX).unwrap_or_else(|| {
            panic!("page size {page_size}: minted token {token:?} carries no 'tnl-' prefix")
        });
        assert!(
            digits.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "page size {page_size}: minted token {token:?} must be lowercase hex"
        );
        assert!(
            is_modelled_next_token(&token),
            "page size {page_size}: minted token {token:?} must match the modelled pattern"
        );
        let bytes = token_bytes(&token);
        assert!(
            bytes.len() > AUTHENTICATOR_BYTES,
            "page size {page_size}: minted token {token:?} must carry an authenticator over a \
             cursor, got {} bytes",
            bytes.len()
        );
        assert_eq!(
            render(&bytes),
            token,
            "page size {page_size}: a minted token must be its own canonical rendering"
        );
        // The authenticator is this store's own over exactly the cursor the token
        // carries, which is what no caller can produce: mint the same cursor under
        // the store's secret and the whole token comes out identical.
        let cursor = cursor_of(&token);
        assert_eq!(
            render(&key.mint(&cursor)),
            token,
            "page size {page_size}: the token must be the cursor {cursor:?} authenticated under \
             this store's secret"
        );

        let resumed = page_after(&router, &token).await;
        let expected: Vec<String> = newest_first
            .iter()
            .skip(usize::try_from(page_size).unwrap_or(1))
            .cloned()
            .collect();
        assert_eq!(
            resumed, expected,
            "page size {page_size}: the minted token must resume after its page, got {resumed:?}"
        );
    }
}

/// A string whose FORM is not this service's rendering of a token is refused with a
/// 400 `ValidationException` and serves no page, however close to a real cursor it
/// looks.
#[tokio::test]
async fn list_tunnels_refuses_every_non_canonical_next_token() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = mount(&engine);

    open_tunnels(&router, 11).await;
    let token = minted_token(&router, 1)
        .await
        .expect("a listing of eleven tunnels at page size 1 mints a nextToken");
    let digits = token
        .strip_prefix(PREFIX)
        .expect("a minted token carries the 'tnl-' prefix")
        .to_string();

    // One row per way a cursor can be garbled, each a function of the token the
    // service really minted: (label, forged token).
    let forged: Vec<(&str, String)> = vec![
        ("the digits re-cased to upper case", format!("tnl-{}", digits.to_ascii_uppercase())),
        ("one extra digit", format!("tnl-0{digits}")),
        ("four extra digits", format!("tnl-0000{digits}")),
        ("the last digit dropped", format!("tnl-{}", &digits[..digits.len() - 1])),
        ("the first digit dropped", format!("tnl-{}", &digits[1..])),
        ("every digit dropped", "tnl-".to_string()),
        ("the prefix dropped", digits.clone()),
        ("the prefix re-cased", format!("TNL-{digits}")),
        ("the prefix doubled", format!("tnl-{token}")),
        (
            "a non-hex digit substituted",
            format!("tnl-{}z", &digits[..digits.len() - 1]),
        ),
        ("a trailing '=' appended", format!("{token}=")),
        ("a token-shaped string of another service", "ListTunnels-0".to_string()),
    ];

    for (label, candidate) in forged {
        assert_ne!(
            candidate, token,
            "case {label:?}: the row must differ from the minted token {token:?}"
        );
        assert!(
            is_modelled_next_token(&candidate),
            "case {label:?}: the row must match the modelled pattern so it reaches the handler, \
             got {candidate:?}"
        );
        let (status, body) = call(&router, "ListTunnels", &json!({"nextToken": candidate})).await;
        assert_eq!(
            status, 400,
            "case {label:?} ({candidate:?}): a token this service cannot mint must be refused, \
             answered {body:?}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some("ValidationException"),
            "case {label:?} ({candidate:?}): answered {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(NEXT_TOKEN_UNISSUED_MESSAGE),
            "case {label:?} ({candidate:?}): expected this service's one wording for a token it \
             did not issue, answered {:?}",
            body["message"]
        );
        assert!(
            body.get("tunnelSummaries").is_none(),
            "case {label:?} ({candidate:?}): a refusal must serve no page, answered {body:?}"
        );
    }

    // The minted token itself is still accepted, so the refusals above are
    // about the form of the string and not about paging being broken.
    let (status, listed) = call(&router, "ListTunnels", &json!({"nextToken": token})).await;
    assert_eq!(
        status, 200,
        "the minted token {token:?} must still be accepted, answered {listed:?}"
    );
}

/// A value with a minted token's exact SHAPE — the prefix, an authenticator-width
/// head and a cursor behind it, all canonical hex — is served only when this store
/// authenticated that very cursor: a cursor rewritten to a position the listing
/// never issued, or carried behind an authenticator the caller produced, is refused
/// with a 400 `ValidationException` and no page.
#[tokio::test]
async fn list_tunnels_refuses_a_token_this_store_did_not_authenticate() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = mount(&engine);
    let key = store_key(&engine);

    let opened = open_tunnels(&router, 11).await;
    let newest_first: Vec<String> = opened.iter().rev().cloned().collect();
    let token = minted_token(&router, 1)
        .await
        .expect("a listing of eleven tunnels at page size 1 mints a nextToken");
    let bytes = token_bytes(&token);
    let authenticator = bytes[..AUTHENTICATOR_BYTES].to_vec();
    let cursor = cursor_of(&token);

    // The cursor's LAST field is the creation sequence the page stopped at, and
    // what precedes it is the scope of the listing that minted it: the rows below
    // rewrite each half while leaving the authenticator exactly as it came, which
    // is what a caller holding one real token can do.
    let (scope, sequence) = cursor
        .rsplit_once('\u{1f}')
        .unwrap_or_else(|| panic!("the cursor {cursor:?} must carry a scope and a sequence"));
    let seq: u64 = sequence
        .parse()
        .unwrap_or_else(|_| panic!("the cursor {cursor:?} must end in a creation sequence"));

    /// A token carrying `cursor` behind `authenticator`, with nothing recomputed.
    fn behind(authenticator: &[u8], cursor: &str) -> String {
        render(&[authenticator, cursor.as_bytes()].concat())
    }

    let mut rows: Vec<(String, String)> = vec![
        (
            "the sequence field rewritten to the largest u64, which would restart the listing"
                .to_string(),
            behind(&authenticator, &format!("{scope}\u{1f}{}", u64::MAX)),
        ),
        (
            "the sequence field rewritten to zero, which would empty the page".to_string(),
            behind(&authenticator, &format!("{scope}\u{1f}0")),
        ),
        (
            "the sequence field rewritten one position later".to_string(),
            behind(&authenticator, &format!("{scope}\u{1f}{}", seq.saturating_add(1))),
        ),
        (
            "the sequence field rewritten one position earlier".to_string(),
            behind(&authenticator, &format!("{scope}\u{1f}{}", seq.saturating_sub(1))),
        ),
        (
            "the scope field rewritten to another Region".to_string(),
            behind(&authenticator, &format!("us-east-1\u{1f}0\u{1f}{seq}")),
        ),
        (
            "the cursor sent without its authenticator".to_string(),
            render(cursor.as_bytes()),
        ),
        (
            "an all-zero authenticator over the real cursor".to_string(),
            behind(&[0_u8; AUTHENTICATOR_BYTES], &cursor),
        ),
        (
            "an all-ones authenticator over the real cursor".to_string(),
            behind(&[0xff_u8; AUTHENTICATOR_BYTES], &cursor),
        ),
        (
            "the token with its last byte dropped".to_string(),
            render(&bytes[..bytes.len() - 1]),
        ),
        (
            "the token with a byte appended".to_string(),
            render(&[bytes.as_slice(), b"0"].concat()),
        ),
        (
            "another store's secret over this listing's own cursor".to_string(),
            encode_next_token(
                &PageTokenKey::new(),
                TunnelListing {
                    region: REGION,
                    thing_name: None,
                },
                seq,
            ),
        ),
    ];
    // Every single-byte change to a minted token, in the authenticator and in the
    // cursor alike: a token differs from one this store minted anywhere at all, and
    // the refusal is the same.
    for index in 0..bytes.len() {
        let mut tampered = bytes.clone();
        tampered[index] ^= 0x01;
        rows.push((format!("byte {index} flipped"), render(&tampered)));
    }

    for (case, candidate) in &rows {
        assert_ne!(
            candidate, &token,
            "case {case:?}: the row must differ from the minted token {token:?}"
        );
        assert!(
            is_modelled_next_token(candidate),
            "case {case:?}: the row must match the modelled pattern so it reaches the handler, \
             got {candidate:?}"
        );
        let (status, body) = call(&router, "ListTunnels", &json!({"nextToken": candidate})).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({candidate:?}): a position this listing never issued must be refused, \
             answered {body:?}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some("ValidationException"),
            "case {case:?} ({candidate:?}): answered {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(NEXT_TOKEN_UNISSUED_MESSAGE),
            "case {case:?} ({candidate:?}): expected this service's one wording for a token it \
             did not issue, answered {:?}",
            body["message"]
        );
        assert!(
            body.get("tunnelSummaries").is_none(),
            "case {case:?} ({candidate:?}): a refusal must serve no page, answered {body:?}"
        );
    }

    // This store's own secret over the scope of a DIFFERENT listing of the same
    // Region: the scope is inside the authenticated cursor, so the token is one
    // this store minted and still names no position here — refused with the
    // wording for a token of another listing.
    let other_listing = encode_next_token(
        &key,
        TunnelListing {
            region: REGION,
            thing_name: Some("thing-1"),
        },
        seq,
    );
    let (status, body) = call(
        &router,
        "ListTunnels",
        &json!({"nextToken": other_listing}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"this store's secret over another listing's scope\": expected HTTP 400, answered \
         {body:?}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(NEXT_TOKEN_OTHER_LISTING_MESSAGE),
        "case \"this store's secret over another listing's scope\": expected the wording for a \
         token of another listing, answered {:?}",
        body["message"]
    );

    // The real token still resumes the listing where its page stopped, so the
    // refusals above are about the cursor's authenticity and not about paging.
    assert_eq!(
        page_after(&router, &token).await,
        newest_first[1..].to_vec(),
        "the minted token {token:?} must still resume after its own page"
    );
}
