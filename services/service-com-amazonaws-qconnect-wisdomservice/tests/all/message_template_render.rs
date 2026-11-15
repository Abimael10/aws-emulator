// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `RenderMessageTemplate`.
//!
//! Every assertion goes over the wire, through the same [`Emulator`] harness the rest of
//! this crate's suites drive the production router with: only the HTTP status, the
//! `x-amzn-errortype` header and the JSON body are inspected, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! * a render answers with the STORED content of the template the label addresses, with
//!   every `{{<attribute key>}}` variable the request's `attributes` fills replaced by that
//!   attribute's value and every one it does not filled from the template's own
//!   `defaultAttributes`; a variable neither fills keeps its placeholder exactly as stored
//!   and its key is reported in `attributesNotInterpolated`. The table spans every channel
//!   subtype the model declares, every attribute category, a variable filled by the request,
//!   one filled by the default attributes, one the request overrides, one nothing fills,
//!   content naming no variable at all, and a template holding no content;
//! * the name of a variable is the key of the attribute that fills it: a map category
//!   carries its keys itself, and the structured categories name theirs by member, qualified
//!   by the category — asserted over EVERY member the model declares on each of the three
//!   structured categories, not only the ones a single request names;
//! * a label's version qualifier selects WHICH copy is rendered: `$LATEST` (the default)
//!   renders the working copy, a number renders the snapshot published under it and
//!   `$ACTIVE_VERSION` the activated one, so a later write to the working copy cannot change
//!   what a version renders;
//! * a render REPORTS: two renders of one template answer alike and neither changes what a
//!   later read of the template sees;
//! * a label this knowledge base holds no template under, a knowledge base this account
//!   does not hold, and a qualifier naming a version that was never published are
//!   `ResourceNotFoundException` (HTTP 404); a qualifier that names neither a version nor a
//!   managed qualifier is `ValidationException` (HTTP 400).
//!
//! This model serves the shared `wisdom` endpoint identity (see `src/service.rs`), which is
//! what every request below is signed for.

use serde_json::{json, Map, Value};
use service_com_amazonaws_qconnect_wisdomservice::state::MAX_RENDERED_CONTENT_BYTES;

use super::resource_tags::{label, wisdom_arn, Emulator, Reply, ABSENT_UUID, OTHER_ACCOUNT, REGION};

/// The published bound of ONE attribute value
/// (`smithy_query(query="shape",shape="MessageTemplateAttributeValue")`: `len 1..32767`),
/// which is the longest piece of text one substitution can put into the rendered content.
const ATTRIBUTE_VALUE_MAX: usize = 32767;

/// The version qualifiers the operation's own documentation names
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an actual version
/// number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST"). Written
/// out here rather than imported from the implementation.
const QUALIFIER_LATEST: &str = "$LATEST";
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";

/// A qualifier that names neither a version number nor a managed qualifier, while still
/// satisfying the modelled label pattern
/// (`smithy_query(query="operation",operation="RenderMessageTemplate")`: `messageTemplateId`
/// admits `(:[A-Z0-9_$]+){0,1}`).
const QUALIFIER_UNKNOWN: &str = "$NO_SUCH_QUALIFIER";

/// The fixed sentences this endpoint answers a resource it does not hold with, written out
/// here rather than imported from the implementation.
const MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND: &str = "MessageTemplate does not exist.";
const MESSAGE_KNOWLEDGE_BASE_NOT_FOUND: &str = "Knowledge Base does not exist.";
const MESSAGE_MESSAGE_TEMPLATE_VERSION_NOT_FOUND: &str = "Message template version does not exist.";

/// Every member the model declares on `SystemAttributes`
/// (`smithy_query(query="shape",shape="SystemAttributes")`), paired with the variable name
/// it fills: the `address` of each endpoint structure is reached through the member that
/// holds it.
const SYSTEM_MEMBERS: [(&str, &str); 3] = [
    ("name", "System.Name"),
    ("customerEndpoint", "System.CustomerEndpoint.Address"),
    ("systemEndpoint", "System.SystemEndpoint.Address"),
];

/// Every member the model declares on `AgentAttributes`
/// (`smithy_query(query="shape",shape="AgentAttributes")`), paired with the variable name
/// it fills.
const AGENT_MEMBERS: [(&str, &str); 2] = [
    ("firstName", "Agent.FirstName"),
    ("lastName", "Agent.LastName"),
];

