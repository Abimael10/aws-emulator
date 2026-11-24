// SPDX-License-Identifier: CC0-1.0

//! ONE refusal for the billing view a request identifies by `arn` when nothing
//! is stored under it, end to end over the awsJson1_0 wire.
//!
//! The rule: an operation that resolves the billing view its own request
//! identifies by `arn` refuses an ARN of the caller's account that no stored
//! view answers to with the modeled `ResourceNotFoundException` (404) carrying
//! this service's ARN-validation sentence over the requested ARN, the ARN as
//! `resourceId` and the billing-view `resourceType` — so which of those
//! operations was asked cannot change the answer to the same ARN.
//!
//! Sources (the model, not a recording): `smithy_query(query="operation",
//! operation="GetBillingView,ListSourceViewsForBillingView,ListBillingViewSegments,AssociateSourceViews,DisassociateSourceViews")`
//! — those are every operation whose input carries a `BillingViewArn` member
//! named `arn` for the view it acts on, and each one declares
//! `ResourceNotFoundException`: "The specified ARN in the request doesn't
//! exist." The ARN forms the rows use come from that member's modeled pattern,
//! `^arn:aws[a-z-]*:(billing)::[0-9]{12}:billingview/[a-zA-Z0-9/:_\+=\.\-@]{0,75}[a-zA-Z0-9]$`.
//!
//! The rows cross every such operation (three reads and two writers) with the
//! forms an unresolvable ARN of this account can take: the shortest and the
//! longest resource id the pattern admits, a nested id, one spelling every
//! punctuation character the pattern allows, a partition other than `aws`, and
//! an ARN the store answered for once and answers for no longer. The controls
//! are the same operations over an ARN that DOES resolve, and the refusal of an
//! unresolvable `sourceViews` entry, which is a different case and keeps its own
//! member-named wording.
//!
//! The harness (`call`, `emulator_with_engine`, `plant_source_views`, …) is the
//! one `billing_views.rs` owns.

use serde_json::{json, Value};

use crate::billing_views::{
    call, emulator_with_engine, error_code, plant_source_views, ACCOUNT_ID, PRIMARY_ARN,
};
use crate::rejection_fidelity::message;

/// A pattern-valid `BillingViewArn` of `account` for the resource id `view_id`.
fn view_arn(account: &str, view_id: &str) -> String {
    format!("arn:aws:billing::{account}:billingview/{view_id}")
}

/// The whole sentence a refusal of an unresolvable billing-view ARN must
/// serialize — written out here, independently of the service code that builds
/// it, so the suite pins the text rather than whatever the helper happens to
/// return.
fn expected_message(arn: &str) -> String {
    format!("[{arn}] is an invalid billing view arn")
}

/// The `resourceType` every billing-view refusal reports.
const BILLING_VIEW_RESOURCE_TYPE: &str = "billingview";

/// The source views the writers' rows name, planted so a request is never
/// refused for a source instead of for the view it acts on.
fn held_sources() -> Vec<String> {
    (0..2)
        .map(|index| view_arn(ACCOUNT_ID, &format!("unresolvable-suite-source-{index}")))
        .collect()
}

/// Every ARN form a row uses, paired with the case it names, given the ARN of a
/// view that was created and then deleted.
///
/// The resource-id forms are read off the member's modeled pattern: its
/// character class `[a-zA-Z0-9/:_\+=\.\-@]` repeated 0..75 times followed by one
/// alphanumeric character, so the shortest id it admits is one character and the
/// longest is 76.
fn arn_forms(deleted: &str) -> Vec<(String, String)> {
    let longest = format!("{}z", "long-".repeat(15));
    vec![
        (
            "a plain resource id never created".to_string(),
            view_arn(ACCOUNT_ID, "never-created"),
        ),
        (
            "the shortest resource id the pattern admits".to_string(),
            view_arn(ACCOUNT_ID, "a"),
        ),
        (
            "the longest resource id the pattern admits".to_string(),
            view_arn(ACCOUNT_ID, &longest),
        ),
        (
            "a nested resource id".to_string(),
            view_arn(ACCOUNT_ID, "custom/nested/never-created"),
        ),
        (
            "every punctuation character the pattern admits".to_string(),
            view_arn(ACCOUNT_ID, "a_+=.-@:0"),
        ),
        (
            "a partition other than aws".to_string(),
            format!("arn:aws-cn:billing::{ACCOUNT_ID}:billingview/never-created"),
        ),
        (
            "an ARN that resolved until its view was deleted".to_string(),
            deleted.to_string(),
        ),
    ]
}

/// Every request that identifies the view it acts on by `arn`, as the body each
/// sends for `arn`. `sourceViews` names a view the store really holds, so a
/// writer's row is never refused for its sources instead of for `arn`.
fn requests(arn: &str, source: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetBillingView", json!({ "arn": arn })),
        ("ListSourceViewsForBillingView", json!({ "arn": arn })),
        ("ListBillingViewSegments", json!({ "arn": arn })),
        (
            "AssociateSourceViews",
            json!({ "arn": arn, "sourceViews": [source] }),
        ),
        (
            "DisassociateSourceViews",
            json!({ "arn": arn, "sourceViews": [source] }),
        ),
    ]
}

