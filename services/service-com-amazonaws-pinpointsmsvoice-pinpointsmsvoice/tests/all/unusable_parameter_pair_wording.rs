// SPDX-License-Identifier: CC0-1.0

//! One request in which a URI path label AND a body member are both unusable, and the ONE
//! refusal it is answered with.
//!
//! RULE: a request is refused ONCE, and the one sentence names EVERY parameter the refusing
//! layer cannot use, in the order the operation's input binds them — never merely the first
//! one the handler looked at. Which of this service's two sentences is worded is decided by
//! the LAYER a parameter failed in: a `@required` URI parameter that bound no value at all is
//! the request-binding layer's `Missing required request parameters: [<Member>]`, by the
//! modelled member name, and that layer runs first; everything else reached this service's own
//! validation, which answers `The following parameter is null, empty or invalid: ['<param>']`
//! by its own lower-camel spelling.
//!
//! `CreateConfigurationSetEventDestination` is the one operation where a `@required` path
//! label meets a body member this service's own validation reads — its input binds
//! `ConfigurationSetName` (a `@required` `httpLabel`) and then `EventDestinationName`
//! (`smithy_query(query="operation",operation="CreateConfigurationSetEventDestination",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`)
//! — so it is where both can be unusable in a single request.
//!
//! The suite sends every combination of the forms each of the two parameters can arrive in
//! and derives the expected answer FROM THE RULE rather than from a sentence written out per
//! row: the rows differ in which parameter is unusable, in which layer each failed, and in the
//! FORM of each value (absent, empty, blank in several kinds of whitespace, padded content,
//! content). A combination in which neither is unusable is not refused at all: it
//! reaches the lookup, so it is served or answered `NotFoundException` by whether the set
//! exists, which is what keeps a refusal from passing as a lookup failure.
//!
//! The suite drives the real `restJson1` router, so these are the bytes an AWS SDK
//! `pinpoint-sms-voice` client sends.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The emulator under test: the service mounted on the production router, over the one
/// shared engine the test also reads state from.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

/// One HTTP answer: the status, the `x-amzn-errortype` header, and the parsed JSON body
/// (`Value::Null` when empty).
struct Answer {
    status: u16,
    error_type_header: String,
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
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's own
        // concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // This suite is about how a MALFORMED request is worded, which is answered ahead of
        // the authorization decision — the account holds the entitlement this API's
        // resource-scoped actions need, so a row that names a usable set reaches the lookup
        // rather than that decision.
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router, engine }
    }

    /// Send one request and read the answer back.
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
        let error_type_header = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        Answer {
            status,
            error_type_header,
            body,
        }
    }

    /// The configuration set names currently in the shared store.
    fn stored_set_names(&self) -> Vec<String> {
        self.engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .configuration_sets
            .keys()
            .cloned()
            .collect()
    }
}

/// WHERE one parameter of a request failed, which is the only thing that decides which of
/// this service's two sentences the whole request is refused with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// The parameter bound no value at all, so the request never named a target and the
    /// request-binding layer reports it — by the MODELLED member name. That layer runs
    /// first, so this service's own validation is never reached.
    Binding,
    /// A value bound that this service's own validation cannot use, so that layer names the
    /// parameter by the service's own spelling.
    Validation,
}

/// One parameter of this operation, spelled as each of the two layers reports it.
#[derive(Debug, Clone, Copy)]
struct Parameter {
    /// The modelled member name, which is how the request-binding layer reports it.
    modelled: &'static str,
    /// This service's own spelling, which is how its validation layer reports it.
    validated: &'static str,
}

/// The `{ConfigurationSetName}` path label — the FIRST parameter this operation's input
/// binds.
const CONFIGURATION_SET_NAME: Parameter = Parameter {
    modelled: "ConfigurationSetName",
    validated: "configurationSetName",
};

/// The `EventDestinationName` body member — the SECOND parameter this operation's input
/// binds.
const EVENT_DESTINATION_NAME: Parameter = Parameter {
    modelled: "EventDestinationName",
    validated: "eventDestinationName",
};

/// The ONE sentence a request whose parameters failed as `findings` says — given in the order
/// the input binds them — must be refused with, or `None` where nothing is unusable.
///
/// This is the RULE, not a per-row expectation: the earlier layer answers alone when any
/// parameter failed in it, and either way its sentence names EVERY parameter that failed in
/// it, in the order given.
///
/// It is stated here INDEPENDENTLY of the module that implements it on purpose: asking that
/// module for the sentence would compare it with itself and pass whatever it said. The
/// module's own unit tests cover the helper; this one covers what a client reads off the
/// wire, so the two must be written out separately.
fn one_sentence(findings: &[(Parameter, Layer)]) -> Option<String> {
    let bound_nothing: Vec<&str> = findings
        .iter()
        .filter(|(_, layer)| *layer == Layer::Binding)
        .map(|(parameter, _)| parameter.modelled)
        .collect();
    if !bound_nothing.is_empty() {
        return Some(format!(
            "Missing required request parameters: [{}]",
            bound_nothing.join(", ")
        ));
    }

    let unreadable: Vec<String> = findings
        .iter()
        .filter(|(_, layer)| *layer == Layer::Validation)
        .map(|(parameter, _)| format!("'{}'", parameter.validated))
        .collect();
    (!unreadable.is_empty()).then(|| {
        format!(
            "The following parameter is null, empty or invalid: [{}]",
            unreadable.join(", ")
        )
    })
}

