// SPDX-License-Identifier: CC0-1.0

//! What a templated send and a render preview state their VALUES as, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this service
//! registered — through the shared harness in the sibling [`crate::configuration_sets`]
//! suite, so routing, deserialization, the handler and the error envelope are all
//! exercised.
//!
//! The rules these cases pin, each over a SET of inputs rather than one:
//!
//! * a bulk send STATES the default values its messages are rendered from: a request whose
//!   `DefaultContent$Template` carries no `TemplateData` member at all has not stated them,
//!   and is refused `400 BadRequestException` with one fixed sentence — a failure of the
//!   REQUEST, so it is answered ahead of every gate on the account and its identities, and a
//!   request that DOES carry the member (the empty object included) passes the gate and is
//!   answered by whatever gate applies to it next;
//! * rendering DATA that is not the set of key-value pairs the model says it is — text the
//!   JSON reader cannot read at all as much as a JSON value that is no object — is ONE
//!   failure of the member, refused with one fixed sentence by every surface that reads it:
//!   the render preview, the single send, a bulk send's default data and one bulk entry's
//!   replacement data;
//! * data that IS such a set but states no value for an attribute the template names is the
//!   DISTINCT refusal naming that attribute, it changes nothing the account holds, and data
//!   stating every attribute renders the template's parts into one MIME document.

use mailparse::MailHeaderMap;
use serde_json::{json, Value};

use crate::configuration_sets::{Reply, Ses};

/// The routes these cases drive, as the model binds them
/// (`smithy_query(query="operation",operation="SendBulkEmail,SendEmail,TestRenderEmailTemplate")`).
const OUTBOUND_BULK_EMAILS: &str = "/v2/email/outbound-bulk-emails";
const OUTBOUND_EMAILS: &str = "/v2/email/outbound-emails";
const TEMPLATES: &str = "/v2/email/templates";

/// The Region `Ses::call` signs for, which is the Region the calls below execute in.
const HOME: &str = "us-east-1";

/// The domain identity the account holds, and the address inside it every accepted send is
/// from.
const SENDER_DOMAIN: &str = "senders.example.com";
const SENDER: &str = "sender@senders.example.com";

/// An address no identity of this account authorises, so a send from it is refused by the
/// verification gate — the gate the request-shape refusals below have to precede.
const STRANGER: &str = "stranger@example.com";

/// The template the cases store, and its parts: one message variable, named in the subject
/// line and in the body.
const TEMPLATE: &str = "receipt";
const TEMPLATE_SUBJECT: &str = "Your receipt, {{name}}";
const TEMPLATE_TEXT: &str = "Thanks {{name}}.";

/// Well-formed default data for that template.
const DATA: &str = "{\"name\":\"Ada\"}";

/// AWS's sentence for a bulk send that states no default values at all: wholly fixed text.
const NO_DEFAULT_DATA: &str = "The default template data does not exist";

/// AWS's sentence for rendering data that is not a set of key-value pairs: fixed text, with
/// no variable part and no trailing full stop.
const INVALID_DATA: &str = "Template rendering data is invalid";

/// The calls these cases drive beside the shared harness's own.
trait TemplateDataCalls {
    async fn store_template(&self, name: &str, content: Value) -> Reply;
    async fn read_template(&self, name: &str) -> Reply;
    async fn render(&self, name: &str, data: &str) -> Reply;
    async fn send(&self, body: Value) -> Reply;
    async fn send_bulk(&self, body: Value) -> Reply;
    async fn sent_last_24_hours(&self) -> f64;
}

impl TemplateDataCalls for Ses {
    async fn store_template(&self, name: &str, content: Value) -> Reply {
        self.call(
            "POST",
            TEMPLATES,
            Some(json!({"TemplateName": name, "TemplateContent": content})),
        )
        .await
    }

    async fn read_template(&self, name: &str) -> Reply {
        self.call("GET", &format!("{TEMPLATES}/{name}"), None).await
    }

    async fn render(&self, name: &str, data: &str) -> Reply {
        self.call(
            "POST",
            &format!("{TEMPLATES}/{name}/render"),
            Some(json!({"TemplateData": data})),
        )
        .await
    }

    async fn send(&self, body: Value) -> Reply {
        self.call("POST", OUTBOUND_EMAILS, Some(body)).await
    }