/// A world holding the suite's source views plus a `CUSTOM` view that was
/// created and deleted again, and the ARN that view had.
async fn world() -> (smithy_server_runtime::MultiServiceRouter, String) {
    let (router, engine) = emulator_with_engine();
    plant_source_views(&engine, &held_sources());

    let (status, created) = call(
        &router,
        "CreateBillingView",
        &json!({
            "name": "deleted-since",
            "sourceViews": [PRIMARY_ARN],
            "clientToken": "deleted-since",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the view that is deleted next to be created, got {status} {created:?}"
    );
    let deleted = created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateBillingView must answer an arn, got {created:?}"))
        .to_string();
    let (status, body) = call(&router, "DeleteBillingView", &json!({ "arn": deleted })).await;
    assert_eq!(
        status, 200,
        "setup: expected the delete to answer 200, got {status} {body:?}"
    );

    (router, deleted)
}

#[tokio::test]
async fn every_operation_resolving_its_own_arn_refuses_an_unresolvable_one_identically() {
    let (router, deleted) = world().await;
    let source = held_sources()[0].clone();

    for (form, arn) in arn_forms(&deleted) {
        for (operation, body) in requests(&arn, &source) {
            let case = format!("{operation} over {form} ({arn})");
            let (status, refused) = call(&router, operation, &body).await;

            assert_eq!(
                (status, error_code(&refused).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: expected 404 ResourceNotFoundException, got {status} {refused:?}"
            );
            assert_eq!(
                message(&refused),
                expected_message(&arn),
                "case {case:?}: expected {:?}, got {:?}",
                expected_message(&arn),
                message(&refused)
            );
            assert_eq!(
                refused["resourceId"].as_str(),
                Some(arn.as_str()),
                "case {case:?}: expected the unresolvable ARN as resourceId, got {:?}",
                refused["resourceId"]
            );
            assert_eq!(
                refused["resourceType"].as_str(),
                Some(BILLING_VIEW_RESOURCE_TYPE),
                "case {case:?}: expected the billing-view resourceType, got {:?}",
                refused["resourceType"]
            );
        }
    }
}

#[tokio::test]
async fn the_same_operations_answer_an_arn_that_resolves() {
    // The control: the refusal above is about the ARN resolving to nothing, not
    // about the operation. `ListBillingViewSegments` is asked over the account's
    // PRIMARY view, the only `BillingViewType` its own `arn` member admits; the
    // writers are asked over a `CUSTOM` view, the only kind whose source list a
    // caller owns.
    let (router, _deleted) = world().await;
    let sources = held_sources();

    let (status, created) = call(
        &router,
        "CreateBillingView",
        &json!({
            "name": "resolves",
            "sourceViews": [sources[0].clone()],
            "clientToken": "resolves",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the control view to be created, got {status} {created:?}"
    );
    let custom = created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateBillingView must answer an arn, got {created:?}"))
        .to_string();

    let answered: Vec<(&str, Value)> = vec![
        ("GetBillingView", json!({ "arn": custom })),
        ("ListSourceViewsForBillingView", json!({ "arn": custom })),
        ("ListBillingViewSegments", json!({ "arn": PRIMARY_ARN })),
        (
            "AssociateSourceViews",
            json!({ "arn": custom, "sourceViews": [sources[1].clone()] }),
        ),
        (
            "DisassociateSourceViews",
            json!({ "arn": custom, "sourceViews": [sources[1].clone()] }),
        ),
    ];

    for (operation, body) in answered {
        let (status, body) = call(&router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: an ARN that resolves is answered, got {status} {body:?}"
        );
    }
}

#[tokio::test]
async fn an_unresolvable_source_view_keeps_its_own_member_named_refusal() {
    // The other case the rule does NOT cover: a billing-view ARN a request names
    // BESIDE the view it acts on — a source that view would draw from — is
    // refused in the wording that names the member it arrived in, so a caller
    // can tell "the view you asked about does not exist" from "a source you
    // named does not exist".
    let (router, _deleted) = world().await;
    let sources = held_sources();

    let (status, created) = call(
        &router,
        "CreateBillingView",
        &json!({
            "name": "source-member",
            "sourceViews": [sources[0].clone()],
            "clientToken": "source-member",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the target view to be created, got {status} {created:?}"
    );
    let target = created["arn"].as_str().unwrap_or_default().to_string();
    let absent = view_arn(ACCOUNT_ID, "no-such-source");

    let (status, refused) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": target, "sourceViews": [absent.clone()] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (404, "ResourceNotFoundException"),
        "an unresolvable source is a 404, got {status} {refused:?}"
    );
    assert_eq!(
        message(&refused),
        format!("sourceViews: billing view {absent} does not exist"),
        "the refusal must name the member that carried the ARN, got {:?}",
        message(&refused)
    );
    assert_eq!(
        refused["resourceId"].as_str(),
        Some(absent.as_str()),
        "expected the unresolvable source as resourceId, got {:?}",
        refused["resourceId"]
    );
}
