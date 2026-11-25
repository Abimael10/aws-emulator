// SPDX-License-Identifier: CC0-1.0

//! The tagging operations over a DATASET's ARN, on the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: RekognitionService.<Operation>`), dispatched through the same
//! [`MultiServiceRouter`] the production emulator mounts.
//!
//! Every assertion goes over HTTP and reads only the status, the wire error code and the
//! JSON body: nothing reaches into the service's state struct, so a response that is not
//! computed from persisted state cannot pass.
//!
//! THE RULE UNDER TEST: `TagResource`, `UntagResource` and `ListTagsForResource` work on the
//! tag set held on whichever LIVE resource of this store the ARN names — the set the
//! resource's own create attached, changed by exactly the keys a request carries — and an
//! ARN that names no live resource is not found by any of the three. A dataset is one of
//! those resources: `CreateDataset` takes a `TagMap` of its own
//! (`smithy_query(query="operation",operation="CreateDataset")`), and "if you try to access
//! the dataset after it is deleted, you get a `ResourceNotFoundException` exception", so its
//! tags live and die with the dataset record rather than with the ARN that names it.
//!
//! The rows differ along what the rule generalises over: which half of its project's data
//! the dataset is (both values the modelled `DatasetType` admits), whether its create
//! attached tags at all, which form of its ARN the request names it by (with and without the
//! leading `arn:` label, both of which the tagging operations resolve), and what one request
//! changes — the steps of [`run_round_trip`], the same table a collection and a project are
//! put through.
//!
//! The collection suite owns the router harness and the dispatch helpers, and the tagging
//! suite owns the resource table, the round trip, the tag reader and the not-found sweep, so
//! nothing here builds a second one.
//!
//! Every wait here is a poll for the CONDITION under a generous deadline: a dataset's
//! completion and its disappearance are transitions the clock owes, and nothing here asserts
//! how long one took.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::face_collections::{call, harness};
use crate::resource_tags::{
    assert_every_resource, assert_not_found_by_every_tagging_operation, dataset_resource,
    project_resource, run_round_trip,
};

/// The two values the modelled `DatasetType` admits
/// (`smithy_query(query="shape",shape="DatasetType")`: `[TRAIN, TEST]`).
const TRAIN: &str = "TRAIN";
const TEST: &str = "TEST";

/// The `DatasetStatus` a dataset whose creation the clock has completed reports (modelled
/// `DatasetStatus`), which is also the status a dataset is deletable from.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one poll waits before looking again, and how long a whole wait may take.
///
/// The deadline is a ceiling on the WAIT and nothing else: the creation and deletion spans
/// are this service's own, so each loop looks again until the condition holds, and the
/// deadline is generous enough that a loaded host running the whole workspace's tests at once
/// still gets there.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn a_datasets_tags_are_its_own_set_reached_by_either_form_of_its_arn() {
    let router = harness();
    // The owning project carries a tag set of its own, and every resource's tag values are
    // marked with its own marker, so one resource's tags leaking onto another is a failure
    // rather than a match.
    let owner = project_resource(
        &router,
        "the owning project",
        "p1",
        "tagged-project",
        &[("team", "vision")],
    )
    .await;
    let mut datasets = vec![
        dataset_resource(
            &router,
            "the training dataset, created with tags",
            "d1",
            &owner.arns[0],
            TRAIN,
            &[("env", "sim2real"), ("owner", "behavioral")],
        )
        .await,
        dataset_resource(
            &router,
            "the test dataset, created with no tags",
            "d2",
            &owner.arns[0],
            TEST,
            &[],
        )
        .await,
    ];

    // What each create attached is the set the round trip starts from.
    assert_every_resource(&router, "the tags the create attached", &datasets).await;

    // The round trip, with the owning project as the resource no step touches: a dataset's
    // tags are its own, and so are its project's.
    run_round_trip(&router, &mut datasets, std::slice::from_ref(&owner)).await;
}

#[tokio::test]
async fn a_dataset_the_clock_has_deleted_is_not_found_by_any_tagging_operation() {
    let router = harness();
    let owner = project_resource(
        &router,
        "the owning project",
        "p1",
        "deleted-dataset-project",
        &[("team", "vision")],
    )
    .await;
    // Rows: both halves of the project's data, each created with tags of its own, so the
    // deletion of one is read against a sibling that is still there.
    let datasets = vec![
        dataset_resource(
            &router,
            "the deleted training dataset",
            "d1",
            &owner.arns[0],
            TRAIN,
            &[("env", "sim2real"), ("owner", "behavioral")],
        )
        .await,
        dataset_resource(
            &router,
            "the surviving test dataset",
            "d2",
            &owner.arns[0],
            TEST,
            &[("env", "staging")],
        )
        .await,
    ];
    assert_every_resource(&router, "before the delete", &datasets).await;

    let (removed, kept) = (&datasets[0], &datasets[1]);
    // A dataset is deletable once the clock has finished creating it, so the delete waits for
    // that status rather than for a span.
    poll_until_create_complete(&router, removed.case, &removed.arns[0]).await;
    let started = call(
        &router,
        "DeleteDataset",
        json!({ "DatasetArn": removed.arns[0] }),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {:?}: deleting it must succeed, got {:?}",
        removed.case, started.body
    );

    // Once the clock has completed the deletion the ARN names no live resource, in either of
    // its forms, so none of the three tagging operations finds it.
    for arn in &removed.arns {
        poll_until_not_found(&router, removed.case, arn).await;
        assert_not_found_by_every_tagging_operation(&router, removed.case, arn).await;
    }

    // Only the dataset the delete named went: the sibling and the owning project still report
    // their own sets, through either form of their ARNs.
    assert_every_resource(&router, "after the delete", std::slice::from_ref(kept)).await;
    assert_every_resource(&router, "after the delete", std::slice::from_ref(&owner)).await;
}

/// Look again until the dataset `arn` names reports [`CREATE_COMPLETE`], under [`DEADLINE`]:
/// the completion is a transition the clock owes, so the test waits for the CONDITION rather
/// than for a span, and fails on the status it was still reporting when the deadline passed.
async fn poll_until_create_complete(router: &MultiServiceRouter, case: &str, arn: &str) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reply = call(router, "DescribeDataset", json!({ "DatasetArn": arn })).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: DescribeDataset on {arn} must answer 200, got {:?}",
            reply.body
        );
        let status = reply
            .field("DatasetDescription")
            .and_then(|description| description.get("Status"))
            .and_then(|status| status.as_str())
            .unwrap_or_default()
            .to_string();
        if status == CREATE_COMPLETE || Instant::now() >= until {
            assert_eq!(
                status, CREATE_COMPLETE,
                "case {case:?}: {arn} must reach {CREATE_COMPLETE}, got {status:?}"
            );
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Look again until `ListTagsForResource` stops serving `arn`, under [`DEADLINE`]: the
/// dataset's disappearance is a transition the clock owes, so the test waits for the
/// CONDITION rather than for a span, and fails on the answer it was still getting when the
/// deadline passed.
async fn poll_until_not_found(router: &MultiServiceRouter, case: &str, arn: &str) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reply = call(router, "ListTagsForResource", json!({ "ResourceArn": arn })).await;
        if reply.status != 200 || Instant::now() >= until {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: {arn} must stop resolving once the deletion is complete, got \
                 {:?}",
                reply.body
            );
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}