    async fn send_bulk(&self, body: Value) -> Reply {
        self.call("POST", OUTBOUND_BULK_EMAILS, Some(body)).await
    }

    /// What `GetAccount` reports of the sending quota this window has charged, which a
    /// refused call must leave where it was.
    async fn sent_last_24_hours(&self) -> f64 {
        let account = self.call("GET", "/v2/email/account", None).await;
        account
            .pointer("/SendQuota/SentLast24Hours")
            .and_then(|value| value.as_f64())
            .unwrap_or_default()
    }
}

/// A service holding the stored template, and the sending domain when `verified`.
async fn seeded(verified: bool) -> Ses {
    let ses = Ses::new();
    // These cases are about the default template data a bulk send states, not about the
    // sandbox's destination gate: the account is given production access, so an entry may
    // address a mailbox no account verified (`send_sandbox_destination_gate.rs`).
    ses.grant_production_access();
    let stored = ses
        .store_template(
            TEMPLATE,
            json!({"Subject": TEMPLATE_SUBJECT, "Text": TEMPLATE_TEXT}),
        )
        .await;
    assert_eq!(
        stored.status,
        200,
        "the template must be stored first, got {} {:?}",
        stored.status,
        stored.message()
    );
    if verified {
        ses.verify_identity(SENDER_DOMAIN).await;
    }
    ses
}

/// How a request's default content identifies the template its messages are rendered from.
#[derive(Debug, Clone, Copy)]
enum Identified {
    /// The stored template, by `TemplateName`.
    ByName,
    /// The same stored template, by `TemplateArn`.
    ByArn,
    /// A name the account holds nothing under.
    AbsentName,
    /// Content the request carries inline, naming no stored template at all.
    Inline,
}

/// The `Template` document for `identified`, with `data` as its `TemplateData` when the
/// request carries that member at all.
fn template_of(identified: Identified, data: Option<&str>) -> Value {
    let mut template = match identified {
        Identified::ByName => json!({"TemplateName": TEMPLATE}),
        Identified::ByArn => json!({
            "TemplateArn": format!(
                "arn:aws:ses:{HOME}:{}:template/{TEMPLATE}",
                engine::emulator_account_id()
            ),
        }),
        Identified::AbsentName => json!({"TemplateName": "no-such-template"}),
        Identified::Inline => json!({
            "TemplateContent": {"Subject": TEMPLATE_SUBJECT, "Text": TEMPLATE_TEXT},
        }),
    };
    if let Some(data) = data {
        template["TemplateData"] = json!(data);
    }
    template
}

/// A `SendBulkEmail` body from `from` through `template`, addressing `destinations`
/// recipients.
fn bulk(from: &str, template: Value, destinations: usize) -> Value {
    let entries: Vec<Value> = (0..destinations)
        .map(|index| {
            json!({"Destination": {"ToAddresses": [format!("reader-{index}@example.net")]}})
        })
        .collect();
    json!({
        "FromEmailAddress": from,
        "DefaultContent": {"Template": template},
        "BulkEmailEntries": entries,
    })
}

/// One row of the missing-default-data table.
struct GateRow {
    case: &'static str,
    identified: Identified,
    /// Whether the account holds the verified identity the row's send is from.
    verified: bool,
    /// How many destinations the call addresses.
    destinations: usize,
}

