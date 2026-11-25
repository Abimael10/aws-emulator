// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for the BODY of two refusal classes, driven
//! through the real `MultiServiceRouter` so every assertion goes over the wire
//! the SDK speaks.
//!
//! The rules under test:
//!
//! 1. A create refused because the requested name is already taken is worded by
//!    the SCOPE the name is unique in. A domain's name is unique for the Amazon
//!    Web Services account, so the 409 reports that account and interpolates
//!    neither the requested name nor any identifier of the domain that holds it.
//! 2. A field value whose `FieldValueUnion` variant is not the variant the
//!    field's STORED `FieldType` takes is refused with a 400 naming that field
//!    and the variant its type requires — what the caller has to send to repair
//!    the call — and neither the field's type, the variant that was supplied,
//!    nor the request member that carried it. The rule holds wherever a value is
//!    written against the domain's field schema, so it is asserted through
//!    `CreateCase`, `UpdateCase` and both of `CreateRelatedItem`'s value-bearing
//!    contents.
//!
//! The tables below are the classes, not one recorded call: the first's rows
//! differ along the FORM and LENGTH of the taken name, including each published
//! bound of the modelled length and one past it on either side, and along the
//! credential the second call is signed with; the second's rows span every
//! `FieldType` the model declares crossed with every union variant that carries
//! data, and then every operation that writes a value.

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::ops::types::FIELD_TYPE_VALUES;

use crate::harness::{
    domain, field_of_type, fields_of_every_type, get_case_fields, string_at, template, title_value,
    Emulator, BODY_REJECTION_CODE,
};

/// The one account a generated emulator serves, which every ARN it mints
/// carries too (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// The published bounds of `CreateDomain`'s `name`: `DomainName` is
/// `@length(1..100)` with `@pattern ^.*[\S]$`
/// (`smithy_query(query="operation",operation="CreateDomain")`).
const DOMAIN_NAME_MIN_LENGTH: usize = 1;
const DOMAIN_NAME_MAX_LENGTH: usize = 100;

/// An Amazon Connect user ARN: a well-formed `userArnValue`.
const AGENT_ARN: &str = "arn:aws:connect:us-east-1:000000000000:instance/i-1/agent/a-1";

/// A second access key id, so a row can be signed by another caller than the
/// harness's default one.
const SECOND_ACCESS_KEY_ID: &str = "AKIDSECONDCALLER";

/// Every `FieldType` the model declares, paired with the `FieldValueUnion`
/// variant a value of that type is written in and a well-formed value of that
/// variant. Source: the model's own shapes
/// (`smithy_query(query="shape",shape="FieldType,FieldValueUnion")`) — a
/// `Boolean` field takes `booleanValue`, a `Number` field `doubleValue`, a
/// `User` field `userArnValue`, and the string-valued types `DateTime`,
/// `SingleSelect`, `Text` and `Url` take `stringValue`.
fn typed_values() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("Boolean", "booleanValue", json!({ "booleanValue": true })),
        (
            "DateTime",
            "stringValue",
            json!({ "stringValue": "2026-11-14T18:33:56Z" }),
        ),
        ("Number", "doubleValue", json!({ "doubleValue": 42.0 })),
        (
            "SingleSelect",
            "stringValue",
            json!({ "stringValue": "an-option" }),
        ),
        (
            "Text",
            "stringValue",
            json!({ "stringValue": "a customer note" }),
        ),
        (
            "Url",
            "stringValue",
            json!({ "stringValue": "https://example.invalid/tickets/1" }),
        ),
        ("User", "userArnValue", json!({ "userArnValue": AGENT_ARN })),
    ]
}

/// One well-formed value of every `FieldValueUnion` variant that carries data.
/// `emptyValue` is left out: it is "An empty value"
/// (`smithy_query(query="shape",shape="FieldValueUnion")`) and clears a field of
/// any type, so it is asserted ACCEPTED for every type instead.
fn values_by_variant() -> Vec<(&'static str, Value)> {
    vec![
        ("stringValue", json!({ "stringValue": "a supplied string" })),
        ("doubleValue", json!({ "doubleValue": 7.5 })),
        ("booleanValue", json!({ "booleanValue": false })),
        ("userArnValue", json!({ "userArnValue": AGENT_ARN })),
    ]
}

