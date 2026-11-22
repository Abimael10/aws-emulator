// SPDX-License-Identifier: CC0-1.0

//! WHICH ERROR CLASS a tagging call's refusal carries, end to end.
//!
//! Every call goes through the production path: this service is mounted in the real
//! `MultiServiceRouter` by the harness this crate's core suite owns
//! ([`crate::virtual_cluster_job_run_loop::Emr`]), so routing, the `@httpLabel` ARN,
//! constraint validation, the handler and serialization all run exactly as they do for an
//! AWS SDK client — and the class under test is read where a restJson1 client reads it, off
//! the `x-amzn-errortype` response header.
//!
//! The rule under test: the three `/tags/{resourceArn}` operations share ONE route, answered
//! by the resource-tagging layer in front of this service, and that layer refuses a resource
//! reference it cannot act on — outside the `RsiArn` length or pattern the model declares,
//! or inside them and resolving to no resource this endpoint holds — as a bad request about
//! the value it was handed: HTTP 400 under the layer's OWN error code, which is none of the
//! three shapes these operations model. The code is the same whichever way the reference
//! failed, because a client dispatches on it; and a refusal the OPERATION makes itself keeps
//! the modelled shape it declares, which is what keeps the two tellable apart.
//!
//! [`crate::tagging_resource_arn_form`] owns the WORDING of that class and the span of
//! references it covers; this suite owns the class each refusal is carried in.

use std::collections::BTreeMap;

use serde_json::json;
use service_com_amazonaws_emrcontainers_awschicagowebservice::state;

use crate::resource_tag_removal::{
    cluster_tags, encoded, tag_object, tagged_cluster, tagged_job_run, tags,
};
use crate::tagging_resource_arn_form::{
    answers, minted_arn_of_length, shaped_arn_of_length, ARN_MAX_CHARS, ARN_MIN_CHARS,
    INVALID_RESOURCE_ARN, OTHER_ACCOUNT, UNUSABLE_RESOURCE_ARN_CODE,
};
use crate::virtual_cluster_job_run_loop::{text, Emr, REGION};

/// Every error shape the three tagging operations declare (smithy_query:
/// `query="operation"`, `operation="TagResource,UntagResource,ListTagsForResource"` —
/// `InternalServerException` 500, `ResourceNotFoundException` 400, `ValidationException`
/// 400). The layer's refusal is carried under NONE of them, which is the half of the rule a
/// caller's `catch` depends on.
const MODELLED_ERROR_SHAPES: [&str; 3] = [
    "InternalServerException",
    "ResourceNotFoundException",
    "ValidationException",
];

/// The class a refusal the OPERATION itself makes is carried in: the modelled
/// `ValidationException` (HTTP 400) every one of the three declares.
const MODELLED_INVALID_REQUEST: &str = "ValidationException";

/// Where in the request path the reference was refused — the two halves the rule spans,
/// which must answer the SAME class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusedby {
    /// The modelled `RsiArn` constraint, before any handler ran.
    TheModelledConstraint,
    /// The resolution of a reference the constraint admits.
    TheResolution,
}

/// One row: a `resourceArn`, and which half of the rule it belongs to.
struct ClassCase {
    case: String,
    resource_arn: String,
    refused_by: Refusedby,
}

fn row(case: &str, resource_arn: String, refused_by: Refusedby) -> ClassCase {
    ClassCase {
        case: case.to_string(),
        resource_arn,
        refused_by,
    }
}