#[tokio::test]
async fn a_bulk_send_that_states_no_default_template_data_is_refused_for_that() {
    // The rows span how the default content identifies the template (the stored name, its
    // ARN, a name the account holds nothing under, content carried inline), the state of the
    // sender (an address no identity authorises, an address inside a verified domain
    // identity) and how many destinations the call addresses — none of which changes the
    // refusal, because what the REQUEST has not stated is settled before any of them is
    // weighed.
    let rows = [
        GateRow {
            case: "the stored template, from an address no identity authorises",
            identified: Identified::ByName,
            verified: false,
            destinations: 3,
        },
        GateRow {
            case: "the stored template, from a verified domain identity",
            identified: Identified::ByName,
            verified: true,
            destinations: 1,
        },
        GateRow {
            case: "the stored template by ARN, from an unverified address",
            identified: Identified::ByArn,
            verified: false,
            destinations: 2,
        },
        GateRow {
            case: "a template name the account holds nothing under",
            identified: Identified::AbsentName,
            verified: true,
            destinations: 1,
        },
        GateRow {
            case: "content carried inline, from an unverified address",
            identified: Identified::Inline,
            verified: false,
            destinations: 3,
        },
        GateRow {
            case: "content carried inline, from a verified domain identity",
            identified: Identified::Inline,
            verified: true,
            destinations: 1,
        },
    ];

    for row in rows {
        let ses = seeded(row.verified).await;
        let from = if row.verified { SENDER } else { STRANGER };
        let reply = ses
            .send_bulk(bulk(
                from,
                template_of(row.identified, None),
                row.destinations,
            ))
            .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {:?}: expected 400 BadRequestException, got {} {:?}",
            row.case,
            reply.status,
            reply.error_code()
        );
        assert_eq!(
            reply.message(),
            NO_DEFAULT_DATA,
            "case {:?}: expected the message {NO_DEFAULT_DATA:?}, got {:?}",
            row.case,
            reply.message()
        );
        // The refused request sent nothing: no entry of it was charged against the quota.
        let charged = ses.sent_last_24_hours().await;
        assert_eq!(
            charged, 0.0,
            "case {:?}: a refused bulk send must charge nothing, got {charged}",
            row.case
        );
    }
}

#[tokio::test]
async fn default_template_data_the_request_does_state_passes_the_gate() {
    // The counter-case: every form of the member a caller may send — the empty object, the
    // empty string the model's `length: min 0` admits, and a stated pair — passes this gate,
    // so the call is answered by whatever gate applies to it NEXT: the verification gate for
    // an unverified sender, and acceptance for a verified one.
    let data = ["{}", "", DATA];
    for carried in data {
        for verified in [false, true] {
            let case = format!("TemplateData {carried:?} from a verified sender: {verified}");
            let ses = seeded(verified).await;
            let from = if verified { SENDER } else { STRANGER };
            let reply = ses
                .send_bulk(bulk(from, template_of(Identified::ByName, Some(carried)), 2))
                .await;
            assert_ne!(
                reply.error_code().as_str(),
                "BadRequestException",
                "case {case:?}: stated data must pass the gate, got {} {:?}",
                reply.status,
                reply.message()
            );
            let expected = if verified { 200 } else { 400 };
            assert_eq!(
                reply.status, expected,
                "case {case:?}: expected {expected}, got {} {:?}",
                reply.status,
                reply.message()
            );
            if verified {
                let results = entry_results(&reply);
                assert_eq!(
                    results.len(),
                    2,
                    "case {case:?}: expected one result per entry, got {results:?}"
                );
                for (status, message_id, error) in &results {
                    assert_eq!(
                        (status.as_str(), message_id.is_empty(), error.as_str()),
                        ("SUCCESS", false, ""),
                        "case {case:?}: expected every entry accepted with an id, got \
                         {results:?}"
                    );
                }
            } else {
                assert_eq!(
                    reply.error_code().as_str(),
                    "MessageRejected",
                    "case {case:?}: expected the verification gate to answer, got {:?}",
                    reply.message()
                );
            }
        }
    }
}

