// SPDX-License-Identifier: CC0-1.0

//! The bounds on the caller strings a write PERSISTS, end to end over the wire.
//!
//! Every assertion goes through the real request path (`MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies) and reads the effect back through `GetBenefitApplication`.
//!
//! The rule under test: a caller string a write operation persists is accepted only
//! within BOTH ends of the length the model states for that value — the bounds on the
//! member it is stored in and reported through, and on the inputs the model constrains —
//! and a value outside either end is refused 400 `ValidationException` with nothing
//! stored, whether or not the operation's OWN input member happens to carry that
//! constraint in the model.
//!
//! The rows span what the rule generalises over: which string (the reason that becomes
//! `StatusReason`, the `ClientToken` the record keeps, the resource ARN an application's
//! `AssociatedResources` holds), which operation writes it (`RecallBenefitApplication`,
//! whose members the model leaves unconstrained, and `CancelBenefitApplication`, whose
//! `Reason` the model bounds; `CreateBenefitApplication`, whose `Arns` elements carry no
//! length, and `AssociateBenefitApplicationResource`, whose `ResourceArn` carries the
//! model's own), the position relative to each published bound (at the maximum, one past
//! it, at the minimum and below it), the character width of the value (ASCII and
//! multi-byte, since a `@length` counts characters) and — for a value supplied as a list
//! element — which element of the list carries it.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    member_validation_exception, MAX_ASSOCIATED_RESOURCE_ARN_LEN, MAX_CLIENT_TOKEN_LEN,
    MAX_STATUS_REASON_LEN, MIN_STATUS_REASON_LEN, STATUS_IN_REVIEW, STATUS_PENDING_SUBMISSION,
    VALIDATION_REASON_FIELD,
};

use crate::benefit_application_lifecycle::{error_code, text, Harness, CATALOG};
use crate::disassociate_benefit_application_resource::{associated, body as resource_body};
use crate::recall_benefit_application::{recall_body, seed, submit};
use crate::reserved_tag_keys::element_requirement;
use crate::service_error_rules::{assert_validation_invariants, clause, message_of, one};

/// The requirement a value longer than `max` fails, as this service words a length.
fn too_long(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The requirement a value shorter than `min` fails, as this service words a length.
fn too_short(min: usize) -> String {
    format!("Member must have length greater than or equal to {min}")
}

/// `count` repetitions of `unit`, so a row can name a length in CHARACTERS whatever the
/// width of the character it is built from.
fn repeat(unit: &str, count: usize) -> String {
    unit.repeat(count)
}

/// One application of `CATALOG`, submitted so it is recallable, as `(id, its reason)`.
async fn recallable(harness: &Harness, case: &str, token_seed: &str) -> String {
    let (id, _) = seed(harness, case, CATALOG, token_seed).await;
    submit(harness, case, CATALOG, &id).await;
    id
}

/// Assert `out` is this service's one-member `ValidationException` naming `member` and
/// `requirement`.
fn assert_member_refusal(case: &str, status: u16, out: &Value, member: &str, requirement: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 outside the bound on {member:?}, got {status} {out:?}"
    );
    assert_eq!(
        error_code(out),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {out:?}"
    );
    let message = message_of(out)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no Message: {out:?}"));
    assert_validation_invariants(case, &message);
    let expected = member_validation_exception(member, requirement, VALIDATION_REASON_FIELD);
    assert_eq!(
        message, expected.message,
        "case {case:?}: expected the message {:?}, got {message:?}",
        expected.message
    );
    assert_eq!(
        out["Reason"],
        json!(expected.reason),
        "case {case:?}: expected the reason {:?}, got {out:?}",
        expected.reason
    );
}

