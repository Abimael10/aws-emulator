// SPDX-License-Identifier: CC0-1.0

//! `AssociateBenefitApplicationResource`, end to end over the wire.
//!
//! Every assertion goes through the real request path — the shared `Harness` of
//! `tests/all/benefit_application_lifecycle.rs` (`MultiServiceRouter::dispatch` with
//! `awsJson1_0` bodies, `POST /` and `X-Amz-Target:
//! PartnerCentralBenefitsService.<Operation>`) — and every effect is read back through
//! `GetBenefitApplication`, so what these tests pin is the emulator's HTTP behaviour on
//! the persisted record.
//!
//! The rule under test: an association adds exactly the `ResourceArn` it names to the
//! stored application's `AssociatedResources`, keeps every association the record already
//! carried, and advances the application's revision and `UpdatedAt` only when the record
//! actually changed — so a resource the application already carries is a success that
//! changes nothing, and the call is idempotent. The application must exist (404
//! `ResourceNotFoundException`) and must be one AWS has not finished with (409
//! `ConflictException` from a terminal status), and nothing is persisted on either refusal.
//!
//! The tables' rows differ along what the rule generalises over: how many resources the
//! application already carries, whether the named one is among them (and in which
//! position), which ARN form and which ARN length the resource has, which identifier form
//! names the application, which catalog it lives in, and which of the six modelled statuses
//! it is in.
//!
//! The last table states the rule the bound on an application's association list is refused
//! under: the refusal names an input member of the request as it was SENT — the
//! `ResourceArn` the request carried the resource on, the one member of the three a caller
//! can act on — and not the stored list the bound is published on, which is a member of the
//! create's input and the get's output and of no request this operation serves.

use serde_json::{json, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::ops::types::BENEFIT_APPLICATION_STATUS_VALUES;
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    MAX_ASSOCIATED_RESOURCES_PER_APPLICATION, SEEDED_CATALOGS, STATUS_ACTION_REQUIRED,
    STATUS_IN_REVIEW, STATUS_PENDING_SUBMISSION, TERMINAL_STATUSES, VALIDATION_CODE_INVALID_VALUE,
    VALIDATION_REASON_FIELD,
};

use crate::application_status_gates::reach;
use crate::benefit_application_lifecycle::{error_code, instant, text, Harness, CATALOG};
use crate::disassociate_benefit_application_resource::{
    associated, body, seed, RESOURCE_ARNS, UNKNOWN_APPLICATION,
};
use crate::refusal_member_path::walk_spelling;
use crate::service_error_rules::{assert_validation_invariants, clause, message_of, one};

/// The longest `ResourceArn` the model admits: the member's `@length` is `min: 0, max:
/// 1600` (`smithy_query(query="operation",operation="AssociateBenefitApplicationResource")`),
/// so this ARN is exactly 1600 characters and is accepted, while [`too_long_arn`] is one
/// character past the bound and refused by the centralized walk.
fn longest_arn() -> String {
    let prefix = "arn:aws:s3:::";
    format!("{prefix}{}", "a".repeat(1600 - prefix.len()))
}

/// One character past the modelled `@length` bound above.
fn too_long_arn() -> String {
    format!("{}a", longest_arn())
}

