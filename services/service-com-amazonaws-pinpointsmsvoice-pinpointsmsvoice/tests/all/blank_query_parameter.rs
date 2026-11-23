// SPDX-License-Identifier: CC0-1.0

//! What `ListConfigurationSets` does with a query parameter that is PRESENT but carries
//! only whitespace.
//!
//! RULE: a parameter bound NOTHING only when it is absent or carries an empty value, and
//! only then does this service's own default apply; a value that is present but BLANK did
//! bind, so it reached this service's own validation, which cannot use it — the request is
//! refused with the modelled `BadRequestException` (HTTP 400) in the class that parameter
//! belongs to, exactly as any other unusable value of it is. Padding around a value with
//! content is not part of the value, so such a value is still read.
//!
//! For a cursor this matters beyond the wording: a `NextToken` is only ever what this
//! service minted, and nothing it mints is whitespace, so honouring a blanked-out cursor as
//! "no cursor" would silently restart a paginating client at the beginning of the collection
//! instead of telling it the token is unusable.
//!
//! The rows differ along what the rule generalises over: WHICH query parameter carries the
//! value (the cursor, whose refusal this service names, and the page bound, whose refusal
//! carries no sentence), and the FORM of the value (absent, empty, one whitespace byte,
//! several of them, a whitespace character outside ASCII, content padded with whitespace,
//! and content alone).
//!
//! The suite drives the real `restJson1` router, so these are the bytes an AWS SDK
//! `pinpoint-sms-voice` client sends and the body it parses.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The emulator under test: the service mounted on the production router over one shared
/// engine.
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

    /// The detail text under the member this service reports it in.
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }

    /// The names the listing reported, in the order they travelled.
    fn listed_names(&self) -> Vec<String> {
        self.body
            .get("ConfigurationSets")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The cursor the listing handed back for the rest of the collection, if any.
    fn next_token(&self) -> Option<&str> {
        self.body.get("NextToken").and_then(Value::as_str)
    }
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's
        // own concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // The listing is account-scoped, but the creates that fill the store are
        // resource-scoped, so the account holds this legacy API's entitlement. The
        // entitlement itself is pinned in `legacy_surface_authorization.rs`.
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router }
    }

    /// Send one request and read the answer back. The `Authorization` credential scope
    /// names `sms-voice`, this service's sigv4 signing name, which is how the router picks
    /// the service that claims it.
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

    /// List with the given query string, which the rows below write themselves so a value
    /// can carry percent-encoded bytes.
    async fn list_sets(&self, query: &str) -> Answer {
        self.call(
            "GET",
            &format!("/v1/sms-voice/configuration-sets{query}"),
            None,
        )
        .await
    }

    /// Fill the store with `STORED_SETS`, so every row below reads the same collection.
    async fn with_stored_sets() -> Self {
        let harness = Self::new();
        for name in STORED_SETS {
            let created = harness
                .call(
                    "POST",
                    "/v1/sms-voice/configuration-sets",
                    Some(json!({ "ConfigurationSetName": name })),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "setup {name:?}: the set must be created, got {} {}",
                created.status, created.body
            );
        }
        harness
    }
}

/// The collection every row lists, in the order the store keeps it (a `BTreeMap`, so the
/// listing order is the names sorted). Fewer than the server's own default page bound, so a
/// request that names no usable bound answers all of them.
const STORED_SETS: [&str; 4] = ["set-a", "set-b", "set-c", "set-d"];

/// The sentence this service words for a `NextToken` its own validation cannot use — the
/// lower-camel spelling of the modelled member, as `refusal_wording` renders it.
const INVALID_NEXT_TOKEN: &str = "The following parameter is null, empty or invalid: ['nextToken']";

