// SPDX-License-Identifier: CC0-1.0

//! The `BenefitApplicationDetails` document guards, end to end over the wire.
//!
//! Every assertion goes through the real request path — the shared
//! [`crate::benefit_application_lifecycle::Harness`], i.e. `MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies, `POST /` and `X-Amz-Target:
//! PartnerCentralBenefitsService.<Operation>` — so what it pins is the emulator's HTTP
//! behaviour, not an internal helper.
//!
//! The rule under test: a refusal this service makes on an input member of its own states
//! the member and the requirement it failed through the AWS validation envelope — `N
//! validation error(s) detected: Value at '<member>' failed to satisfy constraint:
//! <requirement>` — so a client can read which member and which requirement failed from
//! any refusal of the service, not only from the ones the centralized constraint walk
//! produced. `BenefitApplicationDetails` is an unconstrained `Document`
//! (`smithy_query(query="shape",shape="CreateBenefitApplicationInput")`), so its depth,
//! member-count and text-size bounds are enforced by the handlers that write it —
//! `CreateBenefitApplication`, `UpdateBenefitApplication` and `AmendBenefitApplication` —
//! and each of those refusals is a refusal of a member THAT operation binds: the document
//! member for the two that carry one, and the amendment that produced the refused document
//! for the amendment, which binds no document member (see
//! `tests/all/refusal_member_path.rs` for that rule over the whole class).
//!
//! The tables span what the rule generalises over rather than the one document the issue
//! named: which of the three bounds the document breaks, the FORM it breaks it in (nested
//! objects, nested arrays, the two alternating; members flat, in a list, or spread over
//! nested objects; text in one value, in a member name, or spread over many members), by
//! how much it breaks it, and every bound exactly AT its published value, which is served
//! and read back.
//!
//! The text bound is a LENGTH, so the last two tables also pin the unit it is counted in:
//! characters, the unit this service counts every length it enforces in and the unit the
//! requirement it refuses with names. Their rows span the character width of the text (one
//! to four UTF-8 bytes), where the text sits, and the count relative to the published
//! bound, so a document inside the stated maximum is served whatever its byte weight.

use serde_json::{json, Map, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    MAX_DETAILS_DEPTH, MAX_DETAILS_MEMBERS, MAX_DETAILS_TEXT_LEN, MAX_DETAILS_VALUE_LEN,
};

use crate::benefit_application_lifecycle::{error_code, text, Harness, CATALOG};
use crate::refusal_member_path::{amendment_member_path, AMENDMENT_NEW_VALUE};
use crate::reserved_tag_keys::application_count;
use crate::service_error_rules::{assert_validation_invariants, clause, message_of, one};

/// The input member every refusal below names, spelled as the wire spells it.
const MEMBER: &str = "BenefitApplicationDetails";

/// The PATH that member is named by in a refusal: the member in lowerCamel, the spelling
/// the centralized walk reports for an input member, so a client reads a refusal this
/// service's handlers state exactly as it reads one the walk produced.
pub(crate) const MEMBER_PATH: &str = "benefitApplicationDetails";

/// The requirement a document nested past the depth bound fails, written out here rather
/// than read from the service's own helper so the wording itself is under test.
pub(crate) fn depth_requirement() -> String {
    format!("Member must nest no deeper than {MAX_DETAILS_DEPTH} levels")
}

/// The requirement a document holding more members than the service stores fails.
pub(crate) fn member_requirement() -> String {
    format!("Member must hold no more than {MAX_DETAILS_MEMBERS} members")
}

/// The requirement a document carrying more text than the service stores fails: a length
/// bound, so it is worded the way this service words every other length
/// (`Member must have length less than or equal to <max>`) and COUNTED the way this
/// service counts every other length — in characters.
pub(crate) fn text_requirement() -> String {
    format!("Member must have length less than or equal to {MAX_DETAILS_TEXT_LEN}")
}

// ---------------------------------------------------------------------------
// The documents
// ---------------------------------------------------------------------------

