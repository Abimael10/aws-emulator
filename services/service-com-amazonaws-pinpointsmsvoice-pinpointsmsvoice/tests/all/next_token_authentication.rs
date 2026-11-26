// SPDX-License-Identifier: CC0-1.0

//! A `ListConfigurationSets` `NextToken` names a position in THIS store's listing only
//! when this store issued it, driven end to end over the real `restJson1` router.
//!
//! The position a continuation token names travels inside the token, so a read that only
//! checks the token's SHAPE honours any value of that shape: a caller that assembles one
//! resumes the listing after a position the service never handed out. What this service
//! documents for a token it did not mint is a refusal. The suites here assert that rule
//! over a table of assembled candidates — every one of them built the way a caller can
//! build it, from text in the open and from a key of the caller's own — and the other
//! half of it, that a token this store DID mint resumes exactly where it says.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

use service_common::page_tokens::{PageTokenKey, AUTHENTICATOR_BYTES};

/// The sentence this service words for a `NextToken` its validation cannot use; the
/// member name is the wire spelling, as pinned in `parameter_refusal_wording.rs`.
const INVALID_NEXT_TOKEN: &str = "The following parameter is null, empty or invalid: ['nextToken']";

/// The cursor prefixes a caller can assemble a token-shaped value from.
///
/// This is the only part of a minted token that is written in the open (the handler's
/// `TOKEN_SCOPE` constant, which carries the collection and the layout version), so it
/// is exactly what a caller reads off the source: the rows cover the live spelling, the
/// neighbouring layout versions, and no prefix at all.
const ASSEMBLED_PREFIXES: &[&str] = &[
    "pinpointsmsvoice:configuration-sets:2:",
    "pinpointsmsvoice:configuration-sets:1:",
    "pinpointsmsvoice:configuration-sets:0:",
    "pinpointsmsvoice:configuration-sets:3:",
    "pinpointsmsvoice:configuration-sets:",
    "",
];

/// The configuration sets every suite here stores, in listing order.
const STORED_SETS: &[&str] = &["set-a", "set-b", "set-c", "set-d", "set-e"];

