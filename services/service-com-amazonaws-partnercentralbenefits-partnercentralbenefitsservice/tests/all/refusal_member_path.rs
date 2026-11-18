// SPDX-License-Identifier: CC0-1.0

//! The member path a refusal of this service names, end to end over the wire.
//!
//! Every assertion goes through the real request path — the shared
//! [`crate::benefit_application_lifecycle::Harness`], i.e. `MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies, `POST /` and `X-Amz-Target:
//! PartnerCentralBenefitsService.<Operation>`.
//!
//! The rule under test, stated over the class of refusals it covers:
//!
//! **A refusal of an input member names that member by the path the centralized constraint
//! walk reports for it — the member in lowerCamel, followed by the steps a handler
//! addressed below it — in the clause of the AWS validation envelope and in
//! `ValidationExceptionField$Name` alike.** So one input member has ONE path whichever
//! bound it broke: a constraint the model declares, which the centralized walk enforces, or
//! a rule a handler enforces itself.
//!
//! Two tables span what the rule generalises over:
//!
//! 1. One member, two bounds: the same member refused once by the walk and once by a
//!    handler must be named the same way. Rows: the cursor member of all three paginated
//!    listings (its modelled `@length` on both sides of the bound, against the cursor rule
//!    the handler enforces) and the benefit member of `CreateBenefitApplication` (its
//!    modelled `@pattern`, against the status rule the handler enforces).
//! 2. Every refusal a HANDLER of this service decides about a member: the envelope over the
//!    clause for that path, and the one-member `FieldList` naming it. The rows cross the
//!    case the WIRE spells the member in (`NextToken`, `Tags` and
//!    `BenefitApplicationDetails` in PascalCase; `tags` and `tagKeys` already in
//!    lowerCamel), the operation deciding the refusal, the kind of rule it enforces (a
//!    cursor this service never issued, the reserved tag namespace, a document guard, a
//!    benefit the catalog no longer offers) and the shape of the path (the member alone, and
//!    a member reached below it inside a collection element).
//!
//! The rule has a corollary, which the last two tables pin: **the member a refusal names is
//! a member of the request as it was SENT** — an operation can only name an input member it
//! binds, so where a rule is about state the request reaches INDIRECTLY, the refusal names
//! the member that state arrived on. `AmendBenefitApplication` is where the two differ:
//! it binds no `BenefitApplicationDetails`
//! (`smithy_query(query="operation",operation="AmendBenefitApplication")` binds `Catalog`,
//! `ClientToken`, `Revision`, `Identifier`, `AmendmentReason` and `Amendments`, and nothing
//! else), and the document is what its amendments PRODUCE, so both rules stated about that
//! document name the amendment element that produced it. Table 3 crosses which rule refuses
//! (each document guard, the benefit's request schema), where in the document the rule is
//! stated (a declared field, a field above the written one, the document itself), how many
//! amendments the request carries and which of them wrote the refused position, and the two
//! `FieldPath` spellings. Table 4 states the corollary over every refusal class of that
//! operation.


use serde_json::{json, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    encode_page_token, ALLOCATION_ID_PREFIX, APPLICATION_ID_PREFIX, BENEFIT_ID_PREFIX, ID_BODY_LEN,
    MAX_AMENDMENTS_PER_REQUEST, MAX_DETAILS_DEPTH, MAX_DETAILS_MEMBERS, MAX_DETAILS_VALUE_LEN,
    MAX_FIELD_PATH_LEN, RESERVED_TAG_KEY_PREFIX, VALIDATION_CODE_INVALID_VALUE,
    VALIDATION_REASON_FIELD,
};

use crate::amend_benefit_application::{amend_body, amendment};
use crate::benefit_application_lifecycle::{error_code, text, Harness, CATALOG};
use crate::benefit_catalog_record::{catalog_benefits, declared_fields, UNDECLARED_FIELD};
use crate::details_document_guard::{
    byte_requirement, flat_object, member_requirement, members_past_the_byte_bound,
    text_spread_over_members, MEMBER_PATH as DETAILS_PATH,
};
use crate::member_refusal_one_construction::withdrawn_benefit;
use crate::reserved_tag_keys::{element_requirement, tag_key_path};
use crate::resource_tagging::tag_payload;
use crate::service_error_rules::{assert_validation_invariants, clause, message_of, one};