/// A document nesting exactly `levels` levels of objects: one object step per level, with
/// a string leaf at the bottom (the leaf is itself a level).
fn nested_objects(levels: usize) -> Value {
    (1..levels).fold(json!("leaf"), |inner, _| json!({ "n": inner }))
}

/// The same depth reached through arrays instead of objects.
fn nested_arrays(levels: usize) -> Value {
    (1..levels).fold(json!("leaf"), |inner, _| json!([inner]))
}

/// The same depth reached through objects and arrays alternating.
fn nested_alternating(levels: usize) -> Value {
    (1..levels).fold(json!("leaf"), |inner, step| {
        if step % 2 == 0 {
            json!([inner])
        } else {
            json!({ "n": inner })
        }
    })
}

/// A flat object of `members` members, each a one-character value.
pub(crate) fn flat_object(members: usize) -> Value {
    let mut document = Map::new();
    for index in 0..members {
        document.insert(format!("m{index}"), json!("x"));
    }
    Value::Object(document)
}

/// A flat array of `elements` one-character elements.
fn flat_array(elements: usize) -> Value {
    Value::Array((0..elements).map(|_| json!("x")).collect())
}

/// An object of `groups` objects, each holding `each` members: `groups * (each + 1)`
/// members over the whole tree, so the count is spread rather than flat.
fn grouped_object(groups: usize, each: usize) -> Value {
    let mut document = Map::new();
    for group in 0..groups {
        document.insert(format!("g{group}"), flat_object(each));
    }
    Value::Object(document)
}

/// An object carrying exactly `characters` characters of text in ONE value: a
/// one-character member name and a value of `characters - 1` characters.
fn text_in_one_value(characters: usize) -> Value {
    json!({ "m": "v".repeat(characters.saturating_sub(1)) })
}

/// An object carrying exactly `characters` characters of text in its member NAME: a name
/// of `characters - 1` characters and a one-character value.
fn text_in_a_member_name(characters: usize) -> Value {
    let mut document = Map::new();
    document.insert("n".repeat(characters.saturating_sub(1)), json!("v"));
    Value::Object(document)
}

/// An object spreading its text over `members` members, each carrying a value of
/// [`MAX_DETAILS_VALUE_LEN`] characters.
pub(crate) fn text_spread_over_members(members: usize) -> Value {
    let value = "v".repeat(MAX_DETAILS_VALUE_LEN);
    let mut document = Map::new();
    for index in 0..members {
        document.insert(format!("m{index}"), json!(value));
    }
    Value::Object(document)
}

/// How many members over [`MAX_DETAILS_TEXT_LEN`] of text it takes to pass the text
/// bound.
pub(crate) fn members_past_the_text_bound() -> usize {
    MAX_DETAILS_TEXT_LEN / MAX_DETAILS_VALUE_LEN + 1
}

/// One row: a document, and the requirement the service must say it failed.
struct Row {
    case: &'static str,
    details: Value,
    requirement: String,
}

