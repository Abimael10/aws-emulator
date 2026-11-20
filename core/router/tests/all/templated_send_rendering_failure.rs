// SPDX-License-Identifier: CC0-1.0

//! The RENDERING_FAILURE event a templated SESv2 send publishes when the values it states do
//! not go with the template it renders — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: a templated send whose rendering data states no value for a message
//! variable its template names renders NO message. The send is still accepted, with a
//! `MessageId` of its own ("because it already accepted the message, Amazon SES doesn't
//! return a message stating that it was unable to send the email"), nothing is handed to any
//! recipient — so no `Delivery` event follows — and the render that produced nothing is
//! reported asynchronously as the modelled `RENDERING_FAILURE` event type
//! (`smithy_query(query="shape",shape="EventType")`), published like every other event: to
//! each event destination of the named configuration set that is `Enabled` and whose
//! `MatchingEventTypes` names that type. The published document carries `eventType`
//! 'Rendering Failure', the `mail` object identifying the message by the id the send
//! answered, and a `failure` object naming the template the send was to render and the
//! attribute that had no value, in the sentence SES words that condition with. A send whose
//! data states every variable its template names renders its message, is delivered, and
//! publishes no such event.
//!
//! Three services have to agree for that to be observable — SESv2 publishes, SNS fans out,
//! SQS holds what arrived — and the publish SESv2 makes is an in-process call addressed the
//! way a client's own `sns`-scoped call is addressed, so WHICH mounted model claims it is only
//! decidable with every service mounted. The crate-local suite
//! `services/service-com-amazonaws-sesv2-simpleemailservice-v2/tests/all/send_bulk_email.rs`
//! pins what such a send RENDERS (nothing at all, rather than the reference substituted by
//! the empty string) over a table of data shapes; what only this suite can reach is the
//! published event that reports it.
//!
//! The table spans what the rule generalises over: WHICH part of the template names the
//! variable (the subject line, the text body, the HTML body), how the reference is spelled
//! (the escaped and the two unescaped forms, a dotted path), how the data falls short (an
//! empty object, an object naming something else, data that stops a dotted path short, no
//! `TemplateData` member at all), how the send identifies the template (by name, by the ARN
//! that addresses the same stored template, or as content the request carries inline, which
//! belongs to no named template), and the controls that must produce no failure at all: every
//! variable stated, a stated JSON null, and a template that names no variable.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    drain, drain_until, given_topic_with_subscriber, Grant,
};

/// The Region the whole wiring of these cases lives in.
const REGION: &str = "us-west-2";

/// The domain identity that authorises every send below, and the sender it covers.
const SENDER_DOMAIN: &str = "senders.example.com";
const SENDER: &str = "sender@senders.example.com";

/// The configuration set every send names, and the event destination hung off it.
const SET: &str = "rendering-failure-publication";
const DESTINATION: &str = "events";

/// The stored template every send that names one renders.
const TEMPLATE: &str = "probe";

/// The recipient every send is addressed to: the mailbox simulator's accepting mailbox, so a
/// send whose message was rendered produces a `Delivery` and one whose message was not
/// produces nothing beyond the acceptance and the failure.
const RECIPIENT: &str = "success@simulator.amazonses.com";

