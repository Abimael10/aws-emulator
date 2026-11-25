// SPDX-License-Identifier: CC0-1.0

//! One `SendVoiceMessage` request with several unusable parameters, and the ONE refusal it is
//! answered with.
//!
//! RULE: a handler reads EVERY parameter its own validation can reject before anything is
//! refused, and the one refusal reports what each of them contributes, in the order the
//! operation's input binds them — a parameter this service names contributes its name to the
//! single `The following parameter is null, empty or invalid: [...]` list, and a parameter
//! whose class this service states a sentence of its own for contributes THAT sentence. So a
//! request whose body is unusable AND whose numbers or set name are unusable learns both
//! faults from one answer, instead of being told the fault the handler happened to look at
//! first and having to send the request again for the rest.
//!
//! `SendVoiceMessage` is where the two contributions meet: its input binds
//! `ConfigurationSetName`, `Content`, `DestinationPhoneNumber` and `OriginationPhoneNumber`
//! in that order
//! (`smithy_query(query="operation",operation="SendVoiceMessage",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`),
//! and the `Content` is the one of them refused in a sentence of its own, because what is
//! unusable is WHICH of the three bodies `VoiceMessageContent` declares the request chose
//! rather than which parameter it sent.
//!
//! The suite sends every combination of the forms the four parameters arrive in and derives
//! the expected answer FROM THE RULE rather than from a sentence written out per row: the
//! rows differ in which parameters are unusable, how many, what each contributes, where in
//! the binding order each sits, and the FORM of every value (absent, empty, blank in several
//! kinds of whitespace, padded content, content, and a `Content` carrying none, one, two or
//! all three bodies). A combination in which nothing is unusable is not refused at all: it is
//! sent, and recorded.
//!
//! The suite drives the real `restJson1` router, so these are the bytes an AWS SDK
//! `pinpoint-sms-voice` client sends.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The sentence this service states for a `Content` that does not carry exactly one message
/// body — the one class of this operation's own validation refused in its own words rather
/// than by naming the parameter.
const UNUSABLE_CONTENT: &str = "Message is invalid. The message must have either a plain \
     text, SSML, or call instructions message";

/// What one refusal's contributions are separated by when a request earned more than one.
const SENTENCE_SEPARATOR: &str = ". ";