/// The documents no operation may store, each breaking exactly ONE of the three bounds —
/// the guards are applied in order (depth, member count, text size), so a row that broke
/// two would not pin the one it names.
fn refused_rows() -> Vec<Row> {
    vec![
        Row {
            case: "objects nesting one level past the depth bound",
            details: nested_objects(MAX_DETAILS_DEPTH + 1),
            requirement: depth_requirement(),
        },
        Row {
            case: "arrays nesting one level past the depth bound",
            details: nested_arrays(MAX_DETAILS_DEPTH + 1),
            requirement: depth_requirement(),
        },
        Row {
            case: "objects and arrays alternating one level past the depth bound",
            details: nested_alternating(MAX_DETAILS_DEPTH + 1),
            requirement: depth_requirement(),
        },
        Row {
            case: "a document nesting far past the depth bound",
            details: nested_objects(MAX_DETAILS_DEPTH * 2),
            requirement: depth_requirement(),
        },
        Row {
            case: "a flat object holding one member too many",
            details: flat_object(MAX_DETAILS_MEMBERS + 1),
            requirement: member_requirement(),
        },
        Row {
            case: "an array holding one element too many",
            details: flat_array(MAX_DETAILS_MEMBERS + 1),
            requirement: member_requirement(),
        },
        Row {
            case: "members spread over nested objects, one group too many",
            details: grouped_object(16, 16),
            requirement: member_requirement(),
        },
        Row {
            case: "one value one character past the text bound",
            details: text_in_one_value(MAX_DETAILS_TEXT_LEN + 1),
            requirement: text_requirement(),
        },
        Row {
            case: "a member name one character past the text bound",
            details: text_in_a_member_name(MAX_DETAILS_TEXT_LEN + 1),
            requirement: text_requirement(),
        },
        Row {
            case: "text spread over many members, past the text bound",
            details: text_spread_over_members(members_past_the_text_bound()),
            requirement: text_requirement(),
        },
    ]
}

/// The documents each bound still admits: every one of them sits EXACTLY at a published
/// bound, so the guards refuse what is past them and nothing else. Each is an object at
/// the top, which is what the seeded benefit's `BenefitRequestSchema` declares, so what
/// these rows exercise is the guard and not the schema.
fn accepted_rows() -> Vec<(&'static str, Value)> {
    vec![
        ("an empty document", json!({})),
        (
            "objects nesting exactly to the depth bound",
            nested_objects(MAX_DETAILS_DEPTH),
        ),
        (
            "arrays nesting exactly to the depth bound",
            json!({ "items": nested_arrays(MAX_DETAILS_DEPTH - 1) }),
        ),
        (
            "objects and arrays alternating exactly to the depth bound",
            nested_alternating(MAX_DETAILS_DEPTH),
        ),
        (
            "a flat object holding exactly the most members",
            flat_object(MAX_DETAILS_MEMBERS),
        ),
        (
            "an array holding exactly the most elements",
            json!({ "items": flat_array(MAX_DETAILS_MEMBERS - 1) }),
        ),
        (
            "one value carrying exactly the most text",
            text_in_one_value(MAX_DETAILS_TEXT_LEN),
        ),
        (
            "a member name carrying exactly the most text",
            text_in_a_member_name(MAX_DETAILS_TEXT_LEN),
        ),
        (
            "text spread over members, within the text bound",
            text_spread_over_members(members_past_the_text_bound() - 1),
        ),
    ]
}

// ---------------------------------------------------------------------------
// The shared assertion
// ---------------------------------------------------------------------------

/// Require that `operation` refused `body` as a document-guard violation: HTTP 400, the
/// modelled `ValidationException`, and a `Message` that is the AWS envelope over the one
/// clause naming `path` and `requirement` — with the same member path and requirement on
/// the `FieldList` beside it.
///
/// `path` is the member the refusal must name, which is the member the REFUSED OPERATION
/// binds: [`MEMBER_PATH`] where the operation carries the document itself, and the
/// amendment that produced it where the document is what the request's amendments produce.
async fn assert_guard_refusal(
    harness: &Harness,
    case: &str,
    operation: &str,
    path: &str,
    requirement: &str,
    body: &Value,
) {
    let (status, out) = harness.call(operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to refuse the document with 400, \
         got {status} {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {case:?}: expected ValidationException from {operation}, got {out:?}"
    );
    let message = message_of(&out).unwrap_or_else(|| {
        panic!("case {case:?}: the refusal carries no modelled Message member: {out:?}")
    });
    assert_eq!(
        message,
        one(&clause(path, requirement)),
        "case {case:?}: expected the envelope over the clause for {path}, got {message:?}"
    );
    assert_validation_invariants(case, &message);
    let field = out["FieldList"]
        .as_array()
        .and_then(|fields| fields.first())
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        field["Name"],
        json!(path),
        "case {case:?}: the refusal's FieldList must name {path}, got {out:?}"
    );
    assert_eq!(
        field["Message"],
        json!(requirement),
        "case {case:?}: the FieldList must state the requirement, got {out:?}"
    );
    assert_eq!(
        field["Code"],
        json!("INVALID_VALUE"),
        "case {case:?}: the FieldList must carry the invalid-value code, got {out:?}"
    );
}

