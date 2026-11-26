// SPDX-License-Identifier: CC0-1.0

//! The one question every resource metrics configuration operation asks about the ARN it is
//! given, driven off the CATALOG this service publishes rather than off any single resource: does
//! this service collect detailed metrics for the KIND of resource that ARN names?
//!
//! The rule: an ARN naming a resource of a kind outside that catalog is a fault of the REQUEST
//! (refused with the request-validation class and the one sentence that says the resource type is
//! not supported), while an ARN of a kind INSIDE it is judged as a LOOKUP — the configuration the
//! store holds for that resource, or the modelled `ResourceNotFoundException` when it holds none.
//! The kind is the service namespace and the resource type TOGETHER.
//!
//! These cases iterate `state::DETAILED_METRIC_RESOURCE_TYPES`, so a kind added to the catalog is
//! covered here without a row being written for it. The refusal class itself — the status, the
//! shape id, the `x-amzn-query-error` header and the message, over a table of resources no
//! detailed metrics are collected for — is asserted by the sibling suite that owns this
//! operation's reads (`crate::resource_metrics_configurations`), whose constants and helpers this
//! one reuses so the two cannot drift apart.

use crate::harness::{build_router_with_engine, error_code, error_message};
use crate::resource_metrics_configurations::{
    given_configuration, read, reported, ACCOUNT, UNSUPPORTED_RESOURCE_TYPE,
};
use serde_json::Value;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

/// The ARN of one resource of the kind `(vendor, resource_type)` names, written with `separator`
/// between the type and the id — both forms AWS writes a resource part in.
fn kind_arn(vendor: &str, region: &str, resource_type: &str, separator: char, id: &str) -> String {
    format!("arn:aws:{vendor}:{region}:{ACCOUNT}:{resource_type}{separator}{id}")
}

/// EVERY kind the published catalog covers is judged as a LOOKUP rather than as a fault of the
/// request: a configuration held for a resource of that kind is reported, and a resource of that
/// kind with none is reported absent.
#[tokio::test]
async fn every_covered_kind_is_looked_up() {
    assert!(
        !cloudwatch::DETAILED_METRIC_RESOURCE_TYPES.is_empty(),
        "case \"the published catalog\": a catalog with no kind in it leaves these cases nothing \
         to assert, and no resource able to hold a configuration at all"
    );

    for &(vendor, resource_type) in &cloudwatch::DETAILED_METRIC_RESOURCE_TYPES {
        let case = format!("{vendor} {resource_type}");
        let held_for = kind_arn(vendor, "us-west-2", resource_type, '/', "a-resource");
        let nothing_held_for = kind_arn(vendor, "us-west-2", resource_type, '/', "another");

        let (router, engine) = build_router_with_engine();
        let held = given_configuration(&engine, &held_for, Some(vec!["OneMetric".to_string()]));

        let (status, body) = read(&router, &held_for).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a held configuration of a covered kind must be reported with HTTP \
             200, got {status} with body {body}"
        );
        assert_eq!(
            reported(&body).get("ResourceArn").and_then(Value::as_str),
            Some(held.resource_arn.as_str()),
            "case {case:?}: the configuration reported must be the one held for that resource, \
             got {body}"
        );

        let (status, body) = read(&router, &nothing_held_for).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a covered kind with no configuration must be reported absent with \
             HTTP 404, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: an absent configuration must carry the modelled code, got {body}"
        );
    }
}

/// The kind is the service and the resource type TOGETHER, not the type token alone: the same
/// token under a service the catalog covers no kind of is refused as a fault of the request, in
/// both forms a resource part is written in.
#[tokio::test]
async fn the_kind_is_the_service_and_the_type_together() {
    let (router, _engine) = build_router_with_engine();
    for &(vendor, resource_type) in &cloudwatch::DETAILED_METRIC_RESOURCE_TYPES {
        for separator in ['/', ':'] {
            let covered = kind_arn(vendor, "us-east-1", resource_type, separator, "a-resource");
            let (status, body) = read(&router, &covered).await;
            assert_eq!(
                status, 404,
                "case {vendor:?} with {separator:?}: a covered kind must reach the lookup, got \
                 {status} with body {body}"
            );

            // The same resource type token, owned by a service the catalog covers no kind of.
            let elsewhere = kind_arn(
                "emr-containers",
                "us-east-1",
                resource_type,
                separator,
                "a-resource",
            );
            let (status, body) = read(&router, &elsewhere).await;
            assert_eq!(
                status, 400,
                "case {vendor:?} with {separator:?}: the same type under an uncovered service \
                 must be refused as a request fault, got {status} with body {body}"
            );
            assert_eq!(
                error_message(&body),
                UNSUPPORTED_RESOURCE_TYPE,
                "case {vendor:?} with {separator:?}: the refusal must be the one the resource \
                 earns, got {body}"
            );
        }
    }
}

/// A resource part naming a covered type with NO resource after it addresses no resource whose
/// metrics could be collected, so it is a fault of the request rather than a lookup — for every
/// covered kind, and in both forms a resource part is written in.
#[tokio::test]
async fn a_covered_type_with_no_resource_after_it_is_refused() {
    let (router, _engine) = build_router_with_engine();
    for &(vendor, resource_type) in &cloudwatch::DETAILED_METRIC_RESOURCE_TYPES {
        let cases = [
            ("the type alone", format!("arn:aws:{vendor}:us-east-1:{ACCOUNT}:{resource_type}")),
            (
                "the type and an empty id",
                kind_arn(vendor, "us-east-1", resource_type, '/', ""),
            ),
            (
                "the type and an empty id, colon form",
                kind_arn(vendor, "us-east-1", resource_type, ':', ""),
            ),
        ];
        for (form, resource_arn) in cases {
            let (status, body) = read(&router, &resource_arn).await;
            assert_eq!(
                status, 400,
                "case {vendor:?} with {form:?}: a resource part addressing no resource must be \
                 refused with HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_message(&body),
                UNSUPPORTED_RESOURCE_TYPE,
                "case {vendor:?} with {form:?}: the refusal must say what about the request \
                 cannot be served, got {body}"
            );
        }
    }
}