/// WHAT one unusable parameter contributes to the one refusal the request earns.
#[derive(Debug, Clone, Copy)]
enum Contribution {
    /// Its name, as this service's own validation spells it, in the single list sentence.
    Named(&'static str),
    /// The whole sentence its class is worded with, in place of a name.
    Sentence(&'static str),
}

/// The ONE sentence a request whose parameters failed as `findings` — given in the order the
/// input binds them — must be refused with, or `None` where nothing is unusable.
///
/// This is the RULE, not a per-row expectation, and it is stated here INDEPENDENTLY of the
/// module that implements it on purpose: asking that module for the sentence would compare it
/// with itself and pass whatever it said.
fn one_refusal(findings: &[Contribution]) -> Option<String> {
    let names: Vec<String> = findings
        .iter()
        .filter_map(|finding| match finding {
            Contribution::Named(name) => Some(format!("'{name}'")),
            Contribution::Sentence(_) => None,
        })
        .collect();

    let mut listed = false;
    let mut sentences: Vec<String> = Vec::new();
    for finding in findings {
        match finding {
            Contribution::Sentence(sentence) => sentences.push((*sentence).to_string()),
            // Every named parameter is in ONE list, worded where the earliest of them binds.
            Contribution::Named(_) if !listed => {
                listed = true;
                sentences.push(format!(
                    "The following parameter is null, empty or invalid: [{}]",
                    names.join(", ")
                ));
            }
            Contribution::Named(_) => {}
        }
    }
    (!sentences.is_empty()).then(|| sentences.join(SENTENCE_SEPARATOR))
}

/// What a request's body carries for one optional string member.
#[derive(Debug, Clone, Copy)]
enum Sent {
    /// The body has no such member at all.
    Omitted,
    /// The member carries this text.
    Text(&'static str),
}

/// One form a string member of this request arrives in.
#[derive(Debug, Clone, Copy)]
struct MemberForm {
    /// What makes this form different from the others.
    name: &'static str,
    /// What the body carries for the member.
    sent: Sent,
    /// What this value contributes to the refusal, or `None` where it is usable.
    contributes: Option<Contribution>,
}

/// One form the `Content` member arrives in.
#[derive(Debug, Clone, Copy)]
struct ContentForm {
    /// What makes this form different from the others.
    name: &'static str,
    /// The body kinds the `Content` carries, or `None` where the request has no `Content`
    /// member at all.
    bodies: Option<&'static [&'static str]>,
    /// What this value contributes to the refusal, or `None` where it is usable.
    contributes: Option<Contribution>,
}

/// The `ConfigurationSetName` member — the first parameter of this request whose value this
/// service's own validation reads. Its shape is `WordCharactersWithDelimiters`, so a value
/// carrying anything else is one this service names in the refusal; binding nothing names no
/// configuration set, which is served.
const NAME_FORMS: &[MemberForm] = &[
    MemberForm {
        name: "no ConfigurationSetName at all",
        sent: Sent::Omitted,
        contributes: None,
    },
    MemberForm {
        name: "a ConfigurationSetName the shape admits",
        sent: Sent::Text("voice-events"),
        contributes: None,
    },
    MemberForm {
        name: "a ConfigurationSetName carrying a space and punctuation",
        sent: Sent::Text("voice events!!"),
        contributes: Some(Contribution::Named("configurationSetName")),
    },
    MemberForm {
        name: "an EMPTY ConfigurationSetName",
        sent: Sent::Text(""),
        contributes: Some(Contribution::Named("configurationSetName")),
    },
];

/// The `Content` member, which binds BEFORE both numbers. The body kinds named here are the
/// three `VoiceMessageContent` declares
/// (`smithy_query(query="shape",shape="VoiceMessageContent",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`:
/// `CallInstructionsMessage`, `PlainTextMessage`, `SSMLMessage`); exactly one of them is a
/// message this service can place, and none or several is not.
const CONTENT_FORMS: &[ContentForm] = &[
    ContentForm {
        name: "no Content at all",
        bodies: None,
        contributes: Some(Contribution::Sentence(UNUSABLE_CONTENT)),
    },
    ContentForm {
        name: "a Content carrying no message body",
        bodies: Some(&[]),
        contributes: Some(Contribution::Sentence(UNUSABLE_CONTENT)),
    },
    ContentForm {
        name: "a Content carrying two message bodies",
        bodies: Some(&["PlainTextMessage", "SSMLMessage"]),
        contributes: Some(Contribution::Sentence(UNUSABLE_CONTENT)),
    },
    ContentForm {
        name: "a Content carrying all three message bodies",
        bodies: Some(&[
            "CallInstructionsMessage",
            "PlainTextMessage",
            "SSMLMessage",
        ]),
        contributes: Some(Contribution::Sentence(UNUSABLE_CONTENT)),
    },
    ContentForm {
        name: "a Content carrying a plain text body",
        bodies: Some(&["PlainTextMessage"]),
        contributes: None,
    },
    ContentForm {
        name: "a Content carrying an SSML body",
        bodies: Some(&["SSMLMessage"]),
        contributes: None,
    },
    ContentForm {
        name: "a Content carrying a call instructions body",
        bodies: Some(&["CallInstructionsMessage"]),
        contributes: None,
    },
];

/// The `DestinationPhoneNumber` member — a `NonEmptyString` there is no message without, so
/// a blank one and an absent one are both parameters this service names in the refusal.
const DESTINATION_FORMS: &[MemberForm] = &[
    MemberForm {
        name: "a destination number carrying content",
        sent: Sent::Text("+12065550100"),
        contributes: None,
    },
    MemberForm {
        name: "a destination number whose content is padded",
        sent: Sent::Text("  +12065550101  "),
        contributes: None,
    },
    MemberForm {
        name: "no DestinationPhoneNumber at all",
        sent: Sent::Omitted,
        contributes: Some(Contribution::Named("destinationPhoneNumber")),
    },
    MemberForm {
        name: "a destination number of one space",
        sent: Sent::Text(" "),
        contributes: Some(Contribution::Named("destinationPhoneNumber")),
    },
    MemberForm {
        name: "a destination number of several kinds of whitespace",
        sent: Sent::Text(" \t\r\n "),
        contributes: Some(Contribution::Named("destinationPhoneNumber")),
    },
];

/// The `OriginationPhoneNumber` member — the LAST parameter this request binds, optional, so
/// binding nothing is served while a blank value is named in the refusal like any other.
const ORIGINATION_FORMS: &[MemberForm] = &[
    MemberForm {
        name: "no OriginationPhoneNumber at all",
        sent: Sent::Omitted,
        contributes: None,
    },
    MemberForm {
        name: "an origination number carrying content",
        sent: Sent::Text("+12065550199"),
        contributes: None,
    },
    MemberForm {
        name: "an origination number of one tab",
        sent: Sent::Text("\t"),
        contributes: Some(Contribution::Named("originationPhoneNumber")),
    },
];

/// The `Content` one row sends: each named body kind filled with the text member its shape
/// declares.
fn content_value(bodies: &[&str]) -> Value {
    let mut content = json!({});
    for kind in bodies {
        content[*kind] = match *kind {
            "SSMLMessage" => json!({ "Text": "<speak>hello</speak>" }),
            "CallInstructionsMessage" => json!({ "Text": "say hello" }),
            _ => json!({ "Text": "hello", "LanguageCode": "en-US", "VoiceId": "Matthew" }),
        };
    }
    content
}

/// The body one row sends: each member in the form the row binds it to.
fn body_for(
    name: &MemberForm,
    content: &ContentForm,
    destination: &MemberForm,
    origination: &MemberForm,
) -> Value {
    let mut body = json!({});
    for (member, sent) in [
        ("ConfigurationSetName", name.sent),
        ("DestinationPhoneNumber", destination.sent),
        ("OriginationPhoneNumber", origination.sent),
    ] {
        if let Sent::Text(text) = sent {
            body[member] = json!(text);
        }
    }
    if let Some(bodies) = content.bodies {
        body["Content"] = content_value(bodies);
    }
    body
}

/// One HTTP answer: the status, the `x-amzn-errortype` header and the parsed JSON body
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

/// The emulator under test: the service mounted on the production router, over the one shared
/// engine the test also reads the record of a sent message from.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
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
        // resource-scoped actions need, so a row with nothing unusable is sent rather than
        // refused (`legacy_surface_authorization.rs` owns that half).
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router, engine }
    }

    /// Send one `SendVoiceMessage` request carrying `body`.
    async fn send_voice(&self, body: Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri("https://sms-voice.us-east-1.amazonaws.com/v1/sms-voice/voice/message")
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

    /// How many messages the store holds.
    fn recorded_count(&self) -> usize {
        self.engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .sent_messages
            .len()
    }

    /// `(destination number, which body kinds were recorded)` of the most recently recorded
    /// message, with the body kinds named as the model names them.
    fn last_record(&self) -> (Option<String>, Vec<&'static str>) {
        let store = self
            .engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>();
        let recorded = store.lock();
        let last = recorded
            .sent_messages
            .last()
            .expect("an accepted send must be recorded");
        let mut kinds: Vec<&'static str> = Vec::new();
        if last.content.call_instructions.is_some() {
            kinds.push("CallInstructionsMessage");
        }
        if last.content.plain_text.is_some() {
            kinds.push("PlainTextMessage");
        }
        if last.content.ssml.is_some() {
            kinds.push("SSMLMessage");
        }
        (last.destination_phone_number.clone(), kinds)
    }
}

