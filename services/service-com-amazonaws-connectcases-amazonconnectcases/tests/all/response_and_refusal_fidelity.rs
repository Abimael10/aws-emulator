// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for WHAT THE SERVICE SAYS: the values a reporting
//! operation projects out of stored state, and the wording of the refusals every
//! operation shares. Driven through the real `MultiServiceRouter`, so every
//! assertion goes over the wire the SDK speaks.
//!
//! The rules under test, each asserted over a table that spans its class rather
//! than over one recorded call:
//!
//! 1. A reporting operation projects a case's field values out of what the case
//!    HOLDS: a requested field the case was never written against contributes no
//!    entry, while one explicitly written with the union's `emptyValue` — "An
//!    empty value", a value a caller writes — is reported like any other value.
//!    That is what keeps "cleared" distinguishable from "never set".
//! 2. A request that references a field id the domain's schema does not define is
//!    refused with `ValidationException` (400). A member that CONFIGURES a
//!    resource with field references (a layout's `content`, a template's
//!    `requiredFields`) is ATTRIBUTED in the sentence, so a caller can tell which
//!    part of its payload was rejected; a call that names field ids to read,
//!    write or filter a case's values carries the service's bare sentence, which
//!    attributes them to no member. Either way the ids are the ones the request
//!    spelled, in its own order.
//! 3. `ResourceNotFoundException` (404) carries the sentence this service sends
//!    for the KIND of resource that could not be resolved (its generic one for
//!    the kinds that have none of their own); the resource that could not be
//!    resolved is reported in the error's own `resourceId` / `resourceType`
//!    members, which the model makes required.
//! 4. A create refused because the template it names requires a value the request
//!    did not provide answers `ValidationException` (400) with the service's fixed
//!    sentence naming that one field id and nothing else.
//! 5. `CreateRelatedItem` refuses a request whose `type` contradicts the variant
//!    its `content` carries with `ValidationException` (400), led by the declared
//!    type upper-cased, and associates nothing.
//! 6. A reserved `System` field is reported under the service's DISPLAY name for
//!    it, which the model keeps distinct from the reserved id it is addressed by,
//!    so a client that only knows the display name can resolve the id to send.
//! 7. A listing of layouts reports the layouts the domain still holds, computed
//!    from the stored records: the reader `UpdateLayout`'s refusal above is
//!    checked against, which was a scaffolded 501 before this change.
//! 8. A put of field options against a field whose stored `FieldType` is not
//!    `SingleSelect` is refused with `ValidationException` (400) naming that
//!    STORED type, and no field id — the field is already addressed by the
//!    request's own path labels.
//! 9. Every member of this model that targets a timestamp declares
//!    `@timestampFormat("date-time")`, so every instant this service reports
//!    travels as an RFC 3339 / ISO-8601 date-time STRING naming the instant the
//!    service wrote — never as the epoch-seconds NUMBER the restJson1 protocol
//!    defaults to.
//! 10. A read reports a resource's AGGREGATE members — its tag map, a template's
//!     case-rule, tag-propagation and default-value lists, and a template's
//!     layout configuration — on every record it answers, carrying the empty map,
//!     the empty list or a configuration of explicit nulls when the record holds
//!     nothing in them, so a client reads them without first testing for the key.

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::state::{
    FIELD_NAMESPACE_SYSTEM, RESERVED_SYSTEM_FIELDS, RESOURCE_NOT_FOUND_MESSAGE,
};

use crate::harness::{
    domain, field_of_type, fields_of_every_type, get_case_fields, instant_at, string_at, tags_path,
    template, Emulator,
};

/// An Amazon Connect contact ARN, as a `Contact` related item links to one.
const CONTACT_ARN: &str = "arn:aws:connect:us-east-1:000000000000:instance/i-1/contact/c-1";

/// One `GetCase` entry, as the wire spells it.
fn entry(field_id: &str, value: &Value) -> Value {
    json!({ "id": field_id, "value": value })
}

/// The `fields` list a `SearchCases` answer carries for `case_id`.
async fn search_projection(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    case_id: &str,
    ids: &[String],
) -> Vec<Value> {
    let projection: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
    let response = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases-search"),
            json!({ "fields": projection }),
        )
        .await;
    response
        .get("cases")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no cases list in {response:?}"))
        .iter()
        .find(|item| item.get("caseId").and_then(Value::as_str) == Some(case_id))
        .and_then(|item| item.get("fields"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no fields for {case_id} in {response:?}"))
        .clone()
}

#[tokio::test]
async fn a_case_read_reports_only_the_fields_the_case_holds() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "held-values-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let text = field_of_type(&fields, "Text");
    let number = field_of_type(&fields, "Number");
    let boolean = field_of_type(&fields, "Boolean");
    let datetime = field_of_type(&fields, "DateTime");
    let single_select = field_of_type(&fields, "SingleSelect");
    let template_id = template(&emulator, &domain_id, "held-values-template", &[]).await;

    // The case is written against three of the domain's fields: a Text field
    // with a value, a SingleSelect field with the union's `emptyValue` (a value
    // a caller WROTE, so the case holds it), and the reserved `title`. Every
    // other field of the domain's schema is in schema and unset.
    let text_value = json!({ "stringValue": "the printer is jammed" });
    let empty_value = json!({ "emptyValue": {} });
    let title_value = json!({ "stringValue": "a jammed printer" });
    let created = emulator
        .ok(
            "CreateCase holding three values",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [
                    { "id": text, "value": text_value },
                    { "id": single_select, "value": empty_value },
                    { "id": "title", "value": title_value },
                ],
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", "CreateCase holding three values");

    // The rows differ along which ids are requested, how many of them the case
    // holds, where the unheld ones sit among the held ones, how many times an id
    // is repeated, and whether the field is a Custom one or a reserved System
    // one.
    let rows: Vec<(&str, Vec<String>, Vec<Value>)> = vec![
        (
            "one unset field alone",
            vec![number.clone()],
            vec![],
        ),
        (
            "one held field alone",
            vec![text.clone()],
            vec![entry(&text, &text_value)],
        ),
        (
            "a field written with the union's emptyValue is held",
            vec![single_select.clone()],
            vec![entry(&single_select, &empty_value)],
        ),
        (
            "an unset field between two held ones",
            vec![text.clone(), number.clone(), single_select.clone()],
            vec![
                entry(&text, &text_value),
                entry(&single_select, &empty_value),
            ],
        ),
        (
            "an unset field before a held one",
            vec![boolean.clone(), text.clone()],
            vec![entry(&text, &text_value)],
        ),
        (
            "several unset fields and nothing else",
            vec![number.clone(), boolean.clone(), datetime.clone()],
            vec![],
        ),
        (
            "an unset reserved System field",
            vec!["summary".to_string()],
            vec![],
        ),
        (
            "a held reserved System field",
            vec!["title".to_string()],
            vec![entry("title", &title_value)],
        ),
        (
            "the same unset id twice",
            vec![number.clone(), number.clone()],
            vec![],
        ),
        (
            "the same held id twice",
            vec![text.clone(), text.clone()],
            vec![entry(&text, &text_value), entry(&text, &text_value)],
        ),
        (
            "every field the domain defines a value could be written against",
            {
                let mut every: Vec<String> = fields.iter().map(|(_, id)| id.clone()).collect();
                every.push("title".to_string());
                every.push("status".to_string());
                every
            },
            vec![
                entry(&single_select, &empty_value),
                entry(&text, &text_value),
                entry("title", &title_value),
            ],
        ),
    ];

    for (case, requested, expected) in rows {
        let read = get_case_fields(&emulator, case, &domain_id, &case_id, &requested).await;
        assert_eq!(
            read, expected,
            "case {case:?}: expected {expected:?}, got {read:?}"
        );

        // The same rule in the other reporting operation: a search projects the
        // values the case holds for the ids it was asked to project, so the two
        // readers cannot disagree about what the case carries.
        let projected = search_projection(&emulator, case, &domain_id, &case_id, &requested).await;
        assert_eq!(
            projected, expected,
            "case {case:?}: a search must project what GetCase reports, expected {expected:?}, \
             got {projected:?}"
        );
    }

    // `GetCaseResponse$fields` is a required member, so a read that reports no
    // value still carries the (empty) list, beside the case's own templateId.
    let case = "a read whose every requested field is unset";
    let response = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": number }] }),
        )
        .await;
    assert_eq!(
        response.get("fields"),
        Some(&json!([])),
        "case {case:?}: the required fields list must be reported as empty, got {response:?}"
    );
    assert_eq!(
        string_at(&response, "templateId", case),
        template_id,
        "case {case:?}: the case's stored templateId must still be reported, got {response:?}"
    );

    // The case still holds what was written: dropping the unheld ids from the
    // answer is a projection rule, not a write.
    let read = get_case_fields(
        &emulator,
        "the case after the reads",
        &domain_id,
        &case_id,
        &[text.clone(), single_select.clone()],
    )
    .await;
    assert_eq!(
        read,
        vec![
            entry(&text, &text_value),
            entry(&single_select, &empty_value),
        ],
        "case \"the case after the reads\": a read must leave the stored values alone, got {read:?}"
    );
}