/// The create body for `details`, applying for the catalog's first `ACTIVE` benefit.
async fn create_body(harness: &Harness, token: &str, details: &Value) -> Value {
    let benefit = harness.active_benefit().await;
    json!({
        "Catalog": CATALOG,
        "ClientToken": token,
        "BenefitIdentifier": benefit["Id"],
        "Name": "a document-guard probe",
        "BenefitApplicationDetails": details,
    })
}

// ---------------------------------------------------------------------------
// CreateBenefitApplication — the member's first writer
// ---------------------------------------------------------------------------

/// Every document past a bound is refused through the envelope, and nothing is minted.
#[tokio::test]
async fn create_refuses_a_document_past_a_bound_through_the_envelope() {
    let harness = Harness::new();
    let before = application_count(&harness, "before", CATALOG).await;

    for (index, row) in refused_rows().into_iter().enumerate() {
        let token = format!("tok-guard-create-{index}");
        let body = create_body(&harness, &token, &row.details).await;
        assert_guard_refusal(
            &harness,
            row.case,
            "CreateBenefitApplication",
            MEMBER_PATH,
            &row.requirement,
            &body,
        )
        .await;
        let after = application_count(&harness, row.case, CATALOG).await;
        assert_eq!(
            after, before,
            "case {:?}: a refused create must mint nothing, got {after} applications \
             (expected {before})",
            row.case
        );
    }
}

/// Every document AT a bound is stored and read back unchanged through
/// `GetBenefitApplication`.
#[tokio::test]
async fn create_stores_a_document_at_every_bound() {
    let harness = Harness::new();

    for (index, (case, details)) in accepted_rows().into_iter().enumerate() {
        let token = format!("tok-guard-create-ok-{index}");
        let body = create_body(&harness, &token, &details).await;
        let created = harness.create(case, &body).await;
        let id = text(case, &created, "Id");
        let stored = harness.get(case, &id).await;
        assert_eq!(
            stored["BenefitApplicationDetails"], details,
            "case {case:?}: the stored document must read back unchanged, got {:?}",
            stored["BenefitApplicationDetails"]
        );
    }
}

// ---------------------------------------------------------------------------
// UpdateBenefitApplication — the same member, written again
// ---------------------------------------------------------------------------

/// An update is refused in the same words as a create, changes nothing, and still stores a
/// document that sits at a bound.
#[tokio::test]
async fn update_refuses_a_document_past_a_bound_through_the_envelope() {
    let harness = Harness::new();
    let seeded = json!({ "kept": "yes" });
    let body = create_body(&harness, "tok-guard-update-seed", &seeded).await;
    let created = harness.create("seed", &body).await;
    let id = text("seed", &created, "Id");
    let before = harness.get("seed", &id).await;

    for (index, row) in refused_rows().into_iter().enumerate() {
        assert_guard_refusal(
            &harness,
            row.case,
            "UpdateBenefitApplication",
            MEMBER_PATH,
            &row.requirement,
            &json!({
                "Catalog": CATALOG,
                "ClientToken": format!("tok-guard-update-{index}"),
                "Identifier": id,
                "Revision": text(row.case, &before, "Revision"),
                "BenefitApplicationDetails": row.details,
            }),
        )
        .await;
        let after = harness.get(row.case, &id).await;
        assert_eq!(
            after, before,
            "case {:?}: a refused update must change nothing, got {after:?}",
            row.case
        );
    }

    // The same member, at a bound, is still written and read back.
    let at_the_bound = flat_object(MAX_DETAILS_MEMBERS);
    harness
        .ok(
            "an update at the member bound",
            "UpdateBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-guard-update-ok",
                "Identifier": id,
                "Revision": text("at the bound", &before, "Revision"),
                "BenefitApplicationDetails": at_the_bound,
            }),
        )
        .await;
    let stored = harness.get("an update at the member bound", &id).await;
    assert_eq!(
        stored["BenefitApplicationDetails"], at_the_bound,
        "case \"an update at the member bound\": the update must have applied, got {:?}",
        stored["BenefitApplicationDetails"]
    );
}