/// The sentence a value of the wrong variant earns: the field it was written
/// against and the variant that field's stored type takes.
fn variant_sentence(field_id: &str, expected_variant: &str) -> String {
    format!("Field '{field_id}' value should be a {expected_variant}")
}

/// What a value-of-the-wrong-variant refusal is about: the field the value was
/// written against, that field's declared type, the variant the type takes and
/// the variant that was supplied.
struct Mismatch<'a> {
    field_id: &'a str,
    field_type: &'a str,
    expected_variant: &'a str,
    supplied_variant: &'a str,
}

/// Assert one call is the modelled 400 whose message is exactly the per-field
/// variant sentence, carrying none of the things AWS leaves out of it.
async fn refused_for_the_variant(
    emulator: &Emulator,
    case: &str,
    method: &str,
    path: &str,
    body: Value,
    mismatch: &Mismatch<'_>,
) {
    let Mismatch {
        field_id,
        field_type,
        expected_variant,
        supplied_variant,
    } = *mismatch;
    let (status, code, message) = emulator
        .failure_detail(case, method, path, Some(body))
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} {code}: {message}"
    );
    assert_eq!(
        message,
        variant_sentence(field_id, expected_variant),
        "case {case:?}: expected the per-field variant sentence, got {message:?}"
    );
    // What the sentence must NOT carry: the field's type, the variant that was
    // supplied, or the request member the value rode in.
    for (what, absent) in [
        ("the field's type", field_type),
        ("the supplied variant", supplied_variant),
        ("the request member", "fields"),
        ("the content member", "content"),
    ] {
        assert!(
            !message.contains(absent),
            "case {case:?}: the sentence must carry no {what} ({absent:?}), got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_duplicate_domain_name_is_refused_with_the_account_the_name_is_taken_in() {
    let emulator = Emulator::new();
    let expected = format!("Domain name already exists for account: {ACCOUNT_ID}");

    // The rows differ along the FORM and the LENGTH of the name that is already
    // taken, including both published bounds of the modelled length.
    let names: Vec<(&'static str, String)> = vec![
        (
            "a name of the minted 8-4-4-4-12 form",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        ),
        (
            "a name carrying spaces and punctuation",
            "Customer care (EU), tier 2".to_string(),
        ),
        (
            "the shortest name the model allows",
            "7".repeat(DOMAIN_NAME_MIN_LENGTH),
        ),
        (
            "the longest name the model allows",
            "n".repeat(DOMAIN_NAME_MAX_LENGTH),
        ),
        (
            "a name of non-ASCII characters",
            "kundenbetreuung-süd".to_string(),
        ),
    ];

    for (name_case, name) in &names {
        let created = emulator
            .ok(
                &format!("CreateDomain with {name_case}"),
                "POST",
                "/domains",
                json!({ "name": name }),
            )
            .await;
        let domain_id = string_at(&created, "domainId", name_case);

        let case = format!("CreateDomain again with {name_case}");
        let (status, code, message) = emulator
            .failure_detail(&case, "POST", "/domains", Some(json!({ "name": name })))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (409, "ConflictException"),
            "case {case:?}: expected 409 ConflictException, got {status} {code}: {message}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the account-scoped sentence, got {message:?}"
        );
        // The account is the ONLY variable part: neither the requested name nor
        // the identifier of the domain that holds it reaches the sentence.
        for (what, absent) in [("name", name.as_str()), ("domainId", domain_id.as_str())] {
            assert!(
                !message.contains(absent),
                "case {case:?}: the sentence must carry no {what} ({absent:?}), got {message:?}"
            );
        }

        // The scope is the ACCOUNT, not the credential the call was signed
        // with: another access key id reads back the same sentence.
        let case = format!("CreateDomain again with {name_case}, signed by another caller");
        let (status, response) = emulator
            .send_as(
                SECOND_ACCESS_KEY_ID,
                "POST",
                "/domains",
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(
            (status, response.get("__type").and_then(Value::as_str)),
            (409, Some("ConflictException")),
            "case {case:?}: expected 409 ConflictException, got {status} {response:?}"
        );
        assert_eq!(
            string_at(&response, "message", &case),
            expected,
            "case {case:?}: every caller of this account reads the same sentence, \
             got {response:?}"
        );
    }

    // The refusals created nothing: each name is held by exactly one domain.
    let listed = emulator
        .ok("ListDomains after the conflicts", "POST", "/domains-list", json!({}))
        .await;
    let summaries = listed
        .get("domains")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ListDomains returned no domains list: {listed:?}"))
        .clone();
    assert_eq!(
        listed.get("nextToken"),
        None,
        "the domains of this test fit one page, got {listed:?}"
    );
    for (name_case, name) in &names {
        let held = summaries
            .iter()
            .filter(|summary| summary.get("name").and_then(Value::as_str) == Some(name.as_str()))
            .count();
        assert_eq!(
            held, 1,
            "case {name_case:?}: exactly one domain must hold the name, got {held} in {summaries:?}"
        );
    }

    // A name outside the modelled length is not this refusal at all: the
    // request's BODY is rejected for its own shape, ahead of the handler and
    // before any name is resolved.
    let outside: Vec<(&'static str, String)> = vec![
        (
            "a name one character past the longest the model allows",
            "n".repeat(DOMAIN_NAME_MAX_LENGTH + 1),
        ),
        (
            "a name one character short of the shortest the model allows",
            String::new(),
        ),
        (
            "a name the model's pattern forbids",
            "a name that ends in a space ".to_string(),
        ),
    ];
    for (case, name) in &outside {
        let (status, code) = emulator
            .failure(case, "POST", "/domains", Some(json!({ "name": name })))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, BODY_REJECTION_CODE),
            "case {case:?}: a name outside the modelled shape must be refused as invalid, \
             got {status} {code}"
        );
    }
}