/// Every member the model declares on `CustomerProfileAttributes`
/// (`smithy_query(query="shape",shape="CustomerProfileAttributes")`), in the order it
/// declares them, paired with the variable name it fills. The `custom` map is not here: its
/// keys name their own variables, which the table below covers separately.
const CUSTOMER_PROFILE_MEMBERS: [(&str, &str); 57] = [
    ("profileId", "Customer.ProfileId"),
    ("profileARN", "Customer.ProfileARN"),
    ("firstName", "Customer.FirstName"),
    ("middleName", "Customer.MiddleName"),
    ("lastName", "Customer.LastName"),
    ("accountNumber", "Customer.AccountNumber"),
    ("emailAddress", "Customer.EmailAddress"),
    ("phoneNumber", "Customer.PhoneNumber"),
    ("additionalInformation", "Customer.AdditionalInformation"),
    ("partyType", "Customer.PartyType"),
    ("businessName", "Customer.BusinessName"),
    ("birthDate", "Customer.BirthDate"),
    ("gender", "Customer.Gender"),
    ("mobilePhoneNumber", "Customer.MobilePhoneNumber"),
    ("homePhoneNumber", "Customer.HomePhoneNumber"),
    ("businessPhoneNumber", "Customer.BusinessPhoneNumber"),
    ("businessEmailAddress", "Customer.BusinessEmailAddress"),
    ("address1", "Customer.Address1"),
    ("address2", "Customer.Address2"),
    ("address3", "Customer.Address3"),
    ("address4", "Customer.Address4"),
    ("city", "Customer.City"),
    ("county", "Customer.County"),
    ("country", "Customer.Country"),
    ("postalCode", "Customer.PostalCode"),
    ("province", "Customer.Province"),
    ("state", "Customer.State"),
    ("shippingAddress1", "Customer.ShippingAddress1"),
    ("shippingAddress2", "Customer.ShippingAddress2"),
    ("shippingAddress3", "Customer.ShippingAddress3"),
    ("shippingAddress4", "Customer.ShippingAddress4"),
    ("shippingCity", "Customer.ShippingCity"),
    ("shippingCounty", "Customer.ShippingCounty"),
    ("shippingCountry", "Customer.ShippingCountry"),
    ("shippingPostalCode", "Customer.ShippingPostalCode"),
    ("shippingProvince", "Customer.ShippingProvince"),
    ("shippingState", "Customer.ShippingState"),
    ("mailingAddress1", "Customer.MailingAddress1"),
    ("mailingAddress2", "Customer.MailingAddress2"),
    ("mailingAddress3", "Customer.MailingAddress3"),
    ("mailingAddress4", "Customer.MailingAddress4"),
    ("mailingCity", "Customer.MailingCity"),
    ("mailingCounty", "Customer.MailingCounty"),
    ("mailingCountry", "Customer.MailingCountry"),
    ("mailingPostalCode", "Customer.MailingPostalCode"),
    ("mailingProvince", "Customer.MailingProvince"),
    ("mailingState", "Customer.MailingState"),
    ("billingAddress1", "Customer.BillingAddress1"),
    ("billingAddress2", "Customer.BillingAddress2"),
    ("billingAddress3", "Customer.BillingAddress3"),
    ("billingAddress4", "Customer.BillingAddress4"),
    ("billingCity", "Customer.BillingCity"),
    ("billingCounty", "Customer.BillingCounty"),
    ("billingCountry", "Customer.BillingCountry"),
    ("billingPostalCode", "Customer.BillingPostalCode"),
    ("billingProvince", "Customer.BillingProvince"),
    ("billingState", "Customer.BillingState"),
];

/// `CreateKnowledgeBase` of the type that holds message templates, answering with the id
/// its create minted.
async fn knowledge_base(emulator: &Emulator, name: &str) -> String {
    let reply = emulator
        .send(
            "POST",
            "/knowledgeBases",
            Some(json!({ "name": name, "knowledgeBaseType": "MESSAGE_TEMPLATES" })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: the knowledge base this suite nests under must be created, got HTTP {} \
         with body {}",
        reply.status, reply.body
    );
    reply.string("/knowledgeBase/knowledgeBaseId")
}

/// `CreateMessageTemplate`, answering with the `messageTemplateId` its create minted.
async fn message_template(
    emulator: &Emulator,
    knowledge_base_id: &str,
    name: &str,
    channel_subtype: &str,
    content: Option<Value>,
    default_attributes: Option<Value>,
) -> String {
    let mut body = json!({ "name": name, "channelSubtype": channel_subtype });
    if let Some(content) = content {
        body["content"] = content;
    }
    if let Some(default_attributes) = default_attributes {
        body["defaultAttributes"] = default_attributes;
    }
    let reply = emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates",
                label(knowledge_base_id)
            ),
            Some(body),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: the message template this row renders must be created, got HTTP {} with \
         body {}",
        reply.status, reply.body
    );
    reply.string("/messageTemplate/messageTemplateId")
}