/// An association adds exactly the named resource, keeps the ones already stored, and
/// moves the revision and `UpdatedAt` only when the association set changed.
///
/// The rows span an empty starting set, sets of one and of several, a resource already
/// stored (first, middle, last, duplicated), every ARN form the modelled pattern admits,
/// an ARN at the modelled length bound, and an ARN differing from a stored one only in its
/// region — and each row's call is repeated, so idempotence is asserted on every one of
/// them.
#[tokio::test]
async fn an_association_adds_exactly_the_named_resource() {
    let harness = Harness::new();
    let [bucket, instance, role, function, access_point, cell] = RESOURCE_ARNS.map(str::to_string);
    let long = longest_arn();

    // (case, the associations the create stores, the ARN associated, what must be stored
    // afterwards — in the order the service reports it)
    let rows: Vec<(&str, Vec<String>, String, Vec<String>)> = vec![
        (
            "the first association of a record carrying none",
            vec![],
            bucket.clone(),
            vec![bucket.clone()],
        ),
        (
            "a second, different resource",
            vec![bucket.clone()],
            instance.clone(),
            vec![bucket.clone(), instance.clone()],
        ),
        (
            "a fourth resource on a record carrying three",
            vec![bucket.clone(), instance.clone(), role.clone()],
            function.clone(),
            vec![
                bucket.clone(),
                instance.clone(),
                role.clone(),
                function.clone(),
            ],
        ),
        (
            "the resource the record already carries alone",
            vec![bucket.clone()],
            bucket.clone(),
            vec![bucket.clone()],
        ),
        (
            "the first of three the record already carries",
            vec![bucket.clone(), instance.clone(), role.clone()],
            bucket.clone(),
            vec![bucket.clone(), instance.clone(), role.clone()],
        ),
        (
            "the middle of three the record already carries",
            vec![bucket.clone(), instance.clone(), role.clone()],
            instance.clone(),
            vec![bucket.clone(), instance.clone(), role.clone()],
        ),
        (
            "the last of three the record already carries",
            vec![bucket.clone(), instance.clone(), role.clone()],
            role.clone(),
            vec![bucket.clone(), instance.clone(), role.clone()],
        ),
        (
            // The create stores its association list as a set, so a create naming one
            // resource twice linked the application to it once; associating that same
            // resource then stores nothing either.
            "a resource the create named twice",
            vec![bucket.clone(), bucket.clone()],
            bucket.clone(),
            vec![bucket.clone()],
        ),
        (
            "a record already carrying every other ARN form the pattern admits",
            vec![
                bucket.clone(),
                instance.clone(),
                role.clone(),
                function.clone(),
                cell.clone(),
            ],
            access_point.clone(),
            vec![
                bucket.clone(),
                instance.clone(),
                role.clone(),
                function.clone(),
                cell.clone(),
                access_point.clone(),
            ],
        ),
        (
            // Exact match only: an ARN naming the same resource in another region is
            // another resource, so it is added beside the one already stored.
            "an ARN differing from a stored one only in its region",
            vec![instance.clone()],
            "arn:aws:ec2:us-east-1:000000000000:instance/i-0123456789abcdef0".to_string(),
            vec![
                instance.clone(),
                "arn:aws:ec2:us-east-1:000000000000:instance/i-0123456789abcdef0".to_string(),
            ],
        ),
        (
            "an ARN at the modelled length bound",
            vec![bucket.clone()],
            long.clone(),
            vec![bucket.clone(), long.clone()],
        ),
    ];

    for (row, (case, stored, added, expected)) in rows.iter().enumerate() {
        let initial: Vec<&str> = stored.iter().map(String::as_str).collect();
        let id = seed(
            &harness,
            case,
            CATALOG,
            &format!("tok-associate-{row}"),
            &initial,
        )
        .await;
        let before = harness.get_in(case, CATALOG, &id).await;
        let changed = associated(&before) != *expected;

        let out = harness
            .ok(
                case,
                "AssociateBenefitApplicationResource",
                &body(CATALOG, &id, added),
            )
            .await;
        assert_eq!(
            text(case, &out, "Id"),
            id,
            "case {case:?}: expected the application's own id, got {out:?}"
        );
        assert_eq!(
            text(case, &out, "Arn"),
            text(case, &before, "Arn"),
            "case {case:?}: expected the application's stored ARN, got {out:?}"
        );

        let after = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            associated(&after),
            *expected,
            "case {case:?}: expected the associations {expected:?}, got {after:?}"
        );
        assert_eq!(
            text(case, &out, "Revision"),
            text(case, &after, "Revision"),
            "case {case:?}: the response's revision must be the stored one, got {out:?}"
        );
        let expected_revision = if changed { "2" } else { "1" };
        assert_eq!(
            text(case, &after, "Revision"),
            expected_revision,
            "case {case:?}: expected revision {expected_revision}, got {after:?}"
        );
        if changed {
            assert!(
                instant(case, &after, "UpdatedAt") > instant(case, &after, "CreatedAt"),
                "case {case:?}: an added association must advance UpdatedAt, got {after:?}"
            );
        } else {
            assert_eq!(
                instant(case, &after, "UpdatedAt"),
                instant(case, &before, "UpdatedAt"),
                "case {case:?}: adding nothing must leave UpdatedAt, got {after:?}"
            );
        }
        // Nothing else about the record moved.
        for member in [
            "Id",
            "Arn",
            "Catalog",
            "BenefitId",
            "Name",
            "Description",
            "Status",
        ] {
            assert_eq!(
                after[member], before[member],
                "case {case:?}: {member} must not change, got {after:?}"
            );
        }

        // Repeating the same call adds nothing a second time and reports the same
        // revision: the association is idempotent.
        let repeat = harness
            .ok(
                case,
                "AssociateBenefitApplicationResource",
                &body(CATALOG, &id, added),
            )
            .await;
        assert_eq!(
            text(case, &repeat, "Revision"),
            text(case, &after, "Revision"),
            "case {case:?}: a repeated association must not move the revision, got {repeat:?}"
        );
        let twice = harness.get_in(case, CATALOG, &id).await;
        assert_eq!(
            twice, after,
            "case {case:?}: a repeated association must change nothing, got {twice:?}"
        );
    }
}

