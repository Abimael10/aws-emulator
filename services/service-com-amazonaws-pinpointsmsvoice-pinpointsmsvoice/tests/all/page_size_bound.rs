// SPDX-License-Identifier: CC0-1.0

//! What `ListConfigurationSets` reads as a page bound, over the whole class of values a
//! caller can write into `PageSize`.
//!
//! RULE: `PageSize` is modelled as a string, so it is read as a decimal count — an
//! optional `+` followed by ASCII decimal digits — and a count is CLAMPED to the server's
//! maximum page however large it is written. A refusal is reserved for a value that is not
//! a decimal count at all, and for the count zero, which would page nothing; the magnitude
//! of a count never decides between a clamp and a refusal, so the same request is answered
//! the same way whatever width a machine word has on the host serving it.
//!
//! The rows differ along what the rule generalises over: the number of digits (from one to
//! two hundred), the magnitude relative to the server's maximum page and to the counts a
//! 32-bit and a 64-bit word can hold, the leading zeros and surrounding spaces a count may
//! be padded with, the sign it may carry, and — for the refusals — the FORM that makes a
//! value no count at all (fractional, exponent, hexadecimal, grouped, signed negative, a
//! digit of another script, plain text).
//!
//! The suite drives the real `restJson1` router, so these are the bytes an AWS SDK
//! `pinpoint-sms-voice` client sends and the body it parses. The page bound itself
//! (`MAX_PAGE_SIZE`, and that a page never exceeds it) is pinned in
//! `configuration_set_lifecycle.rs`; what is pinned here is WHICH values reach it.

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
/// listing order is the names sorted). Four is enough to tell a bound BELOW the collection
/// from one above it, and small enough that every row is one request.
const STORED_SETS: [&str; 4] = ["set-a", "set-b", "set-c", "set-d"];

/// One `PageSize` this service reads as a count, and the page it must answer with.
struct Served {
    /// What makes this row different from the others.
    name: &'static str,
    /// The value exactly as it travels in the query string, percent-encoded where it
    /// carries a byte a query cannot spell literally.
    sent: String,
    /// How many of `STORED_SETS` the answer must carry, in order.
    page: usize,
}

/// Every count above the server's maximum page, whatever the digits: the server clamps,
/// so each answers the whole (far smaller) collection and no `NextToken`.
fn served_rows() -> Vec<Served> {
    vec![
        Served {
            name: "the smallest count there is",
            sent: "1".to_string(),
            page: 1,
        },
        Served {
            name: "a count below the collection",
            sent: "3".to_string(),
            page: 3,
        },
        Served {
            name: "a count equal to the collection",
            sent: "4".to_string(),
            page: 4,
        },
        Served {
            name: "a count above the collection",
            sent: "9".to_string(),
            page: 4,
        },
        Served {
            name: "the server's own maximum page",
            sent: "1000".to_string(),
            page: 4,
        },
        Served {
            name: "one past the server's maximum page",
            sent: "1001".to_string(),
            page: 4,
        },
        Served {
            name: "a count past what a 32-bit word holds",
            // 2^32: a count a 64-bit host can hold and a 32-bit host cannot, which is the
            // spelling whose answer must not depend on which host serves it.
            sent: "4294967296".to_string(),
            page: 4,
        },
        Served {
            name: "the largest count a 64-bit word holds",
            sent: "18446744073709551615".to_string(),
            page: 4,
        },
        Served {
            name: "one past the largest count a 64-bit word holds",
            sent: "18446744073709551616".to_string(),
            page: 4,
        },
        Served {
            name: "a count of twenty-six digits",
            sent: "99999999999999999999999999".to_string(),
            page: 4,
        },
        Served {
            name: "a count of two hundred digits",
            sent: "9".repeat(200),
            page: 4,
        },
        Served {
            name: "a count written with a leading plus",
            // `%2B` so the `+` travels as a sign and not as a space.
            sent: "%2B7".to_string(),
            page: 4,
        },
        Served {
            name: "a count padded with more leading zeros than any word holds",
            sent: format!("{}2", "0".repeat(40)),
            page: 2,
        },
        Served {
            name: "a count surrounded by spaces",
            sent: "%202%20".to_string(),
            page: 2,
        },
    ]
}