/// The `(Status, MessageId, Error)` of every `BulkEmailEntryResults` entry, in the order the
/// response reported them. An absent member reads as the empty string.
fn entry_results(reply: &Reply) -> Vec<(String, String, String)> {
    reply
        .member("BulkEmailEntryResults")
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let member = |name: &str| {
                        row.get(name)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    (member("Status"), member("MessageId"), member("Error"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The forms rendering data takes when it is not the set of key-value pairs the model says
/// it is: text the JSON reader cannot read at all, and every JSON value that is no object.
const MALFORMED_DATA: [(&str, &str); 10] = [
    ("an object with unquoted text inside it", "{not valid json}"),
    ("text that is no JSON at all", "name=Ada"),
    ("an object that never closes", "{\"name\":"),
    ("a document of another language", "<data name=\"Ada\"/>"),
    ("an empty JSON list", "[]"),
    ("a JSON list of values", "[1,2]"),
    ("a JSON string", "\"Ada\""),
    ("a JSON number", "42"),
    ("a JSON boolean", "true"),
    ("JSON null", "null"),
];

#[tokio::test]
async fn rendering_data_that_is_no_set_of_key_value_pairs_is_refused_in_one_sentence() {
    // One failure of the member, however the data fails to be that set, and one sentence for
    // it wherever the data is read: the surface that PREVIEWS a render, the single send and a
    // bulk send's default data each refuse the whole call, and the same data carried by ONE
    // bulk entry is that entry's own `INVALID_PARAMETER` result inside an accepted call.
    for (case, data) in MALFORMED_DATA {
        let ses = seeded(true).await;

        let rendered = ses.render(TEMPLATE, data).await;
        refused(&format!("TestRenderEmailTemplate: {case}"), &rendered);

        let sent = ses
            .send(json!({
                "FromEmailAddress": SENDER,
                "Destination": {"ToAddresses": ["reader@example.net"]},
                "Content": {"Template": {"TemplateName": TEMPLATE, "TemplateData": data}},
            }))
            .await;
        refused(&format!("SendEmail: {case}"), &sent);

        let bulk_default = ses
            .send_bulk(bulk(SENDER, template_of(Identified::ByName, Some(data)), 2))
            .await;
        refused(&format!("SendBulkEmail default data: {case}"), &bulk_default);

        // The same data as ONE entry's replacement values: the defect belongs to that entry,
        // so the call is accepted and the entry carries the sentence as its own error.
        let mut request = bulk(SENDER, template_of(Identified::ByName, Some(DATA)), 2);
        request["BulkEmailEntries"][1]["ReplacementEmailContent"] = json!({
            "ReplacementTemplate": {"ReplacementTemplateData": data},
        });
        let per_entry = ses.send_bulk(request).await;
        assert_eq!(
            per_entry.status,
            200,
            "case {case:?}: an entry's own defect must not refuse the call, got {} {:?}",
            per_entry.status,
            per_entry.message()
        );
        let results = entry_results(&per_entry);
        assert_eq!(
            results.len(),
            2,
            "case {case:?}: expected one result per entry, got {results:?}"
        );
        assert_eq!(
            (results[1].0.as_str(), results[1].2.as_str()),
            ("INVALID_PARAMETER", INVALID_DATA),
            "case {case:?}: expected the entry to report {INVALID_DATA:?}, got {results:?}"
        );
        assert_eq!(
            results[0].0.as_str(),
            "SUCCESS",
            "case {case:?}: expected the well-formed entry to be sent, got {results:?}"
        );
    }
}

/// Assert `reply` is the one refusal malformed rendering data is answered with.
fn refused(case: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, "BadRequestException"),
        "case {case:?}: expected 400 BadRequestException, got {} {:?}",
        reply.status,
        reply.error_code()
    );
    assert_eq!(
        reply.message(),
        INVALID_DATA,
        "case {case:?}: expected the message {INVALID_DATA:?}, got {:?}",
        reply.message()
    );
}

/// One row of the missing-attribute table: the content a template is stored with, the data a
/// render of it carries, and the attribute the refusal has to name.
struct AttributeRow {
    case: &'static str,
    content: fn() -> Value,
    data: &'static str,
    missing: &'static str,
}

/// The content the order rows store: three attributes, named across all three parts.
fn order_content() -> Value {
    json!({
        "Subject": "Order {{order_id}} confirmed",
        "Text": "Hello {{name}}, order {{order_id}} ships to {{address}}.",
        "Html": "<p>Hello {{name}}, order {{order_id}} ships to {{address}}.</p>",
    })
}

const ATTRIBUTE_ROWS: &[AttributeRow] = &[
    AttributeRow {
        case: "an attribute only the body parts name",
        content: order_content,
        data: r#"{"name":"Mary Major","order_id":"A1"}"#,
        missing: "address",
    },
    AttributeRow {
        case: "another attribute of the same template",
        content: order_content,
        data: r#"{"order_id":"A1","address":"1 Main St"}"#,
        missing: "name",
    },
    AttributeRow {
        case: "the attribute the subject line names, with the body's own stated",
        content: order_content,
        data: r#"{"name":"Mary Major","address":"1 Main St"}"#,
        missing: "order_id",
    },
    AttributeRow {
        case: "a template of one part, whose single attribute is stated for another name",
        content: || json!({"Subject": "Hello {{greeted}}", "Text": "a body of its own"}),
        data: r#"{"other":"Ada"}"#,
        missing: "greeted",
    },
    AttributeRow {
        case: "a dotted path the data stops short of, named as the template wrote it",
        content: || json!({"Subject": "Hi {{customer.first}}", "Text": "a body of its own"}),
        data: r#"{"customer":{"last":"Lovelace"}}"#,
        missing: "customer.first",
    },
];

#[tokio::test]
async fn a_render_whose_data_omits_a_referenced_attribute_names_that_attribute() {
    // The DISTINCT refusal: the data IS a set of key-value pairs, and what is wrong is that
    // it states no value for an attribute the template names — which the sentence reports by
    // name, as the template wrote it. The rows span which part names the missing attribute,
    // how many attributes the template has, and whether the name is a key or a dotted path.
    for row in ATTRIBUTE_ROWS {
        let ses = Ses::new();
        let name = "order";
        let content = (row.content)();
        let stored = ses.store_template(name, content.clone()).await;
        assert_eq!(
            stored.status,
            200,
            "case {:?}: the template must be stored first, got {} {:?}",
            row.case,
            stored.status,
            stored.message()
        );

        let reply = ses.render(name, row.data).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {:?}: expected 400 BadRequestException, got {} {:?}",
            row.case,
            reply.status,
            reply.error_code()
        );
        let expected = format!("Attribute '{}' is not present in the rendering data.", row.missing);
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected the message {expected:?}, got {:?}",
            row.case,
            reply.message()
        );

        // A refused render changes nothing: the stored content reads back exactly as it was
        // saved, after the refusal as before it.
        let read = ses.read_template(name).await;
        assert_eq!(
            (read.status, read.member("TemplateContent")),
            (200, content.clone()),
            "case {:?}: the stored content must be unchanged, got {} {:?}",
            row.case,
            read.status,
            read.member("TemplateContent")
        );
    }
}