#[tokio::test]
async fn a_field_id_outside_the_domains_schema_is_refused_in_the_services_words() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "unknown-field-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let text = field_of_type(&fields, "Text");
    let template_id = template(&emulator, &domain_id, "unknown-field-template", &[]).await;
    let created = emulator
        .ok(
            "CreateCase for the unknown-id table",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": text, "value": { "stringValue": "x" } }],
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", "CreateCase for the unknown-id table");
    let layout = emulator
        .ok(
            "CreateLayout for the unknown-id table",
            "POST",
            &format!("/domains/{domain_id}/layouts"),
            json!({ "name": "a-layout", "content": { "basic": {} } }),
        )
        .await;
    let layout_id = string_at(&layout, "layoutId", "CreateLayout for the unknown-id table");

    /// A layout content placing `ids` in its top panel.
    fn layout_content(ids: &[&str]) -> Value {
        let placed: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
        json!({ "basic": { "topPanel": { "sections": [{ "fieldGroup": { "fields": placed } }] } } })
    }

    // An id one character long and one at the modelled `FieldId` maximum of 500
    // (`smithy_query(query="shape",shape="FieldId")`), plus one that only looks
    // like a server-minted id: the refusal names the ids the REQUEST carried,
    // whatever their form.
    let long_id = "x".repeat(500);
    let minted_looking = "00000000-0000-4000-8000-000000000000";

    let rows: Vec<UnknownFieldRow> = vec![
        (
            "GetCase names one id the schema does not define",
            "POST",
            format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": "no-such-field" }] }),
            None,
            vec!["no-such-field"],
        ),
        (
            "GetCase names two of them, after a known id",
            "POST",
            format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": text }, { "id": "a" }, { "id": minted_looking }] }),
            None,
            vec!["a", minted_looking],
        ),
        (
            "GetCase names an id at the modelled FieldId maximum",
            "POST",
            format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": long_id }] }),
            None,
            vec![long_id.as_str()],
        ),
        (
            "CreateCase writes a value against one",
            "POST",
            format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": "no-such-field", "value": { "stringValue": "x" } }],
            }),
            None,
            vec!["no-such-field"],
        ),
        (
            "UpdateCase writes a value against one",
            "PUT",
            format!("/domains/{domain_id}/cases/{case_id}"),
            json!({ "fields": [{ "id": "no-such-field", "value": { "stringValue": "x" } }] }),
            None,
            vec!["no-such-field"],
        ),
        (
            "CreateTemplate requires one",
            "POST",
            format!("/domains/{domain_id}/templates"),
            json!({ "name": "requires-a-dangling-field", "requiredFields": [{ "fieldId": "no-such-field" }] }),
            Some("requiredFields"),
            vec!["no-such-field"],
        ),
        (
            "CreateTemplate requires two of them",
            "POST",
            format!("/domains/{domain_id}/templates"),
            json!({
                "name": "requires-two-dangling-fields",
                "requiredFields": [
                    { "fieldId": "a" },
                    { "fieldId": text },
                    { "fieldId": minted_looking },
                ],
            }),
            Some("requiredFields"),
            vec!["a", minted_looking],
        ),
        (
            "UpdateTemplate requires one",
            "PUT",
            format!("/domains/{domain_id}/templates/{template_id}"),
            json!({ "requiredFields": [{ "fieldId": "no-such-field" }] }),
            Some("requiredFields"),
            vec!["no-such-field"],
        ),
        (
            "CreateLayout places one",
            "POST",
            format!("/domains/{domain_id}/layouts"),
            json!({ "name": "places-a-dangling-field", "content": layout_content(&["no-such-field"]) }),
            Some("content"),
            vec!["no-such-field"],
        ),
        (
            "CreateLayout places two of them",
            "POST",
            format!("/domains/{domain_id}/layouts"),
            json!({
                "name": "places-two-dangling-fields",
                "content": layout_content(&["a", text.as_str(), minted_looking]),
            }),
            Some("content"),
            vec!["a", minted_looking],
        ),
        (
            "UpdateLayout places one",
            "PUT",
            format!("/domains/{domain_id}/layouts/{layout_id}"),
            json!({ "content": layout_content(&["no-such-field"]) }),
            Some("content"),
            vec!["no-such-field"],
        ),
        (
            "SearchCases projects one",
            "POST",
            format!("/domains/{domain_id}/cases-search"),
            json!({ "fields": [{ "id": "no-such-field" }] }),
            None,
            vec!["no-such-field"],
        ),
        (
            "SearchCases sorts on one",
            "POST",
            format!("/domains/{domain_id}/cases-search"),
            json!({ "sorts": [{ "fieldId": "no-such-field", "sortOrder": "Asc" }] }),
            None,
            vec!["no-such-field"],
        ),
        (
            "SearchCases filters on one",
            "POST",
            format!("/domains/{domain_id}/cases-search"),
            json!({
                "filter": { "field": { "equalTo": {
                    "id": "no-such-field",
                    "value": { "stringValue": "x" },
                } } },
            }),
            None,
            vec!["no-such-field"],
        ),
        (
            "SearchAllRelatedItems filters a custom item on one",
            "POST",
            format!("/domains/{domain_id}/related-items-search"),
            json!({
                "filters": [{ "custom": { "fields": { "field": { "equalTo": {
                    "id": "no-such-field",
                    "value": { "stringValue": "x" },
                } } } } }],
            }),
            None,
            vec!["no-such-field"],
        ),
        (
            "CreateRelatedItem writes a Custom value against one",
            "POST",
            format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
            json!({
                "type": "Custom",
                "content": { "custom": { "fields": [
                    { "id": "no-such-field", "value": { "stringValue": "x" } },
                ] } },
            }),
            None,
            vec!["no-such-field"],
        ),
        (
            "CreateRelatedItem tracks an SLA against one",
            "POST",
            format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
            json!({
                "type": "Sla",
                "content": { "sla": { "slaInputConfiguration": {
                    "name": "a target on a dangling field",
                    "type": "CaseField",
                    "fieldId": "no-such-field",
                    "targetSlaMinutes": 60,
                } } },
            }),
            None,
            vec!["no-such-field"],
        ),
    ];

    // Both halves of the rule are driven: every configuration member that
    // references the domain's fields, and the case paths that name field ids to
    // read, write or filter values.
    let mut attributed: Vec<&str> = rows
        .iter()
        .filter_map(|(_, _, _, _, member, _)| *member)
        .collect();
    attributed.sort_unstable();
    attributed.dedup();
    assert_eq!(
        attributed,
        vec!["content", "requiredFields"],
        "the table must span every input member that configures a resource with field \
         references, got {attributed:?}"
    );
    assert!(
        rows.iter().any(|(_, _, _, _, member, _)| member.is_none()),
        "the table must also drive the case paths, which attribute their ids to no member"
    );

    for (case, method, path, body, member, unknown) in rows {
        let (status, code, message) = emulator
            .failure_detail(case, method, &path, Some(body))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code}: {message}"
        );
        let expected = unknown_field_sentence(member, &unknown);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(domain_id.as_str()),
            "case {case:?}: the refusal must carry no domain id, got {message:?}"
        );
    }
}

