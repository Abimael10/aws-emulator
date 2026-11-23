// SPDX-License-Identifier: CC0-1.0

//! Whether a refusal of this service CARRIES the member its detail text travels in, for the
//! error classes that produce a sentence and for the one that produces none.
//!
//! RULE: every refusal this service answers reports its detail member — the sentence where a
//! rule produces one, and JSON `null` where the class produces none at all — so a caller can
//! tell "this error class carries no sentence" from "the sentence is the empty string". The
//! member is never left out: an SDK substitutes the EMPTY STRING for a detail key a JSON
//! error document does not carry, so an omitted member would state the second where this
//! service means the first.
//!
//! `ListConfigurationSets`' unreadable `PageSize` is this service's one sentence-less class
//! (`ops/list_configuration_sets.rs`), so its rows are the `null` half of the rule and every
//! other refusal is the sentence half. The rows differ along what the rule generalises over:
//! the operation, the HTTP method, the error class and its status (the modelled
//! `BadRequestException` 400, `NotFoundException` 404 and `AlreadyExistsException` 409, and
//! the unmodelled authorization refusal at 403, whose class this model declares no shape
//! for), and — for the sentence-less class — the FORM of the value that cannot be read as a
//! page bound.
//!
//! The suite drives the real `restJson1` router, so these are the bytes an AWS SDK
//! `pinpoint-sms-voice` client sends and the body it parses. The exact sentences are pinned
//! in `parameter_refusal_wording.rs`; what is pinned here is the member's PRESENCE and the
//! null-against-sentence distinction, on every class at once.

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

    /// The detail member exactly as it travels, or `None` where the body does not carry it
    /// at all — which is what the rule says never happens on a refusal.
    fn detail(&self) -> Option<&Value> {
        self.body.get("message")
    }
}

impl Harness {
    /// The service over a fresh engine. `entitled` is the account's entitlement for this
    /// legacy API's resource-scoped actions: with it the rows reach their own error class,
    /// without it a resource-scoped call reaches the authorization refusal instead.
    fn new(entitled: bool) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the framework's own concern and is covered where it
        // lives; this suite drives unsigned requests.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = entitled;
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
            .uri(format!(
                "https://sms-voice.pinpoint.us-east-1.amazonaws.com{path}"
            ))
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
}

/// What the detail member carries on one refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// The class produces a sentence, so the member is a non-empty string.
    Sentence,
    /// The class produces none, so the member is present and JSON `null`.
    NoSentence,
}

/// One refusal: how it is sent, the class it comes back as, and whether that class states a
/// sentence.
struct Case {
    /// What makes this row different from the others.
    name: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<fn() -> Value>,
    status: u16,
    code: &'static str,
    detail: Detail,
}

/// A configuration set the rows that need an existing one resolve, so a row cannot pass
/// merely because the set was missing.
const EXISTING_SET: &str = "detail-member-set";