/// A reason a write records as `StatusReason` is accepted up to the bound of the member
/// it is reported through, and refused one past it — on the operation whose model bounds
/// it and on the operation whose model does not.
#[tokio::test]
async fn a_persisted_reason_is_bounded_by_the_member_it_is_reported_through() {
    let harness = Harness::new();

    // (case, the character the reason is built from) — one byte and several, because the
    // bound counts characters.
    let widths: [(&str, &str); 2] = [("ascii", "r"), ("multi-byte", "é")];

    // AT the bound: the recall applies and the read reports exactly what was sent.
    for (row, (width, unit)) in widths.iter().enumerate() {
        let case = format!("recall, {width}, at the bound");
        let id = recallable(&harness, &case, &format!("tok-reason-at-{row}")).await;
        let reason = repeat(unit, MAX_STATUS_REASON_LEN);
        harness
            .ok(
                &case,
                "RecallBenefitApplication",
                &recall_body(CATALOG, &id, &reason, Some(&format!("tok-reason-{row}"))),
            )
            .await;
        let after = harness.get_in(&case, CATALOG, &id).await;
        assert_eq!(
            after["Status"],
            json!(STATUS_PENDING_SUBMISSION),
            "case {case:?}: a reason at the bound must be accepted, got {after:?}"
        );
        assert_eq!(
            after["StatusReason"],
            json!(reason),
            "case {case:?}: the reason at the bound must round-trip, got {after:?}"
        );
        let stored = text(&case, &after, "StatusReason").chars().count();
        assert_eq!(
            stored, MAX_STATUS_REASON_LEN,
            "case {case:?}: expected {MAX_STATUS_REASON_LEN} stored characters, got {stored}"
        );
    }

    // ONE PAST it: refused, and the record is left submitted with no reason recorded.
    for (row, (width, unit)) in widths.iter().enumerate() {
        let case = format!("recall, {width}, one past the bound");
        let id = recallable(&harness, &case, &format!("tok-reason-past-{row}")).await;
        let before = harness.get_in(&case, CATALOG, &id).await;
        let reason = repeat(unit, MAX_STATUS_REASON_LEN + 1);
        let (status, out) = harness
            .call(
                "RecallBenefitApplication",
                &recall_body(CATALOG, &id, &reason, Some(&format!("tok-reason-past-{row}"))),
            )
            .await;
        assert_member_refusal(&case, status, &out, "reason", &too_long(MAX_STATUS_REASON_LEN));
        let after = harness.get_in(&case, CATALOG, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused recall must change nothing, got {after:?}"
        );
        assert_eq!(
            after["Status"],
            json!(STATUS_IN_REVIEW),
            "case {case:?}: the application must still be submitted, got {after:?}"
        );
    }

    // The same class on the OTHER operation that writes `StatusReason`: its `Reason` is
    // modelled `@length(max: 1000)`, so the same over-long value is refused there too and
    // the record keeps no reason outside the bound either way.
    for (row, (width, unit)) in widths.iter().enumerate() {
        let case = format!("cancel, {width}, one past the bound");
        let id = recallable(&harness, &case, &format!("tok-cancel-past-{row}")).await;
        let before = harness.get_in(&case, CATALOG, &id).await;
        let (status, out) = harness
            .call(
                "CancelBenefitApplication",
                &json!({
                    "Catalog": CATALOG,
                    "ClientToken": format!("tok-cancel-past-tok-{row}"),
                    "Identifier": id,
                    "Reason": repeat(unit, MAX_STATUS_REASON_LEN + 1),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for a reason past the bound, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );
        let message = message_of(&out)
            .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no Message: {out:?}"));
        assert_validation_invariants(&case, &message);
        assert!(
            message.contains(&too_long(MAX_STATUS_REASON_LEN)),
            "case {case:?}: expected the clause {:?}, got {message:?}",
            too_long(MAX_STATUS_REASON_LEN)
        );
        let after = harness.get_in(&case, CATALOG, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused cancellation must change nothing, got {after:?}"
        );
    }

    // And a reason at the bound IS accepted on that operation, so the rows above pin a
    // bound rather than a blanket refusal.
    let case = "cancel, at the bound";
    let id = recallable(&harness, case, "tok-cancel-at").await;
    let reason = repeat("c", MAX_STATUS_REASON_LEN);
    harness
        .ok(
            case,
            "CancelBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-cancel-at-tok",
                "Identifier": id,
                "Reason": reason,
            }),
        )
        .await;
    let after = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        after["StatusReason"],
        json!(reason),
        "case {case:?}: a cancellation reason at the bound must round-trip, got {after:?}"
    );
}