/// One row of the unknown-field-reference table: the case, the call to make
/// (method, path, body), the input member the refusal must attribute the ids to
/// (`None` where the refusal attributes them to no member), and the unknown ids
/// the message must name, in order.
type UnknownFieldRow<'a> = (
    &'a str,
    &'a str,
    String,
    Value,
    Option<&'a str>,
    Vec<&'a str>,
);

/// The sentence a request that references field ids the domain's schema does not
/// define earns.
///
/// A member that CONFIGURES a resource with field references (a layout's
/// `content`, a template's `requiredFields`) is named in the sentence, so the
/// caller can tell which part of the payload was rejected; a call that names
/// field ids to read, write or filter a case's VALUES is refused with the
/// service's bare sentence instead, which attributes them to no member. Both
/// carry the ids in the order the request spelled them.
fn unknown_field_sentence(member: Option<&str>, ids: &[&str]) -> String {
    match member {
        Some(member) => format!(
            "The following field IDs provided in {member} do not exist: {}",
            ids.join(", ")
        ),
        None => format!("Unknown field Ids: {}", ids.join(", ")),
    }
}

#[tokio::test]
async fn field_options_on_a_field_of_another_type_are_refused_by_that_fields_type() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "field-option-type-wording-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let option = json!({ "name": "an option", "value": "an-option", "active": true });

    // One row per `FieldType` the model declares (`FIELD_TYPE_VALUES`, the
    // catalog `fields_of_every_type` creates from): the single select field takes
    // options, and every other type is refused by the type the field is STORED
    // with — the sentence's one variable part.
    for (field_type, field_id) in &fields {
        let case = format!("BatchPutFieldOptions on a {field_type} field");
        let path = format!("/domains/{domain_id}/fields/{field_id}/options");
        let body = json!({ "options": [option.clone()] });
        if *field_type == "SingleSelect" {
            emulator.ok(&case, "PUT", &path, body).await;
            continue;
        }
        let (status, code, message) = emulator
            .failure_detail(&case, "PUT", &path, Some(body))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code}: {message}"
        );
        assert_eq!(
            message,
            format!(
                "Field type ({field_type}) does not support field options. Only SingleSelect \
                 field types are supported"
            ),
            "case {case:?}: expected the stored-type sentence, got {message:?}"
        );
        // The field's type is the ONLY variable part: the field the request
        // addressed by its path labels is not named in the sentence.
        assert!(
            !message.contains(field_id.as_str()),
            "case {case:?}: the sentence must carry no field id, got {message:?}"
        );
    }

    // The refusals wrote nothing, and the one field that takes options holds the
    // option that was accepted.
    for (field_type, field_id) in &fields {
        let case = format!("ListFieldOptions on a {field_type} field");
        let listed = emulator
            .ok(
                &case,
                "POST",
                &format!("/domains/{domain_id}/fields/{field_id}/options-list"),
                json!({}),
            )
            .await;
        let values: Vec<&str> = listed
            .get("options")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no options list in {listed:?}"))
            .iter()
            .filter_map(|held| held.get("value").and_then(Value::as_str))
            .collect();
        let expected: Vec<&str> = if *field_type == "SingleSelect" {
            vec!["an-option"]
        } else {
            Vec::new()
        };
        assert_eq!(
            values, expected,
            "case {case:?}: expected {expected:?} held, got {values:?}"
        );
    }
}

/// The sentence the not-found class carries for a resource of `resource_type`
/// addressed by `resource_id`, written out here rather than taken from the
/// crate so the expectation is independent of the code under test. A call that
/// ADDRESSES a case has the service's own sentence naming the id; the kinds the
/// rows below address answer the generic one.
fn expected_not_found_message(resource_type: &str, resource_id: &str) -> String {
    if resource_type == "case" {
        format!("A case with id '{resource_id}' could not be found.")
    } else {
        RESOURCE_NOT_FOUND_MESSAGE.to_string()
    }
}