// ---------------------------------------------------------------------------
// AmendBenefitApplication — the member as the amendments leave it
// ---------------------------------------------------------------------------

/// An amendment that would take the STORED document past a bound is refused in the same
/// words, and changes nothing.
///
/// The rows are the two bounds an amendment can reach: a `FieldPath` carries at most
/// `MAX_DETAILS_DEPTH` segments, so no single amendment can nest the document past the
/// depth bound, while both the member count and the text size can be grown by adding a
/// member to a document already at the bound.
///
/// The requirement is the one the create and the update state for the same document; the
/// member named is NOT theirs, because this operation binds no document member: it is the
/// amendment that wrote the refused document, the one thing its caller can act on.
#[tokio::test]
async fn amend_refuses_a_document_past_a_bound_through_the_envelope() {
    let harness = Harness::new();
    let value = "v".repeat(MAX_DETAILS_VALUE_LEN);
    let rows: Vec<(&str, Value, String, String)> = vec![
        (
            "an amendment adding one member too many",
            flat_object(MAX_DETAILS_MEMBERS),
            "x".to_string(),
            member_requirement(),
        ),
        (
            "an amendment adding text past the text bound",
            text_spread_over_members(members_past_the_text_bound() - 1),
            value.clone(),
            text_requirement(),
        ),
    ];

    for (index, (case, seeded, new_value, requirement)) in rows.into_iter().enumerate() {
        let body = create_body(&harness, &format!("tok-guard-amend-seed-{index}"), &seeded).await;
        let created = harness.create(case, &body).await;
        let id = text(case, &created, "Id");
        let before = harness.get(case, &id).await;

        assert_guard_refusal(
            &harness,
            case,
            "AmendBenefitApplication",
            &amendment_member_path(0, AMENDMENT_NEW_VALUE),
            &requirement,
            &json!({
                "Catalog": CATALOG,
                "ClientToken": format!("tok-guard-amend-{index}"),
                "Identifier": id,
                "Revision": text(case, &before, "Revision"),
                "AmendmentReason": "the partner corrected the application",
                "Amendments": [{
                    "FieldPath": format!("{MEMBER}.added"),
                    "NewValue": new_value,
                }],
            }),
        )
        .await;
        let after = harness.get(case, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused amendment must change nothing, got {after:?}"
        );

        // An amendment that CHANGES a member the document already holds keeps it inside
        // the bound, so it applies — the guard refuses growth, not every amendment.
        harness
            .ok(
                case,
                "AmendBenefitApplication",
                &json!({
                    "Catalog": CATALOG,
                    "ClientToken": format!("tok-guard-amend-ok-{index}"),
                    "Identifier": id,
                    "Revision": text(case, &before, "Revision"),
                    "AmendmentReason": "the partner corrected the application",
                    "Amendments": [{
                        "FieldPath": format!("{MEMBER}.m0"),
                        "NewValue": new_value,
                    }],
                }),
            )
            .await;
        let amended = harness.get(case, &id).await;
        assert_eq!(
            amended["BenefitApplicationDetails"]["m0"],
            json!(new_value),
            "case {case:?}: the amendment within the bound must have applied, got {:?}",
            amended["BenefitApplicationDetails"]
        );
    }
}

// ---------------------------------------------------------------------------
// The UNIT the text bound is counted in
// ---------------------------------------------------------------------------