/// Every value of the SESv2 `EventType` enum, in the model's own order
/// (`smithy_query(query="shape",shape="EventType")`), so the event types a send PRODUCES are
/// the only thing that decides what the subscribed queue holds.
const EVERY_EVENT_TYPE: &[&str] = &[
    "SEND",
    "REJECT",
    "BOUNCE",
    "COMPLAINT",
    "DELIVERY",
    "OPEN",
    "CLICK",
    "RENDERING_FAILURE",
    "DELIVERY_DELAY",
    "SUBSCRIPTION",
];

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine
/// kept so a case can bring the verification transition the clock owes an identity into the
/// past instead of waiting out its span.
fn router() -> (MultiServiceRouter, std::sync::Arc<aws_emulator_router::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(std::sync::Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    (router, engine)
}

/// One answered SESv2 call: the status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The modelled error code the response names, or an empty string on success.
    fn error_code(&self) -> String {
        let raw = self
            .body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message an error carried.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A string at `pointer`, or `""` when the body carries nothing there.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// One SESv2 call: `restJson1` at the `email` endpoint, in SESv2's own `/v2/email/…` path
/// space, routed by a SigV4 credential scope naming the signing name the SES family shares.
async fn sesv2(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(value.to_string()),
        None => bytes::Bytes::new(),
    };
    let host = format!("email.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ses/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// The same call, which must answer 200.
async fn sesv2_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let answered = sesv2(router, method, path, body).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from SESv2 {method} {path}, got {} {} ({})",
        answered.status,
        answered.error_code(),
        answered.message()
    );
    answered
}

/// A verified sending domain, so every send below is authorised.
async fn given_verified_sender(
    router: &MultiServiceRouter,
    engine: &aws_emulator_router::InMemoryEngine,
    case: &str,
) {
    sesv2_ok(
        router,
        case,
        "POST",
        "/v2/email/identities",
        Some(json!({"EmailIdentity": SENDER_DOMAIN})),
    )
    .await;
    let state = engine.state::<service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State>();
    let mut store = state.lock();
    let past = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .expect("an instant before now");
    for record in store.identities.values_mut() {
        record.verification_due_at = past;
    }
    store.settle(std::time::SystemTime::now());
}

/// The configuration set [`SET`] with one SNS event destination publishing to `topic`, on and
/// matching `matching`.
async fn given_configuration_set(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    matching: &[&str],
) {
    sesv2_ok(
        router,
        case,
        "POST",
        "/v2/email/configuration-sets",
        Some(json!({"ConfigurationSetName": SET})),
    )
    .await;
    sesv2_ok(
        router,
        case,
        "POST",
        &format!("/v2/email/configuration-sets/{SET}/event-destinations"),
        Some(json!({
            "EventDestinationName": DESTINATION,
            "EventDestination": {
                "Enabled": true,
                "MatchingEventTypes": matching,
                "SnsDestination": {"TopicArn": topic},
            },
        })),
    )
    .await;
}

/// The stored template [`TEMPLATE`], with `content` as its `TemplateContent`.
async fn given_template(router: &MultiServiceRouter, case: &str, content: Value) {
    sesv2_ok(
        router,
        case,
        "POST",
        "/v2/email/templates",
        Some(json!({"TemplateName": TEMPLATE, "TemplateContent": content})),
    )
    .await;
}

/// `SendEmail` with `template` as its `EmailContent$Template`, answering the `MessageId` the
/// accepted message was minted.
async fn send_templated(router: &MultiServiceRouter, case: &str, template: Value) -> String {
    let answered = sesv2_ok(
        router,
        case,
        "POST",
        "/v2/email/outbound-emails",
        Some(json!({
            "FromEmailAddress": SENDER,
            "ConfigurationSetName": SET,
            "Destination": {"ToAddresses": [RECIPIENT]},
            "Content": {"Template": template},
        })),
    )
    .await;
    let message_id = answered.text("/MessageId");
    assert!(
        !message_id.is_empty(),
        "case {case:?}: an accepted send must answer a MessageId, got {}",
        answered.body
    );
    message_id
}

/// The event types among `events`, sorted, which is how a set of separate publications is
/// compared: they carry no order of their own.
fn sorted_event_types(events: &[Value]) -> Vec<String> {
    let mut types: Vec<String> = events
        .iter()
        .map(|event| event["eventType"].as_str().unwrap_or_default().to_string())
        .collect();
    types.sort();
    types
}

/// `wanted`, sorted, as [`sorted_event_types`] reports what arrived.
fn sorted(wanted: &[&str]) -> Vec<String> {
    let mut types: Vec<String> = wanted.iter().map(|at| (*at).to_string()).collect();
    types.sort();
    types
}

/// The one event of type `event_type` among `events`, which must be there exactly once.
fn one_event<'a>(case: &str, events: &'a [Value], event_type: &str) -> &'a Value {
    let matched: Vec<&Value> = events
        .iter()
        .filter(|event| event["eventType"].as_str() == Some(event_type))
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "case {case:?}: expected exactly one {event_type} event, got {events:?}"
    );
    matched[0]
}

/// The sentence SES words a render whose data states no value for an attribute with: the fixed
/// text around the attribute's own name, which is the sentence `TestRenderEmailTemplate`
/// refuses such a render with on the same template store.
fn missing_attribute_message(attribute: &str) -> String {
    format!("Attribute '{attribute}' is not present in the rendering data.")
}

/// How a send identifies the template it renders.
#[derive(Clone, Copy)]
enum Identified {
    /// `Template$TemplateName`: the template the account holds under that name.
    ByName,
    /// `Template$TemplateArn`: the ARN that addresses the same stored template.
    ByArn,
    /// `Template$TemplateContent`: content the request carries itself, which belongs to no
    /// named template.
    Inline,
}