/// `RenderMessageTemplate` with the attributes the row carries.
async fn render(
    emulator: &Emulator,
    knowledge_base: &str,
    message_template: &str,
    attributes: Value,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/render",
                label(knowledge_base),
                label(message_template)
            ),
            Some(json!({ "attributes": attributes })),
        )
        .await
}

/// `GetMessageTemplate`, whose label may carry a version qualifier.
async fn read(emulator: &Emulator, knowledge_base_id: &str, message_template: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base_id),
                label(message_template)
            ),
            None,
        )
        .await
}

/// `CreateMessageTemplateVersion`, answering with the `versionNumber` it published.
async fn publish(emulator: &Emulator, knowledge_base_id: &str, message_template: &str) -> i64 {
    let reply = emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "the version this row renders must be published, got HTTP {} with body {}",
        reply.status, reply.body
    );
    reply
        .field("/messageTemplate/versionNumber")
        .and_then(Value::as_i64)
        .unwrap_or_default()
}

/// `UpdateMessageTemplate` with whatever members the row changes.
async fn update(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
    body: Value,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(body),
        )
        .await
}

/// `ActivateMessageTemplate` for one `versionNumber`.
async fn activate(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
    version_number: i64,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/activate",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(json!({ "versionNumber": version_number })),
        )
        .await
}

/// An `sms` content union member carrying one body text.
fn sms_content(body: &str) -> Value {
    json!({ "sms": { "body": { "plainText": { "content": body } } } })
}