#[tokio::test]
async fn every_field_value_of_the_wrong_variant_names_the_variant_its_field_type_takes() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "variant-refusal-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    // No required fields: each call below writes exactly one value, so the only
    // rule under test is the value's variant.
    let template_id = template(&emulator, &domain_id, "variant-refusal-template", &[]).await;

    // The catalog is the whole class: every `FieldType` the model declares.
    let catalog = typed_values();
    let mut covered: Vec<&str> = catalog.iter().map(|(declared, _, _)| *declared).collect();
    covered.sort_unstable();
    let mut declared: Vec<&str> = FIELD_TYPE_VALUES.to_vec();
    declared.sort_unstable();
    assert_eq!(
        covered, declared,
        "the table must cover every FieldType the model declares, got {covered:?}"
    );

    for (field_type, expected_variant, accepted) in &catalog {
        let field_id = field_of_type(&fields, field_type);
        for (supplied_variant, value) in values_by_variant() {
            if supplied_variant == *expected_variant {
                continue;
            }
            let case =
                format!("CreateCase writing a {supplied_variant} against a {field_type} field");
            refused_for_the_variant(
                &emulator,
                &case,
                "POST",
                &format!("/domains/{domain_id}/cases"),
                json!({
                    "templateId": template_id,
                    "fields": [{ "id": field_id, "value": value }],
                }),
                &Mismatch {
                    field_id: &field_id,
                    field_type,
                    expected_variant,
                    supplied_variant,
                },
            )
            .await;
        }

        // The value the field's type takes is accepted, and so is the absent
        // value, which clears a field of any type.
        for (accepted_case, value) in [
            ("its own variant", accepted.clone()),
            ("the absent value", json!({ "emptyValue": {} })),
        ] {
            let case = format!("CreateCase writing {accepted_case} against a {field_type} field");
            let created = emulator
                .ok(
                    &case,
                    "POST",
                    &format!("/domains/{domain_id}/cases"),
                    json!({
                        "templateId": template_id,
                        "fields": [
                            title_value("a typed case"),
                            { "id": field_id, "value": value },
                        ],
                    }),
                )
                .await;
            // Round trip: the stored value reads back exactly as written,
            // variant included.
            let case_id = string_at(&created, "caseId", &case);
            let read = get_case_fields(
                &emulator,
                &case,
                &domain_id,
                &case_id,
                std::slice::from_ref(&field_id),
            )
            .await;
            assert_eq!(
                read[0].get("value"),
                Some(&value),
                "case {case:?}: expected {value:?} back, got {read:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_variant_refusal_is_worded_alike_wherever_a_value_is_written() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "variant-refusal-paths-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let number = field_of_type(&fields, "Number");
    let text = field_of_type(&fields, "Text");
    let template_id = template(&emulator, &domain_id, "variant-refusal-paths-template", &[]).await;
    let held = json!({ "stringValue": "the value the case holds" });
    let case = "CreateCase to write against";
    let created = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [
                    title_value("a case to write against"),
                    { "id": text, "value": held },
                ],
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", case);

    // Two pairs of (field, value of another variant), each put to every
    // operation that writes a value against the domain's field schema: the
    // update of a case, the association of a custom related item, and the
    // target values of an SLA that tracks a case field.
    let pairs: Vec<(&'static str, &str, &'static str, &'static str, Value)> = vec![
        (
            "Number",
            number.as_str(),
            "doubleValue",
            "stringValue",
            json!({ "stringValue": "42" }),
        ),
        (
            "Text",
            text.as_str(),
            "stringValue",
            "doubleValue",
            json!({ "doubleValue": 42.0 }),
        ),
    ];

    for (field_type, field_id, expected_variant, supplied_variant, value) in &pairs {
        let writes: Vec<(&'static str, &'static str, String, Value)> = vec![
            (
                "UpdateCase",
                "PUT",
                format!("/domains/{domain_id}/cases/{case_id}"),
                json!({ "fields": [{ "id": field_id, "value": value }] }),
            ),
            (
                "CreateRelatedItem with a custom item's field values",
                "POST",
                format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
                json!({
                    "type": "Custom",
                    "content": {
                        "custom": { "fields": [{ "id": field_id, "value": value }] },
                    },
                }),
            ),
            (
                "CreateRelatedItem with an SLA's target field values",
                "POST",
                format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
                json!({
                    "type": "Sla",
                    "content": {
                        "sla": {
                            "slaInputConfiguration": {
                                "name": "resolve before the target",
                                "type": "CaseField",
                                "fieldId": field_id,
                                "targetFieldValues": [value],
                                "targetSlaMinutes": 60,
                            },
                        },
                    },
                }),
            ),
        ];

        for (operation, method, path, body) in writes {
            let case =
                format!("{operation} writing a {supplied_variant} against a {field_type} field");
            refused_for_the_variant(
                &emulator,
                &case,
                method,
                &path,
                body,
                &Mismatch {
                    field_id,
                    field_type,
                    expected_variant,
                    supplied_variant,
                },
            )
            .await;
        }
    }

    // Every refusal above wrote nothing: the case still holds what it was
    // created with, and the value the field's type takes is accepted.
    let read = get_case_fields(
        &emulator,
        "GetCase after the refusals",
        &domain_id,
        &case_id,
        &[text.clone(), number.clone()],
    )
    .await;
    let text_value = read
        .iter()
        .find(|field| field.get("id").and_then(Value::as_str) == Some(text.as_str()))
        .and_then(|field| field.get("value"))
        .cloned();
    assert_eq!(
        text_value,
        Some(held.clone()),
        "case \"GetCase after the refusals\": the case must still hold what it was created \
         with, got {read:?}"
    );

    let accepted = json!({ "doubleValue": 42.0 });
    let case = "UpdateCase with the variant the field's type takes";
    emulator
        .ok(
            case,
            "PUT",
            &format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": number, "value": accepted }] }),
        )
        .await;
    let read = get_case_fields(
        &emulator,
        case,
        &domain_id,
        &case_id,
        std::slice::from_ref(&number),
    )
    .await;
    assert_eq!(
        read[0].get("value"),
        Some(&accepted),
        "case {case:?}: expected {accepted:?} back, got {read:?}"
    );
}