#[tokio::test]
async fn a_resource_that_cannot_be_resolved_is_refused_with_its_classs_sentence() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "not-found-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let text = field_of_type(&fields, "Text");
    let template_id = template(&emulator, &domain_id, "not-found-template", &[]).await;
    let layout = emulator
        .ok(
            "CreateLayout for the not-found table",
            "POST",
            &format!("/domains/{domain_id}/layouts"),
            json!({ "name": "a-layout-to-delete", "content": { "basic": {} } }),
        )
        .await;
    let layout_id = string_at(&layout, "layoutId", "CreateLayout for the not-found table");
    let deleted = emulator
        .send(
            "DELETE",
            &format!("/domains/{domain_id}/layouts/{layout_id}"),
            None,
        )
        .await;
    assert_eq!(
        deleted.0, 200,
        "case \"DeleteLayout for the not-found table\": expected HTTP 200, got {deleted:?}"
    );

    // The rows differ in the kind of resource that could not be resolved, the
    // operation that could not resolve it, and whether it never existed or was
    // deleted — one sentence covers every one of them.
    // One row of the not-found table: what it is a case of, the method and path
    // that cannot resolve it, the body, and the kind and id of the resource the
    // error must report.
    type NotFoundRow = (&'static str, &'static str, String, Option<Value>, &'static str, String);
    let rows: Vec<NotFoundRow> = vec![
        (
            "GetDomain on a domain that does not exist",
            "POST",
            "/domains/no-such-domain".to_string(),
            Some(json!({})),
            "domain",
            "no-such-domain".to_string(),
        ),
        (
            "GetCase on a case that does not exist",
            "POST",
            format!("/domains/{domain_id}/cases/no-such-case"),
            Some(json!({ "fields": [{ "id": text }] })),
            "case",
            "no-such-case".to_string(),
        ),
        (
            "GetTemplate on a template that does not exist",
            "POST",
            format!("/domains/{domain_id}/templates/no-such-template"),
            Some(json!({})),
            "template",
            "no-such-template".to_string(),
        ),
        (
            "GetLayout on a layout that does not exist",
            "POST",
            format!("/domains/{domain_id}/layouts/no-such-layout"),
            Some(json!({})),
            "layout",
            "no-such-layout".to_string(),
        ),
        (
            "UpdateLayout on a layout that was deleted",
            "PUT",
            format!("/domains/{domain_id}/layouts/{layout_id}"),
            Some(json!({ "name": "renamed-after-the-delete" })),
            "layout",
            layout_id.clone(),
        ),
        (
            "UpdateCase on a case that does not exist",
            "PUT",
            format!("/domains/{domain_id}/cases/no-such-case"),
            Some(json!({ "fields": [{ "id": text, "value": { "stringValue": "x" } }] })),
            "case",
            "no-such-case".to_string(),
        ),
        (
            "DeleteCase on a case that does not exist",
            "DELETE",
            format!("/domains/{domain_id}/cases/no-such-case"),
            None,
            "case",
            "no-such-case".to_string(),
        ),
        (
            "CreateCase naming a template that does not exist",
            "POST",
            format!("/domains/{domain_id}/cases"),
            Some(json!({ "templateId": "no-such-template", "fields": [] })),
            "template",
            "no-such-template".to_string(),
        ),
        (
            "CreateField in a domain that does not exist",
            "POST",
            "/domains/no-such-domain/fields".to_string(),
            Some(json!({ "name": "a-field", "type": "Text" })),
            "domain",
            "no-such-domain".to_string(),
        ),
    ];

    for (case, method, path, body, resource_type, resource_id) in rows {
        let (status, response) = emulator.send(method, &path, body).await;
        assert_eq!(
            (status, response.get("__type").and_then(Value::as_str)),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} {response:?}"
        );
        let expected = expected_not_found_message(resource_type, &resource_id);
        assert_eq!(
            response.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the sentence {expected:?}, got {response:?}"
        );
        // Beyond the id the case sentence quotes, nothing from the request is
        // interpolated: the resource is reported in the error's own required
        // members instead.
        let message = string_at(&response, "message", case);
        assert!(
            !message.contains(domain_id.as_str()),
            "case {case:?}: the sentence must carry no domain id, got {message:?}"
        );
        assert!(
            resource_type == "case" || !message.contains(resource_id.as_str()),
            "case {case:?}: the sentence must carry no part of the request, got {message:?}"
        );
        assert_eq!(
            string_at(&response, "resourceType", case),
            resource_type,
            "case {case:?}: resourceType must name the kind of resource, got {response:?}"
        );
        assert_eq!(
            string_at(&response, "resourceId", case),
            resource_id,
            "case {case:?}: resourceId must name the resource, got {response:?}"
        );
    }

    // The template the layout table left alone is still there, so the refusals
    // above wrote nothing.
    emulator
        .ok(
            "GetTemplate after the refusals",
            "POST",
            &format!("/domains/{domain_id}/templates/{template_id}"),
            json!({}),
        )
        .await;
}