/// An association is the inverse of a disassociation: the resource a disassociation
/// removed can be associated again, and the association a request added can be removed,
/// each step observed on the stored record.
#[tokio::test]
async fn an_association_and_a_disassociation_are_inverses() {
    let harness = Harness::new();
    let [bucket, instance, ..] = RESOURCE_ARNS;
    let case = "the association round trip";

    let id = seed(&harness, case, CATALOG, "tok-associate-roundtrip", &[bucket]).await;

    harness
        .ok(
            case,
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, instance),
        )
        .await;
    let both = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        associated(&both),
        vec![bucket.to_string(), instance.to_string()],
        "case {case:?}: expected both resources after the association, got {both:?}"
    );

    harness
        .ok(
            case,
            "DisassociateBenefitApplicationResource",
            &body(CATALOG, &id, instance),
        )
        .await;
    let removed = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        associated(&removed),
        vec![bucket.to_string()],
        "case {case:?}: expected the removal to undo the association, got {removed:?}"
    );

    harness
        .ok(
            case,
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, instance),
        )
        .await;
    let again = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        associated(&again),
        vec![bucket.to_string(), instance.to_string()],
        "case {case:?}: a removed resource must be associable again, got {again:?}"
    );
    // Each of the three accepted writes moved the revision once, from the create's 1.
    assert_eq!(
        text(case, &again, "Revision"),
        "4",
        "case {case:?}: expected three content changes on top of the create, got {again:?}"
    );
}

/// The application may be named by either modelled `BenefitApplicationIdentifier` form —
/// its id or its ARN — in either seeded catalog, and the answer names the application's
/// own id and ARN whichever form was sent.
#[tokio::test]
async fn an_association_accepts_every_identifier_form_in_every_catalog() {
    let harness = Harness::new();
    let [bucket, instance, ..] = RESOURCE_ARNS;

    let mut row = 0usize;
    for catalog in SEEDED_CATALOGS {
        for form in ["the id", "the ARN"] {
            let case = format!("{form} in {catalog}");
            let id = seed(
                &harness,
                &case,
                catalog,
                &format!("tok-associate-form-{row}"),
                &[bucket],
            )
            .await;
            row += 1;
            let created = harness.get_in(&case, catalog, &id).await;
            let arn = text(&case, &created, "Arn");
            let identifier = if form == "the id" {
                id.clone()
            } else {
                arn.clone()
            };

            let out = harness
                .ok(
                    &case,
                    "AssociateBenefitApplicationResource",
                    &body(catalog, &identifier, instance),
                )
                .await;
            assert_eq!(
                text(&case, &out, "Id"),
                id,
                "case {case:?}: expected the application's id, got {out:?}"
            );
            assert_eq!(
                text(&case, &out, "Arn"),
                arn,
                "case {case:?}: expected the application's ARN, got {out:?}"
            );

            let after = harness.get_in(&case, catalog, &id).await;
            assert_eq!(
                associated(&after),
                vec![bucket.to_string(), instance.to_string()],
                "case {case:?}: expected the stored resource and the added one, got {after:?}"
            );
            assert_eq!(
                text(&case, &after, "Revision"),
                "2",
                "case {case:?}: an accepted association must bump the revision, got {after:?}"
            );
        }
    }
}