/// The `attributesNotInterpolated` list one render reported, in the order it reported it.
fn not_interpolated(reply: &Reply) -> Vec<String> {
    reply
        .field("/attributesNotInterpolated")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .map(|key| key.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the render table.
struct RenderCase {
    label: &'static str,
    channel_subtype: &'static str,
    /// The content the template is created with; `None` creates a template holding none.
    content: Option<Value>,
    /// The `defaultAttributes` the template is created with.
    default_attributes: Option<Value>,
    /// The `attributes` the render request carries.
    attributes: Value,
    /// The content the render must answer with.
    rendered: Option<Value>,
    /// The keys the render must report as not interpolated, in order.
    not_interpolated: Vec<&'static str>,
}

/// Every row of the render table. The rows differ along what the rule generalises over:
/// the channel subtype the content is given under, the attribute category the variable's
/// value comes from, whether the request or the template's default attributes fill it,
/// whether anything fills it at all, how many variables one text names, and whether the
/// content names a variable at all.
fn render_cases() -> Vec<RenderCase> {
    vec![
        // Every text an EMAIL content carries is template text: the subject, both body
        // formats and the headers.
        RenderCase {
            label: "email, every text filled from the request",
            channel_subtype: "EMAIL",
            content: Some(json!({
                "email": {
                    "subject": "Order for {{Customer.FirstName}}",
                    "body": {
                        "plainText": { "content": "Hello {{Customer.FirstName}}, I am {{Agent.FirstName}}." },
                        "html": { "content": "<p>Hello {{Customer.FirstName}}.</p>" },
                    },
                    "headers": [{ "name": "X-Agent", "value": "{{Agent.LastName}}" }],
                }
            })),
            default_attributes: None,
            attributes: json!({
                "customerProfileAttributes": { "firstName": "Grace" },
                "agentAttributes": { "firstName": "Ada", "lastName": "Lovelace" },
            }),
            rendered: Some(json!({
                "email": {
                    "subject": "Order for Grace",
                    "body": {
                        "plainText": { "content": "Hello Grace, I am Ada." },
                        "html": { "content": "<p>Hello Grace.</p>" },
                    },
                    "headers": [{ "name": "X-Agent", "value": "Lovelace" }],
                }
            })),
            not_interpolated: Vec::new(),
        },
        // A custom attribute names its own variable: the map's key IS the variable name.
        RenderCase {
            label: "sms, a custom attribute filled by its own key",
            channel_subtype: "SMS",
            content: Some(sms_content("Order {{orderId}} ships on {{shipDate}}.")),
            default_attributes: None,
            attributes: json!({ "customAttributes": { "orderId": "A-1", "shipDate": "Monday" } }),
            rendered: Some(sms_content("Order A-1 ships on Monday.")),
            not_interpolated: Vec::new(),
        },
        // The template's own default attribute fills a variable the request does not.
        RenderCase {
            label: "sms, a variable filled by the template's default attributes",
            channel_subtype: "SMS",
            content: Some(sms_content("Hello {{Customer.FirstName}} of {{brand}}.")),
            default_attributes: Some(json!({
                "customerProfileAttributes": { "firstName": "Unknown" },
                "customAttributes": { "brand": "Example" },
            })),
            attributes: json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
            // The request parameter is consulted first and the default attribute is the
            // fallback, so the request's value wins and the default fills the rest.
            rendered: Some(sms_content("Hello Grace of Example.")),
            not_interpolated: Vec::new(),
        },
        // A variable neither object fills keeps its placeholder and is reported.
        RenderCase {
            label: "sms, one variable filled and one nothing fills",
            channel_subtype: "SMS",
            content: Some(sms_content(
                "Hello {{Customer.FirstName}}, your order {{orderId}} is late.",
            )),
            default_attributes: None,
            attributes: json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
            rendered: Some(sms_content("Hello Grace, your order {{orderId}} is late.")),
            not_interpolated: vec!["orderId"],
        },
        // A key repeated, written with whitespace inside the braces, is filled everywhere
        // it appears; a key nothing fills is reported ONCE however often it appears.
        RenderCase {
            label: "sms, repeated variables with whitespace inside the braces",
            channel_subtype: "SMS",
            content: Some(sms_content(
                "{{ Customer.FirstName }} and {{Customer.FirstName}}: {{ orderId }} and {{orderId}}.",
            )),
            default_attributes: None,
            attributes: json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
            rendered: Some(sms_content("Grace and Grace: {{ orderId }} and {{orderId}}.")),
            not_interpolated: vec!["orderId"],
        },
        // An unterminated placeholder and an empty one name no attribute, so neither is
        // filled nor reported.
        RenderCase {
            label: "sms, an unterminated placeholder and an empty one",
            channel_subtype: "SMS",
            content: Some(sms_content("{{orderId and {{}} are not variables")),
            default_attributes: None,
            attributes: json!({ "customAttributes": { "orderId": "A-1" } }),
            rendered: Some(sms_content("{{orderId and {{}} are not variables")),
            not_interpolated: Vec::new(),
        },
        // The `custom` map of the customer-profile category names its own variables too.
        RenderCase {
            label: "sms, a customer-profile custom attribute",
            channel_subtype: "SMS",
            content: Some(sms_content("Your tier is {{tier}}.")),
            default_attributes: None,
            attributes: json!({ "customerProfileAttributes": { "custom": { "tier": "gold" } } }),
            rendered: Some(sms_content("Your tier is gold.")),
            not_interpolated: Vec::new(),
        },
        // The WHATSAPP content carries its template data as one text.
        RenderCase {
            label: "whatsapp, a variable in the content data",
            channel_subtype: "WHATSAPP",
            content: Some(json!({ "whatsApp": { "data": "{\"name\":\"{{template}}\"}" } })),
            default_attributes: None,
            attributes: json!({ "customAttributes": { "template": "order_update" } }),
            rendered: Some(json!({ "whatsApp": { "data": "{\"name\":\"order_update\"}" } })),
            not_interpolated: Vec::new(),
        },
        // The PUSH content carries one structure per push provider, each with its own
        // texts; the system category fills variables through the endpoint it names.
        RenderCase {
            label: "push, variables in two provider structures",
            channel_subtype: "PUSH",
            content: Some(json!({
                "push": {
                    "apns": {
                        "title": "Hello {{System.Name}}",
                        "body": { "content": "We will call {{System.CustomerEndpoint.Address}}." },
                        "action": "OPEN_APP",
                    },
                    "fcm": {
                        "title": "From {{System.SystemEndpoint.Address}}",
                        "body": { "content": "Hello {{Agent.FirstName}}." },
                        "action": "OPEN_APP",
                    },
                }
            })),
            default_attributes: None,
            attributes: json!({
                "systemAttributes": {
                    "name": "Contact",
                    "customerEndpoint": { "address": "+15550100" },
                    "systemEndpoint": { "address": "+15550199" },
                },
            }),
            rendered: Some(json!({
                "push": {
                    "apns": {
                        "title": "Hello Contact",
                        "body": { "content": "We will call +15550100." },
                        "action": "OPEN_APP",
                    },
                    "fcm": {
                        "title": "From +15550199",
                        "body": { "content": "Hello {{Agent.FirstName}}." },
                        "action": "OPEN_APP",
                    },
                }
            })),
            not_interpolated: vec!["Agent.FirstName"],
        },
        // Content that names no variable comes back exactly as it was stored, whatever
        // attributes the request carries.
        RenderCase {
            label: "email, content naming no variable at all",
            channel_subtype: "EMAIL",
            content: Some(json!({
                "email": {
                    "subject": "Your order has shipped",
                    "body": { "plainText": { "content": "Hello, your order is on its way." } },
                }
            })),
            default_attributes: Some(json!({ "customAttributes": { "orderId": "A-1" } })),
            attributes: json!({ "customAttributes": { "orderId": "A-2" } }),
            rendered: Some(json!({
                "email": {
                    "subject": "Your order has shipped",
                    "body": { "plainText": { "content": "Hello, your order is on its way." } },
                }
            })),
            not_interpolated: Vec::new(),
        },
        // A template that holds no content names no variable, so a render of it reports
        // none and no missing key.
        RenderCase {
            label: "email, a template holding no content",
            channel_subtype: "EMAIL",
            content: None,
            default_attributes: None,
            attributes: json!({ "customAttributes": { "orderId": "A-1" } }),
            rendered: None,
            not_interpolated: Vec::new(),
        },
    ]
}

/// The rule: a render answers with the stored content of the addressed template, with every
/// variable the request's attributes fill replaced by its value and every other one filled
/// from the template's `defaultAttributes`; a variable nothing fills keeps its placeholder
/// and its key is reported in `attributesNotInterpolated`.
#[tokio::test]
async fn a_render_substitutes_the_variables_the_attributes_fill_and_reports_the_rest() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-table").await;

    for (index, case) in render_cases().into_iter().enumerate() {
        let case_label = case.label;
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            &format!("render-{index}"),
            case.channel_subtype,
            case.content.clone(),
            case.default_attributes.clone(),
        )
        .await;

        let reply = render(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            case.attributes.clone(),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {case_label:?}: expected HTTP 200, got HTTP {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.field("/content"),
            case.rendered.as_ref(),
            "case {case_label:?}: expected the rendered content {:?}, got {:?}",
            case.rendered,
            reply.field("/content")
        );
        assert_eq!(
            not_interpolated(&reply),
            case.not_interpolated,
            "case {case_label:?}: expected the unresolved keys {:?}, got {:?}",
            case.not_interpolated,
            not_interpolated(&reply)
        );
        // The template holds no attachment — `CreateMessageTemplateAttachment` is its own
        // operation — so a render of it claims none.
        assert_eq!(
            reply.field("/attachments"),
            None,
            "case {case_label:?}: a template holding no attachment renders none, got {:?}",
            reply.field("/attachments")
        );

        // A render reports: the stored template is exactly what it was before.
        let stored = read(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            stored.field("/messageTemplate/content"),
            case.content.as_ref(),
            "case {case_label:?}: the render must not change the stored content, got {:?}",
            stored.field("/messageTemplate/content")
        );
    }
}

/// One text naming every variable in `members`, and the attributes object that fills each
/// of them with a value of its own, so a name mapped to the wrong member cannot pass.
fn catalog_text(members: &[(&str, &str)]) -> String {
    members
        .iter()
        .map(|(_, name)| format!("{{{{{name}}}}}"))
        .collect::<Vec<String>>()
        .join("|")
}

/// The value one catalog row fills its variable with: distinct per member.
fn catalog_value(member: &str) -> String {
    format!("value-of-{member}")
}

/// The text a render of [`catalog_text`] must answer with once every variable is filled.
fn catalog_rendered(members: &[(&str, &str)]) -> String {
    members
        .iter()
        .map(|(member, _)| catalog_value(member))
        .collect::<Vec<String>>()
        .join("|")
}

/// One structured-category row: the request member that carries the category, the
/// member-to-variable catalog of that category, and the attributes object that fills every
/// one of its members.
type CategoryCase = (&'static str, &'static [(&'static str, &'static str)], Value);

/// The rule: the name of a variable is the key of the attribute that fills it — a map
/// category carries its keys itself, and each structured category names its members
/// qualified by the category. Asserted over EVERY member the model declares on each of the
/// three structured categories, and over map keys of both map categories.
#[tokio::test]
async fn every_attribute_the_model_declares_fills_the_variable_of_its_own_name() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-catalog").await;

    // The customer-profile category: all 57 members at once, each with its own value.
    let mut profile = Map::new();
    for (member, _) in CUSTOMER_PROFILE_MEMBERS {
        profile.insert(member.to_string(), json!(catalog_value(member)));
    }
    // The agent category.
    let mut agent = Map::new();
    for (member, _) in AGENT_MEMBERS {
        agent.insert(member.to_string(), json!(catalog_value(member)));
    }
    // The system category, whose two endpoint members carry their value under `address`.
    let system = json!({
        "name": catalog_value("name"),
        "customerEndpoint": { "address": catalog_value("customerEndpoint") },
        "systemEndpoint": { "address": catalog_value("systemEndpoint") },
    });

    let catalogs: [CategoryCase; 3] = [
        (
            "customerProfileAttributes",
            &CUSTOMER_PROFILE_MEMBERS,
            json!({ "customerProfileAttributes": Value::Object(profile) }),
        ),
        (
            "agentAttributes",
            &AGENT_MEMBERS,
            json!({ "agentAttributes": Value::Object(agent) }),
        ),
        (
            "systemAttributes",
            &SYSTEM_MEMBERS,
            json!({ "systemAttributes": system }),
        ),
    ];

    for (category, members, attributes) in catalogs {
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            &format!("catalog-{category}"),
            "SMS",
            Some(sms_content(&catalog_text(members))),
            None,
        )
        .await;
        let reply = render(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            attributes,
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {category:?}: expected HTTP 200, got HTTP {} with body {}",
            reply.status, reply.body
        );
        let expected = catalog_rendered(members);
        assert_eq!(
            reply.string("/content/sms/body/plainText/content"),
            expected,
            "case {category:?}: expected every member to fill the variable of its own name \
             ({expected:?}), got {:?}",
            reply.string("/content/sms/body/plainText/content")
        );
        assert_eq!(
            not_interpolated(&reply),
            Vec::<String>::new(),
            "case {category:?}: every variable is filled, so none is unresolved, got {:?}",
            not_interpolated(&reply)
        );
    }

    // The two MAP categories, whose keys name their own variables: one row per map, each
    // with a key the table above does not name.
    let maps: [(&str, Value); 2] = [
        (
            "customAttributes",
            json!({ "customAttributes": { "warranty.years": "3" } }),
        ),
        (
            "customerProfileAttributes.custom",
            json!({ "customerProfileAttributes": { "custom": { "warranty.years": "3" } } }),
        ),
    ];
    for (category, attributes) in maps {
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            &format!("catalog-map-{category}"),
            "SMS",
            Some(sms_content("Warranty: {{warranty.years}} years.")),
            None,
        )
        .await;
        let reply = render(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            attributes,
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {category:?}: expected HTTP 200, got HTTP {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.string("/content/sms/body/plainText/content"),
            "Warranty: 3 years.",
            "case {category:?}: expected the map key to fill the variable of its own name, got \
             {:?}",
            reply.string("/content/sms/body/plainText/content")
        );
    }
}