/// RULE: a request is refused ONCE, so the one refusal reports what EVERY parameter this
/// handler's own validation cannot use contributes — a name in the single list, or the
/// sentence that parameter's own class is worded with — in the order the input binds them,
/// never merely the fault the handler looked at first. A request with nothing unusable is not
/// refused at all: it is sent, and recorded exactly as it arrived.
#[tokio::test]
async fn one_refusal_reports_every_fault_of_the_send_in_binding_order() {
    let harness = Harness::new();
    let mut served = 0usize;

    for name in NAME_FORMS {
        for content in CONTENT_FORMS {
            for destination in DESTINATION_FORMS {
                for origination in ORIGINATION_FORMS {
                    let case = format!(
                        "{} with {} and {} and {}",
                        name.name, content.name, destination.name, origination.name
                    );
                    // The findings in the order the input binds the members they belong to.
                    let findings: Vec<Contribution> = [
                        name.contributes,
                        content.contributes,
                        destination.contributes,
                        origination.contributes,
                    ]
                    .into_iter()
                    .flatten()
                    .collect();

                    let answer = harness
                        .send_voice(body_for(name, content, destination, origination))
                        .await;

                    match one_refusal(&findings) {
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
                                "case {case:?}: expected x-amzn-errortype BadRequestException, \
                                 got {:?}",
                                answer.error_type_header
                            );
                            assert_eq!(
                                answer.message(),
                                expected,
                                "case {case:?}: expected the message {expected:?}, got {:?}",
                                answer.body
                            );
                        }
                        // Nothing unusable: the send is served and recorded.
                        None => {
                            assert_eq!(
                                answer.status, 200,
                                "case {case:?}: expected the send to be served, got {} {:?}",
                                answer.status, answer.body
                            );
                            let message_id = answer
                                .body
                                .get("MessageId")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            assert!(
                                !message_id.is_empty(),
                                "case {case:?}: expected a MessageId, got {:?}",
                                answer.body
                            );
                            served += 1;
                            assert_eq!(
                                harness.recorded_count(),
                                served,
                                "case {case:?}: expected {served} records after the send, got {}",
                                harness.recorded_count()
                            );
                            let (recorded_destination, recorded_bodies) = harness.last_record();
                            let sent_destination = match destination.sent {
                                Sent::Text(text) => Some(text),
                                Sent::Omitted => None,
                            };
                            assert_eq!(
                                recorded_destination.as_deref(),
                                sent_destination,
                                "case {case:?}: expected the destination recorded exactly as \
                                 sent, got {recorded_destination:?}"
                            );
                            assert_eq!(
                                recorded_bodies,
                                content.bodies.unwrap_or_default(),
                                "case {case:?}: expected the body kind sent to be the one \
                                 recorded, got {recorded_bodies:?}"
                            );
                        }
                    }

                    assert_eq!(
                        harness.recorded_count(),
                        served,
                        "case {case:?}: a refused send must record nothing, got {} records for \
                         {served} served sends",
                        harness.recorded_count()
                    );
                }
            }
        }
    }

    assert!(
        served > 0,
        "the table must contain rows with nothing unusable, got {served} served sends"
    );
}