/// An association naming no stored application is refused with a 404
/// `ResourceNotFoundException`.
///
/// The rows span the ways the lookup can fail: an id of the modelled form that nothing
/// holds, that same id's ARN, the ARN of an application of another catalog, and a stored
/// application named through a catalog the service does not serve.
#[tokio::test]
async fn an_association_with_an_application_that_does_not_exist_is_a_404() {
    let harness = Harness::new();
    let [bucket, instance, ..] = RESOURCE_ARNS;

    let id = seed(
        &harness,
        "the stored application",
        CATALOG,
        "tok-associate-404",
        &[bucket],
    )
    .await;
    let stored = harness.get_in("the stored application", CATALOG, &id).await;
    let arn = text("the stored application", &stored, "Arn");
    let other_catalog = SEEDED_CATALOGS
        .iter()
        .find(|catalog| **catalog != CATALOG)
        .expect("the service seeds more than one catalog");

    let rows: Vec<(String, &str, String)> = vec![
        (
            "an id nothing holds".to_string(),
            CATALOG,
            UNKNOWN_APPLICATION.to_string(),
        ),
        (
            "the ARN of an id nothing holds".to_string(),
            CATALOG,
            arn.replace(&id, UNKNOWN_APPLICATION),
        ),
        (
            "a stored application named through another catalog".to_string(),
            other_catalog,
            id.clone(),
        ),
        (
            "a stored application named through a catalog the service does not serve".to_string(),
            "no-such-catalog",
            id.clone(),
        ),
        (
            "the stored ARN named through another catalog".to_string(),
            other_catalog,
            arn.clone(),
        ),
    ];

    for (case, catalog, identifier) in &rows {
        let (http, out) = harness
            .call(
                "AssociateBenefitApplicationResource",
                &body(catalog, identifier, instance),
            )
            .await;
        assert_eq!(
            http, 404,
            "case {case:?}: expected 404, got {http} with {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {out:?}"
        );
    }

    // And the stored application gained no association through any refusal.
    let after = harness.get_in("the stored application", CATALOG, &id).await;
    assert_eq!(
        after, stored,
        "case \"the stored application\": a refused association must persist nothing, got {after:?}"
    );
}

/// A `ResourceArn` past the modelled `@length` bound is refused by the centralized walk
/// with a 400 `ValidationException`, and nothing is persisted — while the ARN exactly at
/// the bound is accepted (asserted in the table above).
#[tokio::test]
async fn an_association_whose_resource_arn_is_too_long_is_a_400() {
    let harness = Harness::new();
    let [bucket, ..] = RESOURCE_ARNS;
    let case = "a ResourceArn one character past the modelled bound";

    let id = seed(&harness, case, CATALOG, "tok-associate-length", &[bucket]).await;
    let before = harness.get_in(case, CATALOG, &id).await;

    let (http, out) = harness
        .call(
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, &too_long_arn()),
        )
        .await;
    assert_eq!(
        http, 400,
        "case {case:?}: expected 400, got {http} with {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {out:?}"
    );
    let after = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        after, before,
        "case {case:?}: a refused association must persist nothing, got {after:?}"
    );
}

