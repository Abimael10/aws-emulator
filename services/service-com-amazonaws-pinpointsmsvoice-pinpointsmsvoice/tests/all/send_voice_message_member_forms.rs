// SPDX-License-Identifier: CC0-1.0

//! Which values `SendVoiceMessage` accepts for the members whose shapes name a form.
//!
//! RULE: the form a value may take belongs to the SHAPE the member targets, so it is the
//! same for every member targeting that shape, whichever operation the member belongs to
//! (`smithy_query(query="shape",shape="SendVoiceMessageRequest",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`:
//! `ConfigurationSetName` targets `WordCharactersWithDelimiters`, the shape
//! `CreateConfigurationSetRequest$ConfigurationSetName` also targets, and
//! `DestinationPhoneNumber` and `OriginationPhoneNumber` both target `NonEmptyString`, the
//! shape `CreateConfigurationSetEventDestinationRequest$EventDestinationName` also targets).
//! A value outside its member's shape's form is a parameter this service's validation cannot
//! use: the modelled `BadRequestException` (HTTP 400) naming that member, with nothing
//! recorded; a value the form admits is accepted and recorded exactly as sent.
//!
//! The suite drives the real `restJson1` router, so the requests here are the bytes an AWS
//! SDK `pinpoint-sms-voice` client sends. The rows differ along what the rule generalises
//! over: which shape the member targets, which character class the value carries and where
//! in the value it sits, whether a value bound at all, and how many of the request's
//! members are unusable at once.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The sentence a request carrying a `ConfigurationSetName` outside the name form earns:
/// this service's wording for a parameter its validation cannot use, naming that member.
const INVALID_CONFIGURATION_SET_NAME: &str =
    "The following parameter is null, empty or invalid: ['configurationSetName']";

/// The same sentence for the origination number.
const INVALID_ORIGINATION_PHONE_NUMBER: &str =
    "The following parameter is null, empty or invalid: ['originationPhoneNumber']";

/// A destination number every row that is not about the destination carries, so such a row
/// fails on the member it is about.
const USABLE_DESTINATION: &str = "+12065550100";

/// One HTTP answer: the status and the parsed JSON body (`Value::Null` when empty).
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The modelled error code the answer carries, or the empty string on a success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The message the answer carries, or `None` when it carries none.
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }
}

/// The emulator under test, over the one shared engine the test also reads state from (the
/// record of a sent message is observable only there).
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's
        // own concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // The account holds the entitlement this API's resource-scoped actions need, so a
        // value the shape admits reaches the send rather than the authorization refusal
        // (`legacy_surface_authorization.rs` owns that half).
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router, engine }
    }

    /// Dispatch one request with `body` to `path`.
    async fn post(&self, path: &str, body: Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://sms-voice.us-east-1.amazonaws.com{path}"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sms-voice/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(body.to_string()))
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

    /// `CreateConfigurationSet` for `name`.
    async fn create_set(&self, name: &str) -> Answer {
        self.post(
            "/v1/sms-voice/configuration-sets",
            json!({ "ConfigurationSetName": name }),
        )
        .await
    }

    /// `SendVoiceMessage` with `body`.
    async fn send_voice(&self, body: Value) -> Answer {
        self.post("/v1/sms-voice/voice/message", body).await
    }

    /// How many messages the store holds.
    fn recorded_count(&self) -> usize {
        self.engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .sent_messages
            .len()
    }

    /// `(configuration set name, origination number)` of the most recently recorded message.
    fn last_record(&self) -> (Option<String>, Option<String>) {
        let store = self
            .engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>();
        let recorded = store.lock();
        let last = recorded
            .sent_messages
            .last()
            .expect("an accepted send must be recorded");
        (
            last.configuration_set_name.clone(),
            last.origination_phone_number.clone(),
        )
    }
}

/// A `Content` carrying exactly one body, so a row fails on the member it is about rather
/// than on the content.
fn one_body() -> Value {
    json!({ "PlainTextMessage": { "Text": "hello" } })
}

/// Names the `WordCharactersWithDelimiters` form admits — word characters (ASCII letters,
/// digits, `_`) written with the delimiters `-` and `.`. The rows walk the classes rather
/// than one example of them: lower case, upper case, digits, a digit first, `_`, `-`, `.`,
/// a single character, and a long name mixing all of it.
const ADMITTED_NAMES: &[&str] = &[
    "lowercase",
    "UPPERCASE",
    "MixedCase",
    "0123456789",
    "9lives",
    "with_underscores",
    "with-dashes",
    "with.dots",
    "a",
    "_",
    "voice.events_2026-config",
];