#[tokio::test]
async fn data_stating_every_referenced_attribute_renders_the_whole_message() {
    // The positive control of the two refusals above: the same template, with every attribute
    // it names stated, renders one MIME document whose subject line and whose two body parts
    // carry the substituted values.
    let rows: [(&str, &str, &str, &str, &str); 2] = [
        (
            "every attribute stated",
            r#"{"name":"Mary Major","order_id":"A1","address":"1 Main St"}"#,
            "Order A1 confirmed",
            "Hello Mary Major, order A1 ships to 1 Main St.",
            "<p>Hello Mary Major, order A1 ships to 1 Main St.</p>",
        ),
        (
            "the same attributes with other values",
            r#"{"name":"Ada","order_id":"Z9","address":"2 Elm Rd"}"#,
            "Order Z9 confirmed",
            "Hello Ada, order Z9 ships to 2 Elm Rd.",
            "<p>Hello Ada, order Z9 ships to 2 Elm Rd.</p>",
        ),
    ];

    for (case, data, subject, text, html) in rows {
        let ses = Ses::new();
        let name = "order";
        let stored = ses.store_template(name, order_content()).await;
        assert_eq!(
            stored.status,
            200,
            "case {case:?}: the template must be stored first, got {} {:?}",
            stored.status,
            stored.message()
        );

        let reply = ses.render(name, data).await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: expected the render to be accepted, got {} {:?}",
            reply.status,
            reply.message()
        );
        let document = reply
            .member("RenderedTemplate")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let parsed = mailparse::parse_mail(document.as_bytes())
            .unwrap_or_else(|error| panic!("case {case:?}: the render must be a MIME message, got {document:?} ({error})"));
        assert_eq!(
            parsed.headers.get_first_value("Subject").as_deref(),
            Some(subject),
            "case {case:?}: expected the substituted subject, got {document:?}"
        );
        for (media_type, expected) in [("text/plain", text), ("text/html", html)] {
            let part = parsed
                .subparts
                .iter()
                .find(|part| part.ctype.mimetype == media_type)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a {media_type} part, got {document:?}")
                });
            let body = part
                .get_body()
                .unwrap_or_else(|error| panic!("case {case:?}: decode the part ({error})"));
            assert_eq!(
                body.trim_end(),
                expected,
                "case {case:?}: expected the substituted {media_type} part, got {body:?}"
            );
        }
    }
}