/// What the service must answer for one value of one query parameter.
#[derive(Debug)]
enum Expected {
    /// Served: how many of `STORED_SETS` the answer carries, in order.
    Page(usize),
    /// Refused with the modelled `BadRequestException` — `Some(sentence)` where this
    /// service names the parameter, `None` where that class of refusal carries no sentence
    /// at all and the member travels as JSON `null`.
    Refused(Option<&'static str>),
}

/// Every form a caller can write into either query parameter, and the answer it earns.
fn rows() -> Vec<(&'static str, String, Expected)> {
    vec![
        // ---- the cursor: a value that bound nothing starts the listing over ----------
        (
            "no NextToken at all",
            String::new(),
            Expected::Page(STORED_SETS.len()),
        ),
        (
            "an EMPTY NextToken, which bound no value",
            "?NextToken=".to_string(),
            Expected::Page(STORED_SETS.len()),
        ),
        // ---- the cursor: a value that is present but blank is refused ----------------
        (
            "a NextToken of one space",
            "?NextToken=%20".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
        (
            "a NextToken of one tab",
            "?NextToken=%09".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
        (
            "a NextToken of several whitespace bytes",
            "?NextToken=%20%09%0A%0D".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
        (
            // `%E2%80%83` is U+2003 EM SPACE: whitespace that is not an ASCII byte.
            "a NextToken of whitespace outside ASCII",
            "?NextToken=%E2%80%83".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
        (
            // The control this service already refuses: a value it never minted.
            "a NextToken this service never minted",
            "?NextToken=0g".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
        // ---- the page bound: a value that bound nothing takes the server default -----
        (
            "no PageSize at all",
            String::new(),
            Expected::Page(STORED_SETS.len()),
        ),
        (
            "an EMPTY PageSize, which bound no value",
            "?PageSize=".to_string(),
            Expected::Page(STORED_SETS.len()),
        ),
        // ---- the page bound: a value that is present but blank is refused ------------
        (
            "a PageSize of one space",
            "?PageSize=%20".to_string(),
            Expected::Refused(None),
        ),
        (
            "a PageSize of one tab",
            "?PageSize=%09".to_string(),
            Expected::Refused(None),
        ),
        (
            "a PageSize of several whitespace bytes",
            "?PageSize=%20%20%0A".to_string(),
            Expected::Refused(None),
        ),
        (
            "a PageSize of whitespace outside ASCII",
            "?PageSize=%E2%80%83".to_string(),
            Expected::Refused(None),
        ),
        // ---- content, with and without padding, is read either way ------------------
        (
            "a PageSize that is a count",
            "?PageSize=2".to_string(),
            Expected::Page(2),
        ),
        (
            "a PageSize padded with spaces around a count",
            "?PageSize=%202%20".to_string(),
            Expected::Page(2),
        ),
        (
            "a PageSize padded with a tab and a newline around a count",
            "?PageSize=%093%0A".to_string(),
            Expected::Page(3),
        ),
        // ---- both parameters blank: one refusal, worded by the class that names ------
        (
            "both query parameters blank",
            "?NextToken=%20&PageSize=%20".to_string(),
            Expected::Refused(Some(INVALID_NEXT_TOKEN)),
        ),
    ]
}

/// RULE: only an absent or empty query parameter bound nothing, so only those take this
/// service's own default; a present but BLANK value reached this service's validation and is
/// refused with the modelled `BadRequestException`, while padding around content is not part
/// of the value.
#[tokio::test]
async fn a_blank_query_parameter_is_refused_and_an_empty_one_bound_nothing() {
    let harness = Harness::with_stored_sets().await;

    for (case, query, expected) in rows() {
        let answer = harness.list_sets(&query).await;
        match expected {
            Expected::Page(count) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?} ({query:?}): expected HTTP 200, got {} {}",
                    answer.status, answer.body
                );
                let names: Vec<String> = STORED_SETS
                    .iter()
                    .take(count)
                    .map(|name| (*name).to_string())
                    .collect();
                assert_eq!(
                    answer.listed_names(),
                    names,
                    "case {case:?} ({query:?}): expected {names:?}, got {}",
                    answer.body
                );
            }
            Expected::Refused(sentence) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?} ({query:?}): expected HTTP 400, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code(),
                    "BadRequestException",
                    "case {case:?} ({query:?}): expected the modelled BadRequestException, got {}",
                    answer.body
                );
                assert_eq!(
                    answer.message(),
                    sentence,
                    "case {case:?} ({query:?}): expected the message {sentence:?}, got {}",
                    answer.body
                );
                // A refusal is not a listing: no page can have travelled with it.
                assert!(
                    answer.listed_names().is_empty(),
                    "case {case:?} ({query:?}): a refusal must carry no page, got {}",
                    answer.body
                );
            }
        }
    }
}

