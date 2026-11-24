// SPDX-License-Identifier: CC0-1.0

//! What `SendVoiceMessage` records about the message body the caller supplied.
//!
//! RULE: the stored record mirrors the model's optionality exactly, so which of the
//! three `VoiceMessageContent` bodies the request carried is recorded by that body's
//! own presence, independently of the members inside it — every member of all three
//! body shapes is optional
//! (`smithy_query(query="shape",shape="CallInstructionsMessageType,PlainTextMessageType,SSMLMessageType")`:
//! none carries `@required`), so a body the caller supplied with nothing inside it is
//! still a body that was supplied, and must not read back as the no-body request the
//! handler refuses.
//!
//! The rows differ along what the rule generalises over: which body kind the request
//! chose, and how many of that body's optional members it filled (all of them, only
//! the text, none at all).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::state::VoiceContentRecord;
use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The emulator under test: the service mounted on the production router, over the one
/// shared engine the test also reads state from (the record is observable only there).
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

/// One HTTP answer: the status and the parsed JSON body (`Value::Null` when empty).
struct Answer {
    status: u16,
    body: Value,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's
        // own concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // This suite is about what an ACCEPTED send records, so the account holds the
        // entitlement this API's resource-scoped actions need; the entitlement itself is
        // pinned in `legacy_surface_authorization.rs`.
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router, engine }
    }

    /// Send one `SendVoiceMessage` request whose `Content` is `content`.
    ///
    /// The `Authorization` credential scope names `sms-voice`, this service's sigv4
    /// signing name, which is how the router picks the service that claims it.
    async fn send_voice(&self, content: Value) -> Answer {
        let payload = json!({
            "DestinationPhoneNumber": "+12065550100",
            "Content": content,
        })
        .to_string();
        let request = http::Request::builder()
            .method("POST")
            .uri("https://sms-voice.us-east-1.amazonaws.com/v1/sms-voice/voice/message")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sms-voice/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
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

    /// How many messages the store holds.
    fn recorded_count(&self) -> usize {
        self.engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .sent_messages
            .len()
    }

    /// The content of the most recently recorded message.
    fn last_content(&self) -> VoiceContentRecord {
        let store = self
            .engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>();
        let recorded = store.lock();
        recorded
            .sent_messages
            .last()
            .expect("an accepted send must be recorded")
            .content
            .clone()
    }
}

/// Which of the three `VoiceMessageContent` bodies a record says was supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    CallInstructions,
    PlainText,
    Ssml,
}

/// Every body the record marks present — the rule is that this is exactly the one the
/// request carried, whatever that body held.
fn bodies_present(content: &VoiceContentRecord) -> Vec<Body> {
    let mut present = Vec::new();
    if content.call_instructions.is_some() {
        present.push(Body::CallInstructions);
    }
    if content.plain_text.is_some() {
        present.push(Body::PlainText);
    }
    if content.ssml.is_some() {
        present.push(Body::Ssml);
    }
    present
}

/// The `Text`, `LanguageCode` and `VoiceId` the record holds for the present body
/// (`CallInstructionsMessageType` declares only `Text`, so the other two are never set
/// for it).
fn members(content: &VoiceContentRecord) -> (Option<String>, Option<String>, Option<String>) {
    if let Some(body) = &content.call_instructions {
        return (body.text.clone(), None, None);
    }
    if let Some(body) = &content.plain_text {
        return (
            body.text.clone(),
            body.language_code.clone(),
            body.voice_id.clone(),
        );
    }
    if let Some(body) = &content.ssml {
        return (
            body.text.clone(),
            body.language_code.clone(),
            body.voice_id.clone(),
        );
    }
    (None, None, None)
}

/// One row: the `Content` sent, the body it supplies, and the members that body carried.
struct Case {
    name: &'static str,
    content: Value,
    body: Body,
    text: Option<&'static str>,
    language_code: Option<&'static str>,
    voice_id: Option<&'static str>,
}