/// An association past the store's bound on one application's association list is refused
/// with a 400 `ValidationException`, and nothing is persisted — while the association that
/// fills the list exactly to the bound is accepted.
///
/// The bound is the service's own `MAX_ASSOCIATED_RESOURCES_PER_APPLICATION`, read from the
/// crate so this suite cannot drift from it; the rows are the association that reaches it
/// and the one past it.
#[tokio::test]
async fn an_association_past_the_stores_bound_is_a_400() {
    let harness = Harness::new();
    let case = "the association that fills the list";
    // One distinct ARN per slot, one short of the bound, so the next association reaches it
    // exactly and the one after is past it.
    let filled: Vec<String> = (0..MAX_ASSOCIATED_RESOURCES_PER_APPLICATION - 1)
        .map(|slot| format!("arn:aws:s3:::partner-bucket-{slot}"))
        .collect();
    let stored: Vec<&str> = filled.iter().map(String::as_str).collect();
    let id = seed(&harness, case, CATALOG, "tok-associate-bound", &stored).await;

    let last = "arn:aws:s3:::partner-bucket-last";
    harness
        .ok(
            case,
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, last),
        )
        .await;
    let full = harness.get_in(case, CATALOG, &id).await;
    assert_eq!(
        associated(&full).len(),
        MAX_ASSOCIATED_RESOURCES_PER_APPLICATION,
        "case {case:?}: expected the list filled to the bound, got {full:?}"
    );

    let past = "the association past the bound";
    let (http, out) = harness
        .call(
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, "arn:aws:s3:::partner-bucket-past"),
        )
        .await;
    assert_eq!(
        http, 400,
        "case {past:?}: expected 400, got {http} with {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {past:?}: expected ValidationException, got {out:?}"
    );
    let message = text(past, &out, "Message");
    assert!(
        message.contains(&MAX_ASSOCIATED_RESOURCES_PER_APPLICATION.to_string()),
        "case {past:?}: the refusal must state the bound, got {message:?}"
    );
    assert!(
        !message.contains("partner-bucket-past"),
        "case {past:?}: the refusal must not echo the rejected ARN, got {message:?}"
    );
    let after = harness.get_in(past, CATALOG, &id).await;
    assert_eq!(
        after, full,
        "case {past:?}: a refused association must persist nothing, got {after:?}"
    );

    // A resource the full record already carries is not growth, so it is still a success
    // that changes nothing.
    let repeat = harness
        .ok(
            "an association of a resource the full record carries",
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, last),
        )
        .await;
    assert_eq!(
        text(past, &repeat, "Revision"),
        text(past, &full, "Revision"),
        "case {past:?}: a resource already carried must not move the revision, got {repeat:?}"
    );
}

/// An association is accepted while AWS has not finished with the application, and refused
/// with a 409 `ConflictException` once its status is terminal — with nothing persisted.
///
/// The rows are every modelled `BenefitApplicationStatus`
/// (`BENEFIT_APPLICATION_STATUS_VALUES`, so the set cannot drift from the model), each
/// naming a different ARN form so the gate is not specific to one resource.
#[tokio::test]
async fn an_association_is_refused_once_the_application_is_terminal() {
    let harness = Harness::new();

    for (row, status) in BENEFIT_APPLICATION_STATUS_VALUES.iter().enumerate() {
        let added = RESOURCE_ARNS[row % RESOURCE_ARNS.len()];
        let stored = RESOURCE_ARNS[(row + 1) % RESOURCE_ARNS.len()];
        let case = format!("{status}, associating {added}");
        let id = seed(
            &harness,
            &case,
            CATALOG,
            &format!("tok-associate-gate-{row}"),
            &[stored],
        )
        .await;
        reach(&harness, &case, CATALOG, &id, status).await;
        let before = harness.get_in(&case, CATALOG, &id).await;

        if TERMINAL_STATUSES.contains(status) {
            let (http, out) = harness
                .call(
                    "AssociateBenefitApplicationResource",
                    &body(CATALOG, &id, added),
                )
                .await;
            assert_eq!(
                http, 409,
                "case {case:?}: expected 409 from a {status} application, got {http} {out:?}"
            );
            assert_eq!(
                error_code(&out),
                "ConflictException",
                "case {case:?}: expected ConflictException, got {out:?}"
            );
            let after = harness.get_in(&case, CATALOG, &id).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused association must persist nothing, got {after:?}"
            );
            continue;
        }

        // Not terminal: tracking the application's resources is what this operation is
        // for, so the association applies and the read reports it, without moving the
        // status.
        harness
            .ok(
                &case,
                "AssociateBenefitApplicationResource",
                &body(CATALOG, &id, added),
            )
            .await;
        let after = harness.get_in(&case, CATALOG, &id).await;
        assert_eq!(
            associated(&after),
            vec![stored.to_string(), added.to_string()],
            "case {case:?}: expected the stored resource and the added one, got {after:?}"
        );
        assert_ne!(
            text(&case, &after, "Revision"),
            text(&case, &before, "Revision"),
            "case {case:?}: an accepted association must bump the revision, got {after:?}"
        );
        assert_eq!(
            after["Status"],
            json!(status),
            "case {case:?}: an association must not move the status, got {after:?}"
        );
    }
}