#[tokio::test]
async fn a_required_field_with_no_value_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "required-sentence-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let text = field_of_type(&fields, "Text");
    let number = field_of_type(&fields, "Number");
    let boolean = field_of_type(&fields, "Boolean");

    let one = template(
        &emulator,
        &domain_id,
        "requires-one",
        std::slice::from_ref(&text),
    )
    .await;
    let three = template(
        &emulator,
        &domain_id,
        "requires-three",
        &[text.clone(), number.clone(), boolean.clone()],
    )
    .await;
    // A template may require a reserved System field like any other, so the
    // sentence has to render a reserved id too.
    let reserved = template(
        &emulator,
        &domain_id,
        "requires-the-reserved-title",
        &["title".to_string(), text.clone()],
    )
    .await;

    let text_value = json!({ "id": text, "value": { "stringValue": "a note" } });
    let number_value = json!({ "id": number, "value": { "doubleValue": 1.5 } });
    let boolean_value = json!({ "id": boolean, "value": { "booleanValue": true } });
    let title_value = json!({ "id": "title", "value": { "stringValue": "a title" } });

    // The rows differ in how many fields the template requires, how many of them
    // the request provides, WHICH one it leaves out (first, middle, last) and
    // whether the unprovided field is a Custom or a reserved System one. The
    // field the sentence names is the first unprovided one in the order the
    // template declares its `requiredFields`.
    let rows: Vec<(&str, &String, Vec<Value>, Option<String>)> = vec![
        ("one required field, none provided", &one, vec![], Some(text.clone())),
        (
            "one required field, provided",
            &one,
            vec![text_value.clone()],
            None,
        ),
        (
            "three required, none provided",
            &three,
            vec![],
            Some(text.clone()),
        ),
        (
            "three required, the first provided",
            &three,
            vec![text_value.clone()],
            Some(number.clone()),
        ),
        (
            "three required, the last one missing",
            &three,
            vec![text_value.clone(), number_value.clone()],
            Some(boolean.clone()),
        ),
        (
            "three required, only the middle one missing",
            &three,
            vec![text_value.clone(), boolean_value.clone()],
            Some(number.clone()),
        ),
        (
            "three required, all provided",
            &three,
            vec![
                text_value.clone(),
                number_value.clone(),
                boolean_value.clone(),
            ],
            None,
        ),
        (
            "three required, a field it does not require provided instead",
            &three,
            vec![title_value.clone()],
            Some(text.clone()),
        ),
        (
            "a reserved System field required and missing",
            &reserved,
            vec![text_value.clone()],
            Some("title".to_string()),
        ),
        (
            "a reserved System field required and provided",
            &reserved,
            vec![title_value.clone(), text_value.clone()],
            None,
        ),
    ];

    for (case, template_id, case_fields, missing) in rows {
        let body = json!({ "templateId": template_id, "fields": case_fields });
        let path = format!("/domains/{domain_id}/cases");
        match missing {
            None => {
                emulator.ok(case, "POST", &path, body).await;
            }
            Some(field_id) => {
                let (status, code, message) = emulator
                    .failure_detail(case, "POST", &path, Some(body))
                    .await;
                assert_eq!(
                    (status, code.as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: expected 400 ValidationException, got {status} {code}: \
                     {message}"
                );
                let expected =
                    format!("Field value is not provided for a required field: {field_id}");
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                assert!(
                    !message.contains(template_id.as_str()),
                    "case {case:?}: the sentence must not carry the template id, got {message:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_related_item_whose_type_contradicts_its_content_is_refused_and_nothing_is_stored() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "mismatched-content-domain").await;
    let fields = fields_of_every_type(&emulator, &domain_id).await;
    let text = field_of_type(&fields, "Text");
    let template_id = template(&emulator, &domain_id, "mismatched-content-template", &[]).await;
    let created = emulator
        .ok(
            "CreateCase for the mismatch table",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": text, "value": { "stringValue": "x" } }],
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", "CreateCase for the mismatch table");
    let path = format!("/domains/{domain_id}/cases/{case_id}/related-items/");
    // `SearchRelatedItems` is still a scaffolded 501 (its own open stub issue),
    // so the case's own audit history is the reader that shows whether an
    // association was stored: associating an item records an edit of the case.
    let history_path = format!("/domains/{domain_id}/cases/{case_id}/audit-history");

    let comment = json!({ "comment": { "body": "a note", "contentType": "Text/Plain" } });
    let contact = json!({ "contact": { "contactArn": CONTACT_ARN } });

    // One row per value of `RelatedItemType`
    // (`smithy_query(query="shape",shape="RelatedItemType")` = Contact |
    // Comment | File | Sla | ConnectCase | Custom), each declared against
    // content of ANOTHER variant, so the rule is asserted over the whole type
    // vocabulary rather than over the one recorded pair.
    let rows: Vec<(&str, Value)> = vec![
        ("Contact", comment.clone()),
        ("Comment", contact.clone()),
        ("File", comment.clone()),
        ("Sla", comment.clone()),
        ("ConnectCase", comment.clone()),
        ("Custom", comment.clone()),
    ];

    for (declared, content) in rows {
        let case = format!("{declared} declared with another variant's content");
        let (status, code, message) = emulator
            .failure_detail(
                &case,
                "POST",
                &path,
                Some(json!({ "type": declared, "content": content })),
            )
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code}: {message}"
        );
        let expected = format!(
            "{} type was specified with a mismatched related-item content.",
            declared.to_uppercase()
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // A refused association leaves the case's history exactly as it was:
        // one event, its own creation.
        let answered = emulator.ok(&case, "POST", &history_path, json!({})).await;
        let events = answered
            .get("auditEvents")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no auditEvents list in {answered:?}"));
        assert_eq!(
            events.len(),
            1,
            "case {case:?}: a refused association must store nothing, got {answered:?}"
        );
    }

    // The control: a pair that agrees is accepted, and the association is then
    // recorded as an edit of the case — so the refusals above are about the
    // mismatch and not about the operation.
    let case = "a type that matches its content";
    let accepted = emulator
        .ok(
            case,
            "POST",
            &path,
            json!({ "type": "Contact", "content": contact }),
        )
        .await;
    let related_item_id = string_at(&accepted, "relatedItemId", case);
    assert!(
        !related_item_id.is_empty(),
        "case {case:?}: an accepted association must mint an id, got {accepted:?}"
    );
    let answered = emulator.ok(case, "POST", &history_path, json!({})).await;
    let events = answered
        .get("auditEvents")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no auditEvents list in {answered:?}"));
    assert_eq!(
        events.len(),
        2,
        "case {case:?}: the accepted association must be recorded beside the case's creation, \
         got {answered:?}"
    );
    assert_eq!(
        events[0].get("relatedItemType").and_then(Value::as_str),
        Some("Contact"),
        "case {case:?}: the recorded edit must name the type of the item it is about, \
         got {answered:?}"
    );
}

#[tokio::test]
async fn a_reserved_system_field_is_reported_under_its_display_name() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "display-name-domain").await;

    let case = "ListFields on a domain nothing but CreateDomain touched";
    let response = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/fields-list"),
            json!({}),
        )
        .await;
    let listed = response
        .get("fields")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no fields list in {response:?}"))
        .clone();

    // Two entries of the catalog pinned as literals — the one the issue cited
    // and one it did not — so a wrong catalog entry fails here.
    for (field_id, name, field_type) in [
        ("title", "Title", "Text"),
        ("status", "Status", "SingleSelect"),
    ] {
        assert!(
            RESERVED_SYSTEM_FIELDS.contains(&(field_id, name, field_type)),
            "case \"the reserved catalog\": {field_id} must be reserved as a {field_type} field \
             named {name:?}, got {RESERVED_SYSTEM_FIELDS:?}"
        );
    }

    // Source of the table: `state::RESERVED_SYSTEM_FIELDS`, the service's own
    // reserved vocabulary, as `(fieldId, name, FieldType)`.
    for (field_id, name, field_type) in RESERVED_SYSTEM_FIELDS {
        let row = format!("the reserved field {field_id}");
        let summary = listed
            .iter()
            .find(|entry| entry.get("fieldId").and_then(Value::as_str) == Some(field_id))
            .unwrap_or_else(|| panic!("case {row:?}: {field_id} missing from {listed:?}"));
        assert_eq!(
            string_at(summary, "namespace", &row),
            FIELD_NAMESPACE_SYSTEM,
            "case {row:?}: a reserved field is service-defined, got {summary:?}"
        );
        assert_eq!(
            string_at(summary, "type", &row),
            *field_type,
            "case {row:?}: the reserved FieldType must be reported, got {summary:?}"
        );
        assert_eq!(
            string_at(summary, "name", &row),
            *name,
            "case {row:?}: the field must be reported under its display name, got {summary:?}"
        );
        // `FieldName` is `@length(1..100)` with pattern `^.*[\S]$`
        // (`smithy_query(query="shape",shape="FieldName")`).
        let reported = string_at(summary, "name", &row);
        assert!(
            (1..=100).contains(&reported.chars().count())
                && reported.trim_end() == reported,
            "case {row:?}: the name must be a well-formed FieldName, got {reported:?}"
        );
    }

    // A name that is not its id is the whole point: the display name of a
    // multi-word field reads as words, not as the reserved word.
    let renamed: Vec<&str> = RESERVED_SYSTEM_FIELDS
        .iter()
        .filter(|(field_id, name, _)| field_id != name)
        .map(|(field_id, _, _)| *field_id)
        .collect();
    assert!(
        renamed.len() >= 5,
        "case \"the display names\": most reserved fields are named by words rather than by \
         their reserved id, got {renamed:?}"
    );

    // The documented flow a client follows: resolve the case-title field by its
    // display name, then write a case against the id that lookup hands back and
    // read it straight back.
    let case = "resolve the title field by its display name";
    let resolved = listed
        .iter()
        .find(|entry| {
            entry.get("namespace").and_then(Value::as_str) == Some(FIELD_NAMESPACE_SYSTEM)
                && entry.get("name").and_then(Value::as_str) == Some("Title")
        })
        .unwrap_or_else(|| panic!("case {case:?}: no System field named Title in {listed:?}"));
    let title_id = string_at(resolved, "fieldId", case);
    assert_eq!(
        title_id, "title",
        "case {case:?}: the display name must resolve to the reserved id, got {resolved:?}"
    );

    let template_id = template(
        &emulator,
        &domain_id,
        "display-name-template",
        std::slice::from_ref(&title_id),
    )
    .await;
    let title_value = json!({ "stringValue": "a case titled through the resolved id" });
    let created = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases"),
            json!({
                "templateId": template_id,
                "fields": [{ "id": title_id, "value": title_value }],
            }),
        )
        .await;
    let case_id = string_at(&created, "caseId", case);
    let read = get_case_fields(
        &emulator,
        case,
        &domain_id,
        &case_id,
        std::slice::from_ref(&title_id),
    )
    .await;
    assert_eq!(
        read,
        vec![entry(&title_id, &title_value)],
        "case {case:?}: the case must read back the title it was written with, got {read:?}"
    );
}