/// The rule: the label's version qualifier selects WHICH copy of the template is rendered —
/// `$LATEST` (the default) the working copy, a number the snapshot published under it, and
/// `$ACTIVE_VERSION` the activated one — so a later write to the working copy cannot change
/// what a published version renders.
#[tokio::test]
async fn a_version_qualifier_renders_the_copy_it_addresses() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-versions").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "versioned",
        "SMS",
        Some(sms_content("v1 for {{Customer.FirstName}}")),
        None,
    )
    .await;

    let first = publish(&emulator, &knowledge_base_id, &message_template_id).await;
    let updated = update(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        json!({ "content": sms_content("v2 for {{Customer.FirstName}}") }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "the working copy must be updated, got HTTP {} with body {}",
        updated.status, updated.body
    );
    let second = publish(&emulator, &knowledge_base_id, &message_template_id).await;
    let activated = activate(&emulator, &knowledge_base_id, &message_template_id, first).await;
    assert_eq!(
        activated.status, 200,
        "the first version must be activated, got HTTP {} with body {}",
        activated.status, activated.body
    );
    // And one more write to the working copy, which no published version follows.
    let updated = update(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        json!({ "content": sms_content("v3 for {{Customer.FirstName}}") }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "the working copy must be updated again, got HTTP {} with body {}",
        updated.status, updated.body
    );

    // The rows differ along which copy the qualifier addresses.
    let cases: [(String, &str); 5] = [
        (String::new(), "v3 for Grace"),
        (format!(":{QUALIFIER_LATEST}"), "v3 for Grace"),
        (format!(":{first}"), "v1 for Grace"),
        (format!(":{second}"), "v2 for Grace"),
        (format!(":{QUALIFIER_ACTIVE}"), "v1 for Grace"),
    ];
    for (qualifier, expected) in cases {
        let addressed = format!("{message_template_id}{qualifier}");
        let reply = render(
            &emulator,
            &knowledge_base_id,
            &addressed,
            json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {qualifier:?}: expected HTTP 200, got HTTP {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.string("/content/sms/body/plainText/content"),
            expected,
            "case {qualifier:?}: expected the copy the qualifier addresses to render \
             {expected:?}, got {:?}",
            reply.string("/content/sms/body/plainText/content")
        );
    }
}

/// The rule: a render reports. Two renders of one template answer alike, and neither
/// changes what a later read of the template sees.
#[tokio::test]
async fn a_render_changes_nothing_a_later_caller_reads() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-reports").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "reported",
        "SMS",
        Some(sms_content("Hello {{Customer.FirstName}} and {{orderId}}")),
        Some(json!({ "customAttributes": { "brand": "Example" } })),
    )
    .await;

    let before = read(&emulator, &knowledge_base_id, &message_template_id).await;
    let attributes = json!({ "customerProfileAttributes": { "firstName": "Grace" } });
    let first = render(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        attributes.clone(),
    )
    .await;
    let second = render(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        attributes,
    )
    .await;
    assert_eq!(
        first.body, second.body,
        "two renders of one template with the same attributes answer alike, got {:?} then {:?}",
        first.body, second.body
    );

    let after = read(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        before.body, after.body,
        "a render writes nothing, so the stored template is unchanged: expected {:?}, got {:?}",
        before.body, after.body
    );
}