/// A reason a write records as `StatusReason` is accepted only from the MINIMUM of that
/// member's bound upwards: the empty reason is refused 400 `ValidationException` with
/// nothing stored on both operations that write the member — the one whose model bounds
/// its own input and the one whose model leaves it unconstrained — while every reason of
/// at least that length applies and round-trips.
#[tokio::test]
async fn a_persisted_reason_is_refused_below_the_minimum_of_its_bound() {
    let harness = Harness::new();

    // (case, the reason, whether it is inside the bound) — the shortest reason in one
    // byte and in several (the bound counts characters), a whitespace reason of that
    // length (whitespace is a character, which is what the modelled sibling's
    // `^[\s\S]{1,1000}$` admits), a longer reason well inside the bound, and the empty
    // reason, the one length below the minimum.
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "the shortest reason",
            repeat("r", MIN_STATUS_REASON_LEN),
            true,
        ),
        (
            "a multi-byte reason of the shortest length",
            repeat("é", MIN_STATUS_REASON_LEN),
            true,
        ),
        (
            "a whitespace reason of the shortest length",
            repeat(" ", MIN_STATUS_REASON_LEN),
            true,
        ),
        (
            "a reason well inside the bound",
            repeat("r", MIN_STATUS_REASON_LEN + 40),
            true,
        ),
        ("an empty reason", String::new(), false),
    ];

    for (row, (case, reason, inside)) in rows.iter().enumerate() {
        let id = recallable(&harness, case, &format!("tok-min-seed-{row}")).await;
        let before = harness.get_in(case, CATALOG, &id).await;
        let body = recall_body(CATALOG, &id, reason, Some(&format!("tok-min-{row}")));

        if *inside {
            harness.ok(case, "RecallBenefitApplication", &body).await;
            let after = harness.get_in(case, CATALOG, &id).await;
            assert_eq!(
                after["Status"],
                json!(STATUS_PENDING_SUBMISSION),
                "case {case:?}: a reason inside the bound must be accepted, got {after:?}"
            );
            assert_eq!(
                after["StatusReason"],
                json!(reason),
                "case {case:?}: the reason must round-trip, got {after:?}"
            );
            continue;
        }

        let (status, out) = harness.call("RecallBenefitApplication", &body).await;
        assert_member_refusal(
            case,
            status,
            &out,
            "reason",
            &too_short(MIN_STATUS_REASON_LEN),
        );
        let after = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused recall must change nothing, got {after:?}"
        );
        assert_eq!(
            after["Status"],
            json!(STATUS_IN_REVIEW),
            "case {case:?}: the application must still be submitted, got {after:?}"
        );
        assert_eq!(
            after["StatusReason"],
            Value::Null,
            "case {case:?}: a refused recall must record no reason, got {after:?}"
        );

        // The refusal left the application recallable, so the rows pin a bound rather
        // than a blanket refusal of this application.
        harness
            .ok(
                &format!("{case}, then a reason inside the bound"),
                "RecallBenefitApplication",
                &recall_body(
                    CATALOG,
                    &id,
                    "recalled for review",
                    Some(&format!("tok-min-ok-{row}")),
                ),
            )
            .await;
        let recalled = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            recalled["Status"],
            json!(STATUS_PENDING_SUBMISSION),
            "case {case:?}: the application must still be recallable after the refusal, \
             got {recalled:?}"
        );
    }

    // The same class on the OTHER operation that writes `StatusReason`: its `Reason` is
    // modelled to admit 1 to 1000 characters, so the empty reason is refused there too —
    // one stored member, one acceptance rule between the siblings that write it.
    let case = "cancel, an empty reason";
    let id = recallable(&harness, case, "tok-min-cancel-empty").await;
    let before = harness.get_in(case, CATALOG, &id).await;
    let (status, out) = harness
        .call(
            "CancelBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-min-cancel-empty-tok",
                "Identifier": id,
                "Reason": "",
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 for an empty reason, got {status} {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {out:?}"
    );
    let message = message_of(&out)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no Message: {out:?}"));
    assert_validation_invariants(case, &message);
    let after = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        after, before,
        "case {case:?}: a refused cancellation must change nothing, got {after:?}"
    );

    // And a reason at the minimum IS accepted on that operation.
    let case = "cancel, at the minimum";
    let id = recallable(&harness, case, "tok-min-cancel-at").await;
    let reason = repeat("c", MIN_STATUS_REASON_LEN);
    harness
        .ok(
            case,
            "CancelBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-min-cancel-at-tok",
                "Identifier": id,
                "Reason": reason,
            }),
        )
        .await;
    let after = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        after["StatusReason"],
        json!(reason),
        "case {case:?}: a cancellation reason at the minimum must round-trip, got {after:?}"
    );
}

