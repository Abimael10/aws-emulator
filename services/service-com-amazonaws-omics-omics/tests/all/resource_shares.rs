// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the cross-account resource-share slice of this service.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. The HTTP harness is the one
//! `private_workflow_runs` owns; only the share helpers live here.
//!
//! The rules under test:
//!
//! * a share is a resource of its own, stored once: the offer `CreateShare` was asked for
//!   is what its own response and a later `GetShare` report, each member taken from that
//!   one record and nothing invented for a member the request left out;
//!   * a created share is `PENDING` — the offer waits for the subscriber to accept it, so
//!     nothing but a caller moves it;
//!   * the share names the resource it was created for and the account that owns it;
//! * a share is BACKED by a resource this emulator holds: an ARN naming no such resource
//!   is the modelled 404, and an ARN that names no shareable resource type at all is the
//!   modelled 400;
//! * one standing offer per (resource, subscriber) pair: a second is the modelled 409,
//!   and a share whose delete has been asked for no longer stands;
//! * `DeleteShare` moves the share to `DELETING` and the clock settles it to `DELETED`;
//!   a share that has already been asked to go away is the modelled 404, however it is
//!   reached, though the record itself stays readable with its terminal status.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::private_workflow_runs::{
    assert_absent, assert_members, assert_timestamp, call, emulator, error_code, get_workflow,
    workflow_for_runs, ACCOUNT, POLL_INTERVAL, REGION, SETTLE_DEADLINE,
};

/// Every value the modelled `ShareStatus` enum admits.
/// Source: `smithy_query(query="shape",shape="ShareStatus")` -> PENDING | ACTIVATING |
/// ACTIVE | DELETING | DELETED | FAILED.
const SHARE_STATUSES: &[&str] = &[
    "PENDING",
    "ACTIVATING",
    "ACTIVE",
    "DELETING",
    "DELETED",
    "FAILED",
];

/// The published upper bound of the modelled `ShareName` length (1..256).
/// Source: the `shareName` member of `CreateShareRequest`.
const SHARE_NAME_MAX: usize = 256;

/// The sentence the service answers a share operation with when the share the request
/// names is not one it can act on.
///
/// Source: the recorded `ResourceNotFoundException` of `DeleteShare` for an id no share
/// carries — fixed text that names the kind of resource requested and echoes nothing of
/// the request, so it is written out here rather than read from the emulator's own
/// constant.
const SHARE_NOT_FOUND_SENTENCE: &str = "Requested share not found";

/// Accounts a share can be offered to: twelve-digit ids (the account-id form
/// `service_common::arn::is_account_id` reads), none of them the account this emulator
/// serves (which is the resource owner and so cannot be a subscriber).
const SUBSCRIBERS: &[&str] = &["123456789012", "210987654321", "999999999999"];

/// The members `CreateShareResponse` does NOT model, so no create response may carry them
/// however much the request named: they are read through `GetShare`.
const NOT_ON_THE_CREATE_RESPONSE: &[&str] = &[
    "resourceArn",
    "resourceId",
    "principalSubscriber",
    "ownerId",
    "statusMessage",
    "creationTime",
    "updateTime",
];

/// `POST /share`.
async fn create_share(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/share", Some(body)).await
}

/// `GET /share/{shareId}`.
async fn get_share(router: &MultiServiceRouter, share_id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/share/{share_id}"), None).await
}

/// `DELETE /share/{shareId}`.
async fn delete_share(router: &MultiServiceRouter, share_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/share/{share_id}"), None).await
}

/// The `shareId` of a 200 create response, after asserting the status.
fn created_share_id(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
    body.get("shareId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: response carried no shareId, got {body:?}"))
        .to_string()
}

/// The `share` object of a 200 `GetShare`, after asserting the status.
async fn share_details(router: &MultiServiceRouter, case: &str, share_id: &str) -> Value {
    let (status, body) = get_share(router, share_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetShare, got {body:?}"
    );
    body.get("share")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetShare carried no share, got {body:?}"))
}