#[tokio::test]
async fn a_layout_listing_reports_the_layouts_the_domain_still_holds() {
    // The reader the refusal table above checks `UpdateLayout` against: a
    // listing of layouts is computed from the stored records, reports a layout
    // the domain holds under the id, ARN and name it was created with, and
    // leaves a deleted one out while `GetLayout` still reports it.
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "layout-listing-domain").await;

    // The rows differ in how many layouts the domain holds and in the page size
    // asked for, which is what the listing's paging generalises over.
    let mut created: Vec<(String, String, String)> = Vec::new();
    for index in 0..3 {
        let name = format!("layout-{index}");
        let case = format!("CreateLayout {name}");
        let response = emulator
            .ok(
                &case,
                "POST",
                &format!("/domains/{domain_id}/layouts"),
                json!({ "name": name, "content": { "basic": {} } }),
            )
            .await;
        created.push((
            string_at(&response, "layoutId", &case),
            string_at(&response, "layoutArn", &case),
            name,
        ));
    }

    /// Every summary a listing reports, following the cursor to its end.
    async fn walk(
        emulator: &Emulator,
        case: &str,
        domain_id: &str,
        max_results: Option<i32>,
    ) -> Vec<Value> {
        let mut collected: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({});
            if let Some(max_results) = max_results {
                body["maxResults"] = json!(max_results);
            }
            if let Some(token) = token.as_deref() {
                body["nextToken"] = json!(token);
            }
            let response = emulator
                .ok(case, "POST", &format!("/domains/{domain_id}/layouts-list"), body)
                .await;
            let page = response
                .get("layouts")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {case:?}: no layouts list in {response:?}"))
                .clone();
            if let Some(max_results) = max_results {
                assert!(
                    page.len() <= max_results as usize,
                    "case {case:?}: a page must not exceed the requested size, got {page:?}"
                );
            }
            collected.extend(page);
            token = response
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        collected
    }

    let expected: Vec<Value> = created
        .iter()
        .map(|(layout_id, layout_arn, name)| {
            json!({ "layoutId": layout_id, "layoutArn": layout_arn, "name": name })
        })
        .collect();
    for (case, max_results) in [
        ("ListLayouts with no page size", None),
        ("ListLayouts one layout at a time", Some(1)),
        ("ListLayouts two at a time", Some(2)),
    ] {
        let listed = walk(&emulator, case, &domain_id, max_results).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: the listing must report every stored layout in creation order, \
             expected {expected:?}, got {listed:?}"
        );
    }

    // A deleted layout is left out of the listing, and only that one: the
    // record itself is still readable, as `DeleteLayout` documents.
    let (deleted_id, _, deleted_name) = created[1].clone();
    let deleted = emulator
        .send(
            "DELETE",
            &format!("/domains/{domain_id}/layouts/{deleted_id}"),
            None,
        )
        .await;
    assert_eq!(
        deleted.0, 200,
        "case \"DeleteLayout one of three\": expected HTTP 200, got {deleted:?}"
    );
    let case = "ListLayouts after a delete";
    let listed = walk(&emulator, case, &domain_id, None).await;
    let names: Vec<String> = listed
        .iter()
        .map(|summary| string_at(summary, "name", case))
        .collect();
    assert_eq!(
        names,
        vec!["layout-0".to_string(), "layout-2".to_string()],
        "case {case:?}: a deleted layout must be left out and no other, got {listed:?}"
    );
    let fetched = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/layouts/{deleted_id}"),
            json!({}),
        )
        .await;
    assert_eq!(
        (
            string_at(&fetched, "name", case),
            fetched.get("deleted").and_then(Value::as_bool),
        ),
        (deleted_name, Some(true)),
        "case {case:?}: the deleted record must still be readable, got {fetched:?}"
    );

    // A domain the store does not hold is the modelled 404, with the same fixed
    // sentence every other unresolvable resource answers.
    let case = "ListLayouts in a domain that does not exist";
    let (status, response) = emulator
        .send("POST", "/domains/no-such-domain/layouts-list", Some(json!({})))
        .await;
    assert_eq!(
        (status, response.get("__type").and_then(Value::as_str)),
        (404, Some("ResourceNotFoundException")),
        "case {case:?}: expected 404 ResourceNotFoundException, got {status} {response:?}"
    );
    assert_eq!(
        response.get("message").and_then(Value::as_str),
        Some(RESOURCE_NOT_FOUND_MESSAGE),
        "case {case:?}: expected the fixed sentence, got {response:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 9, on the model: every timestamp this service declares is a `date-time`
// ---------------------------------------------------------------------------

/// This service's own namespace, so the model walk below skips the Smithy
/// prelude's shapes.
const SERVICE_NAMESPACE: &str = "com.amazonaws.connectcases";

/// How far from this test's own clock reading a reported instant may sit. What
/// is asserted is that the member names the instant the service wrote rather
/// than zero or a fixed literal, so the slack is generous enough that a loaded
/// machine cannot turn it into a timing failure.
const INSTANT_SLACK_SECONDS: f64 = 300.0;

/// The region and account every ARN this emulator mints is scoped to: the
/// harness signs for `us-east-1`.
const HOME_REGION: &str = "us-east-1";
const ACCOUNT: &str = "000000000000";

/// This service's own Smithy model, as shipped in this crate.
fn shipped_model() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-connectcases-amazonconnectcases.json"
    );
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

/// The `@timestampFormat` a member-like node declares, on the node itself or on
/// the shape it targets — Smithy admits the trait in either place.
fn declared_format<'model>(
    shapes: &'model serde_json::Map<String, Value>,
    node: &'model Value,
) -> Option<&'model str> {
    let on_node = node
        .get("traits")
        .and_then(|traits| traits.get("smithy.api#timestampFormat"))
        .and_then(Value::as_str);
    let on_target = node
        .get("target")
        .and_then(Value::as_str)
        .and_then(|target| shapes.get(target))
        .and_then(|shape| shape.get("traits"))
        .and_then(|traits| traits.get("smithy.api#timestampFormat"))
        .and_then(Value::as_str);
    on_node.or(on_target)
}

/// Whether a member-like node targets a timestamp: the Smithy prelude's own
/// `Timestamp`, or a model shape declared `"type": "timestamp"`.
fn targets_a_timestamp(shapes: &serde_json::Map<String, Value>, node: &Value) -> bool {
    match node.get("target").and_then(Value::as_str) {
        Some("smithy.api#Timestamp") => true,
        Some(target) => {
            shapes.get(target).and_then(|shape| shape.get("type")) == Some(&json!("timestamp"))
        }
        None => false,
    }
}

