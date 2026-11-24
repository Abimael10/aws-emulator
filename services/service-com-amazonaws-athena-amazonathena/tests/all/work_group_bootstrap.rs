// SPDX-License-Identifier: CC0-1.0

//! The workgroup every account HAS, driven end to end over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a resource the service creates for the account is there before any
//! caller creates one and cannot be taken away — the account's default workgroup
//! (`DEFAULT_WORK_GROUP`) is readable, listable, usable and taggable from the first call, in
//! `State` ENABLED, without any `CreateWorkGroup`; a create under its name is the refusal a
//! name the account already holds gets; and `DeleteWorkGroup` on it is refused with Athena's
//! fixed sentence for the one workgroup a delete cannot remove, leaving it exactly as it was.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along which
//! operation addresses the workgroup, whether the account holds any created workgroup beside
//! it, whether the delete asks for the contents to go with it, and — for the contrast a
//! not-found row draws — along the shape of a name nothing created.
//!
//! Nothing here waits on the wall clock: the default workgroup has no lifecycle the clock owes,
//! so every assertion holds at the instant it is made.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT, PRIMARY_WORK_GROUP_NOT_DELETABLE,
    WORK_GROUP_NOT_FOUND, WORK_GROUP_STATE_ENABLED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, create_work_group, engine_and_router, error_code, router};

/// The wire error code every refusal of this slice carries: the one client error each of its
/// operations models (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// Read one workgroup, and hand back the whole answer.
async fn read(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetWorkGroup", &json!({ "WorkGroup": name })).await
}

/// Every workgroup name the account's listing reports.
async fn listed(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "ListWorkGroups", &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        status, 200,
        "the account's workgroups must be listable, got {status} with {body:?}"
    );
    body["WorkGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| summary["Name"].as_str().map(ToString::to_string))
        .collect()
}

/// RULE: the account's default workgroup is there without a create — read, listed and ENABLED
/// — whether or not the account holds any workgroup a caller made.
#[tokio::test]
async fn the_accounts_default_work_group_is_there_without_a_create() {
    let router = router();

    // (case, what the account holds besides it)
    for case in ["a fresh account", "an account holding a created workgroup"] {
        if case == "an account holding a created workgroup" {
            create_work_group(&router, "made-by-a-caller").await;
        }
        let (status, body) = read(&router, DEFAULT_WORK_GROUP).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the default workgroup must be readable, got {status} with {body:?}"
        );
        let group = body["WorkGroup"].clone();
        assert_eq!(
            group["Name"].as_str(),
            Some(DEFAULT_WORK_GROUP),
            "case {case:?}: the read must report the workgroup addressed, got {group:?}"
        );
        assert_eq!(
            group["State"].as_str(),
            Some(WORK_GROUP_STATE_ENABLED),
            "case {case:?}: the default workgroup is enabled, got {group:?}"
        );
        assert!(
            group["CreationTime"].as_f64().is_some(),
            "case {case:?}: it reports when the account's registries were opened, got {group:?}"
        );
        assert!(
            listed(&router).await.contains(&DEFAULT_WORK_GROUP.to_string()),
            "case {case:?}: the listing reports it beside the created ones"
        );
    }

    // A read is a read: the second one reports what the first did, and a name nothing created
    // still misses.
    let (first, second) = (
        read(&router, DEFAULT_WORK_GROUP).await.1,
        read(&router, DEFAULT_WORK_GROUP).await.1,
    );
    assert_eq!(
        first, second,
        "two reads of the default workgroup must report the same record"
    );
    for absent in ["no-such-workgroup", "Primary", "primary-2", "prim"] {
        let (status, refused) = read(&router, absent).await;
        assert_refusal(
            absent,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            WORK_GROUP_NOT_FOUND,
        );
    }
}

/// RULE: the default workgroup is the account's already, so a create under its name is refused
/// as the name being taken — and the workgroup is left exactly as it was.
#[tokio::test]
async fn a_create_under_the_default_work_groups_name_is_refused() {
    let router = router();
    let before = read(&router, DEFAULT_WORK_GROUP).await.1;

    // Rows differ along what the create would have configured, since the refusal is about the
    // NAME rather than about the configuration.
    for (case, body) in [
        ("a create naming nothing else", json!({ "Name": DEFAULT_WORK_GROUP })),
        (
            "a create carrying a description",
            json!({ "Name": DEFAULT_WORK_GROUP, "Description": "mine now" }),
        ),
        (
            "a create carrying a configuration",
            json!({
                "Name": DEFAULT_WORK_GROUP,
                "Configuration": {
                    "ResultConfiguration": { "OutputLocation": "s3://taken/over/" }
                }
            }),
        ),
    ] {
        let (status, refused) = call(&router, "CreateWorkGroup", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {refused:?}"
        );
        assert_eq!(
            read(&router, DEFAULT_WORK_GROUP).await.1,
            before,
            "case {case:?}: the refused create must leave the workgroup as it was"
        );
    }
}