/// A workflow this emulator holds, and the ARN its own reader reports for it: the ARN a
/// share is created against is the one the service itself hands out, never one spelled by
/// hand here.
async fn shareable_workflow(router: &MultiServiceRouter, request_id: &str) -> (String, String) {
    let id = workflow_for_runs(router, request_id).await;
    let (status, body) = get_workflow(router, &id).await;
    assert_eq!(
        status, 200,
        "fixture {request_id:?}: expected 200 from GetWorkflow, got {body:?}"
    );
    let arn = body
        .get("arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {request_id:?}: workflow carried no arn, got {body:?}"))
        .to_string();
    (id, arn)
}

/// Read the share until it reports `expected`, and answer that `ShareDetails`.
///
/// Polling rather than sleeping a fixed span: the status is owed to the clock, so what is
/// asserted is that it arrives, not when a single read happened to land.
async fn await_share_status(router: &MultiServiceRouter, share_id: &str, expected: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let share = share_details(router, expected, share_id).await;
        if share.get("status").and_then(Value::as_str) == Some(expected) {
            return share;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "share {share_id}: expected status {expected} within {SETTLE_DEADLINE:?}, \
             last read {share:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `CreateShare` body for `resource_arn` and `subscriber`, naming `share_name` when the
/// row carries one.
fn share_body(resource_arn: &str, subscriber: &str, share_name: Option<&str>) -> Value {
    let mut body = Map::new();
    body.insert("resourceArn".to_string(), json!(resource_arn));
    body.insert("principalSubscriber".to_string(), json!(subscriber));
    if let Some(name) = share_name {
        body.insert("shareName".to_string(), json!(name));
    }
    Value::Object(body)
}

/// A row of the create/read-back table: which fixture resource is offered, to whom, and
/// under what name.
struct ShareCase {
    /// How the row is reported in a failure.
    case: String,
    /// Index into the fixture workflows the suite created.
    workflow: usize,
    subscriber: &'static str,
    share_name: Option<String>,
}

/// The table: one row per way a share can differ, so the rule is asserted over the class
/// and not over one request. The rows vary along the resource offered, the account it is
/// offered to, whether the offer is named, and the form and length of that name across the
/// modelled `ShareName` bounds.
fn share_cases() -> Vec<ShareCase> {
    let mut cases = vec![
        ShareCase {
            case: "only the required members".to_string(),
            workflow: 0,
            subscriber: SUBSCRIBERS[0],
            share_name: None,
        },
        ShareCase {
            case: "a named share".to_string(),
            workflow: 0,
            subscriber: SUBSCRIBERS[1],
            share_name: Some("autumn_cohort-share".to_string()),
        },
        ShareCase {
            // The modelled `ShareName` pattern `^[a-zA-Z0-9_-]+$` admits a single
            // character, which is also the published lower length bound.
            case: "a name at the lower length bound".to_string(),
            workflow: 1,
            subscriber: SUBSCRIBERS[0],
            share_name: Some("a".to_string()),
        },
        ShareCase {
            case: "a name at the upper length bound".to_string(),
            workflow: 1,
            subscriber: SUBSCRIBERS[1],
            share_name: Some("n".repeat(SHARE_NAME_MAX)),
        },
        ShareCase {
            // The same pattern admits digits, underscores and hyphens anywhere.
            case: "a name of punctuation and digits".to_string(),
            workflow: 1,
            subscriber: SUBSCRIBERS[2],
            share_name: Some("_-9-_".to_string()),
        },
    ];
    // The same resource offered to every account the catalog names: the subscriber is part
    // of what makes a share, so each row is a share of its own.
    for subscriber in SUBSCRIBERS {
        cases.push(ShareCase {
            case: format!("workflow 2 offered to {subscriber}"),
            workflow: 2,
            subscriber,
            share_name: Some(format!("share-for-{subscriber}")),
        });
    }
    cases
}

#[tokio::test]
async fn a_created_share_reads_back_from_state() {
    let router = emulator();
    let mut workflows = Vec::new();
    for n in 0..3 {
        workflows.push(shareable_workflow(&router, &format!("shared-{n}")).await);
    }
    let cases = share_cases();

    // Create them all first, so each read-back also proves the store keeps the rows apart
    // rather than answering with the most recent one.
    let mut created = Vec::new();
    let mut ids = BTreeSet::new();
    for share in &cases {
        let (workflow_id, workflow_arn) = &workflows[share.workflow];
        let body = share_body(workflow_arn, share.subscriber, share.share_name.as_deref());
        let (status, response) = create_share(&router, &body).await;
        let share_id = created_share_id(&share.case, status, &response);
        assert_eq!(
            response.get("status").and_then(Value::as_str),
            Some("PENDING"),
            "case {:?}: a created share is an offer awaiting acceptance, so it must report \
             PENDING, got {response:?}",
            share.case
        );
        assert_eq!(
            response.get("shareName").and_then(Value::as_str),
            share.share_name.as_deref(),
            "case {:?}: the create must echo the name it stored, got {response:?}",
            share.case
        );
        // The create models exactly three members; the rest are read through GetShare.
        assert_absent(&share.case, NOT_ON_THE_CREATE_RESPONSE, &response);
        assert!(
            ids.insert(share_id.clone()),
            "case {:?}: share id {share_id} was already handed out",
            share.case
        );
        created.push((share_id, workflow_id.clone(), workflow_arn.clone()));
    }

    for (share, (share_id, workflow_id, workflow_arn)) in cases.iter().zip(&created) {
        let details = share_details(&router, &share.case, share_id).await;
        let mut expected = json!({
            "shareId": share_id,
            "resourceArn": workflow_arn,
            // The resource's own id, read out of the ARN the share was created against.
            "resourceId": workflow_id,
            "principalSubscriber": share.subscriber,
            // The account this emulator serves owns every resource it holds, so it is the
            // owner that made the offer.
            "ownerId": ACCOUNT,
            "status": "PENDING",
        });
        if let Some(name) = &share.share_name {
            expected
                .as_object_mut()
                .expect("the expected members are an object")
                .insert("shareName".to_string(), json!(name));
        } else {
            assert_absent(&share.case, &["shareName"], &details);
        }
        assert_members(&share.case, "GetShare", &expected, &details);
        // Nothing is invented for a status the share has nothing to say about.
        assert_absent(&share.case, &["statusMessage"], &details);
        let created_at = assert_timestamp(&share.case, "creationTime", &details);
        let updated_at = assert_timestamp(&share.case, "updateTime", &details);
        assert_eq!(
            created_at, updated_at,
            "case {:?}: a share nothing has moved was last updated when it was created, \
             got {details:?}",
            share.case
        );
        let status = details.get("status").and_then(Value::as_str).unwrap_or("");
        assert!(
            SHARE_STATUSES.contains(&status),
            "case {:?}: expected a modelled ShareStatus, got {status:?}",
            share.case
        );
        // Reading does not move the share or restamp it: a second read is identical.
        let again = share_details(&router, &share.case, share_id).await;
        assert_eq!(
            again, details,
            "case {:?}: a second read must report the same record, got {again:?}",
            share.case
        );
    }
}

#[tokio::test]
async fn a_share_settles_to_deleted_on_the_clock() {
    let router = emulator();
    let (_, workflow_arn) = shareable_workflow(&router, "deleted-share").await;

    // The rule generalises over whether the offer was named and over which account it was
    // made to: a delete moves whatever share it names.
    let rows: Vec<(&str, &'static str, Option<&str>)> = vec![
        ("an unnamed share", SUBSCRIBERS[0], None),
        ("a named share", SUBSCRIBERS[1], Some("doomed-share")),
        ("a share named at the bound", SUBSCRIBERS[2], Some("d")),
    ];

    for (case, subscriber, share_name) in rows {
        let body = share_body(&workflow_arn, subscriber, share_name);
        let (status, response) = create_share(&router, &body).await;
        let share_id = created_share_id(case, status, &response);
        let created = share_details(&router, case, &share_id).await;
        let created_at = assert_timestamp(case, "creationTime", &created);

        let (status, response) = delete_share(&router, &share_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DeleteShare, got {response:?}"
        );
        assert_eq!(
            response.get("status").and_then(Value::as_str),
            Some("DELETING"),
            "case {case:?}: the delete reports the status it moved the share to, \
             got {response:?}"
        );

        // The DELETED the clock then owes it arrives without anything else being called.
        let settled = await_share_status(&router, &share_id, "DELETED").await;
        let updated_at = assert_timestamp(case, "updateTime", &settled);
        assert!(
            updated_at >= created_at,
            "case {case:?}: the delete must move updateTime no earlier than creationTime \
             ({created_at}), got {updated_at} in {settled:?}"
        );
        assert_members(
            case,
            "GetShare",
            &json!({ "shareId": share_id, "resourceArn": workflow_arn, "status": "DELETED" }),
            &settled,
        );

        // A share that has already been asked to go away is no longer one a delete can
        // act on, though the record itself stays readable with its terminal status.
        let (status, response) = delete_share(&router, &share_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from a second DeleteShare, got {response:?}"
        );
        assert_eq!(
            error_code(&response),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {response:?}"
        );
        assert_eq!(
            response.get("message").and_then(Value::as_str),
            Some(SHARE_NOT_FOUND_SENTENCE),
            "case {case:?}: a spent share is refused in the same words as one no share \
             carries, got {response:?}"
        );
        let after = share_details(&router, case, &share_id).await;
        assert_eq!(
            after.get("status").and_then(Value::as_str),
            Some("DELETED"),
            "case {case:?}: a refused delete must leave the record where it was, \
             got {after:?}"
        );

        // The offer no longer stands, so the same resource can be offered to the same
        // account again — as a share of its own.
        let (status, response) = create_share(&router, &body).await;
        let fresh = created_share_id(case, status, &response);
        assert_ne!(
            fresh, share_id,
            "case {case:?}: re-offering a deleted share must mint a new share, \
             got {response:?}"
        );
        assert_eq!(
            response.get("status").and_then(Value::as_str),
            Some("PENDING"),
            "case {case:?}: the re-offered share starts over as PENDING, got {response:?}"
        );
    }
}

#[tokio::test]
async fn an_unknown_share_is_not_found() {
    let router = emulator();
    let (workflow_id, workflow_arn) = shareable_workflow(&router, "the-only-share").await;
    // One real share exists, so the 404s below are about the id, not about an empty
    // registry.
    let (status, response) =
        create_share(&router, &share_body(&workflow_arn, SUBSCRIBERS[0], None)).await;
    let existing = created_share_id("the only share", status, &response);

    // The modelled `shareId` is a plain string with no pattern, so the rule generalises
    // over the shape of the id as well as its value: anything no share carries is not
    // found, including a well-formed id of another resource type.
    let ids = [
        "b3c1a8d0-0000-4000-8000-000000000000".to_string(),
        "0".to_string(),
        "not-a-share-id".to_string(),
        "9".repeat(64),
        workflow_id.clone(),
    ];
    for id in &ids {
        assert_ne!(id, &existing, "the fixture must not collide with the real id");
        for (operation, (status, body)) in [
            ("GetShare", get_share(&router, id).await),
            ("DeleteShare", delete_share(&router, id).await),
        ] {
            assert_eq!(
                status, 404,
                "case {id:?}: expected 404 from {operation}, got {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "ResourceNotFoundException",
                "case {id:?}: expected ResourceNotFoundException from {operation}, \
                 got {body:?}"
            );
            // The service words this refusal as FIXED text: it names the kind of
            // resource requested and echoes nothing of the request, so the same
            // sentence answers every id above and both operations.
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(SHARE_NOT_FOUND_SENTENCE),
                "case {id:?}: expected the service's share not-found sentence from \
                 {operation}, got {body:?}"
            );
            assert!(
                !body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains(id.as_str()),
                "case {id:?}: the refusal must not echo the requested id, got {body:?}"
            );
        }
    }

    // The real share is untouched by every refusal above.
    let details = share_details(&router, "the only share", &existing).await;
    assert_members(
        "the only share",
        "GetShare",
        &json!({ "shareId": existing, "status": "PENDING" }),
        &details,
    );
}

#[tokio::test]
async fn sharing_a_resource_this_emulator_does_not_hold_is_refused() {
    let router = emulator();
    let (workflow_id, workflow_arn) = shareable_workflow(&router, "held-workflow").await;
    let unheld_workflow = format!("arn:aws:omics:{REGION}:{ACCOUNT}:workflow/999999999999");

    // The shareable resource types are the three `CreateShare` documents — HealthOmics
    // variant stores, HealthOmics annotation stores and private workflows — so an ARN
    // naming one of them is "not found" when no such resource is held, while an ARN
    // naming anything else is refused as invalid.
    let not_found = [
        ("a workflow this emulator does not hold", unheld_workflow),
        (
            "a variant store, which nothing can create here yet",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:variantStore/cohort_variants"),
        ),
        (
            "an annotation store, likewise",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:annotationStore/cohort_annotations"),
        ),
    ];
    for (case, resource_arn) in &not_found {
        let (status, body) =
            create_share(&router, &share_body(resource_arn, SUBSCRIBERS[0], None)).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from CreateShare, got {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
    }

    let invalid = [
        (
            "a run, which is not a shareable resource type",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:run/1500000"),
        ),
        (
            "a sequence store, likewise",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:sequenceStore/1234567890"),
        ),
        (
            "another service's resource",
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:omics-emulator-bucket"),
        ),
        ("no ARN at all", workflow_id.clone()),
        (
            "an omics ARN with no resource path",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:"),
        ),
        (
            "a shareable type with no resource named",
            format!("arn:aws:omics:{REGION}:{ACCOUNT}:workflow/"),
        ),
    ];
    for (case, resource_arn) in &invalid {
        let (status, body) =
            create_share(&router, &share_body(resource_arn, SUBSCRIBERS[0], None)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateShare, got {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // The subscriber is the ID of an account OTHER than the resource owner, so the
    // owner's own account and anything that is not an account id are refused. The account
    // id form is twelve decimal digits (the modelled `AwsAccountId` of this service's own
    // `cacheBucketOwnerId`: len 12..12, pattern `^[0-9]+$`).
    for (case, subscriber) in [
        ("the owner's own account", ACCOUNT),
        ("an empty subscriber", ""),
        ("a blank subscriber", "   "),
        ("a subscriber too short to be an account", "12345678901"),
        ("a subscriber too long to be an account", "1234567890123"),
        ("a subscriber that is not digits", "not-an-account"),
        ("a subscriber that is an ARN", "arn:aws:iam::123456789012:root"),
        ("a subscriber padded with spaces", " 123456789012"),
    ] {
        let (status, body) = create_share(&router, &share_body(&workflow_arn, subscriber, None)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateShare, got {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }
}

#[tokio::test]
async fn one_standing_share_per_resource_and_subscriber() {
    let router = emulator();
    let (_, first_workflow) = shareable_workflow(&router, "conflict-one").await;
    let (_, second_workflow) = shareable_workflow(&router, "conflict-two").await;

    let body = share_body(&first_workflow, SUBSCRIBERS[0], Some("the-first-offer"));
    let (status, response) = create_share(&router, &body).await;
    let share_id = created_share_id("the first offer", status, &response);

    // The rule generalises over everything the second request may differ in EXCEPT the
    // pair that makes the offer: the resource and the account it is offered to.
    let repeats = [
        ("the identical request", body.clone()),
        (
            "the same pair under another name",
            share_body(&first_workflow, SUBSCRIBERS[0], Some("another-name")),
        ),
        (
            "the same pair with no name at all",
            share_body(&first_workflow, SUBSCRIBERS[0], None),
        ),
    ];
    for (case, repeat) in &repeats {
        let (status, body) = create_share(&router, repeat).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected 409 from CreateShare, got {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {body:?}"
        );
    }

    // Changing either half of the pair is a share of its own, not a conflict.
    let distinct = [
        (
            "the same resource to another account",
            share_body(&first_workflow, SUBSCRIBERS[1], None),
        ),
        (
            "another resource to the same account",
            share_body(&second_workflow, SUBSCRIBERS[0], None),
        ),
    ];
    let mut ids = BTreeSet::from([share_id]);
    for (case, request) in &distinct {
        let (status, body) = create_share(&router, request).await;
        let id = created_share_id(case, status, &body);
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: share id {id} was already handed out"
        );
    }
}