/// The `ClientToken` a recall persists carries the length every `ClientToken` of this
/// service carries: at each bound the recall applies, outside them it is refused 400 and
/// stores nothing.
#[tokio::test]
async fn a_persisted_client_token_is_bounded_like_every_other_client_token() {
    let harness = Harness::new();

    // (case, the token, whether it is inside the bounds) — the shortest token, the
    // longest, a multi-byte token of the longest length (characters, not bytes), then
    // one past the maximum in each width and one below the minimum.
    let rows: Vec<(&str, String, bool)> = vec![
        ("the shortest token", repeat("t", 1), true),
        (
            "a token of exactly the longest length",
            repeat("t", MAX_CLIENT_TOKEN_LEN),
            true,
        ),
        (
            "a multi-byte token of the longest length",
            repeat("é", MAX_CLIENT_TOKEN_LEN),
            true,
        ),
        (
            "a token one character past its bound",
            repeat("t", MAX_CLIENT_TOKEN_LEN + 1),
            false,
        ),
        (
            "a multi-byte token one character past its bound",
            repeat("é", MAX_CLIENT_TOKEN_LEN + 1),
            false,
        ),
        ("a token far past its bound", repeat("t", 100_000), false),
        ("an empty token", String::new(), false),
    ];

    for (row, (case, token, inside)) in rows.iter().enumerate() {
        let id = recallable(&harness, case, &format!("tok-token-seed-{row}")).await;
        let before = harness.get_in(case, CATALOG, &id).await;
        let body = recall_body(CATALOG, &id, "bounded token", Some(token));

        if *inside {
            harness.ok(case, "RecallBenefitApplication", &body).await;
            let after = harness.get_in(case, CATALOG, &id).await;
            assert_eq!(
                after["Status"],
                json!(STATUS_PENDING_SUBMISSION),
                "case {case:?}: a token inside the bounds must be accepted, got {after:?}"
            );
            // The token was kept, so replaying the identical request is the same recall.
            harness
                .ok(
                    &format!("{case}, replayed"),
                    "RecallBenefitApplication",
                    &body,
                )
                .await;
            let replayed = harness.get_in(case, CATALOG, &id).await;
            assert_eq!(
                replayed, after,
                "case {case:?}: replaying the accepted token must change nothing, \
                 got {replayed:?}"
            );
            continue;
        }

        let requirement = if token.chars().count() < 1 {
            too_short(1)
        } else {
            too_long(MAX_CLIENT_TOKEN_LEN)
        };
        let (status, out) = harness.call("RecallBenefitApplication", &body).await;
        assert_member_refusal(case, status, &out, "clientToken", &requirement);
        let after = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused recall must change nothing, got {after:?}"
        );

        // The refusal stored no token either: the application is still recallable, and a
        // recall carrying a token inside the bounds applies.
        harness
            .ok(
                &format!("{case}, then a bounded token"),
                "RecallBenefitApplication",
                &recall_body(CATALOG, &id, "bounded token", Some(&format!("tok-ok-{row}"))),
            )
            .await;
        let recalled = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            recalled["Status"],
            json!(STATUS_PENDING_SUBMISSION),
            "case {case:?}: the application must still be recallable after the refusal, \
             got {recalled:?}"
        );
    }
}

/// The ARN prefix every row below builds a resource ARN on: a service, a region and an
/// account, spelled as the modelled `Arn` pattern spells them
/// (`^arn:aws:([a-zA-Z0-9\-])+:([a-z]{2}(-gov)?-[a-z]+-\d{1})?:(\d{12})?:(.+)$`,
/// `smithy_query(query="shape",shape="Arn")`).
const ARN_PREFIX: &str = "arn:aws:s3:us-east-1:123456789012:";