/// The character widths the text rows are written in: `(case, one character of that
/// width)`, one, two, three and four UTF-8 bytes per character, so a given CHARACTER
/// count weighs 1x, 2x, 3x and 4x as many bytes.
const WIDTHS: [(&str, &str); 4] = [
    ("one-byte", "a"),
    ("two-byte", "é"),
    ("three-byte", "\u{4e2d}"),
    ("four-byte", "\u{1d11e}"),
];

/// The text `document` carries, in CHARACTERS and in UTF-8 BYTES: the two units the bound
/// could be counted in, so a row can state that the two differ and which one the service
/// held it to.
fn text_weight(document: &Value) -> (usize, usize) {
    match document {
        Value::String(text) => (text.chars().count(), text.len()),
        Value::Array(items) => items.iter().fold((0, 0), |(characters, bytes), item| {
            let (item_characters, item_bytes) = text_weight(item);
            (characters + item_characters, bytes + item_bytes)
        }),
        Value::Object(entries) => {
            entries
                .iter()
                .fold((0, 0), |(characters, bytes), (name, member)| {
                    let (member_characters, member_bytes) = text_weight(member);
                    (
                        characters + name.chars().count() + member_characters,
                        bytes + name.len() + member_bytes,
                    )
                })
        }
        _ => (0, 0),
    }
}

/// An object carrying exactly `characters` characters of text written in `unit` in ONE
/// value: a one-character ASCII member name and a value of `characters - 1` characters.
fn wide_text_in_one_value(unit: &str, characters: usize) -> Value {
    json!({ "m": unit.repeat(characters.saturating_sub(1)) })
}

/// The same text in the member NAME: a name of `characters - 1` characters written in
/// `unit` and a one-character ASCII value.
fn wide_text_in_a_member_name(unit: &str, characters: usize) -> Value {
    let mut document = Map::new();
    document.insert(unit.repeat(characters.saturating_sub(1)), json!("v"));
    Value::Object(document)
}