/// The modelled maximum of the cursor member (`ListBenefitsInput$NextToken`
/// `@length(min: 1, max: 2048)`, `smithy_query(query="shape",shape="ListBenefitsInput")`),
/// so a row can break the bound from above as well as from below.
const NEXT_TOKEN_MAX: usize = 2048;

/// A tag key in the AWS-reserved namespace, which no caller may supply.
const RESERVED_KEY: &str = "aws:createdBy";

/// `wire_member` as `smithy_schema_server::constraints::path` spells an input member in a
/// validation path: the member with its first character lowercased.
///
/// Written out here rather than read from the service, so a table row cannot adopt a second
/// spelling of a member and still pass.
fn walk_spelling(wire_member: &str) -> String {
    let mut characters = wire_member.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The member path the one clause of `message` names: what it states between `Value at '`
/// and the closing quote.
fn stated_path(case: &str, message: &str) -> String {
    message
        .split_once("Value at '")
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(path, _)| path.to_string())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the refusal states no member clause: {message:?}")
        })
}

/// Call `operation` with `body`, require the modelled `ValidationException` (400) and
/// return the message it carried.
async fn refusal_message(harness: &Harness, case: &str, operation: &str, body: &Value) -> String {
    let (status, out) = harness.call(operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to refuse the request with 400, got {status} {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {case:?}: expected ValidationException from {operation}, got {out:?}"
    );
    let message = message_of(&out).unwrap_or_else(|| {
        panic!("case {case:?}: the refusal carries no modelled Message member: {out:?}")
    });
    assert_validation_invariants(case, &message);
    message
}

/// A cursor well formed under this service's own encoding and naming a record no catalog
/// holds, so it is a token this service never issued.
fn unissued_cursor(catalog: &str, prefix: &str) -> String {
    encode_page_token(catalog, &format!("{prefix}{}", "0".repeat(ID_BODY_LEN)))
}

// ---------------------------------------------------------------------------
// Table 1: one member, two bounds, one path
// ---------------------------------------------------------------------------

/// One member a request can break two ways: the call the centralized walk refuses, and the
/// call a handler refuses, both of which must name `path`.
struct Paired {
    case: String,
    operation: &'static str,
    /// The member as the WIRE spells it.
    wire_member: &'static str,
    /// The path both refusals must state.
    path: &'static str,
    /// A request refused by a constraint the MODEL declares on that member.
    by_the_walk: Value,
    /// A request refused by a rule the HANDLER enforces on the same member.
    by_the_handler: Value,
}

/// The same input member refused by the centralized walk and by a handler is named by the
/// same path in both answers.
#[tokio::test]
async fn one_member_is_named_one_path_whichever_bound_it_broke() {
    let harness = Harness::new();
    let (withdrawn_catalog, withdrawn_id, _) = withdrawn_benefit(&harness).await;
    let active_benefit = text("subject", &harness.active_benefit().await, "Id");

    let mut rows: Vec<Paired> = Vec::new();
    // The cursor member of every paginated listing, on both sides of its modelled length:
    // too short (an empty token) and too long (one character past the maximum).
    for (operation, prefix) in [
        ("ListBenefits", BENEFIT_ID_PREFIX),
        ("ListBenefitApplications", APPLICATION_ID_PREFIX),
        ("ListBenefitAllocations", ALLOCATION_ID_PREFIX),
    ] {
        for (bound, token) in [
            ("under the modelled minimum", String::new()),
            ("past the modelled maximum", "t".repeat(NEXT_TOKEN_MAX + 1)),
        ] {
            rows.push(Paired {
                case: format!("{operation}: a cursor {bound}, and one never issued"),
                operation,
                wire_member: "NextToken",
                path: "nextToken",
                by_the_walk: json!({ "Catalog": CATALOG, "NextToken": token }),
                by_the_handler: json!({
                    "Catalog": CATALOG,
                    "NextToken": unissued_cursor(CATALOG, prefix),
                }),
            });
        }
    }
    // The benefit member of a create: a value outside its modelled pattern, and a value
    // inside it naming a benefit the catalog no longer offers.
    rows.push(Paired {
        case: "CreateBenefitApplication: a malformed benefit, and a withdrawn one".to_string(),
        operation: "CreateBenefitApplication",
        wire_member: "BenefitIdentifier",
        path: "benefitIdentifier",
        by_the_walk: json!({
            "Catalog": CATALOG,
            "ClientToken": "tok-path-walk-benefit",
            "BenefitIdentifier": "not-a-benefit-identifier",
            "Name": "member-path",
        }),
        by_the_handler: json!({
            "Catalog": withdrawn_catalog,
            "ClientToken": "tok-path-handler-benefit",
            "BenefitIdentifier": withdrawn_id,
            "Name": "member-path",
        }),
    });

    for row in &rows {
        let case = &row.case;
        assert_eq!(
            row.path,
            walk_spelling(row.wire_member),
            "case {case:?}: the row's own path must be the wire member in lowerCamel, got {:?}",
            row.path
        );
        for (decided_by, body) in [
            ("the centralized walk", &row.by_the_walk),
            ("the handler", &row.by_the_handler),
        ] {
            let case = format!("{case} ({decided_by})");
            let message = refusal_message(&harness, &case, row.operation, body).await;
            assert_eq!(
                stated_path(&case, &message),
                row.path,
                "case {case:?}: expected the clause to name {:?}, got {message:?}",
                row.path
            );
        }
    }

    // The benefit the other rows apply for is still served, so the refusals above are
    // refusals of the member each row names and not of every request.
    harness
        .create(
            "a create the rules admit",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-path-admitted",
                "BenefitIdentifier": active_benefit,
                "Name": "member-path",
            }),
        )
        .await;
}