/// How one request binds the `{ConfigurationSetName}` path label.
struct LabelForm {
    /// What makes this form different from the others.
    name: &'static str,
    /// The path segment this form sends.
    segment: Segment,
    /// The layer this value fails in, or `None` where it is usable.
    fails_in: Option<Layer>,
    /// Whether a USABLE value names a configuration set that exists, which is what a request
    /// that is not refused is answered by.
    resolves: bool,
}

/// How one request binds the `EventDestinationName` body member.
struct DestinationForm {
    /// What makes this form different from the others.
    name: &'static str,
    /// What the body carries for the member.
    sent: Sent,
    /// The layer this value fails in, or `None` where it is usable.
    fails_in: Option<Layer>,
}

/// The path segment one form binds the label to.
#[derive(Clone, Copy)]
enum Segment {
    /// Exactly this text, already percent-encoded.
    Encoded(&'static str),
    /// The name of the set that EXISTS, with an encoded space on each side — derived from the
    /// fixture's name so the row cannot drift away from it.
    PaddedExistingSet,
}

impl Segment {
    /// The segment as it travels in the request URI.
    fn encoded(self) -> String {
        match self {
            Segment::Encoded(text) => text.to_string(),
            Segment::PaddedExistingSet => format!("%20{EXISTING_SET}%20"),
        }
    }
}

/// What a request's body carries for an optional member.
///
/// A member carrying JSON `null` is not a form here: a value of the wrong JSON TYPE is
/// refused by the boundary deserializer, before any handler of this service runs, so it
/// never reaches the layer this rule is about.
#[derive(Clone, Copy)]
enum Sent {
    /// The body has no such member at all.
    Omitted,
    /// The member carries this text.
    Text(&'static str),
}

/// A configuration set the rows that reach a lookup can resolve, so a served row cannot pass
/// merely because the set was missing.
const EXISTING_SET: &str = "refusal-pair-set";

/// The destination name the one row that is not refused creates, read back through
/// `GetConfigurationSetEventDestinations` at the end of the suite.
const CREATED_DESTINATION: &str = "events";

/// Every form the `{ConfigurationSetName}` label can arrive in: a segment that bound nothing,
/// blank segments of several kinds of whitespace, and usable values — one naming a set that
/// exists, one naming a set that does not, and two whose content is PADDED, which is content
/// and not blankness (the second of them padding the name of the set that DOES exist, which
/// the raw label still does not name).
const LABEL_FORMS: &[LabelForm] = &[
    LabelForm {
        name: "an EMPTY label",
        segment: Segment::Encoded(""),
        fails_in: Some(Layer::Binding),
        resolves: false,
    },
    LabelForm {
        name: "a label of one space",
        segment: Segment::Encoded("%20"),
        fails_in: Some(Layer::Validation),
        resolves: false,
    },
    LabelForm {
        name: "a label of one tab",
        segment: Segment::Encoded("%09"),
        fails_in: Some(Layer::Validation),
        resolves: false,
    },
    LabelForm {
        name: "a label of a newline",
        segment: Segment::Encoded("%0A"),
        fails_in: Some(Layer::Validation),
        resolves: false,
    },
    LabelForm {
        name: "a label of several kinds of whitespace",
        segment: Segment::Encoded("%20%09%20"),
        fails_in: Some(Layer::Validation),
        resolves: false,
    },
    LabelForm {
        name: "a label naming a set that exists",
        segment: Segment::Encoded(EXISTING_SET),
        fails_in: None,
        resolves: true,
    },
    LabelForm {
        name: "a label naming a set that does not exist",
        segment: Segment::Encoded("a-set-nobody-created"),
        fails_in: None,
        resolves: false,
    },
    LabelForm {
        name: "a label whose content is padded: padding is not blankness",
        segment: Segment::Encoded("%20padded%20"),
        fails_in: None,
        resolves: false,
    },
    LabelForm {
        // Padding is not blankness, and it is not stripped from the value either: the set is
        // resolved by the RAW label, so a padded spelling of a set that exists resolves to
        // nothing rather than to that set.
        name: "a label whose padded content spells a set that exists",
        segment: Segment::PaddedExistingSet,
        fails_in: None,
        resolves: false,
    },
];

/// Every form the `EventDestinationName` member can arrive in. A body member that is absent,
/// empty or blank names no destination to create, and each of them reached this service's own
/// validation — a body member binds nothing to the URI, so none of them is the
/// request-binding layer's to report.
const DESTINATION_FORMS: &[DestinationForm] = &[
    DestinationForm {
        name: "no EventDestinationName at all",
        sent: Sent::Omitted,
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "an EMPTY EventDestinationName",
        sent: Sent::Text(""),
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "a name of one space",
        sent: Sent::Text(" "),
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "a name of one tab",
        sent: Sent::Text("\t"),
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "a name of a newline",
        sent: Sent::Text("\n"),
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "a name of several kinds of whitespace",
        sent: Sent::Text(" \t\r\n "),
        fails_in: Some(Layer::Validation),
    },
    DestinationForm {
        name: "a name with content",
        sent: Sent::Text(CREATED_DESTINATION),
        fails_in: None,
    },
];

/// The body one row sends: the definition every row carries, plus the member in the form the
/// row binds it.
fn body_for(sent: Sent) -> Value {
    let mut body = json!({ "EventDestination": { "Enabled": true } });
    match sent {
        Sent::Omitted => {}
        Sent::Text(text) => body["EventDestinationName"] = json!(text),
    }
    body
}

/// RULE: a request is refused ONCE, so the one sentence names every parameter the refusing
/// layer cannot use, in the order the input binds them — and the request-binding layer, which
/// runs first, answers alone when any parameter failed in it. A request on which NEITHER
/// parameter is unusable is not refused at all: it reaches the lookup.
#[tokio::test]
async fn one_refusal_names_every_parameter_of_the_request_the_layer_cannot_use() {
    let harness = Harness::new();
    let created = harness
        .call(
            "POST",
            "/v1/sms-voice/configuration-sets",
            Some(json!({ "ConfigurationSetName": EXISTING_SET })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the fixture configuration set must be created, got {} {:?}",
        created.status, created.body
    );

    for label in LABEL_FORMS {
        for destination in DESTINATION_FORMS {
            let case = format!("{} with {}", label.name, destination.name);
            let findings: Vec<(Parameter, Layer)> = [
                label.fails_in.map(|layer| (CONFIGURATION_SET_NAME, layer)),
                destination
                    .fails_in
                    .map(|layer| (EVENT_DESTINATION_NAME, layer)),
            ]
            .into_iter()
            .flatten()
            .collect();

            let answer = harness
                .call(
                    "POST",
                    &format!(
                        "/v1/sms-voice/configuration-sets/{}/event-destinations",
                        label.segment.encoded()
                    ),
                    Some(body_for(destination.sent)),
                )
                .await;

            match one_sentence(&findings) {
                Some(expected) => {
                    assert_eq!(
                        answer.status, 400,
                        "case {case:?}: expected HTTP 400, got {} {:?}",
                        answer.status, answer.body
                    );
                    assert_eq!(
                        answer.error_code(),
                        "BadRequestException",
                        "case {case:?}: expected the modelled BadRequestException, got {:?}",
                        answer.body
                    );
                    assert_eq!(
                        answer.error_type_header, "BadRequestException",
                        "case {case:?}: expected x-amzn-errortype BadRequestException, got {:?}",
                        answer.error_type_header
                    );
                    assert_eq!(
                        answer.message(),
                        expected,
                        "case {case:?}: expected the message {expected:?}, got {:?}",
                        answer.body
                    );
                }
                // Nothing unusable: the request reached the lookup, and only whether the set
                // exists decides the answer.
                None if label.resolves => assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the create to be served, got {} {:?}",
                    answer.status, answer.body
                ),
                None => {
                    assert_eq!(
                        answer.status, 404,
                        "case {case:?}: expected HTTP 404 for a set that does not exist, got {} {:?}",
                        answer.status, answer.body
                    );
                    assert_eq!(
                        answer.error_code(),
                        "NotFoundException",
                        "case {case:?}: expected the modelled NotFoundException, got {:?}",
                        answer.body
                    );
                }
            }
        }
    }

    // A refused request stores nothing, so the only set is the fixture one and the only
    // destination under it is the one the single served row created — read back through the
    // reader, not out of the store.
    assert_eq!(
        harness.stored_set_names(),
        vec![EXISTING_SET.to_string()],
        "a refusal must leave the store untouched, got {:?}",
        harness.stored_set_names()
    );
    let listed = harness
        .call(
            "GET",
            &format!("/v1/sms-voice/configuration-sets/{EXISTING_SET}/event-destinations"),
            None,
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "reading the destinations back must be served, got {} {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.body,
        json!({ "EventDestinations": [{ "Name": CREATED_DESTINATION, "Enabled": true }] }),
        "expected exactly the destination the served row created, got {:?}",
        listed.body
    );
}
