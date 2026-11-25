// SPDX-License-Identifier: CC0-1.0

//! What this service answers a request whose JSON BODY breaks one of its modelled input
//! constraints.
//!
//! The rule: for the operations whose request body is validated by the layer AHEAD of the
//! handlers, a modelled constraint that body breaks is refused there — HTTP 400 under the
//! error code that layer owns (a shape this service's model does not declare) and ONE
//! fixed sentence for every member and every constraint class: it names neither the
//! member, nor the value, nor the clause it broke, nor how many clauses failed. The code
//! and the sentence are the shared ones ([`::service_common::front_door`]), so this suite
//! pins the rule rather than wording of its own.
//!
//! The rows below span the constraint CLASSES the model declares across this service's
//! operations, so the rule is asserted over the class rather than over one recorded
//! member: a string under its `@length(min:)`, a string over its `@length(max:)`, a string
//! outside its `@pattern`, an absent `@required` member, an `enum` value the model does
//! not declare, a list under its `@length(min:)` and a list over its `@length(max:)`.
//!
//! Three refusals are NOT that answer, and each is asserted here so the fixed sentence
//! cannot spread over them: a value the request carries OUTSIDE its body (an `@httpLabel`
//! path segment), the body of `CreateCase` — the one operation that layer does not front,
//! whose body is refused in the modelled per-member wording — and a rule the model states
//! only in prose, which the operation that owns it refuses naming the fields it is about.

use serde_json::{json, Value};

use crate::harness::{
    domain, mandatory_fields_refusal, template, title_value, Emulator, ERROR_TYPE_HEADER,
    INVALID_REQUEST_BODY,
};

/// The error code the body-validating layer refuses under, as the shared front door names
/// it: a shape this service's model does not declare, because the rejection happens ahead
/// of the code that owns the modelled errors.
const BODY_REJECTION_CODE: &str = ::service_common::front_door::BAD_REQUEST_EXCEPTION;

/// The modelled client-input error, `@httpError(400)`, which the layers BEHIND the door
/// state their refusals under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// A string one byte past the longest `@length(max:)` this service puts on a name:
/// `DomainName`, `FieldName` and `TemplateName` are all `@length(1..100)`
/// (`smithy_query(query="operation", operation="CreateDomain,CreateField,CreateTemplate")`).
fn over_long_name() -> String {
    "n".repeat(101)
}

#[tokio::test]
async fn every_constraint_class_a_fronted_request_body_breaks_is_one_fixed_sentence() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "body-rejection-domain").await;

    // Each row is one constraint CLASS, on a member of a different operation where the
    // model declares that class: the answer must not differ by member, by operation, by
    // class or by how many clauses the body broke.
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "CreateDomain name under its @length(min: 1) and outside its @pattern",
            "POST",
            "/domains".to_string(),
            json!({ "name": "" }),
        ),
        (
            "CreateDomain name over its @length(max: 100)",
            "POST",
            "/domains".to_string(),
            json!({ "name": over_long_name() }),
        ),
        (
            "CreateDomain name outside its @pattern (trailing whitespace)",
            "POST",
            "/domains".to_string(),
            json!({ "name": "a name " }),
        ),
        (
            "CreateDomain with no name at all",
            "POST",
            "/domains".to_string(),
            json!({}),
        ),
        (
            "CreateField with no name at all",
            "POST",
            format!("/domains/{domain_id}/fields"),
            json!({ "type": "Text" }),
        ),
        (
            "CreateField with no type at all",
            "POST",
            format!("/domains/{domain_id}/fields"),
            json!({ "name": "a-field" }),
        ),
        (
            "CreateField type outside the FieldType enum",
            "POST",
            format!("/domains/{domain_id}/fields"),
            json!({ "name": "a-field", "type": "NotAFieldType" }),
        ),
        (
            "CreateField description over its @length(max: 255)",
            "POST",
            format!("/domains/{domain_id}/fields"),
            json!({ "name": "a-field", "type": "Text", "description": "d".repeat(256) }),
        ),
        (
            "BatchGetField fields under its @length(min: 1)",
            "POST",
            format!("/domains/{domain_id}/fields-batch"),
            json!({ "fields": [] }),
        ),
        (
            "BatchGetField fields over its @length(max: 50)",
            "POST",
            format!("/domains/{domain_id}/fields-batch"),
            json!({
                "fields": (0..51)
                    .map(|index| json!({ "id": format!("id-{index}") }))
                    .collect::<Vec<_>>(),
            }),
        ),
        (
            "BatchGetField with no fields at all",
            "POST",
            format!("/domains/{domain_id}/fields-batch"),
            json!({}),
        ),
        (
            "CreateTemplate with no name at all",
            "POST",
            format!("/domains/{domain_id}/templates"),
            json!({ "description": "a template with no name" }),
        ),
        (
            "CreateTemplate name under its @length(min: 1)",
            "POST",
            format!("/domains/{domain_id}/templates"),
            json!({ "name": "" }),
        ),
    ];

    for (case, method, path, body) in rows {
        let (status, error_type, message) = emulator
            .failure_error_type(case, method, &path, Some(body))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {error_type:?}: {message:?}"
        );
        assert_eq!(
            message, INVALID_REQUEST_BODY,
            "case {case:?}: expected the fixed body-rejection sentence, got {message:?}"
        );
        assert_eq!(
            error_type, BODY_REJECTION_CODE,
            "case {case:?}: expected the body-rejection code, got {error_type:?}"
        );
    }
}