/// One form the text can sit in: a label, and how a document of a given character count
/// in a given character width is built in that form.
type TextForm = (&'static str, fn(&str, usize) -> Value);

/// The text bound is counted in CHARACTERS — the unit this service counts every length it
/// enforces in, and the unit the requirement it refuses with names — so a document inside
/// the stated maximum is stored whatever the UTF-8 width of its text, and one past it is
/// refused with that length requirement.
///
/// The rows span the character WIDTH of the text (one to four bytes per character), the
/// place the text sits (a member's value, a member's name) and the character count
/// relative to the published bound (well inside it, one short of it, at it, one past it,
/// far past it). Every multi-byte row at or near the bound carries MORE bytes than the
/// bound names, which the row asserts, so a service counting bytes would refuse exactly
/// the documents these rows are served.
#[tokio::test]
async fn the_document_text_bound_counts_characters_not_bytes() {
    let harness = Harness::new();
    let forms: [TextForm; 2] = [
        ("in one value", wide_text_in_one_value),
        ("in a member name", wide_text_in_a_member_name),
    ];
    // Well inside the bound, one character short of it, at it, one past it, far past it.
    let counts: [usize; 5] = [
        1024,
        MAX_DETAILS_TEXT_LEN - 1,
        MAX_DETAILS_TEXT_LEN,
        MAX_DETAILS_TEXT_LEN + 1,
        MAX_DETAILS_TEXT_LEN * 2,
    ];

    let mut row = 0usize;
    for (width, unit) in WIDTHS {
        for (form, build) in forms {
            for characters in counts {
                let case = format!("{width} {form}, {characters} characters");
                let details = build(unit, characters);
                let (built_characters, built_bytes) = text_weight(&details);
                assert_eq!(
                    built_characters, characters,
                    "case {case:?}: the row must build {characters} characters of text, \
                     got {built_characters}"
                );
                assert_eq!(
                    built_bytes,
                    1 + unit.len() * (characters - 1),
                    "case {case:?}: the row must weigh {} bytes, got {built_bytes}",
                    1 + unit.len() * (characters - 1)
                );

                let body = create_body(&harness, &format!("tok-guard-width-{row}"), &details).await;
                if characters <= MAX_DETAILS_TEXT_LEN {
                    let created = harness.create(&case, &body).await;
                    let id = text(&case, &created, "Id");
                    let stored = harness.get(&case, &id).await;
                    assert_eq!(
                        stored["BenefitApplicationDetails"], details,
                        "case {case:?}: a document inside the character bound must be stored \
                         and read back unchanged, got {:?}",
                        stored["BenefitApplicationDetails"]
                    );
                } else {
                    assert_guard_refusal(
                        &harness,
                        &case,
                        "CreateBenefitApplication",
                        MEMBER_PATH,
                        &text_requirement(),
                        &body,
                    )
                    .await;
                }

                row += 1;
            }
        }
    }
}

/// The same unit through `AmendBenefitApplication`, which judges the document its
/// amendments PRODUCED: an amendment that leaves the document at the character bound is
/// applied even though the document carries far more bytes than the bound names, and the
/// next one, which takes the character count past it, is refused with that length
/// requirement and changes nothing.
#[tokio::test]
async fn amend_counts_the_amended_document_in_characters() {
    let harness = Harness::new();
    // Two UTF-8 bytes per character, so a document inside the character bound holds
    // nearly twice the bound's number of bytes.
    let unit = "é";
    let added = "added";
    // The seed sits one amendment's value short of the bound, so one amendment can reach
    // the bound exactly and the next must pass it.
    let seeded_characters = MAX_DETAILS_TEXT_LEN - MAX_DETAILS_VALUE_LEN;
    let to_the_bound = MAX_DETAILS_VALUE_LEN - added.chars().count();
    let body = create_body(
        &harness,
        "tok-guard-wide-seed",
        &wide_text_in_one_value(unit, seeded_characters),
    )
    .await;
    let created = harness.create("a wide seeded document", &body).await;
    let id = text("a wide seeded document", &created, "Id");
    let before = harness.get("a wide seeded document", &id).await;

    let case = "an amendment reaching the character bound";
    harness
        .ok(
            case,
            "AmendBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-guard-wide-at",
                "Identifier": id,
                "Revision": text(case, &before, "Revision"),
                "AmendmentReason": "the partner corrected the application",
                "Amendments": [{
                    "FieldPath": format!("{MEMBER}.{added}"),
                    "NewValue": unit.repeat(to_the_bound),
                }],
            }),
        )
        .await;
    let stored = harness.get(case, &id).await;
    let (characters, bytes) = text_weight(&stored["BenefitApplicationDetails"]);
    assert_eq!(
        characters, MAX_DETAILS_TEXT_LEN,
        "case {case:?}: the amended document must sit exactly at the bound, \
         got {characters} characters"
    );
    assert!(
        bytes > MAX_DETAILS_TEXT_LEN,
        "case {case:?}: the row must carry more bytes than the bound names, so a byte \
         count would have refused it, got {bytes}"
    );

    let past = "an amendment past the character bound";
    let at_the_bound = harness.get(past, &id).await;
    assert_guard_refusal(
        &harness,
        past,
        "AmendBenefitApplication",
        &amendment_member_path(0, AMENDMENT_NEW_VALUE),
        &text_requirement(),
        &json!({
            "Catalog": CATALOG,
            "ClientToken": "tok-guard-wide-past",
            "Identifier": id,
            "Revision": text(past, &at_the_bound, "Revision"),
            "AmendmentReason": "the partner corrected the application",
            "Amendments": [{
                "FieldPath": format!("{MEMBER}.extra"),
                "NewValue": unit.repeat(MAX_DETAILS_VALUE_LEN),
            }],
        }),
    )
    .await;
    let after = harness.get(past, &id).await;
    assert_eq!(
        after, at_the_bound,
        "case {past:?}: a refused amendment must change nothing, got {after:?}"
    );
}