/// One row of the table: the template the row saves, how the send identifies it, the values
/// the send states, and the attribute the published failure must name.
struct Row {
    case: &'static str,
    /// The `EmailTemplateContent` the row saves, as a create states it.
    content: fn() -> Value,
    identified: Identified,
    /// The `TemplateData` the send states, or `None` for a send carrying no such member.
    data: Option<&'static str>,
    /// The attribute the `Rendering Failure` must name, or `None` for a row whose data goes
    /// with its template: that send renders its message, is delivered, and publishes no
    /// failure.
    missing: Option<&'static str>,
}

/// The content every row that does not state its own saves: a subject line and a text body
/// naming the same variable.
fn subject_and_text() -> Value {
    json!({"Subject": "probe {{name}}", "Text": "Hello {{name}}, this is a rendering probe."})
}

const ROWS: &[Row] = &[
    Row {
        case: "the subject line names a variable the data states nothing for",
        content: || json!({"Subject": "probe {{name}}", "Text": "a body with no variable"}),
        identified: Identified::ByName,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "the text body names it, and the data states something else",
        content: || json!({"Subject": "a subject with no variable", "Text": "Hello {{name}}"}),
        identified: Identified::ByName,
        data: Some(r#"{"other":"Ada"}"#),
        missing: Some("name"),
    },
    Row {
        case: "the HTML body names it",
        content: || {
            json!({
                "Subject": "a subject with no variable",
                "Text": "a body with no variable",
                "Html": "<p>Hello {{name}}</p>",
            })
        },
        identified: Identified::ByName,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "the unescaped triple-brace spelling of the reference",
        content: || json!({"Subject": "probe {{{name}}}", "Text": "a body with no variable"}),
        identified: Identified::ByName,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "the unescaped ampersand spelling of the reference",
        content: || json!({"Subject": "probe {{&name}}", "Text": "a body with no variable"}),
        identified: Identified::ByName,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "a dotted path the data stops short of, named as the template wrote it",
        content: || {
            json!({"Subject": "probe {{customer.first}}", "Text": "a body with no variable"})
        },
        identified: Identified::ByName,
        data: Some(r#"{"customer":{"last":"Lovelace"}}"#),
        missing: Some("customer.first"),
    },
    Row {
        case: "a dotted path the data states nothing under at all",
        content: || {
            json!({"Subject": "probe {{customer.first}}", "Text": "a body with no variable"})
        },
        identified: Identified::ByName,
        data: Some("{}"),
        missing: Some("customer.first"),
    },
    Row {
        case: "one of two variables stated, so the other is the one reported",
        content: || json!({"Subject": "probe {{name}}", "Text": "You paid {{amount}}."}),
        identified: Identified::ByName,
        data: Some(r#"{"name":"Ada"}"#),
        missing: Some("amount"),
    },
    Row {
        case: "no TemplateData member at all, so no variable has a value",
        content: subject_and_text,
        identified: Identified::ByName,
        data: None,
        missing: Some("name"),
    },
    Row {
        case: "the same stored template addressed by its ARN",
        content: subject_and_text,
        identified: Identified::ByArn,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "content the request carries inline, which belongs to no named template",
        content: subject_and_text,
        identified: Identified::Inline,
        data: Some("{}"),
        missing: Some("name"),
    },
    Row {
        case: "the control: every variable the template names is stated",
        content: subject_and_text,
        identified: Identified::ByName,
        data: Some(r#"{"name":"control"}"#),
        missing: None,
    },
    Row {
        case: "the control: a stated JSON null is a value, written as nothing",
        content: subject_and_text,
        identified: Identified::ByName,
        data: Some(r#"{"name":null}"#),
        missing: None,
    },
    Row {
        case: "the control: a template that names no variable at all, with empty data",
        content: || json!({"Subject": "a plain subject", "Text": "a plain body"}),
        identified: Identified::ByName,
        data: Some("{}"),
        missing: None,
    },
];

#[tokio::test]
async fn a_templated_send_whose_data_omits_a_referenced_variable_publishes_a_rendering_failure() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router();
        let (topic, queue) = given_topic_with_subscriber(
            &router,
            case,
            REGION,
            "ses-rendering-failure-events",
            Grant::Unscoped,
        )
        .await;
        given_verified_sender(&router, &engine, case).await;
        given_template(&router, case, (row.content)()).await;
        // The destination matches EVERY modelled event type, so what the queue holds is
        // decided by the events the send PRODUCES and by nothing else.
        given_configuration_set(&router, case, &topic, EVERY_EVENT_TYPE).await;

        // How this row's send identifies its template, and the values it states for it.
        let mut template = match row.identified {
            Identified::ByName => json!({"TemplateName": TEMPLATE}),
            Identified::ByArn => json!({
                "TemplateArn": format!(
                    "arn:aws:ses:{REGION}:{}:template/{TEMPLATE}",
                    engine::emulator_account_id()
                ),
            }),
            Identified::Inline => json!({"TemplateContent": (row.content)()}),
        };
        if let Some(data) = row.data {
            template["TemplateData"] = json!(data);
        }

        // Whatever its values state, the send is ACCEPTED and answers an id of its own.
        let message_id = send_templated(&router, case, template).await;

        // A send nothing was rendered for publishes the acceptance and the rendering
        // failure, and NOTHING else: no delivery, because nothing was handed to the
        // recipient. One whose data goes with its template publishes the acceptance and the
        // delivery, and no failure at all.
        let expected: &[&str] = match row.missing {
            Some(_) => &["Send", "Rendering Failure"],
            None => &["Send", "Delivery"],
        };
        let mut events = drain_until(&router, case, REGION, &queue, expected.len()).await;
        // One more read, so a row whose send published MORE than the events expected fails
        // on the comparison rather than passing on timing.
        events.extend(drain(&router, case, REGION, &queue).await);
        assert_eq!(
            sorted_event_types(&events),
            sorted(expected),
            "case {case:?}: expected exactly {expected:?} on the queue, got {events:?}"
        );
        for event in &events {
            assert_eq!(
                event["mail"]["messageId"].as_str(),
                Some(message_id.as_str()),
                "case {case:?}: every event must name the message it is about, got {event}"
            );
        }

        match row.missing {
            Some(attribute) => {
                let failure = one_event(case, &events, "Rendering Failure");
                assert_eq!(
                    failure["failure"]["errorMessage"].as_str(),
                    Some(missing_attribute_message(attribute).as_str()),
                    "case {case:?}: expected the failure to name the attribute {attribute:?} in \
                     the sentence SES words it with, got {failure}"
                );
                // A send that named a STORED template names it in the document; content the
                // request carried inline belongs to no named template, so nothing names it.
                let named = match row.identified {
                    Identified::Inline => Value::Null,
                    Identified::ByName | Identified::ByArn => json!(TEMPLATE),
                };
                assert_eq!(
                    failure["failure"]["templateName"], named,
                    "case {case:?}: expected failure.templateName {named:?}, got {failure}"
                );
            }
            None => {
                assert!(
                    events.iter().all(|event| event["failure"].is_null()),
                    "case {case:?}: no event of a send that rendered its message may carry a \
                     failure, got {events:?}"
                );
                // The message WAS rendered, so its subject carries the template's own text.
                let delivered = one_event(case, &events, "Send");
                assert!(
                    !delivered["mail"]["commonHeaders"]["subject"]
                        .as_str()
                        .unwrap_or_default()
                        .is_empty(),
                    "case {case:?}: a rendered message carries the subject the template \
                     rendered, got {delivered}"
                );
            }
        }
    }
}

#[tokio::test]
async fn only_a_destination_matching_the_rendering_failure_receives_it() {
    // The failure is published like every other event: to a destination that is `Enabled` and
    // whose `MatchingEventTypes` names its type, and to no other. A destination matching the
    // acceptance alone therefore receives the acceptance and nothing else — read against the
    // positive control of the SAME send's failure arriving at a destination that matches it.
    let case = "one destination matching every type and one matching SEND alone";
    let (router, engine) = router();
    let (matching_topic, matching_queue) = given_topic_with_subscriber(
        &router,
        case,
        REGION,
        "ses-rendering-failure-matched",
        Grant::Unscoped,
    )
    .await;
    let (send_only_topic, send_only_queue) = given_topic_with_subscriber(
        &router,
        case,
        REGION,
        "ses-rendering-failure-send-only",
        Grant::Unscoped,
    )
    .await;
    given_verified_sender(&router, &engine, case).await;
    given_template(&router, case, subject_and_text()).await;
    given_configuration_set(&router, case, &matching_topic, EVERY_EVENT_TYPE).await;
    sesv2_ok(
        &router,
        case,
        "POST",
        &format!("/v2/email/configuration-sets/{SET}/event-destinations"),
        Some(json!({
            "EventDestinationName": "send-only",
            "EventDestination": {
                "Enabled": true,
                "MatchingEventTypes": ["SEND"],
                "SnsDestination": {"TopicArn": send_only_topic},
            },
        })),
    )
    .await;

    let message_id = send_templated(
        &router,
        case,
        json!({"TemplateName": TEMPLATE, "TemplateData": "{}"}),
    )
    .await;

    let mut matched = drain_until(&router, case, REGION, &matching_queue, 2).await;
    matched.extend(drain(&router, case, REGION, &matching_queue).await);
    assert_eq!(
        sorted_event_types(&matched),
        sorted(&["Send", "Rendering Failure"]),
        "case {case:?}: the destination matching every type receives the acceptance and the \
         failure, got {matched:?}"
    );
    let mut send_only = drain_until(&router, case, REGION, &send_only_queue, 1).await;
    send_only.extend(drain(&router, case, REGION, &send_only_queue).await);
    assert_eq!(
        sorted_event_types(&send_only),
        sorted(&["Send"]),
        "case {case:?}: a destination matching SEND alone receives only the acceptance, got \
         {send_only:?}"
    );
    assert_eq!(
        send_only[0]["mail"]["messageId"].as_str(),
        Some(message_id.as_str()),
        "case {case:?}: that acceptance is of this send, got {}",
        send_only[0]
    );
}

#[tokio::test]
async fn a_bulk_send_publishes_a_rendering_failure_for_the_entry_whose_values_fall_short() {
    // The same rule over `SendBulkEmail`, which renders every destination's message through
    // the same substitution: the values one destination renders with are the entry's own when
    // it states any, so two entries of ONE request fall on either side of the rule — and each
    // is accepted, with a `MessageId` of its own, whichever side it falls on.
    let case = "one bulk entry whose values go with the template and one whose do not";
    let (router, engine) = router();
    let (topic, queue) = given_topic_with_subscriber(
        &router,
        case,
        REGION,
        "ses-bulk-rendering-failure-events",
        Grant::Unscoped,
    )
    .await;
    given_verified_sender(&router, &engine, case).await;
    given_template(&router, case, subject_and_text()).await;
    given_configuration_set(&router, case, &topic, EVERY_EVENT_TYPE).await;

    let answered = sesv2_ok(
        &router,
        case,
        "POST",
        "/v2/email/outbound-bulk-emails",
        Some(json!({
            "FromEmailAddress": SENDER,
            "ConfigurationSetName": SET,
            "DefaultContent": {"Template": {
                "TemplateName": TEMPLATE,
                "TemplateData": "{\"name\":\"Ada\"}",
            }},
            "BulkEmailEntries": [
                {"Destination": {"ToAddresses": [RECIPIENT]}},
                {
                    "Destination": {"ToAddresses": [RECIPIENT]},
                    "ReplacementEmailContent": {"ReplacementTemplate": {
                        "ReplacementTemplateData": "{\"unrelated\":\"value\"}",
                    }},
                },
            ],
        })),
    )
    .await;
    let results = answered.body["BulkEmailEntryResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        2,
        "case {case:?}: expected one result per entry, got {}",
        answered.body
    );
    let mut ids: Vec<String> = Vec::new();
    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            result["Status"].as_str(),
            Some("SUCCESS"),
            "case {case:?}: entry {index} is accepted whether or not its values render, got \
             {result}"
        );
        let id = result["MessageId"].as_str().unwrap_or_default().to_string();
        assert!(
            !id.is_empty(),
            "case {case:?}: entry {index} must answer a MessageId of its own, got {result}"
        );
        ids.push(id);
    }
    assert_ne!(
        ids[0], ids[1],
        "case {case:?}: two accepted entries mint two MessageIds, got {ids:?}"
    );

    // The first entry renders with the request's default values and is delivered; the second
    // replaces them with values that state nothing for the variable the template names, so
    // nothing is rendered for it and its failure is published in place of a delivery.
    let mut events = drain_until(&router, case, REGION, &queue, 4).await;
    events.extend(drain(&router, case, REGION, &queue).await);
    assert_eq!(
        sorted_event_types(&events),
        sorted(&["Delivery", "Rendering Failure", "Send", "Send"]),
        "case {case:?}: expected the acceptance of both entries, the delivery of the one that \
         rendered and the rendering failure of the one that did not, got {events:?}"
    );
    let failure = one_event(case, &events, "Rendering Failure");
    assert_eq!(
        failure["mail"]["messageId"].as_str(),
        Some(ids[1].as_str()),
        "case {case:?}: the failure must name the second entry's message, got {failure}"
    );
    assert_eq!(
        failure["failure"],
        json!({
            "templateName": TEMPLATE,
            "errorMessage": missing_attribute_message("name"),
        }),
        "case {case:?}: expected the failure to name the template and the attribute, got \
         {failure}"
    );
    let delivery = one_event(case, &events, "Delivery");
    assert_eq!(
        delivery["mail"]["messageId"].as_str(),
        Some(ids[0].as_str()),
        "case {case:?}: only the entry that rendered is delivered, got {delivery}"
    );
}