/// RULE: `DeleteWorkGroup` cannot take the default workgroup away — the refusal is Athena's
/// fixed sentence for it, and the workgroup is still there and still ENABLED afterwards, where
/// a workgroup a caller created and deleted reads back as the miss this service words.
#[tokio::test]
async fn the_default_work_group_cannot_be_deleted() {
    let router = router();
    create_work_group(&router, "deletable").await;

    // Rows differ along what the delete asks for: the option that takes a workgroup's contents
    // with it changes nothing about the one workgroup that cannot be deleted at all.
    for (case, request) in [
        (
            "a plain delete",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP }),
        ),
        (
            "a delete asking for the contents too",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP, "RecursiveDeleteOption": true }),
        ),
        (
            "a delete asking for the contents to stay",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP, "RecursiveDeleteOption": false }),
        ),
    ] {
        let (status, refused) = call(&router, "DeleteWorkGroup", &request).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            PRIMARY_WORK_GROUP_NOT_DELETABLE,
        );
        let (status, read_back) = read(&router, DEFAULT_WORK_GROUP).await;
        assert_eq!(
            (status, read_back["WorkGroup"]["State"].as_str()),
            (200, Some(WORK_GROUP_STATE_ENABLED)),
            "case {case:?}: the refused delete must leave it present and enabled, got \
             {read_back:?}"
        );
        assert!(
            listed(&router).await.contains(&DEFAULT_WORK_GROUP.to_string()),
            "case {case:?}: the refused delete must leave it listed"
        );
    }

    // By contrast, a workgroup a caller created IS deleted, and reads back as the miss.
    let (status, deleted) = call(
        &router,
        "DeleteWorkGroup",
        &json!({ "WorkGroup": "deletable" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a created workgroup must be deletable, got {status} with {deleted:?}"
    );
    let (status, refused) = read(&router, "deletable").await;
    assert_refusal(
        "a deleted workgroup",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        WORK_GROUP_NOT_FOUND,
    );
    assert!(
        !listed(&router).await.contains(&"deletable".to_string()),
        "a deleted workgroup leaves the listing"
    );
}

/// RULE: the default workgroup is a workgroup like any other to the operations that ADDRESS one
/// — a query runs in it, a saved query belongs to it, and an update changes it — so none of
/// them has to be told it is special.
#[tokio::test]
async fn the_default_work_group_serves_the_operations_that_address_one() {
    let (engine, router) = engine_and_router();
    // Athena verifies the resolved output location before it accepts a submission, so the
    // bucket the query below writes to is held first.
    crate::results_bucket::hold_results_bucket(&engine, "s3://athena-primary/results/");

    // (case, the operation, its request, the status it must answer)
    let rows: [(&str, &str, Value, u16); 4] = [
        (
            "a query submitted into it",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "WorkGroup": DEFAULT_WORK_GROUP,
                "ResultConfiguration": { "OutputLocation": "s3://athena-primary/results/" }
            }),
            200,
        ),
        (
            "a query saved in it",
            "CreateNamedQuery",
            json!({
                "Name": "saved-in-the-default",
                "Database": "default",
                "QueryString": "SELECT 1",
                "WorkGroup": DEFAULT_WORK_GROUP
            }),
            200,
        ),
        (
            "its saved queries listed",
            "ListNamedQueries",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP }),
            200,
        ),
        (
            "an update of its description",
            "UpdateWorkGroup",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP, "Description": "the account's own" }),
            200,
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected} from {operation}, got {status} with {body:?}"
        );
    }

    // The update it took is the one a read reports, so the workgroup a caller changed is the
    // same record every other operation resolves.
    let (status, read_back) = read(&router, DEFAULT_WORK_GROUP).await;
    assert_eq!(
        (status, read_back["WorkGroup"]["Description"].as_str()),
        (200, Some("the account's own")),
        "the default workgroup must report the description an update wrote, got {read_back:?}"
    );
}