/// Every member-like node of one shape, as `(member path, node)`: a structure's
/// or union's members, a list's element, and a map's key and value — the four
/// places a model can bind a timestamp.
fn member_nodes(shape_id: &str, shape: &Value) -> Vec<(String, Value)> {
    let mut nodes = Vec::new();
    if let Some(members) = shape.get("members").and_then(Value::as_object) {
        for (name, node) in members {
            nodes.push((format!("{shape_id}${name}"), node.clone()));
        }
    }
    for single in ["member", "key", "value"] {
        if let Some(node) = shape.get(single) {
            nodes.push((format!("{shape_id}${single}"), node.clone()));
        }
    }
    nodes
}

/// RULE 9 on the model: every member of this model that targets a timestamp
/// declares `@timestampFormat("date-time")`.
///
/// This is what makes the service-wide date-time codec (`src/service.rs`'s
/// `override_protocol`) the model's own rule rather than a guess: a member that
/// declared `epoch-seconds` or `http-date` would be carried in the wrong form,
/// and would fail here instead. Read off the shipped model, so a model refresh
/// that added such a member is reported rather than silently served.
#[test]
fn every_timestamp_member_of_the_model_declares_the_date_time_format() {
    let model = shipped_model();
    let shapes = model
        .get("shapes")
        .and_then(Value::as_object)
        .expect("the model must declare shapes");

    let mut timestamp_members = 0_usize;
    let mut wrong: Vec<String> = Vec::new();
    for (shape_id, shape) in shapes {
        if !shape_id.starts_with(SERVICE_NAMESPACE) {
            continue;
        }
        for (path, node) in member_nodes(shape_id, shape) {
            if !targets_a_timestamp(shapes, &node) {
                continue;
            }
            timestamp_members += 1;
            let format = declared_format(shapes, &node);
            if format != Some("date-time") {
                wrong.push(format!("{path}: {format:?}"));
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "case \"the shipped model\": every timestamp member must declare the date-time format, \
         got {} that do not: {wrong:?}",
        wrong.len()
    );
    // A floor, so a walk that found nothing cannot pass as agreement.
    assert!(
        timestamp_members >= 10,
        "case \"the shipped model\": the walk must have reached this model's timestamp members, \
         got {timestamp_members}"
    );
}

// ---------------------------------------------------------------------------
// RULES 9 and 10, on the wire
// ---------------------------------------------------------------------------

/// One domain carrying one of every record a read of this service reports an
/// instant or an aggregate member for.
struct Records {
    domain_id: String,
    field_id: String,
    layout_id: String,
    template_id: String,
    case_id: String,
}

/// A Cases ARN of this emulator: the domain's own when `suffix` is empty, one of
/// its resources otherwise.
fn cases_arn(domain_id: &str, suffix: &str) -> String {
    format!("arn:aws:cases:{HOME_REGION}:{ACCOUNT}:domain/{domain_id}{suffix}")
}

/// This test's clock reading, in epoch seconds.
fn now_seconds() -> f64 {
    ::aws_smithy_types::DateTime::from(std::time::SystemTime::now()).as_secs_f64()
}

/// Build a domain with a field, a layout, a template, a case and one related
/// item under it.
///
/// `populated` says whether the records carry anything in their aggregate
/// members: with it set, every record is tagged, the template names a default
/// layout and propagates a tag map, and the case is created with tags. Without
/// it, nothing is tagged and the template names no optional member — the records
/// whose aggregates are empty, which is the pair of shapes rule 9 generalises
/// over.
async fn records(emulator: &Emulator, prefix: &str, populated: bool) -> Records {
    let domain_id = domain(emulator, &format!("{prefix}-domain")).await;
    let field = emulator
        .ok(
            prefix,
            "POST",
            &format!("/domains/{domain_id}/fields"),
            json!({ "name": format!("{prefix}-field"), "type": "Text" }),
        )
        .await;
    let field_id = string_at(&field, "fieldId", prefix);
    let layout = emulator
        .ok(
            prefix,
            "POST",
            &format!("/domains/{domain_id}/layouts"),
            json!({ "name": format!("{prefix}-layout"), "content": { "basic": {} } }),
        )
        .await;
    let layout_id = string_at(&layout, "layoutId", prefix);

    let mut template_body = json!({
        "name": format!("{prefix}-template"),
        "requiredFields": [{ "fieldId": field_id }],
    });
    if populated {
        template_body["layoutConfiguration"] = json!({ "defaultLayout": layout_id });
        template_body["tagPropagationConfigurations"] =
            json!([{ "resourceType": "Cases", "tagMap": { "team": "support" } }]);
    }
    let created_template = emulator
        .ok(
            prefix,
            "POST",
            &format!("/domains/{domain_id}/templates"),
            template_body,
        )
        .await;
    let template_id = string_at(&created_template, "templateId", prefix);

    let mut case_body = json!({
        "templateId": template_id,
        "fields": [entry(&field_id, &json!({ "stringValue": "a customer note" }))],
    });
    if populated {
        case_body["tags"] = json!({ "team": "support" });
    }
    let created_case = emulator
        .ok(
            prefix,
            "POST",
            &format!("/domains/{domain_id}/cases"),
            case_body,
        )
        .await;
    let case_id = string_at(&created_case, "caseId", prefix);

    // One association, so the instant a related item was linked at is reported
    // too.
    emulator
        .ok(
            prefix,
            "POST",
            &format!("/domains/{domain_id}/cases/{case_id}/related-items/"),
            json!({
                "type": "Comment",
                "content": { "comment": { "body": "called back", "contentType": "Text/Plain" } },
            }),
        )
        .await;

    if populated {
        // The records a create takes no tags for, tagged through the tagging
        // operation — the only way this service's domains, fields, layouts and
        // templates come to carry any.
        for suffix in [
            String::new(),
            format!("/field/{field_id}"),
            format!("/layout/{layout_id}"),
            format!("/template/{template_id}"),
        ] {
            let arn = cases_arn(&domain_id, &suffix);
            emulator
                .ok(
                    prefix,
                    "POST",
                    &tags_path(&arn),
                    json!({ "tags": { "team": "support" } }),
                )
                .await;
        }
    }

    Records {
        domain_id,
        field_id,
        layout_id,
        template_id,
        case_id,
    }
}

/// Every document a read of those records answers, each the object that carries
/// the members under test.
struct Answers {
    domain: Value,
    layout: Value,
    template: Value,
    /// The `BatchGetField` entry for the one field, not the whole batch answer.
    field: Value,
    case: Value,
    /// The newest `GetCaseAuditEvents` entry.
    audit_event: Value,
    /// The `SearchAllRelatedItems` entry for the one association.
    related_item: Value,
}

/// Read every record back, over the wire.
async fn answers(emulator: &Emulator, records: &Records, case: &str) -> Answers {
    let domain_id = &records.domain_id;
    let batch = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/fields-batch"),
            json!({ "fields": [{ "id": records.field_id }] }),
        )
        .await;
    let history = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/cases/{}/audit-history", records.case_id),
            json!({}),
        )
        .await;
    let related = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/related-items-search"),
            json!({}),
        )
        .await;

    /// The first entry of a list member, which every row above answers one of.
    fn first(document: &Value, member: &str, case: &str) -> Value {
        document
            .get(member)
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {member} entry in {document:?}")
            })
    }

    Answers {
        domain: emulator
            .ok(case, "POST", &format!("/domains/{domain_id}"), json!({}))
            .await,
        layout: emulator
            .ok(
                case,
                "POST",
                &format!("/domains/{domain_id}/layouts/{}", records.layout_id),
                json!({}),
            )
            .await,
        template: emulator
            .ok(
                case,
                "POST",
                &format!("/domains/{domain_id}/templates/{}", records.template_id),
                json!({}),
            )
            .await,
        field: first(&batch, "fields", case),
        case: emulator
            .ok(
                case,
                "POST",
                &format!("/domains/{domain_id}/cases/{}", records.case_id),
                json!({ "fields": [{ "id": records.field_id }] }),
            )
            .await,
        audit_event: first(&history, "auditEvents", case),
        related_item: first(&related, "relatedItems", case),
    }
}