#[tokio::test]
async fn every_supplied_body_is_recorded_as_present_with_the_members_it_carried() {
    let cases = [
        Case {
            name: "call instructions with its text",
            content: json!({ "CallInstructionsMessage": { "Text": "press 1" } }),
            body: Body::CallInstructions,
            text: Some("press 1"),
            language_code: None,
            voice_id: None,
        },
        Case {
            name: "call instructions with no members at all",
            content: json!({ "CallInstructionsMessage": {} }),
            body: Body::CallInstructions,
            text: None,
            language_code: None,
            voice_id: None,
        },
        Case {
            name: "plain text with every member",
            content: json!({
                "PlainTextMessage": { "Text": "hello", "LanguageCode": "en-US", "VoiceId": "Matthew" }
            }),
            body: Body::PlainText,
            text: Some("hello"),
            language_code: Some("en-US"),
            voice_id: Some("Matthew"),
        },
        Case {
            name: "plain text with only its text",
            content: json!({ "PlainTextMessage": { "Text": "bare" } }),
            body: Body::PlainText,
            text: Some("bare"),
            language_code: None,
            voice_id: None,
        },
        Case {
            name: "plain text with no members at all",
            content: json!({ "PlainTextMessage": {} }),
            body: Body::PlainText,
            text: None,
            language_code: None,
            voice_id: None,
        },
        Case {
            name: "ssml with every member",
            content: json!({
                "SSMLMessage": { "Text": "<speak>hi</speak>", "LanguageCode": "en-GB", "VoiceId": "Amy" }
            }),
            body: Body::Ssml,
            text: Some("<speak>hi</speak>"),
            language_code: Some("en-GB"),
            voice_id: Some("Amy"),
        },
        Case {
            name: "ssml with no members at all",
            content: json!({ "SSMLMessage": {} }),
            body: Body::Ssml,
            text: None,
            language_code: None,
            voice_id: None,
        },
    ];

    let harness = Harness::new();

    for (index, case) in cases.iter().enumerate() {
        let answer = harness.send_voice(case.content.clone()).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected 200, got {} {}",
            case.name, answer.status, answer.body
        );
        assert_eq!(
            harness.recorded_count(),
            index + 1,
            "case {:?}: every accepted send must be recorded, got {}",
            case.name,
            harness.recorded_count()
        );

        let content = harness.last_content();
        assert_eq!(
            bodies_present(&content),
            vec![case.body],
            "case {:?}: expected exactly the supplied body {:?} to be present, got {:?}",
            case.name,
            case.body,
            content
        );

        let (text, language_code, voice_id) = members(&content);
        assert_eq!(
            (text.as_deref(), language_code.as_deref(), voice_id.as_deref()),
            (case.text, case.language_code, case.voice_id),
            "case {:?}: expected the members the request carried, got {:?}",
            case.name,
            content
        );
    }
}

#[tokio::test]
async fn a_body_with_no_members_is_not_recorded_as_no_body_at_all() {
    // A `Content` carrying no body is refused and recorded nowhere, so a record that
    // marks no body present can only ever be read as "nothing was supplied" — which is
    // why a body supplied empty has to stay distinguishable from it.
    let harness = Harness::new();
    let refused = harness.send_voice(json!({})).await;
    assert_eq!(
        refused.status, 400,
        "case \"no body\": expected 400, got {} {}",
        refused.status, refused.body
    );
    assert_eq!(
        harness.recorded_count(),
        0,
        "case \"no body\": a refused send must record nothing, got {}",
        harness.recorded_count()
    );

    for (case, content) in [
        ("call instructions", json!({ "CallInstructionsMessage": {} })),
        ("plain text", json!({ "PlainTextMessage": {} })),
        ("ssml", json!({ "SSMLMessage": {} })),
    ] {
        let accepted = harness.send_voice(content).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected 200, got {} {}",
            accepted.status, accepted.body
        );
        let content = harness.last_content();
        assert!(
            !bodies_present(&content).is_empty(),
            "case {case:?}: an empty body is still a supplied body, got {content:?}"
        );
    }
}