/// An ARN of exactly `chars` CHARACTERS that satisfies the modelled `Arn` pattern, its
/// resource tail built from `unit`.
///
/// The pattern's resource tail is `(.+)`, which bounds nothing, so the length under test is
/// reached by padding that tail — in `unit`, so a row names a length in characters whatever
/// the width of the character it is written with.
fn arn_of(unit: &str, chars: usize) -> String {
    let pad = chars.saturating_sub(ARN_PREFIX.chars().count());
    format!("{ARN_PREFIX}{}", unit.repeat(pad))
}

/// One `CreateBenefitApplication` body supplying `resources` as its `AssociatedResources`,
/// which is the one request that supplies the whole list of a new record's resource ARNs.
fn create_with_resources(benefit_id: &str, token: &str, resources: &[String]) -> Value {
    json!({
        "Catalog": CATALOG,
        "ClientToken": token,
        "BenefitIdentifier": benefit_id,
        "Name": "associated-resource-arn-bounds",
        "AssociatedResources": resources,
    })
}

/// The ids `ListBenefitApplications` reports for [`CATALOG`], as a set — what a refused
/// create must leave exactly as it was.
async fn listed_ids(harness: &Harness, case: &str) -> BTreeSet<String> {
    let out = harness
        .ok(case, "ListBenefitApplications", &json!({ "Catalog": CATALOG }))
        .await;
    out["BenefitApplicationSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary list, got {out:?}"))
        .iter()
        .map(|summary| text(case, summary, "Id"))
        .collect()
}

/// Assert `out` is the refusal a write supplying an over-long ARN as an ELEMENT of a list
/// is answered with: 400 `ValidationException`, naming the collection member and carrying
/// the element's requirement set — the way the centralized walk states a violation on a
/// list's own element — with the published length as the requirement.
fn assert_element_refusal(case: &str, status: u16, out: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 past the published ARN bound, got {status} {out:?}"
    );
    assert_eq!(
        error_code(out),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {out:?}"
    );
    let message = message_of(out)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no Message: {out:?}"));
    assert_validation_invariants(case, &message);
    let expected = one(&clause(
        "associatedResources",
        &element_requirement(&too_long(MAX_ASSOCIATED_RESOURCE_ARN_LEN)),
    ));
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
    assert_eq!(
        out["Reason"],
        json!(VALIDATION_REASON_FIELD),
        "case {case:?}: expected the field-validation reason, got {out:?}"
    );
}