/// One row of the refusal table: the labels a render addresses, and the answer it gets.
struct RefusalCase {
    label: &'static str,
    knowledge_base: String,
    message_template: String,
    status: u16,
    error_type: &'static str,
    message: String,
}

/// The rule: a label this knowledge base holds no template under, a knowledge base this
/// account does not hold, and a qualifier naming a version that was never published are
/// `ResourceNotFoundException` (HTTP 404); a qualifier that names neither a version number
/// nor a managed qualifier is `ValidationException` (HTTP 400).
#[tokio::test]
async fn a_render_of_a_label_this_endpoint_holds_nothing_under_is_refused() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-refusals").await;
    let other_knowledge_base_id = knowledge_base(&emulator, "render-refusals-other").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "refused",
        "SMS",
        Some(sms_content("Hello {{Customer.FirstName}}")),
        None,
    )
    .await;
    let other_template_id = message_template(
        &emulator,
        &other_knowledge_base_id,
        "refused-other",
        "SMS",
        Some(sms_content("Hello")),
        None,
    )
    .await;

    // The rows differ along WHICH part of the label names nothing: the knowledge base, the
    // template, the pair, or the version qualifier.
    let cases = vec![
        RefusalCase {
            label: "a knowledge base this account does not hold",
            knowledge_base: ABSENT_UUID.to_string(),
            message_template: message_template_id.clone(),
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_KNOWLEDGE_BASE_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "a knowledge base ARN of another account",
            knowledge_base: wisdom_arn(
                "aws",
                REGION,
                OTHER_ACCOUNT,
                &format!("knowledge-base/{knowledge_base_id}"),
            ),
            message_template: message_template_id.clone(),
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_KNOWLEDGE_BASE_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "a template id this knowledge base holds nothing under",
            knowledge_base: knowledge_base_id.clone(),
            message_template: ABSENT_UUID.to_string(),
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "another knowledge base's template",
            knowledge_base: knowledge_base_id.clone(),
            message_template: other_template_id,
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "a version number the template never published",
            knowledge_base: knowledge_base_id.clone(),
            message_template: format!("{message_template_id}:1"),
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_MESSAGE_TEMPLATE_VERSION_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "the activated version while none is activated",
            knowledge_base: knowledge_base_id.clone(),
            message_template: format!("{message_template_id}:{QUALIFIER_ACTIVE}"),
            status: 404,
            error_type: "ResourceNotFoundException",
            message: MESSAGE_MESSAGE_TEMPLATE_VERSION_NOT_FOUND.to_string(),
        },
        RefusalCase {
            label: "a qualifier that names neither a version nor a managed qualifier",
            knowledge_base: knowledge_base_id.clone(),
            message_template: format!("{message_template_id}:{QUALIFIER_UNKNOWN}"),
            status: 400,
            error_type: "ValidationException",
            message: format!(
                "The qualifier {QUALIFIER_UNKNOWN} is not valid. Use a version number, \
                 {QUALIFIER_ACTIVE} or {QUALIFIER_LATEST}."
            ),
        },
    ];

    for case in cases {
        let case_label = case.label;
        let reply = render(
            &emulator,
            &case.knowledge_base,
            &case.message_template,
            json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
        )
        .await;
        assert_eq!(
            reply.status, case.status,
            "case {case_label:?}: expected HTTP {}, got HTTP {} with body {}",
            case.status, reply.status, reply.body
        );
        assert_eq!(
            reply.error_type, case.error_type,
            "case {case_label:?}: expected the modelled {:?}, got {:?}",
            case.error_type, reply.error_type
        );
        assert_eq!(
            reply.string("/message"),
            case.message,
            "case {case_label:?}: expected the message {:?}, got {:?}",
            case.message,
            reply.string("/message")
        );
        assert_eq!(
            reply.field("/content"),
            None,
            "case {case_label:?}: a refused render answers with no content, got {:?}",
            reply.field("/content")
        );
    }

    // A label in the ARN form resolves to the same stored template as its id form.
    let arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    );
    let reply = render(
        &emulator,
        &knowledge_base_id,
        &arn,
        json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case \"the ARN form of the template label\": expected HTTP 200, got HTTP {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("/content/sms/body/plainText/content"),
        "Hello Grace",
        "case \"the ARN form of the template label\": expected the same rendered content as the \
         id form, got {:?}",
        reply.string("/content/sms/body/plainText/content")
    );
}