/// The emulator under test: the service mounted on the production router over one engine.
struct Harness {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status and the parsed JSON body (`Value::Null` when empty).
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The `__type` discriminator of a `restJson1` error envelope, or `""` on a success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `Message` the refusal carries, or `""` when it carries none.
    ///
    /// Read under the wire spelling this service words its detail with
    /// (`crate::service::WIRE_DETAIL_MEMBER`, as pinned in `refusal_detail_member.rs`).
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `ConfigurationSets` member of a successful listing.
    fn listed(&self) -> Vec<String> {
        self.body
            .get("ConfigurationSets")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .map(|name| name.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `NextToken` the listing handed back, if any.
    fn next_token(&self) -> Option<String> {
        self.body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

impl Harness {
    /// A fresh emulator holding [`STORED_SETS`], each created over the wire.
    async fn with_stored_sets() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Unsigned requests: signature verification is the framework's own concern and is
        // covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        let harness = Self { router };
        for name in STORED_SETS {
            let answer = harness.create_set(name).await;
            assert_eq!(
                answer.status, 200,
                "case {name:?}: creating the set must answer 200, got {} {}",
                answer.status, answer.body
            );
        }
        harness
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(value.to_string())
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://sms-voice.us-east-1.amazonaws.com{path}"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sms-voice/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .header("content-type", "application/json")
            .body(payload)
            .expect("the test request must build");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    async fn create_set(&self, name: &str) -> Answer {
        self.call(
            "POST",
            "/v1/sms-voice/configuration-sets",
            Some(json!({ "ConfigurationSetName": name })),
        )
        .await
    }

    async fn list_sets(&self, query: &str) -> Answer {
        self.call(
            "GET",
            &format!("/v1/sms-voice/configuration-sets{query}"),
            None,
        )
        .await
    }
}

/// Every candidate `NextToken` a caller can assemble without this store's key, named.
///
/// The rows span what "assembled" can mean: a cursor written from the prefix in the open
/// (at every position in the collection, and at one name the store does not hold), the
/// same cursor behind an authenticator of the right WIDTH but the caller's choosing, and
/// the same cursor minted under a key of the caller's own — which is byte for byte the
/// shape of a token this service hands out, and the case only the authenticator can
/// refuse.
fn assembled_tokens() -> Vec<(String, String)> {
    // Positions in the listing: each stored name, a name the store does not hold, and
    // the empty cursor, which would resume from the very beginning.
    let positions: Vec<String> = STORED_SETS
        .iter()
        .map(|name| (*name).to_string())
        .chain(["set-zz".to_string(), String::new()])
        .collect();
    let caller_key = PageTokenKey::new();
    let mut rows = Vec::new();
    for prefix in ASSEMBLED_PREFIXES {
        for position in &positions {
            let cursor = format!("{prefix}{position}");
            // An empty cursor in the clear encodes to the empty string, which binds no
            // value at all — that is the no-token case, covered where the blank and empty
            // forms are (`blank_query_parameter.rs`), not a token a caller assembled.
            if !cursor.is_empty() {
                rows.push((
                    format!("the cursor {cursor:?} in the clear"),
                    ::hex::encode(cursor.as_bytes()),
                ));
            }
            for (shape, authenticator) in [
                ("all-zero", vec![0_u8; AUTHENTICATOR_BYTES]),
                ("all-ones", vec![0xff_u8; AUTHENTICATOR_BYTES]),
            ] {
                rows.push((
                    format!("the cursor {cursor:?} behind an {shape} authenticator"),
                    ::hex::encode([authenticator.as_slice(), cursor.as_bytes()].concat()),
                ));
            }
            rows.push((
                format!("the cursor {cursor:?} minted under the caller's own key"),
                ::hex::encode(caller_key.mint(&cursor)),
            ));
        }
    }
    // Values that are not a cursor at all, including one of exactly the authenticator's
    // width and one a single byte short of it.
    for (name, raw) in [
        ("not hexadecimal", "not-a-token".to_string()),
        ("an odd number of hex digits", "0g".to_string()),
        ("hex of bytes that are not UTF-8", "ffff".to_string()),
        (
            "hex of exactly the authenticator's width",
            ::hex::encode(vec![7_u8; AUTHENTICATOR_BYTES]),
        ),
        (
            "hex of one byte less than the authenticator's width",
            ::hex::encode(vec![7_u8; AUTHENTICATOR_BYTES - 1]),
        ),
    ] {
        rows.push((name.to_string(), raw));
    }
    rows
}

/// RULE: a `NextToken` this store did not issue names no position in its listing, so
/// `ListConfigurationSets` refuses it (HTTP 400 `BadRequestException`) instead of serving
/// a page resumed after a position it never handed out.
#[tokio::test]
async fn a_next_token_this_store_did_not_issue_is_refused() {
    let harness = Harness::with_stored_sets().await;
    let all: Vec<String> = STORED_SETS.iter().map(|name| (*name).to_string()).collect();

    for (case, token) in assembled_tokens() {
        let answer = harness.list_sets(&format!("?NextToken={token}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} (NextToken={token}): expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "BadRequestException",
            "case {case:?} (NextToken={token}): expected BadRequestException, got {}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            INVALID_NEXT_TOKEN,
            "case {case:?} (NextToken={token}): expected the unusable-parameter sentence, got {:?}",
            answer.message()
        );
        assert!(
            answer.listed().is_empty(),
            "case {case:?} (NextToken={token}): a refusal must carry no page, got {:?}",
            answer.listed()
        );
    }

    // None of those refusals consumed or moved anything: the listing still answers the
    // whole collection from the beginning.
    let answer = harness.list_sets("").await;
    assert_eq!(
        answer.status, 200,
        "case \"the listing after every refusal\": expected HTTP 200, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.listed(),
        all,
        "case \"the listing after every refusal\": expected {all:?}, got {:?}",
        answer.listed()
    );
}

/// RULE: a `NextToken` is only ever readable by the store that minted it — the same
/// cursor issued by another store of the same service names no position here — and every
/// alteration of an issued token on the way back is refused rather than seeking somewhere
/// else.
#[tokio::test]
async fn an_issued_token_is_readable_only_by_the_store_that_issued_it() {
    let harness = Harness::with_stored_sets().await;
    let other = Harness::with_stored_sets().await;

    // The token each store issues for the same first page: the cursor inside them is the
    // same position in the same collection, so only the key tells them apart.
    let issued = harness
        .list_sets("?PageSize=2")
        .await
        .next_token()
        .unwrap_or_else(|| panic!("case \"the first page\": expected a NextToken for the rest"));
    let elsewhere = other
        .list_sets("?PageSize=2")
        .await
        .next_token()
        .unwrap_or_else(|| {
            panic!("case \"the first page of the other store\": expected a NextToken")
        });

    // Its own token resumes exactly after the page it ended.
    let resumed = harness
        .list_sets(&format!("?PageSize=2&NextToken={issued}"))
        .await;
    assert_eq!(
        resumed.status, 200,
        "case \"an issued token\": expected HTTP 200, got {} {}",
        resumed.status, resumed.body
    );
    assert_eq!(
        resumed.listed(),
        vec!["set-c".to_string(), "set-d".to_string()],
        "case \"an issued token\": expected the page after the first, got {:?}",
        resumed.listed()
    );

    // The candidates no store minted FOR THIS STORE: the other store's token, and the
    // issued one altered — each byte of the authenticator and of the cursor in turn, at
    // the ends and at their boundary, plus one truncated and one extended.
    let bytes = ::hex::decode(&issued).expect("an issued token is hexadecimal");
    let mut altered: Vec<(String, String)> = vec![(
        "a token another store of this service issued".to_string(),
        elsewhere,
    )];
    for index in [
        0,
        AUTHENTICATOR_BYTES - 1,
        AUTHENTICATOR_BYTES,
        bytes.len() / 2,
        bytes.len() - 1,
    ] {
        let mut tampered = bytes.clone();
        tampered[index] ^= 0x01;
        altered.push((
            format!("an issued token with byte {index} changed"),
            ::hex::encode(tampered),
        ));
    }
    altered.push((
        "an issued token with its last byte dropped".to_string(),
        ::hex::encode(&bytes[..bytes.len() - 1]),
    ));
    altered.push((
        "an issued token with a byte appended".to_string(),
        ::hex::encode([bytes.as_slice(), b"x"].concat()),
    ));

    for (case, token) in altered {
        let answer = harness.list_sets(&format!("?NextToken={token}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            INVALID_NEXT_TOKEN,
            "case {case:?}: expected the unusable-parameter sentence, got {:?}",
            answer.message()
        );
    }
}

/// RULE: walking the pages with the tokens this store issues yields every stored name
/// exactly once, whatever the page bound — the authenticated token is a working cursor,
/// not merely a refusable one.
#[tokio::test]
async fn the_tokens_a_store_issues_walk_the_whole_collection() {
    for page_size in [1_usize, 2, 3, 4, 5] {
        let harness = Harness::with_stored_sets().await;
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            pages += 1;
            assert!(
                pages <= STORED_SETS.len() + 1,
                "case page_size={page_size}: the listing did not terminate after {pages} pages"
            );
            let query = match &token {
                Some(next) => format!("?PageSize={page_size}&NextToken={next}"),
                None => format!("?PageSize={page_size}"),
            };
            let answer = harness.list_sets(&query).await;
            assert_eq!(
                answer.status, 200,
                "case page_size={page_size}: expected HTTP 200, got {} {}",
                answer.status, answer.body
            );
            let page = answer.listed();
            assert!(
                page.len() <= page_size,
                "case page_size={page_size}: a page of {} exceeds the bound",
                page.len()
            );
            walked.extend(page);
            token = answer.next_token();
            if token.is_none() {
                break;
            }
        }

        let expected: Vec<String> = STORED_SETS.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            walked, expected,
            "case page_size={page_size}: walking the issued tokens must yield every name once, got {walked:?}"
        );
    }
}