/// The association is observable through the list filter too: an application is reported
/// for the resource it was associated with, and still for the one it already carried.
#[tokio::test]
async fn an_associated_resource_matches_the_list_filter() {
    let harness = Harness::new();
    let [bucket, instance, ..] = RESOURCE_ARNS;
    let case = "the list filter after an association";

    let id = seed(&harness, case, CATALOG, "tok-associate-filter", &[bucket]).await;

    let matching = |out: &Value| -> bool {
        out["BenefitApplicationSummaries"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|summary| summary["Id"] == json!(id))
    };

    let before = harness
        .ok(
            case,
            "ListBenefitApplications",
            &json!({ "Catalog": CATALOG, "AssociatedResourceArns": [instance] }),
        )
        .await;
    assert!(
        !matching(&before),
        "case {case:?}: the application must not match a resource it does not carry, got {before:?}"
    );

    harness
        .ok(
            case,
            "AssociateBenefitApplicationResource",
            &body(CATALOG, &id, instance),
        )
        .await;

    for arn in [bucket, instance] {
        let out = harness
            .ok(
                case,
                "ListBenefitApplications",
                &json!({ "Catalog": CATALOG, "AssociatedResourceArns": [arn] }),
            )
            .await;
        assert!(
            matching(&out),
            "case {case:?}: the application must match the filter {arn:?}, got {out:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The member the association-bound refusal names
// ---------------------------------------------------------------------------

/// The members this operation BINDS, as the wire spells them
/// (`smithy_query(query="operation",operation="AssociateBenefitApplicationResource")`:
/// `Catalog`, `BenefitApplicationIdentifier` and `ResourceArn`, and nothing else).
///
/// Written out here rather than read from the service, so a refusal cannot adopt a member
/// this request has no slot for and still pass.
const BOUND_MEMBERS: [&str; 3] = ["Catalog", "BenefitApplicationIdentifier", "ResourceArn"];

/// The list the bound is published on, which this operation binds NO member for: it is a
/// member of `CreateBenefitApplicationInput` and of `GetBenefitApplicationOutput`
/// (`smithy_query(query="shape",shape="CreateBenefitApplicationInput")`), so no refusal of
/// this request may name it, in either case the two shapes spell it.
const UNBOUND_LIST_MEMBER: &str = "AssociatedResources";

/// The requirement the refusal states, written out here rather than read from the service so
/// the words themselves are under test: what the bound on the stored list requires of the
/// member this request carries a resource on.
fn bound_requirement() -> String {
    format!(
        "Member must not take the benefit application past \
         {MAX_ASSOCIATED_RESOURCES_PER_APPLICATION} associated resources"
    )
}

/// One ARN of the record a row seeds: a form carrying a region and an account, so a row can
/// name a resource differing from a stored one only in its region.
fn stored_arn(tag: &str, slot: usize) -> String {
    format!("arn:aws:s3:us-west-2:000000000000:accesspoint/{tag}-{slot}")
}

/// How a record reached the bound.
enum Filled {
    /// The create stored every one of its resources.
    AtCreate,
    /// The create stored all but this many, and that many associations grew the list the
    /// rest of the way — so the refused call is refused on a list this operation itself
    /// filled.
    GrownByAssociations(usize),
}

/// One refused association: the record the row seeds, and the request made against it.
struct BoundRow {
    case: &'static str,
    /// What the row's own stored ARNs are named after, so each row's record is distinct.
    tag: &'static str,
    catalog: &'static str,
    filled: Filled,
    /// The status the record is in when the association is refused. Only a NON-terminal
    /// one: a terminal record is refused by the status gate above, before the bound.
    status: &'static str,
    /// Whether the request names the application by its ARN rather than by its id.
    by_arn: bool,
    /// The resource the refused request names, which the record does not already carry.
    refused_arn: String,
}

/// An association past the bound is refused about the member the REQUEST carried the
/// resource on — `ResourceArn`, in the path the centralized constraint walk reports for an
/// input member — and never about a member this operation binds no slot for.
///
/// The rows differ along what the rule generalises over: how the record reached the bound
/// (filled by the create, grown by associations, and in different proportions), which
/// catalog it lives in, which non-terminal status it is in, which identifier form names it,
/// and which ARN form and length the refused resource has — including the longest ARN the
/// model admits and one differing from a stored resource only in its region.
#[tokio::test]
async fn the_association_bound_refusal_names_the_member_the_request_sent() {
    let harness = Harness::new();
    let other_catalog = SEEDED_CATALOGS
        .iter()
        .copied()
        .find(|catalog| *catalog != CATALOG)
        .expect("the service seeds more than one catalog");
    let requirement = bound_requirement();

    let rows = vec![
        BoundRow {
            case: "a record the create filled, refused an ARN with no region or account",
            tag: "at-create",
            catalog: CATALOG,
            filled: Filled::AtCreate,
            status: STATUS_PENDING_SUBMISSION,
            by_arn: false,
            refused_arn: "arn:aws:s3:::assoc-bound-past".to_string(),
        },
        BoundRow {
            case: "a record associations grew to the bound, under review",
            tag: "grown",
            catalog: CATALOG,
            filled: Filled::GrownByAssociations(4),
            status: STATUS_IN_REVIEW,
            by_arn: false,
            refused_arn: RESOURCE_ARNS[1].to_string(),
        },
        BoundRow {
            case: "a record of another catalog, named by its ARN",
            tag: "other-catalog",
            catalog: other_catalog,
            filled: Filled::AtCreate,
            status: STATUS_PENDING_SUBMISSION,
            by_arn: true,
            refused_arn: RESOURCE_ARNS[4].to_string(),
        },
        BoundRow {
            case: "a record awaiting the partner, refused the longest ARN the model admits",
            tag: "longest",
            catalog: CATALOG,
            filled: Filled::GrownByAssociations(MAX_ASSOCIATED_RESOURCES_PER_APPLICATION - 1),
            status: STATUS_ACTION_REQUIRED,
            by_arn: true,
            refused_arn: longest_arn(),
        },
        BoundRow {
            case: "a resource differing from a stored one only in its region",
            tag: "twin",
            catalog: CATALOG,
            filled: Filled::AtCreate,
            status: STATUS_PENDING_SUBMISSION,
            by_arn: false,
            refused_arn: "arn:aws:s3:eu-west-1:000000000000:accesspoint/twin-0".to_string(),
        },
    ];

    // The table spans both ways a record reaches the bound, so it cannot shrink to the one
    // the create produces.
    for grown in [false, true] {
        assert!(
            rows.iter()
                .any(|row| matches!(row.filled, Filled::GrownByAssociations(_)) == grown),
            "case \"table\": expected a row whose record was grown by associations: {grown}"
        );
    }
    // And no row may name a status the status gate refuses first, which would never reach
    // the bound at all.
    for row in &rows {
        assert!(
            !TERMINAL_STATUSES.contains(&row.status),
            "case {:?}: a terminal status is refused before the bound, got {:?}",
            row.case,
            row.status
        );
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let every_slot: Vec<String> = (0..MAX_ASSOCIATED_RESOURCES_PER_APPLICATION)
            .map(|slot| stored_arn(row.tag, slot))
            .collect();
        let grown = match row.filled {
            Filled::AtCreate => 0,
            Filled::GrownByAssociations(grown) => grown,
        };
        let at_create: Vec<&str> = every_slot[..every_slot.len() - grown]
            .iter()
            .map(String::as_str)
            .collect();
        let id = seed(
            &harness,
            case,
            row.catalog,
            &format!("tok-assoc-bound-member-{index}"),
            &at_create,
        )
        .await;
        for arn in &every_slot[every_slot.len() - grown..] {
            harness
                .ok(
                    case,
                    "AssociateBenefitApplicationResource",
                    &body(row.catalog, &id, arn),
                )
                .await;
        }
        reach(&harness, case, row.catalog, &id, row.status).await;

        let before = harness.get_in(case, row.catalog, &id).await;
        assert_eq!(
            associated(&before).len(),
            MAX_ASSOCIATED_RESOURCES_PER_APPLICATION,
            "case {case:?}: expected the record filled to the bound, got {before:?}"
        );
        assert!(
            !associated(&before).contains(&row.refused_arn),
            "case {case:?}: the refused resource must not be one the record carries, \
             got {before:?}"
        );

        let identifier = if row.by_arn {
            text(case, &before, "Arn")
        } else {
            id.clone()
        };
        let request = body(row.catalog, &identifier, &row.refused_arn);
        let (status, out) = harness
            .call("AssociateBenefitApplicationResource", &request)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 past the bound, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );

        // The envelope over the clause for the member the request carried the resource on.
        let message = message_of(&out)
            .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no Message: {out:?}"));
        assert_eq!(
            message,
            one(&clause(&walk_spelling("ResourceArn"), &requirement)),
            "case {case:?}: expected the clause for the resource member, got {message:?}"
        );
        assert_validation_invariants(case, &message);
        assert_eq!(
            out["Reason"],
            json!(VALIDATION_REASON_FIELD),
            "case {case:?}: expected the field-validation reason, got {out:?}"
        );

        // The one-member field list names the same member, with the same requirement.
        let fields = out["FieldList"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no FieldList: {out:?}"));
        assert_eq!(
            fields.len(),
            1,
            "case {case:?}: expected one refused member, got {out:?}"
        );
        let named = text(case, &fields[0], "Name");
        assert_eq!(
            named,
            walk_spelling("ResourceArn"),
            "case {case:?}: expected the resource member to be named, got {out:?}"
        );
        assert_eq!(
            text(case, &fields[0], "Message"),
            requirement,
            "case {case:?}: expected the field to state the requirement, got {out:?}"
        );
        assert_eq!(
            fields[0]["Code"],
            json!(VALIDATION_CODE_INVALID_VALUE),
            "case {case:?}: expected the invalid-value code, got {out:?}"
        );

        // The member named is one the REQUEST sent, which is what a caller can correct:
        // every member of the body, in the walk's spelling, and no other.
        let sent: Vec<String> = request
            .as_object()
            .expect("the request body is an object")
            .keys()
            .map(|member| walk_spelling(member))
            .collect();
        assert!(
            sent.contains(&named),
            "case {case:?}: the refusal names {named:?}, which the request did not send: \
             {sent:?}"
        );
        for member in BOUND_MEMBERS {
            assert!(
                sent.contains(&walk_spelling(member)),
                "case {case:?}: the request must carry every member this operation binds, \
                 missing {member:?} from {sent:?}"
            );
        }
        // And the list the bound is published on is a member of other shapes only, so
        // neither half of the refusal may name it.
        for spelling in [
            UNBOUND_LIST_MEMBER.to_string(),
            walk_spelling(UNBOUND_LIST_MEMBER),
        ] {
            assert!(
                !message.contains(&spelling),
                "case {case:?}: the message names {spelling:?}, which this request has no \
                 slot for: {message:?}"
            );
            assert!(
                !named.contains(&spelling),
                "case {case:?}: the field list names {spelling:?}, which this request has no \
                 slot for: {out:?}"
            );
        }
        // Nothing of the resource that would not fit is echoed, and nothing is persisted.
        assert!(
            !message.contains(&row.refused_arn),
            "case {case:?}: the refusal echoes the rejected ARN, got {message:?}"
        );
        let after = harness.get_in(case, row.catalog, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused association must persist nothing, got {after:?}"
        );
    }
}