/// Names carrying something the form does not admit. The rows differ in the class of the
/// offending character (whitespace, punctuation, a separator, a non-ASCII letter), in where
/// it sits (leading, inner, trailing), and include the degenerate names: empty, whitespace
/// alone, and delimiters alone.
const REFUSED_NAMES: &[(&str, &str)] = &[
    ("an inner space", "voice events"),
    ("a leading space", " voice-events"),
    ("a trailing space", "voice-events "),
    ("a tab", "voice\tevents"),
    ("exclamation marks", "voice-events!!"),
    ("a slash", "voice/events"),
    ("a colon", "voice:events"),
    ("a plus", "voice+events"),
    ("an at sign", "voice@events"),
    ("a comma", "voice,events"),
    ("an asterisk", "voice*"),
    ("a non-ASCII letter", "vöice-events"),
    ("the empty name", ""),
    ("whitespace alone", "   "),
    ("a single dot", "."),
    ("delimiters alone", "-.-"),
];

/// RULE: a name `CreateConfigurationSet` files a set under is a name `SendVoiceMessage`
/// accepts and records, and a name the one refuses is a name the other refuses too —
/// both members target the same shape, so neither can record a name no configuration set
/// could exist under.
#[tokio::test]
async fn both_members_of_the_name_shape_admit_exactly_the_same_names() {
    let harness = Harness::new();

    for name in ADMITTED_NAMES {
        let created = harness.create_set(name).await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: an admitted name must create a set, got {} {}",
            created.status, created.body
        );

        let sent = harness
            .send_voice(json!({
                "ConfigurationSetName": name,
                "DestinationPhoneNumber": USABLE_DESTINATION,
                "Content": one_body(),
            }))
            .await;
        assert_eq!(
            sent.status, 200,
            "case {name:?}: an admitted name must be accepted by the send, got {} {}",
            sent.status, sent.body
        );
        let (recorded_name, _) = harness.last_record();
        assert_eq!(
            recorded_name.as_deref(),
            Some(*name),
            "case {name:?}: the send must record the name exactly as sent, got {recorded_name:?}"
        );
    }

    let accepted = harness.recorded_count();
    for (case, name) in REFUSED_NAMES {
        let created = harness.create_set(name).await;
        assert_eq!(
            created.status, 400,
            "case {case:?} ({name:?}): the create must refuse this name, got {} {}",
            created.status, created.body
        );

        let sent = harness
            .send_voice(json!({
                "ConfigurationSetName": name,
                "DestinationPhoneNumber": USABLE_DESTINATION,
                "Content": one_body(),
            }))
            .await;
        assert_eq!(
            sent.status, 400,
            "case {case:?} ({name:?}): the send must refuse this name too, got {} {}",
            sent.status, sent.body
        );
        assert_eq!(
            sent.error_code(),
            "BadRequestException",
            "case {case:?} ({name:?}): expected the modelled BadRequestException, got {}",
            sent.body
        );
        assert_eq!(
            sent.message(),
            Some(INVALID_CONFIGURATION_SET_NAME),
            "case {case:?} ({name:?}): expected the sentence naming the member, got {}",
            sent.body
        );
    }
    assert_eq!(
        harness.recorded_count(),
        accepted,
        "a refused send must record nothing, got {} records for {accepted} accepted sends",
        harness.recorded_count()
    );
}