/// Every row is one refusal of an ENTITLED account, so each reaches its own error class
/// rather than the authorization refusal. Six `PageSize` forms carry no sentence; the rest
/// carry one, across three modelled classes, four operations and three HTTP methods.
const ENTITLED_CASES: &[Case] = &[
    // ---- the one class that produces NO sentence, over the forms of an unreadable bound --
    Case {
        name: "ListConfigurationSets with a non-numeric PageSize",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=not-a-number",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    Case {
        name: "ListConfigurationSets with a PageSize of zero",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=0",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    Case {
        name: "ListConfigurationSets with a negative PageSize",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=-5",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    Case {
        name: "ListConfigurationSets with a fractional PageSize",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=2.5",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    Case {
        name: "ListConfigurationSets with a PageSize in exponent notation",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=1e3",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    Case {
        name: "ListConfigurationSets with a PageSize written in non-ASCII digits",
        // `%EF%BC%95` is the fullwidth digit five: a digit to a human reader, and not a
        // decimal count this service can read.
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?PageSize=%EF%BC%95",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::NoSentence,
    },
    // ---- the classes that DO state a sentence ------------------------------------------
    Case {
        name: "ListConfigurationSets with a NextToken this service never minted",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets?NextToken=not-a-token",
        body: None,
        status: 400,
        code: "BadRequestException",
        detail: Detail::Sentence,
    },
    Case {
        name: "CreateConfigurationSet with a name that is not word characters",
        method: "POST",
        path: "/v1/sms-voice/configuration-sets",
        body: Some(punctuated_set_name),
        status: 400,
        code: "BadRequestException",
        detail: Detail::Sentence,
    },
    Case {
        name: "CreateConfigurationSet naming a set that already exists",
        method: "POST",
        path: "/v1/sms-voice/configuration-sets",
        body: Some(existing_set_name),
        status: 409,
        code: "AlreadyExistsException",
        detail: Detail::Sentence,
    },
    Case {
        name: "GetConfigurationSetEventDestinations for a set that never existed",
        method: "GET",
        path: "/v1/sms-voice/configuration-sets/absent-set/event-destinations",
        body: None,
        status: 404,
        code: "NotFoundException",
        detail: Detail::Sentence,
    },
    Case {
        name: "DeleteConfigurationSet for a set that never existed",
        method: "DELETE",
        path: "/v1/sms-voice/configuration-sets/absent-set",
        body: None,
        status: 404,
        code: "NotFoundException",
        detail: Detail::Sentence,
    },
    Case {
        name: "CreateConfigurationSetEventDestination for a set that never existed",
        method: "POST",
        path: "/v1/sms-voice/configuration-sets/absent-set/event-destinations",
        body: Some(named_destination),
        status: 404,
        code: "NotFoundException",
        detail: Detail::Sentence,
    },
    Case {
        name: "SendVoiceMessage with no DestinationPhoneNumber",
        method: "POST",
        path: "/v1/sms-voice/voice/message",
        body: Some(message_without_destination),
        status: 400,
        code: "BadRequestException",
        detail: Detail::Sentence,
    },
];

/// The rows of an UNENTITLED account: the authorization refusal, whose class this model
/// declares no shape for, states a sentence of its own and keeps it in the same member.
const UNENTITLED_CASES: &[Case] = &[
    Case {
        name: "CreateConfigurationSet without the entitlement",
        method: "POST",
        path: "/v1/sms-voice/configuration-sets",
        body: Some(existing_set_name),
        status: 403,
        code: "AccessDeniedException",
        detail: Detail::Sentence,
    },
    Case {
        name: "CreateConfigurationSetEventDestination passing another account's role",
        method: "POST",
        path: "/v1/sms-voice/configuration-sets/absent-set/event-destinations",
        body: Some(cross_account_destination),
        status: 403,
        code: "AccessDeniedException",
        detail: Detail::Sentence,
    },
];

fn punctuated_set_name() -> Value {
    json!({ "ConfigurationSetName": "detail member set !!" })
}

fn existing_set_name() -> Value {
    json!({ "ConfigurationSetName": EXISTING_SET })
}

fn named_destination() -> Value {
    json!({ "EventDestinationName": "events", "EventDestination": { "Enabled": true } })
}

fn message_without_destination() -> Value {
    json!({ "Content": { "PlainTextMessage": { "Text": "hello" } } })
}

fn cross_account_destination() -> Value {
    json!({
        "EventDestinationName": "events",
        "EventDestination": {
            "Enabled": true,
            "CloudWatchLogsDestination": {
                "LogGroupArn": "arn:aws:logs:us-east-1:123456789012:log-group:detail:*",
                "IamRoleArn": "arn:aws:iam::123456789012:role/detail-member-role",
            },
        },
    })
}