/// RULE 9 on the wire: every instant this service reports is an RFC 3339
/// date-time STRING naming the moment the service wrote — not a number.
///
/// The rows span the class the rule generalises over: every read path of this
/// service that reports an instant, and every instant member of each.
#[tokio::test]
async fn every_instant_a_read_reports_is_a_date_time_string() {
    let emulator = Emulator::new();
    let records = records(&emulator, "instants", false).await;
    let answers = answers(&emulator, &records, "instants").await;

    let rows: [(&str, &Value, &[&str]); 6] = [
        (
            "GetDomain",
            &answers.domain,
            &["createdTime", "lastModifiedTime"],
        ),
        (
            "GetLayout",
            &answers.layout,
            &["createdTime", "lastModifiedTime"],
        ),
        (
            "GetTemplate",
            &answers.template,
            &["createdTime", "lastModifiedTime"],
        ),
        (
            "BatchGetField entry",
            &answers.field,
            &["createdTime", "lastModifiedTime"],
        ),
        (
            "GetCaseAuditEvents entry",
            &answers.audit_event,
            &["performedTime"],
        ),
        (
            "SearchAllRelatedItems entry",
            &answers.related_item,
            &["associationTime"],
        ),
    ];

    let now = now_seconds();
    for (case, document, members) in rows {
        for member in members {
            let reported = document.get(*member).unwrap_or_else(|| {
                panic!("case {case:?}: no {member} member in {document:?}")
            });
            assert!(
                reported.is_string(),
                "case {case:?}: {member} must be a date-time string, got {reported:?}"
            );
            let instant = instant_at(document, member, case);
            assert!(
                (instant - now).abs() <= INSTANT_SLACK_SECONDS,
                "case {case:?}: {member} must name the instant the service wrote (near {now}), \
                 got {instant}"
            );
        }
    }
}

/// RULE 10: a read reports every aggregate member of the record it answers,
/// carrying the empty value when the record holds nothing in it.
///
/// The rows span what the rule generalises over: every read path that reports
/// one, each of the member KINDS (a map, a list, a structure of nulls), and both
/// forms of record — one whose aggregates are empty and one whose aggregates
/// hold the values a create and a tag write put there.
#[tokio::test]
async fn a_read_reports_every_aggregate_member_whatever_the_record_holds() {
    let emulator = Emulator::new();
    let empty = records(&emulator, "empty-aggregates", false).await;
    let held = records(&emulator, "held-aggregates", true).await;
    let empty = answers(&emulator, &empty, "empty aggregates").await;
    let held_layout = held.layout_id.clone();
    let held = answers(&emulator, &held, "held aggregates").await;

    let propagation = json!([{ "resourceType": "Cases", "tagMap": { "team": "support" } }]);
    let one_tag = json!({ "team": "support" });
    let rows: Vec<(&str, &Value, &str, Value)> = vec![
        // Nothing in any aggregate: the member is the empty value, never absent.
        ("GetDomain, untagged", &empty.domain, "tags", json!({})),
        ("GetLayout, untagged", &empty.layout, "tags", json!({})),
        ("GetCase, untagged", &empty.case, "tags", json!({})),
        (
            "BatchGetField entry, untagged",
            &empty.field,
            "tags",
            json!({}),
        ),
        ("GetTemplate, untagged", &empty.template, "tags", json!({})),
        (
            "GetTemplate with no case rule",
            &empty.template,
            "rules",
            json!([]),
        ),
        (
            "GetTemplate propagating no tags",
            &empty.template,
            "tagPropagationConfigurations",
            json!([]),
        ),
        (
            "GetTemplate with no default values",
            &empty.template,
            "defaultValues",
            json!([]),
        ),
        (
            "GetTemplate naming no default layout",
            &empty.template,
            "layoutConfiguration",
            json!({ "defaultLayout": null, "defaultLayoutName": null }),
        ),
        // The same members on records that DO hold something: the one value the
        // record carries, reported under the same key.
        ("GetDomain, tagged", &held.domain, "tags", one_tag.clone()),
        ("GetLayout, tagged", &held.layout, "tags", one_tag.clone()),
        ("GetCase, tagged", &held.case, "tags", one_tag.clone()),
        (
            "BatchGetField entry, tagged",
            &held.field,
            "tags",
            one_tag.clone(),
        ),
        ("GetTemplate, tagged", &held.template, "tags", one_tag),
        (
            "GetTemplate propagating one tag map",
            &held.template,
            "tagPropagationConfigurations",
            propagation,
        ),
        (
            "GetTemplate naming a default layout",
            &held.template,
            "layoutConfiguration",
            json!({ "defaultLayout": held_layout, "defaultLayoutName": null }),
        ),
        (
            "GetTemplate with no case rule, tagged",
            &held.template,
            "rules",
            json!([]),
        ),
        (
            "GetTemplate with no default values, tagged",
            &held.template,
            "defaultValues",
            json!([]),
        ),
    ];

    for (case, document, member, expected) in rows {
        let reported = document.get(member);
        assert_eq!(
            reported,
            Some(&expected),
            "case {case:?}: expected {member} to be {expected:?}, got {reported:?} in \
             {document:?}"
        );
    }
}

/// RULE 9 on the domain's own instants: a domain reports when it was created and
/// when it was last changed, and the second moves only when the domain changes.
#[tokio::test]
async fn a_domain_reports_the_instant_it_was_last_changed() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "domain-last-modified").await;
    let path = format!("/domains/{domain_id}");

    let case = "a domain nothing has changed";
    let before = emulator.ok(case, "POST", &path, json!({})).await;
    assert_eq!(
        before.get("lastModifiedTime"),
        before.get("createdTime"),
        "case {case:?}: a domain nothing has changed reports its creation instant as its \
         last-modified instant, got {before:?}"
    );

    // The one operation that edits a domain's own configuration.
    let case = "a domain whose case-event configuration was written";
    emulator
        .ok(
            case,
            "PUT",
            &format!("/domains/{domain_id}/case-event-configuration"),
            json!({ "eventBridge": { "enabled": true } }),
        )
        .await;
    let after = emulator.ok(case, "POST", &path, json!({})).await;
    assert_eq!(
        after.get("createdTime"),
        before.get("createdTime"),
        "case {case:?}: a write never moves createdTime, got {after:?}"
    );
    assert!(
        instant_at(&after, "lastModifiedTime", case)
            > instant_at(&before, "lastModifiedTime", case),
        "case {case:?}: a write of the domain's configuration must move lastModifiedTime past \
         {before:?}, got {after:?}"
    );
}