/// RULE: a value bound to a `NonEmptyString` member carries content, so a blank origination
/// number is refused like any other unusable parameter — while binding none at all is a
/// request that named no number, which is served.
#[tokio::test]
async fn an_origination_number_outside_the_non_empty_form_is_refused() {
    let harness = Harness::new();

    // Blank in every form whitespace arrives in, plus the empty value.
    for (case, number) in [
        ("the empty value", ""),
        ("one space", " "),
        ("one tab", "\t"),
        ("a newline", "\n"),
        ("several kinds of whitespace", " \t\r\n "),
    ] {
        let sent = harness
            .send_voice(json!({
                "DestinationPhoneNumber": USABLE_DESTINATION,
                "OriginationPhoneNumber": number,
                "Content": one_body(),
            }))
            .await;
        assert_eq!(
            sent.status, 400,
            "case {case:?} ({number:?}): expected HTTP 400, got {} {}",
            sent.status, sent.body
        );
        assert_eq!(
            sent.error_code(),
            "BadRequestException",
            "case {case:?} ({number:?}): expected the modelled BadRequestException, got {}",
            sent.body
        );
        assert_eq!(
            sent.message(),
            Some(INVALID_ORIGINATION_PHONE_NUMBER),
            "case {case:?} ({number:?}): expected the sentence naming the member, got {}",
            sent.body
        );
    }
    assert_eq!(
        harness.recorded_count(),
        0,
        "a refused send must record nothing, got {}",
        harness.recorded_count()
    );

    // A value carrying content is accepted and recorded exactly as sent, padding included;
    // no number at all is served, and recorded as the absence it was.
    for (case, number, expected) in [
        ("a number", Some("+12065550199"), Some("+12065550199")),
        (
            "a number with padding",
            Some("  +12065550199  "),
            Some("  +12065550199  "),
        ),
        ("no number at all", None, None),
    ] {
        let mut body = json!({
            "DestinationPhoneNumber": USABLE_DESTINATION,
            "Content": one_body(),
        });
        if let Some(number) = number {
            body["OriginationPhoneNumber"] = json!(number);
        }
        let sent = harness.send_voice(body).await;
        assert_eq!(
            sent.status, 200,
            "case {case:?}: expected 200, got {} {}",
            sent.status, sent.body
        );
        let (_, recorded) = harness.last_record();
        assert_eq!(
            recorded.as_deref(),
            expected,
            "case {case:?}: expected {expected:?} recorded, got {recorded:?}"
        );
    }
}

/// RULE: a request is refused once, so the one refusal names EVERY member of it this
/// service's validation cannot use, in the order the input binds them
/// (`ConfigurationSetName`, `DestinationPhoneNumber`, `OriginationPhoneNumber`).
///
/// The rows differ in which of the three members are unusable and how many.
#[tokio::test]
async fn one_refusal_names_every_unusable_member_of_the_send() {
    let harness = Harness::new();

    for (case, name, destination, origination, expected) in [
        (
            "the name alone",
            Some("voice events!!"),
            Some(USABLE_DESTINATION),
            Some("+12065550199"),
            "The following parameter is null, empty or invalid: ['configurationSetName']",
        ),
        (
            "the destination alone",
            Some("voice-events"),
            Some("  "),
            Some("+12065550199"),
            "The following parameter is null, empty or invalid: ['destinationPhoneNumber']",
        ),
        (
            "the origination alone",
            Some("voice-events"),
            Some(USABLE_DESTINATION),
            Some(""),
            "The following parameter is null, empty or invalid: ['originationPhoneNumber']",
        ),
        (
            "the name and the origination",
            Some("voice/events"),
            Some(USABLE_DESTINATION),
            Some("\t"),
            "The following parameter is null, empty or invalid: ['configurationSetName', \
             'originationPhoneNumber']",
        ),
        (
            "the name and the destination",
            Some("."),
            None,
            Some("+12065550199"),
            "The following parameter is null, empty or invalid: ['configurationSetName', \
             'destinationPhoneNumber']",
        ),
        (
            "all three",
            Some(" voice-events"),
            Some(" "),
            Some("   "),
            "The following parameter is null, empty or invalid: ['configurationSetName', \
             'destinationPhoneNumber', 'originationPhoneNumber']",
        ),
    ] {
        let mut body = json!({ "Content": one_body() });
        if let Some(name) = name {
            body["ConfigurationSetName"] = json!(name);
        }
        if let Some(destination) = destination {
            body["DestinationPhoneNumber"] = json!(destination);
        }
        if let Some(origination) = origination {
            body["OriginationPhoneNumber"] = json!(origination);
        }

        let sent = harness.send_voice(body).await;
        assert_eq!(
            sent.status, 400,
            "case {case:?}: expected HTTP 400, got {} {}",
            sent.status, sent.body
        );
        assert_eq!(
            sent.message(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {}",
            sent.body
        );
    }

    assert_eq!(
        harness.recorded_count(),
        0,
        "a refused send must record nothing, got {}",
        harness.recorded_count()
    );
}