/// Assert one row's answer against the rule.
fn assert_case(case: &Case, answer: &Answer) {
    assert_eq!(
        answer.status, case.status,
        "case {:?}: expected HTTP {}, got {} {:?}",
        case.name, case.status, answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        case.code,
        "case {:?}: expected the error class {:?}, got {:?}",
        case.name,
        case.code,
        answer.body
    );
    match case.detail {
        Detail::Sentence => {
            let sentence = answer.detail().and_then(Value::as_str);
            assert!(
                sentence.is_some_and(|text| !text.is_empty()),
                "case {:?}: expected a sentence in the detail member, got {:?}",
                case.name,
                answer.body
            );
        }
        // PRESENT and null: not absent (which an SDK reports as the empty string), and not
        // the empty string itself.
        Detail::NoSentence => assert_eq!(
            answer.detail(),
            Some(&Value::Null),
            "case {:?}: expected the detail member as JSON null, got {:?}",
            case.name,
            answer.body
        ),
    }
    assert!(
        answer.body.get("Message").is_none(),
        "case {:?}: the detail must travel in the one member this service reports it in, \
         got {:?}",
        case.name,
        answer.body
    );
}

#[tokio::test]
async fn every_refusal_carries_its_detail_member_sentence_or_null() {
    let harness = Harness::new(true);
    // The rows that need an existing set must fail on their own class, not on a missing one.
    let created = harness
        .call(
            "POST",
            "/v1/sms-voice/configuration-sets",
            Some(existing_set_name()),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the fixture configuration set must be created, got {} {:?}",
        created.status, created.body
    );

    for case in ENTITLED_CASES {
        let answer = harness
            .call(case.method, case.path, case.body.map(|build| build()))
            .await;
        assert_case(case, &answer);
    }
}

/// RULE (the other half of the same one): a failure whose class this service's model
/// declares NO shape for keeps the sentence its own renderer wrote, in the same member — the
/// null is written for a modelled class that states none, never over words another layer
/// chose.
#[tokio::test]
async fn an_unmodelled_refusal_keeps_its_own_sentence() {
    let harness = Harness::new(false);
    for case in UNENTITLED_CASES {
        let answer = harness
            .call(case.method, case.path, case.body.map(|build| build()))
            .await;
        assert_case(case, &answer);
    }
}

/// RULE (the control the sentence-less refusal sits beside): a `PageSize` this service CAN
/// read as a page bound is no refusal at all — the listing answers 200 with its
/// `ConfigurationSets` member, so the rows above pin a refusal and not a broken path.
///
/// The rows differ in the bound asked for, including one below, one equal to and two above
/// the number of sets stored — the last of them a count larger than any the machine holds,
/// which is a bound to clamp and not a value to refuse — so the control is not tied to one
/// page.
#[tokio::test]
async fn a_page_size_this_service_can_read_is_served() {
    let harness = Harness::new(true);
    for name in ["alpha", "beta"] {
        let created = harness
            .call(
                "POST",
                "/v1/sms-voice/configuration-sets",
                Some(json!({ "ConfigurationSetName": name })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: the set must be created, got {} {:?}",
            created.status, created.body
        );
    }

    for (case, page_size, expected) in [
        ("a bound below what is stored", "1", vec!["alpha"]),
        ("a bound equal to what is stored", "2", vec!["alpha", "beta"]),
        ("a bound above what is stored", "5", vec!["alpha", "beta"]),
        (
            "a bound larger than any count the machine holds",
            "99999999999999999999999999",
            vec!["alpha", "beta"],
        ),
    ] {
        let listed = harness
            .call(
                "GET",
                &format!("/v1/sms-voice/configuration-sets?PageSize={page_size}"),
                None,
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200, got {} {:?}",
            listed.status, listed.body
        );
        let names: Vec<String> = listed
            .body
            .get("ConfigurationSets")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names, expected,
            "case {case:?}: expected {expected:?} under ConfigurationSets, got {:?}",
            listed.body
        );
        assert!(
            listed.body.get("message").is_none(),
            "case {case:?}: a success carries no detail member, got {:?}",
            listed.body
        );
    }
}