/// The rule: a render answers only what it can build within the store's rendered-content
/// ceiling. One substitution puts one attribute value into the content, so a template that
/// names a variable many times multiplies a short request into a long answer; past the
/// ceiling the render is the modelled `ValidationException` (HTTP 400) rather than an
/// arbitrarily large body, and up to it the whole substitution is answered.
#[tokio::test]
async fn a_render_whose_substitutions_pass_the_content_ceiling_is_refused() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-ceiling").await;

    // Every row fills ONE variable with the longest value the model admits, and they differ
    // along how many times the stored content names it: how far the product of the two
    // reaches relative to the ceiling.
    let value = "v".repeat(ATTRIBUTE_VALUE_MAX);
    let occurrences_at_ceiling = MAX_RENDERED_CONTENT_BYTES / ATTRIBUTE_VALUE_MAX;
    let cases: [(&str, usize, bool); 3] = [
        ("one occurrence, far under the ceiling", 1, true),
        (
            "the most occurrences that stay under the ceiling",
            occurrences_at_ceiling,
            true,
        ),
        (
            "one occurrence more than the ceiling admits",
            occurrences_at_ceiling + 1,
            false,
        ),
    ];

    for (index, (case_label, occurrences, within)) in cases.into_iter().enumerate() {
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            &format!("ceiling-{index}"),
            "SMS",
            Some(sms_content(&"{{long}}".repeat(occurrences))),
            None,
        )
        .await;
        let reply = render(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            json!({ "customAttributes": { "long": value } }),
        )
        .await;

        if within {
            assert_eq!(
                reply.status, 200,
                "case {case_label:?}: expected HTTP 200, got HTTP {}",
                reply.status
            );
            assert_eq!(
                reply.string("/content/sms/body/plainText/content").len(),
                occurrences * ATTRIBUTE_VALUE_MAX,
                "case {case_label:?}: expected every occurrence substituted ({} bytes), got {}",
                occurrences * ATTRIBUTE_VALUE_MAX,
                reply.string("/content/sms/body/plainText/content").len()
            );
        } else {
            assert_eq!(
                reply.status, 400,
                "case {case_label:?}: expected HTTP 400, got HTTP {}",
                reply.status
            );
            assert_eq!(
                reply.error_type, "ValidationException",
                "case {case_label:?}: expected the modelled ValidationException, got {:?}",
                reply.error_type
            );
            assert_eq!(
                reply.string("/message"),
                format!(
                    "The rendered message template content must not exceed \
                     {MAX_RENDERED_CONTENT_BYTES} bytes. Provide shorter attribute values, or \
                     a message template whose content uses fewer variables, and try again."
                ),
                "case {case_label:?}: expected the ceiling sentence, got {:?}",
                reply.string("/message")
            );
            assert_eq!(
                reply.field("/content"),
                None,
                "case {case_label:?}: a refused render answers with no content"
            );
        }

        // Either way the stored template is untouched: a render writes nothing.
        let stored = read(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            stored.string("/messageTemplate/content/sms/body/plainText/content").len(),
            occurrences * "{{long}}".len(),
            "case {case_label:?}: the stored content is unchanged by the render"
        );
    }
}