// ---------------------------------------------------------------------------
// Table 2: every refusal a handler decides
// ---------------------------------------------------------------------------

/// One refusal a handler of this service decides about an input member.
struct Row {
    case: String,
    operation: &'static str,
    /// The member as the WIRE spells it.
    wire_member: &'static str,
    /// The path the refusal must state, in the clause and in the `FieldList`: the member in
    /// lowerCamel, plus any steps the handler addressed below it.
    path: String,
    /// The requirement that member failed, as the refusal must word it.
    requirement: String,
    body: Value,
}

/// A details document nesting one level past the depth this service stores, which the
/// handler's document guard refuses.
fn too_deep_details() -> Value {
    (0..=MAX_DETAILS_DEPTH).fold(json!("leaf"), |inner, _| json!({ "n": inner }))
}

/// Every refusal a handler decides about a member states the AWS validation envelope over
/// the clause for the path the walk reports for that member, and names the same path in its
/// one-member `FieldList`.
#[tokio::test]
async fn an_in_handler_refusal_names_the_walks_path_under_the_envelope() {
    let harness = Harness::new();
    let (withdrawn_catalog, withdrawn_id, withdrawn_status) = withdrawn_benefit(&harness).await;
    let active_benefit = text("subject", &harness.active_benefit().await, "Id");
    let subject = harness
        .create(
            "subject",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-member-path-subject",
                "BenefitIdentifier": active_benefit,
                "Name": "member-path",
            }),
        )
        .await;
    let subject_arn = text("subject", &subject, "Arn");
    let reserved_requirement =
        format!("Member must not use the reserved tag key prefix: {RESERVED_TAG_KEY_PREFIX}");

    let mut rows: Vec<Row> = Vec::new();
    // A cursor this service never issued, on every paginated listing: the listing that
    // issues the cursors is the one the requirement names.
    for (operation, prefix) in [
        ("ListBenefits", BENEFIT_ID_PREFIX),
        ("ListBenefitApplications", APPLICATION_ID_PREFIX),
        ("ListBenefitAllocations", ALLOCATION_ID_PREFIX),
    ] {
        rows.push(Row {
            case: format!("{operation}: a cursor it never issued"),
            operation,
            wire_member: "NextToken",
            path: "nextToken".to_string(),
            requirement: format!("Member must be a pagination token issued by {operation}"),
            body: json!({
                "Catalog": CATALOG,
                "NextToken": unissued_cursor(CATALOG, prefix),
            }),
        });
    }
    rows.push(Row {
        case: "CreateBenefitApplication: a reserved tag key".to_string(),
        operation: "CreateBenefitApplication",
        wire_member: "Tags",
        // The key sits in the `Key` member of one element of the list, which keeps its own
        // full path below the member.
        path: tag_key_path("tags", 1),
        requirement: reserved_requirement.clone(),
        body: json!({
            "Catalog": CATALOG,
            "ClientToken": "tok-member-path-create-tags",
            "BenefitIdentifier": active_benefit,
            "Name": "member-path",
            "Tags": tag_payload(&[(RESERVED_KEY, "mine")]),
        }),
    });
    rows.push(Row {
        case: "TagResource: a reserved tag key".to_string(),
        operation: "TagResource",
        wire_member: "tags",
        path: tag_key_path("tags", 1),
        requirement: reserved_requirement.clone(),
        body: json!({
            "resourceArn": subject_arn,
            "tags": tag_payload(&[(RESERVED_KEY, "mine")]),
        }),
    });
    rows.push(Row {
        case: "UntagResource: a reserved tag key".to_string(),
        operation: "UntagResource",
        wire_member: "tagKeys",
        // Here the refused key IS an element of the member, so the path collapses onto the
        // member and the requirement is the element's requirement set.
        path: "tagKeys".to_string(),
        requirement: element_requirement(&reserved_requirement),
        body: json!({ "resourceArn": subject_arn, "tagKeys": [RESERVED_KEY] }),
    });
    rows.push(Row {
        case: "CreateBenefitApplication: a document past the depth bound".to_string(),
        operation: "CreateBenefitApplication",
        wire_member: "BenefitApplicationDetails",
        path: "benefitApplicationDetails".to_string(),
        requirement: format!("Member must nest no deeper than {MAX_DETAILS_DEPTH} levels"),
        body: json!({
            "Catalog": CATALOG,
            "ClientToken": "tok-member-path-create-details",
            "BenefitIdentifier": active_benefit,
            "Name": "member-path",
            "BenefitApplicationDetails": too_deep_details(),
        }),
    });
    rows.push(Row {
        case: format!("CreateBenefitApplication: a {withdrawn_status} benefit"),
        operation: "CreateBenefitApplication",
        wire_member: "BenefitIdentifier",
        path: "benefitIdentifier".to_string(),
        requirement: format!(
            "Member must name a benefit the catalog offers, which a {withdrawn_status} benefit \
             is not"
        ),
        body: json!({
            "Catalog": withdrawn_catalog,
            "ClientToken": "tok-member-path-withdrawn",
            "BenefitIdentifier": withdrawn_id,
            "Name": "member-path",
        }),
    });

    // The table covers a member the wire spells in PascalCase and one it already spells in
    // lowerCamel, so it cannot shrink to the casing of one of them.
    for spelled_as_the_path in [false, true] {
        assert!(
            rows.iter()
                .any(|row| (row.wire_member == row.path) == spelled_as_the_path),
            "case \"table\": expected a row whose wire member is spelled as its path: \
             {spelled_as_the_path}, got {:?}",
            rows.iter().map(|row| row.wire_member).collect::<Vec<_>>()
        );
    }
    // And a path that carries steps below the input member, so it cannot shrink to the
    // top-level members either.
    assert!(
        rows.iter().any(|row| row.path.contains('.')),
        "case \"table\": expected a row whose path carries steps below the member, got {:?}",
        rows.iter().map(|row| row.path.clone()).collect::<Vec<_>>()
    );

    for row in &rows {
        let case = &row.case;
        let named_member = row.path.split('.').next().unwrap_or_default();
        assert_eq!(
            named_member,
            walk_spelling(row.wire_member),
            "case {case:?}: the row's own path must open with the wire member in lowerCamel, \
             got {:?}",
            row.path
        );
        let (status, out) = harness.call(row.operation, &row.body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {} to refuse the request with 400, got {status} {out:?}",
            row.operation
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );
        let message = message_of(&out).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal carries no modelled Message member: {out:?}")
        });
        assert_eq!(
            message,
            one(&clause(&row.path, &row.requirement)),
            "case {case:?}: expected the envelope over the clause for {:?}, got {message:?}",
            row.path
        );
        assert_validation_invariants(case, &message);
        let field = out["FieldList"]
            .as_array()
            .and_then(|fields| fields.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            field["Name"],
            json!(row.path),
            "case {case:?}: the refusal's FieldList must name {:?}, got {out:?}",
            row.path
        );
        assert_eq!(
            field["Message"],
            json!(row.requirement),
            "case {case:?}: the FieldList must state the requirement, got {out:?}"
        );
        assert_eq!(
            field["Code"],
            json!(VALIDATION_CODE_INVALID_VALUE),
            "case {case:?}: the FieldList must carry the invalid-value code, got {out:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tables 3 and 4: the member a refusal names is one the REQUEST carries
// ---------------------------------------------------------------------------

/// The member of one amendment that carries the value to write, as a refusal names it.
pub(crate) const AMENDMENT_NEW_VALUE: &str = "newValue";

/// The path a refusal names `member` of the `Amendments` element at the 0-based `position`
/// by: the list member in the walk's spelling, the element's 1-based position and the
/// collection step, then that member.
///
/// Written out here rather than read from the service's own helper, so the spelling itself
/// is under test. Shared with `tests/all/details_document_guard.rs` and
/// `tests/all/benefit_catalog_record.rs`, which assert the same paths on the document rules.
pub(crate) fn amendment_member_path(position: usize, member: &str) -> String {
    format!("amendments.{}.member.{member}", position + 1)
}

/// The input members `AmendBenefitApplication` binds, in the lowerCamel spelling the walk
/// reports for an input member
/// (`smithy_query(query="operation",operation="AmendBenefitApplication")`: `Catalog`,
/// `ClientToken`, `Revision`, `Identifier`, `AmendmentReason` and `Amendments` — it binds no
/// document member, so no refusal of it may name one).
const AMEND_INPUT_PATHS: &[&str] = &[
    "catalog",
    "clientToken",
    "revision",
    "identifier",
    "amendmentReason",
    "amendments",
];

/// The longest value `Name` accepts, from the modelled `BenefitApplicationName`
/// `@length(min: 1, max: 255)` (`smithy_query(query="shape",shape="BenefitApplicationName")`).
const NAME_MAX_LEN: usize = 255;

/// The input member the create carries a details document on, as the wire spells it: the
/// member `AmendBenefitApplication` does NOT bind.
const DETAILS_MEMBER: &str = "BenefitApplicationDetails";

/// A benefit whose request schema an amendment can contradict: the benefit to apply for,
/// and one field the schema declares with a JSON type an amendment's text `NewValue` cannot
/// satisfy.
struct SchemaSubject {
    benefit: String,
    field: String,
    json_type: String,
}

/// The first `ACTIVE` benefit of [`CATALOG`] whose stored `BenefitRequestSchema` declares a
/// field of a JSON type other than `string`, read through the catalog's own records — so a
/// change to the seeded catalog changes the subject rather than breaking the tables. The
/// schema's own root type is `object`, which [`declared_fields`] asserts.
async fn schema_subject(harness: &Harness) -> SchemaSubject {
    for benefit in catalog_benefits(harness, CATALOG).await {
        if benefit.status != "ACTIVE" {
            continue;
        }
        let Some(schema) = benefit.schema.as_ref() else {
            continue;
        };
        if let Some((field, json_type)) = declared_fields(&benefit.id, schema)
            .into_iter()
            .find(|(_, json_type)| json_type != "string")
        {
            return SchemaSubject {
                benefit: benefit.id,
                field,
                json_type,
            };
        }
    }
    panic!("case \"subject\": the seeded catalog must hold an ACTIVE benefit whose schema declares a non-string field")
}

/// Create one application of [`CATALOG`] for `benefit` carrying `details`, and return its
/// id.
async fn seeded_application(
    harness: &Harness,
    case: &str,
    token: &str,
    benefit: &str,
    details: &Value,
) -> String {
    let created = harness
        .create(
            case,
            &json!({
                "Catalog": CATALOG,
                "ClientToken": token,
                "BenefitIdentifier": benefit,
                "Name": "member-path",
                "BenefitApplicationDetails": details,
            }),
        )
        .await;
    text(case, &created, "Id")
}

/// The `Revision` one stored application is at, read back through `GetBenefitApplication`.
async fn current_revision(harness: &Harness, case: &str, identifier: &str) -> String {
    let stored = harness.get(case, identifier).await;
    text(case, &stored, "Revision")
}

/// One refused amendment of the details document: the document the application is seeded
/// with, the amendments the request carries, and the path and the requirement the refusal
/// must state.
struct AmendRow {
    case: String,
    seeded: Value,
    amendments: Value,
    path: String,
    requirement: String,
}

/// A rule stated about the details document the amendments PRODUCE is refused against the
/// amendment that produced it — in the clause and in the `FieldList` alike — with the same
/// requirement the create and the update state for that document, and nothing is persisted.
#[tokio::test]
async fn an_amend_document_refusal_names_the_amendment_that_wrote_the_refused_position() {
    let harness = Harness::new();
    let subject = schema_subject(&harness).await;
    let field_path = format!("{DETAILS_MEMBER}.{}", subject.field);
    // An `Amendment$NewValue` is a String, so text is all an amendment can write — which is
    // what a field declared with any other JSON type cannot be satisfied by.
    let wrong_type = "not of the declared type";
    let type_requirement = format!("Member must be of type {}", subject.json_type);
    let long_value = "v".repeat(MAX_DETAILS_VALUE_LEN);

    let rows: Vec<AmendRow> = vec![
        AmendRow {
            case: format!(
                "one amendment writing the declared {} field as text",
                subject.json_type
            ),
            seeded: json!({}),
            amendments: json!([amendment(&field_path, wrong_type)]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "the second of two amendments writing it as text".to_string(),
            seeded: json!({}),
            amendments: json!([
                amendment("Name", "amended"),
                amendment(&field_path, wrong_type),
            ]),
            path: amendment_member_path(1, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "a later amendment writing another field of the same document".to_string(),
            seeded: json!({}),
            amendments: json!([
                amendment(&field_path, wrong_type),
                amendment(
                    &format!("{DETAILS_MEMBER}.{UNDECLARED_FIELD}"),
                    "the partner's own note",
                ),
            ]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "two amendments writing the same field".to_string(),
            seeded: json!({}),
            amendments: json!([
                amendment(&field_path, wrong_type),
                amendment(&field_path, "250"),
            ]),
            path: amendment_member_path(1, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "an amendment writing a member below the declared field".to_string(),
            seeded: json!({}),
            amendments: json!([amendment(&format!("{field_path}.Nested"), wrong_type)]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "the JSON-path spelling of the same write".to_string(),
            seeded: json!({}),
            amendments: json!([amendment(&format!("$.{field_path}"), wrong_type)]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            requirement: type_requirement.clone(),
        },
        AmendRow {
            case: "an amendment replacing the whole document with text".to_string(),
            seeded: json!({}),
            amendments: json!([amendment(DETAILS_MEMBER, wrong_type)]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            // The schema's own root type, which every seeded schema declares `object`.
            requirement: "Member must be of type object".to_string(),
        },
        AmendRow {
            case: "an amendment adding one member too many".to_string(),
            seeded: flat_object(MAX_DETAILS_MEMBERS),
            amendments: json!([amendment(&format!("{DETAILS_MEMBER}.added"), "x")]),
            path: amendment_member_path(0, AMENDMENT_NEW_VALUE),
            requirement: member_requirement(),
        },
        AmendRow {
            case: "the second amendment adding text past the byte bound".to_string(),
            seeded: text_spread_over_members(members_past_the_byte_bound() - 1),
            amendments: json!([
                amendment("Name", "amended"),
                amendment(&format!("{DETAILS_MEMBER}.added"), &long_value),
            ]),
            path: amendment_member_path(1, AMENDMENT_NEW_VALUE),
            requirement: byte_requirement(),
        },
    ];

    // The table covers both rules stated about the document — the emulator's own resource
    // guard and the benefit's request schema — and more than one position in the amendment
    // list, so it cannot shrink to one rule or to the first element.
    for requirement in [member_requirement(), byte_requirement()] {
        assert!(
            rows.iter().any(|row| row.requirement == requirement),
            "case \"table\": expected a row the document guard refuses with {requirement:?}, \
             got {:?}",
            rows.iter().map(|row| row.requirement.clone()).collect::<Vec<_>>()
        );
    }
    assert!(
        rows.iter()
            .any(|row| row.requirement.starts_with("Member must be of type")),
        "case \"table\": expected a row the benefit's request schema refuses, got {:?}",
        rows.iter().map(|row| row.requirement.clone()).collect::<Vec<_>>()
    );
    for position in [0usize, 1] {
        assert!(
            rows.iter()
                .any(|row| row.path == amendment_member_path(position, AMENDMENT_NEW_VALUE)),
            "case \"table\": expected a row naming the amendment at position {position}, got {:?}",
            rows.iter().map(|row| row.path.clone()).collect::<Vec<_>>()
        );
    }

    for (index, row) in rows.iter().enumerate() {
        let case = &row.case;
        let id = seeded_application(
            &harness,
            case,
            &format!("tok-amend-named-seed-{index}"),
            &subject.benefit,
            &row.seeded,
        )
        .await;
        let before = harness.get(case, &id).await;
        let body = amend_body(
            &id,
            &text(case, &before, "Revision"),
            &format!("tok-amend-named-{index}"),
            row.amendments.clone(),
        );

        let (status, out) = harness.call("AmendBenefitApplication", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled ValidationException's 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );
        let message = message_of(&out).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal carries no modelled Message member: {out:?}")
        });
        assert_eq!(
            message,
            one(&clause(&row.path, &row.requirement)),
            "case {case:?}: expected the envelope over the clause for {:?}, got {message:?}",
            row.path
        );
        assert_validation_invariants(case, &message);
        assert!(
            !message.contains(DETAILS_PATH),
            "case {case:?}: the refusal names a member this operation does not bind, \
             got {message:?}"
        );
        assert_eq!(
            out["Reason"],
            json!(VALIDATION_REASON_FIELD),
            "case {case:?}: expected the field-validation reason, got {out:?}"
        );
        let field = out["FieldList"]
            .as_array()
            .and_then(|fields| fields.first())
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            field["Name"],
            json!(row.path),
            "case {case:?}: the refusal's FieldList must name {:?}, got {out:?}",
            row.path
        );
        assert_eq!(
            field["Message"],
            json!(row.requirement),
            "case {case:?}: the FieldList must state the requirement, got {out:?}"
        );
        assert_eq!(
            field["Code"],
            json!(VALIDATION_CODE_INVALID_VALUE),
            "case {case:?}: the FieldList must carry the invalid-value code, got {out:?}"
        );
        assert_eq!(
            harness.get(case, &id).await,
            before,
            "case {case:?}: a refused amendment must change nothing"
        );
    }
}

/// One refused amendment request: which application it amends, and the amendments it
/// carries.
struct BoundMemberRow {
    case: String,
    identifier: String,
    revision: String,
    amendments: Value,
}

/// EVERY refusal `AmendBenefitApplication` answers names an input member the operation
/// BINDS, in the walk's spelling — never a member of another operation's input, which its
/// caller could not have sent and cannot act on.
///
/// The rows span the refusal classes of the operation: a path naming nothing amendable, a
/// path past its own length bound, a value past the modelled length of the member it
/// writes, a value past this service's bound on a position of the document, an element
/// position the record does not hold, both ends of the amendment-count bound, each rule
/// stated about the document the amendments produce, and a modelled constraint the
/// centralized walk refuses ahead of the handler.
#[tokio::test]
async fn every_amend_refusal_names_an_input_member_the_operation_binds() {
    let harness = Harness::new();
    let subject = schema_subject(&harness).await;
    let field_path = format!("{DETAILS_MEMBER}.{}", subject.field);
    let plain = seeded_application(
        &harness,
        "a plain application",
        "tok-amend-binds-plain",
        &subject.benefit,
        &json!({}),
    )
    .await;
    let filled = seeded_application(
        &harness,
        "an application at the member bound",
        "tok-amend-binds-filled",
        &subject.benefit,
        &flat_object(MAX_DETAILS_MEMBERS),
    )
    .await;
    let plain_revision = current_revision(&harness, "a plain application", &plain).await;
    let filled_revision =
        current_revision(&harness, "an application at the member bound", &filled).await;
    let amend_one = |field: &str, value: &str| json!([amendment(field, value)]);

    let rows: Vec<BoundMemberRow> = vec![
        BoundMemberRow {
            case: "a path naming no amendable field".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one("Programs", "would-not"),
        },
        BoundMemberRow {
            case: "a path past its own length bound".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one(
                &format!("{DETAILS_MEMBER}.{}", "a".repeat(MAX_FIELD_PATH_LEN)),
                "x",
            ),
        },
        BoundMemberRow {
            case: "a value past the modelled length of the member it writes".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one("Name", &"n".repeat(NAME_MAX_LEN + 1)),
        },
        BoundMemberRow {
            case: "a value past the bound on a position of the document".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one(
                &format!("{DETAILS_MEMBER}.note"),
                &"v".repeat(MAX_DETAILS_VALUE_LEN + 1),
            ),
        },
        BoundMemberRow {
            case: "an element position the record does not hold".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one("PartnerContacts.0.Email", "partner@example.com"),
        },
        BoundMemberRow {
            case: "no amendments at all".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: json!([]),
        },
        BoundMemberRow {
            case: "more amendments than one request may carry".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: Value::Array(
                (0..=MAX_AMENDMENTS_PER_REQUEST)
                    .map(|position| amendment("Name", &format!("amended-{position}")))
                    .collect(),
            ),
        },
        BoundMemberRow {
            case: "a document the resource guard refuses".to_string(),
            identifier: filled.clone(),
            revision: filled_revision.clone(),
            amendments: amend_one(&format!("{DETAILS_MEMBER}.added"), "x"),
        },
        BoundMemberRow {
            case: "a document the benefit's request schema refuses".to_string(),
            identifier: plain.clone(),
            revision: plain_revision.clone(),
            amendments: amend_one(&field_path, "not of the declared type"),
        },
        BoundMemberRow {
            case: "an identifier outside its modelled pattern".to_string(),
            identifier: "not-an-application-identifier".to_string(),
            revision: plain_revision.clone(),
            amendments: amend_one("Name", "amended"),
        },
    ];

    let mut named: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = &row.case;
        let body = amend_body(
            &row.identifier,
            &row.revision,
            &format!("tok-amend-binds-{index}"),
            row.amendments.clone(),
        );
        let (status, out) = harness.call("AmendBenefitApplication", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled ValidationException's 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );
        let message = message_of(&out).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal carries no modelled Message member: {out:?}")
        });
        assert_validation_invariants(case, &message);
        let path = stated_path(case, &message);
        // A refusal the centralized walk produced carries no modelled `FieldList`, so the
        // path is read from the clause; where a handler's own refusal states one, it must
        // name the same path.
        let field = out["FieldList"]
            .as_array()
            .and_then(|fields| fields.first())
            .cloned();
        if let Some(field) = field {
            assert_eq!(
                field["Name"],
                json!(path),
                "case {case:?}: the clause and the FieldList must name one path, got {out:?}"
            );
        }
        let member = path.split('.').next().unwrap_or_default().to_string();
        assert!(
            AMEND_INPUT_PATHS.contains(&member.as_str()),
            "case {case:?}: expected the refusal to name one of the members this operation \
             binds ({AMEND_INPUT_PATHS:?}), got {path:?}"
        );
        named.push(member);
    }

    // The table is not degenerate: the amendment list is named, and so is at least one
    // other member the operation binds.
    assert!(
        named.iter().any(|member| member == "amendments"),
        "case \"table\": expected a row naming the amendment list, got {named:?}"
    );
    assert!(
        named.iter().any(|member| member != "amendments"),
        "case \"table\": expected a row naming another bound member, got {named:?}"
    );
    // And no row named the document member, which is the create's and the update's input.
    assert!(
        !named.iter().any(|member| member == DETAILS_PATH),
        "case \"table\": no refusal of this operation may name {DETAILS_PATH:?}, got {named:?}"
    );
}