/// Every path that writes a resource ARN onto an application applies the SAME published
/// bound to it: an ARN up to [`MAX_ASSOCIATED_RESOURCE_ARN_LEN`] characters is stored and
/// reported back by the create (whose `Arns` elements carry no length of their own) and by
/// the association (whose `ResourceArn` carries the model's own length), and one past it is
/// refused 400 `ValidationException` by both, with nothing stored.
#[tokio::test]
async fn an_associated_resource_arn_is_bounded_by_the_length_the_model_publishes_for_it() {
    let harness = Harness::new();
    let benefit_id = text("seed", &harness.active_benefit().await, "Id");
    let max = MAX_ASSOCIATED_RESOURCE_ARN_LEN;

    // (case, the character the ARN's resource tail is built from, how many characters the
    // whole ARN carries, whether that is inside the published bound) — well inside, at the
    // bound in each character width, one past it in each width, and far past it.
    let rows: Vec<(&str, &str, usize, bool)> = vec![
        ("an ARN well inside the bound", "a", 64, true),
        ("an ascii ARN at the published bound", "a", max, true),
        ("a multi-byte ARN at the published bound", "é", max, true),
        (
            "an ascii ARN one character past the bound",
            "a",
            max + 1,
            false,
        ),
        (
            "a multi-byte ARN one character past the bound",
            "é",
            max + 1,
            false,
        ),
        ("an ARN far past the bound", "a", 10 * max, false),
    ];

    for (row, (case, unit, chars, inside)) in rows.iter().enumerate() {
        let arn = arn_of(unit, *chars);
        let counted = arn.chars().count();
        assert_eq!(
            counted, *chars,
            "case {case:?}: expected an ARN of {chars} characters, got {counted}"
        );

        // 1. the create, which supplies the whole list of a new record's ARNs.
        let listed_before = listed_ids(&harness, case).await;
        let (status, out) = harness
            .call(
                "CreateBenefitApplication",
                &create_with_resources(
                    &benefit_id,
                    &format!("tok-arn-create-{row}"),
                    std::slice::from_ref(&arn),
                ),
            )
            .await;
        if *inside {
            assert_eq!(
                status, 200,
                "case {case:?}: an ARN inside the bound must be created, got {status} {out:?}"
            );
            let stored = harness.get(case, &text(case, &out, "Id")).await;
            assert_eq!(
                associated(&stored),
                vec![arn.clone()],
                "case {case:?}: the created ARN must round-trip, got {stored:?}"
            );
        } else {
            assert_element_refusal(case, status, &out);
            assert_eq!(
                listed_ids(&harness, case).await,
                listed_before,
                "case {case:?}: a refused create must store no application"
            );
        }

        // 2. the association, which names ONE resource on a record that already exists:
        // the member the model publishes the bound on, so the same value is accepted and
        // refused there on the same line.
        let created = harness
            .create(
                case,
                &create_with_resources(&benefit_id, &format!("tok-arn-assoc-{row}"), &[]),
            )
            .await;
        let id = text(case, &created, "Id");
        let before = harness.get(case, &id).await;
        let (status, out) = harness
            .call(
                "AssociateBenefitApplicationResource",
                &resource_body(CATALOG, &id, &arn),
            )
            .await;
        if *inside {
            assert_eq!(
                status, 200,
                "case {case:?}: an ARN inside the bound must be associated, got {status} {out:?}"
            );
            let stored = harness.get(case, &id).await;
            assert_eq!(
                associated(&stored),
                vec![arn.clone()],
                "case {case:?}: the associated ARN must round-trip, got {stored:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400 past the published ARN bound, got {status} {out:?}"
            );
            assert_eq!(
                error_code(&out),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {out:?}"
            );
            let message = message_of(&out).unwrap_or_else(|| {
                panic!("case {case:?}: the refusal carries no Message: {out:?}")
            });
            assert_validation_invariants(case, &message);
            // The SAME requirement the create states for the same value, so one bound is
            // applied however the ARN was supplied.
            assert!(
                message.contains(&too_long(max)),
                "case {case:?}: expected the clause {:?}, got {message:?}",
                too_long(max)
            );
            let after = harness.get(case, &id).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused association must persist nothing, got {after:?}"
            );
        }
    }
}

/// The refusal of an over-long ARN supplied in a list names the COLLECTION member whichever
/// element carries it, no application is created — and a list whose every ARN is inside the
/// bound is stored in full, so the rows pin a bound rather than a blanket refusal.
#[tokio::test]
async fn the_refusal_of_an_over_long_arn_names_the_collection_member() {
    let harness = Harness::new();
    let benefit_id = text("seed", &harness.active_benefit().await, "Id");
    let max = MAX_ASSOCIATED_RESOURCE_ARN_LEN;

    let ok = arn_of("b", 48);
    let at_bound = arn_of("c", max);
    let over = arn_of("d", max + 1);

    // (case, the ARNs the create supplies) — the only element, the second of two, the
    // first of two, and the last of three.
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("the only resource", vec![over.clone()]),
        ("the second of two resources", vec![ok.clone(), over.clone()]),
        ("the first of two resources", vec![over.clone(), ok.clone()]),
        (
            "the third of three resources",
            vec![ok.clone(), at_bound.clone(), over.clone()],
        ),
    ];

    for (row, (case, resources)) in rows.iter().enumerate() {
        let token = format!("tok-arn-element-{row}");
        let listed_before = listed_ids(&harness, case).await;
        let (status, out) = harness
            .call(
                "CreateBenefitApplication",
                &create_with_resources(&benefit_id, &token, resources),
            )
            .await;
        assert_element_refusal(case, status, &out);
        assert_eq!(
            listed_ids(&harness, case).await,
            listed_before,
            "case {case:?}: a refused create must store no application"
        );

        // The refusal spent nothing: the same token, with every ARN inside the bound,
        // creates the application it would have created and stores both ARNs.
        let created = harness
            .create(
                case,
                &create_with_resources(&benefit_id, &token, &[ok.clone(), at_bound.clone()]),
            )
            .await;
        let stored = harness.get(case, &text(case, &created, "Id")).await;
        assert_eq!(
            associated(&stored),
            vec![ok.clone(), at_bound.clone()],
            "case {case:?}: every ARN inside the bound must be stored, got {stored:?}"
        );
    }
}