/// RULE: a blanked-out cursor is refused rather than read as "no cursor", so a paginating
/// client is never silently restarted at the beginning of the collection — while the cursor
/// this service DID mint still resumes the listing, padding around it included.
#[tokio::test]
async fn a_blanked_out_cursor_never_restarts_the_listing() {
    let harness = Harness::with_stored_sets().await;

    let first = harness.list_sets("?PageSize=1").await;
    assert_eq!(
        first.status, 200,
        "case \"the first page\": expected HTTP 200, got {} {}",
        first.status, first.body
    );
    assert_eq!(
        first.listed_names(),
        vec![STORED_SETS[0].to_string()],
        "case \"the first page\": expected the first set alone, got {}",
        first.body
    );
    let token = first
        .next_token()
        .expect("a bounded first page must report a cursor for the rest")
        .to_string();

    // Each blanked-out form of that cursor: refused, and in particular NOT the first page
    // again.
    for (case, sent) in [
        ("the cursor blanked out with a space", "%20".to_string()),
        ("the cursor blanked out with a tab", "%09".to_string()),
        (
            "the cursor blanked out with several whitespace bytes",
            "%20%0A%09".to_string(),
        ),
    ] {
        let answer = harness
            .list_sets(&format!("?NextToken={sent}&PageSize=1"))
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.message(),
            Some(INVALID_NEXT_TOKEN),
            "case {case:?}: expected the message {INVALID_NEXT_TOKEN:?}, got {}",
            answer.body
        );
        assert!(
            answer.listed_names().is_empty(),
            "case {case:?}: the listing must not restart, got {}",
            answer.body
        );
    }

    // The minted cursor still resumes exactly after the page it was handed out for, with
    // or without padding around it — padding is not part of a value that has content.
    for (case, sent) in [
        ("the minted cursor", token.clone()),
        ("the minted cursor padded with spaces", format!("%20{token}%20")),
        ("the minted cursor padded with a tab", format!("%09{token}")),
    ] {
        let answer = harness
            .list_sets(&format!("?NextToken={sent}&PageSize=1"))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.listed_names(),
            vec![STORED_SETS[1].to_string()],
            "case {case:?}: expected the set after the first page, got {}",
            answer.body
        );
    }
}

/// RULE (the control the rows above sit beside): a refused query parameter changes nothing —
/// the collection a later caller reads is the one that was stored, whatever was refused
/// before it.
#[tokio::test]
async fn a_refused_query_parameter_changes_nothing_for_a_later_caller() {
    let harness = Harness::with_stored_sets().await;
    let expected: Vec<String> = STORED_SETS
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    for sent in [
        "?NextToken=%20",
        "?PageSize=%20",
        "?NextToken=%09&PageSize=%0A",
    ] {
        let refused = harness.list_sets(sent).await;
        assert_eq!(
            refused.status, 400,
            "case {sent:?}: expected HTTP 400, got {} {}",
            refused.status, refused.body
        );

        let listed = harness.list_sets("").await;
        assert_eq!(
            listed.status, 200,
            "case {sent:?}: the follow-up listing must succeed, got {} {}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.listed_names(),
            expected,
            "case {sent:?}: expected {expected:?} after the refusal, got {}",
            listed.body
        );
    }
}