/// RULE: a `PageSize` that is a decimal count is served — bounding the page where the count
/// is smaller than the collection, and clamped to the server's maximum page where it is
/// larger, however many digits it is written with.
#[tokio::test]
async fn a_decimal_count_bounds_the_page_whatever_its_magnitude() {
    let harness = Harness::with_stored_sets().await;

    for row in served_rows() {
        let answer = harness.list_sets(&format!("?PageSize={}", row.sent)).await;
        assert_eq!(
            answer.status, 200,
            "case {:?} (PageSize={}): expected HTTP 200, got {} {}",
            row.name, row.sent, answer.status, answer.body
        );
        let expected: Vec<String> = STORED_SETS
            .iter()
            .take(row.page)
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(
            answer.listed_names(),
            expected,
            "case {:?} (PageSize={}): expected {expected:?}, got {}",
            row.name,
            row.sent,
            answer.body
        );
        // A `NextToken` travels exactly when entries remain, so a clamped count — which
        // covers this collection — must report none.
        let token = answer.body.get("NextToken").and_then(Value::as_str);
        assert_eq!(
            token.is_some(),
            row.page < STORED_SETS.len(),
            "case {:?} (PageSize={}): a NextToken travels only when entries remain, got {}",
            row.name,
            row.sent,
            answer.body
        );
    }
}

/// RULE: every count above the server's maximum page is clamped to the SAME bound, so the
/// answers to two such counts are indistinguishable — which is what makes the width of a
/// machine word on the host invisible to the caller.
#[tokio::test]
async fn every_count_above_the_maximum_answers_the_same_page() {
    let harness = Harness::with_stored_sets().await;

    let reference = harness.list_sets("?PageSize=1000").await;
    assert_eq!(
        reference.status, 200,
        "reference PageSize=1000: expected HTTP 200, got {} {}",
        reference.status, reference.body
    );

    for sent in [
        "1001".to_string(),
        "4294967295".to_string(),
        "4294967296".to_string(),
        "18446744073709551615".to_string(),
        "18446744073709551616".to_string(),
        "340282366920938463463374607431768211456".to_string(),
        "9".repeat(64),
        format!("{}1000", "0".repeat(64)),
    ] {
        let answer = harness.list_sets(&format!("?PageSize={sent}")).await;
        assert_eq!(
            answer.status, reference.status,
            "case PageSize={sent}: expected the status of the maximum page ({}), got {} {}",
            reference.status, answer.status, answer.body
        );
        assert_eq!(
            answer.body, reference.body,
            "case PageSize={sent}: expected the answer the maximum page gives ({}), got {}",
            reference.body, answer.body
        );
    }
}

/// RULE: a `PageSize` that is NOT a decimal count, and the count zero, are refused with the
/// modelled `BadRequestException` (HTTP 400) — the only values the page bound refuses.
#[tokio::test]
async fn a_value_that_is_no_decimal_count_is_refused() {
    let harness = Harness::with_stored_sets().await;

    for (case, sent) in [
        ("zero, which would page nothing", "0".to_string()),
        ("zero written in many digits", "0".repeat(40)),
        ("zero carrying a sign", "%2B0".to_string()),
        ("a negative count", "-5".to_string()),
        ("a negative count no word holds", format!("-{}", "9".repeat(40))),
        ("a fractional count", "2.5".to_string()),
        ("a count written as a whole decimal fraction", "5.0".to_string()),
        ("a count in exponent notation", "1e3".to_string()),
        ("a count in hexadecimal", "0x10".to_string()),
        ("a count grouped with commas", "1%2C000".to_string()),
        ("a count grouped with underscores", "1_000".to_string()),
        // `%EF%BC%95` is the fullwidth digit five and `%D9%A5` the Arabic-Indic digit
        // five: digits to a reader, and not the ASCII decimal digits a count is written
        // with.
        ("a digit of another script", "%EF%BC%95".to_string()),
        ("a digit of another script again", "%D9%A5".to_string()),
        ("a count with a unit after it", "5%20items".to_string()),
        ("a count spelled as a word", "five".to_string()),
        ("text that is no count at all", "not-a-number".to_string()),
    ] {
        let answer = harness.list_sets(&format!("?PageSize={sent}")).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} (PageSize={sent}): expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "BadRequestException",
            "case {case:?} (PageSize={sent}): expected the modelled BadRequestException, got {}",
            answer.body
        );
    }
}

/// RULE (the control the rows above sit beside): a refused page bound leaves the collection
/// untouched, and a `PageSize` that is no count never becomes one for a later caller — the
/// listing after each refusal still answers the whole store.
#[tokio::test]
async fn a_refused_page_bound_changes_nothing_for_a_later_caller() {
    let harness = Harness::with_stored_sets().await;
    let expected: Vec<String> = STORED_SETS
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    for sent in ["0", "-5", "2.5", "1e3", "not-a-number"] {
        let refused = harness.list_sets(&format!("?PageSize={sent}")).await;
        assert_eq!(
            refused.status, 400,
            "case PageSize={sent}: expected HTTP 400, got {} {}",
            refused.status, refused.body
        );

        let listed = harness.list_sets("?PageSize=99999999999999999999999999").await;
        assert_eq!(
            listed.status, 200,
            "case PageSize={sent}: the follow-up listing must succeed, got {} {}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.listed_names(),
            expected,
            "case PageSize={sent}: expected {expected:?} after the refusal, got {}",
            listed.body
        );
    }
}