#[tokio::test]
async fn the_body_rejection_names_one_shape_in_its_header_and_in_its_envelope() {
    // A client that reads the `x-amzn-errortype` header and one that reads the envelope
    // cannot be told different things about what was refused.
    let emulator = Emulator::new();
    let (status, json) = emulator
        .send("POST", "/domains", Some(json!({ "name": "" })))
        .await;
    assert_eq!(
        (status, json.get("__type").and_then(Value::as_str)),
        (400, Some(BODY_REJECTION_CODE)),
        "expected 400 {BODY_REJECTION_CODE}, got {status} {json:?}"
    );
    assert_eq!(
        json.get("message").and_then(Value::as_str),
        Some(INVALID_REQUEST_BODY),
        "expected the fixed body-rejection sentence, got {json:?}"
    );
    let (_, error_type, _) = emulator
        .failure_error_type("CreateDomain with an empty name", "POST", "/domains", Some(json!({ "name": "" })))
        .await;
    assert_eq!(
        error_type, BODY_REJECTION_CODE,
        "the {ERROR_TYPE_HEADER} header must name the same shape as the envelope, \
         got {error_type:?}"
    );
}

#[tokio::test]
async fn a_value_the_request_carries_outside_its_body_is_refused_naming_the_member() {
    // A constraint broken by an `@httpLabel` is not a body rejection: the layers behind the
    // door refuse it, naming the member that failed, under the modelled error.
    let emulator = Emulator::new();
    let over_long_label = "d".repeat(501);
    let (status, error_type, message) = emulator
        .failure_error_type(
            "BatchGetField under a domainId over its @length(max: 500)",
            "POST",
            &format!("/domains/{over_long_label}/fields-batch"),
            Some(json!({ "fields": [{ "id": "an-id" }] })),
        )
        .await;
    assert_eq!(
        (status, error_type.as_str()),
        (400, VALIDATION_EXCEPTION),
        "expected 400 {VALIDATION_EXCEPTION} for a path label outside its bound, \
         got {status} {error_type}: {message:?}"
    );
    assert!(
        message.contains("domainId") && message != INVALID_REQUEST_BODY,
        "a URI-bound refusal names the member that failed, got {message:?}"
    );
}

#[tokio::test]
async fn a_create_case_body_is_refused_naming_the_member_that_failed() {
    // `CreateCase` is the one operation the body-validating layer does not front: a body
    // that omits a required member is refused behind the door, under the modelled error and
    // in the per-member wording that names the member and the constraint it broke.
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "create-case-body-domain").await;
    let (status, error_type, message) = emulator
        .failure_error_type(
            "CreateCase with no templateId at all",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            Some(json!({ "fields": [title_value("a title")] })),
        )
        .await;
    assert_eq!(
        (status, error_type.as_str()),
        (400, VALIDATION_EXCEPTION),
        "expected 400 {VALIDATION_EXCEPTION}, got {status} {error_type}: {message:?}"
    );
    assert!(
        message.contains("templateId") && message != INVALID_REQUEST_BODY,
        "CreateCase's refusal names the member that failed, got {message:?}"
    );
}

#[tokio::test]
async fn a_rule_stated_only_in_prose_is_still_refused_by_the_operation_that_owns_it() {
    // The fields a case must carry a value for are state, not shape: a create whose body
    // the model admits is refused by the handler, which names those fields.
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "prose-rule-domain").await;
    let template_id = template(&emulator, &domain_id, "prose-rule-template", &[]).await;
    let (status, code, message) = emulator
        .failure_detail(
            "CreateCase supplying no field values",
            "POST",
            &format!("/domains/{domain_id}/cases"),
            Some(json!({ "templateId": template_id })),
        )
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, VALIDATION_EXCEPTION),
        "expected 400 {VALIDATION_EXCEPTION}, got {status} {code}: {message:?}"
    );
    assert_eq!(
        message,
        mandatory_fields_refusal(&["title".to_string()]),
        "the handler's own sentence names the fields that must be provided, got {message:?}"
    );
}