/// RULE: a `resourceArn` the tagging layer cannot act on is refused under that layer's own
/// code — the same code for a value the modelled constraint rejects and for one it admits
/// that resolves to nothing, at all three operations, and never one of the shapes those
/// operations model.
#[tokio::test]
async fn an_unusable_resource_arn_is_refused_under_the_tagging_layers_own_code() {
    let emr = Emr::new();
    // Resources that really exist, so a row is refused for the reference it sent and
    // nothing else.
    let held = tags(&[("env", "dev")]);
    let (cluster, cluster_arn) = tagged_cluster(&emr, "class-host", &held).await;
    let (run, _) = tagged_job_run(&emr, &cluster, "tok-class-host", &tags(&[("keep", "me")])).await;

    // `ResourceIdString` ids are `^[0-9a-z]+$` (smithy_query: shape `ResourceIdString`), so
    // this names nothing while still being an id this service could have minted.
    let unknown = "doesnotexist7";

    let rows = vec![
        // Outside the modelled constraint: refused before any handler runs.
        row(
            "a bare virtual cluster id, not an ARN at all",
            cluster.clone(),
            Refusedby::TheModelledConstraint,
        ),
        row(
            "the pattern's shortest match, one character under the modelled minimum",
            shaped_arn_of_length(ARN_MIN_CHARS - 1),
            Refusedby::TheModelledConstraint,
        ),
        row(
            "one character past the modelled maximum",
            minted_arn_of_length(ARN_MAX_CHARS + 1),
            Refusedby::TheModelledConstraint,
        ),
        row(
            "an ARN of another AWS service",
            format!(
                "arn:aws:emr:{REGION}:{}:/virtualclusters/{cluster}",
                engine::emulator_account_id()
            ),
            Refusedby::TheModelledConstraint,
        ),
        row(
            "an account id that is not twelve digits",
            format!("arn:aws:emr-containers:{REGION}:12345:/virtualclusters/{cluster}"),
            Refusedby::TheModelledConstraint,
        ),
        // Inside the modelled constraint, and still a reference this endpoint cannot act
        // on: the same class, because a client dispatches on the code.
        row(
            "a virtual cluster ARN this endpoint mints for a cluster nothing holds",
            state::virtual_cluster_arn(REGION, unknown),
            Refusedby::TheResolution,
        ),
        row(
            "exactly the characters the modelled maximum admits, for a cluster nothing holds",
            minted_arn_of_length(ARN_MAX_CHARS),
            Refusedby::TheResolution,
        ),
        row(
            "a job run ARN naming no run under a stored cluster",
            state::job_run_arn(REGION, &cluster, unknown),
            Refusedby::TheResolution,
        ),
        row(
            "a stored job run's id under a cluster nothing holds",
            state::job_run_arn(REGION, unknown, &run),
            Refusedby::TheResolution,
        ),
        row(
            "a managed endpoint ARN naming no endpoint under a stored cluster",
            state::managed_endpoint_arn(REGION, &cluster, unknown),
            Refusedby::TheResolution,
        ),
        row(
            "this cluster's own id in another account",
            format!("arn:aws:emr-containers:{REGION}:{OTHER_ACCOUNT}:/virtualclusters/{cluster}"),
            Refusedby::TheResolution,
        ),
        row(
            "this cluster's own id in another region",
            state::virtual_cluster_arn("eu-west-1", &cluster),
            Refusedby::TheResolution,
        ),
        row(
            "this cluster's own id in another partition",
            format!(
                "arn:aws-us-gov:emr-containers:{REGION}:{}:/virtualclusters/{cluster}",
                engine::emulator_account_id()
            ),
            Refusedby::TheResolution,
        ),
        row(
            "a sub-resource type this service does not model",
            format!("{cluster_arn}/widgets/w1"),
            Refusedby::TheResolution,
        ),
    ];

    // Both halves have to be represented, or the table would say nothing about the two
    // carrying ONE class.
    for half in [Refusedby::TheModelledConstraint, Refusedby::TheResolution] {
        assert!(
            rows.iter().any(|row| row.refused_by == half),
            "the table must carry rows refused by {half:?}"
        );
    }

    for current in &rows {
        for (operation, answer) in answers(&emr, &current.resource_arn).await {
            let case = format!("{operation}, {} ({:?})", current.case, current.refused_by);
            answer.refused(&case, 400, UNUSABLE_RESOURCE_ARN_CODE);
            let reported = answer.error_code.clone().unwrap_or_default();
            assert!(
                !MODELLED_ERROR_SHAPES.contains(&reported.as_str()),
                "case {case:?}: expected a class outside the shapes this operation models \
                 ({MODELLED_ERROR_SHAPES:?}), got {reported:?}"
            );
            // The class and its sentence travel together, so the body is checked with it.
            let message = text(&answer.body, "/message", &case);
            assert_eq!(
                message, INVALID_RESOURCE_ARN,
                "case {case:?}: expected message {INVALID_RESOURCE_ARN:?}, got {message:?}"
            );
        }
    }

    // The control: a refused call is a class and a sentence and nothing else, so the
    // resource that DOES exist still carries exactly the tags it was created with.
    assert_eq!(
        cluster_tags(&emr, &cluster, "after the refusals").await,
        tag_object(&held),
        "a refused tagging call must leave the cluster's tags as they were"
    );
}

/// The rule's BOUNDARY: a refusal the OPERATION makes itself — the merge past `TagMap`'s
/// maximum, and a `tags` map wider than the model admits — keeps the modelled
/// `ValidationException`, so the layer's class is the resource REFERENCE's alone; and a
/// reference the endpoint resolves is served.
#[tokio::test]
async fn a_refusal_the_operation_makes_itself_keeps_its_modelled_class() {
    let emr = Emr::new();
    let full: BTreeMap<String, String> = (0..state::MAX_RESOURCE_TAGS)
        .map(|n| (format!("k{n}"), format!("v{n}")))
        .collect();
    let (cluster, arn) = tagged_cluster(&emr, "class-cap", &full).await;
    let path = format!("/tags/{}", encoded(&arn));

    // One new key on a resource already at the cap: the operation's own refusal.
    let case = "a merge one past the maximum tag count";
    let refused = emr
        .send("POST", &path, Some(json!({"tags": {"one-too-many": "x"}})))
        .await;
    refused.refused(case, 400, MODELLED_INVALID_REQUEST);

    // A `tags` map wider than `TagMap` admits: the modelled constraint refuses the MAP,
    // not the reference, so it keeps the modelled class too.
    let case = "one request carrying more pairs than TagMap admits";
    let too_many: serde_json::Map<String, serde_json::Value> = (0..=state::MAX_RESOURCE_TAGS)
        .map(|n| (format!("t{n}"), json!(format!("v{n}"))))
        .collect();
    emr.send("POST", &path, Some(json!({"tags": too_many})))
        .await
        .refused(case, 400, MODELLED_INVALID_REQUEST);

    // And the reference itself still resolves: the class above is about the request's
    // values, so the same ARN is served when the request is acceptable.
    let case = "a stored cluster's own ARN";
    emr.send("POST", &path, Some(json!({"tags": {"k0": "rewritten"}})))
        .await
        .ok(case);
    let listed = emr.send("GET", &path, None).await;
    let value = text(listed.ok(case), "/tags/k0", case);
    assert_eq!(
        value, "rewritten",
        "case {case:?}: the read must report the value the accepted call wrote, got {value:?}"
    );
    let reported = cluster_tags(&emr, &cluster, case).await;
    assert_eq!(
        reported.as_object().map(serde_json::Map::len),
        Some(state::MAX_RESOURCE_TAGS),
        "case {case:?}: the refused merges must have written nothing, got {reported}"
    );
}
