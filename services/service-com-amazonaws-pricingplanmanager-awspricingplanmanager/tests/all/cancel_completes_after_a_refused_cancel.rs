// SPDX-License-Identifier: CC0-1.0

//! A `CancelSubscription` the subscription's current entity tag admits runs to the
//! subscription's TERMINAL state, and a cancellation refused on its `If-Match` first does
//! not change that.
//!
//! Two rules meet on this path, and a client that cancels under optimistic concurrency
//! walks both of them in one sequence: it reads the subscription, cancels with the tag it
//! read, is refused because the tag has moved, re-reads and cancels again.
//!
//! 1. A refused mutation is INERT: nothing is written, so the subscription keeps its
//!    status, carries no pending change and still publishes the tag it had.
//! 2. An accepted cancellation ENDS the subscription, on the service's own clock and with
//!    no further call of the client's making it happen: once it has ended,
//!    `GetSubscription` answers the modelled `ResourceNotFoundException` (HTTP 404) naming
//!    the ARN the request carried, and `ListSubscriptions` reports neither the
//!    subscription nor the resources it covered.
//!
//! What this suite pins is the COMPOSITION of the two: the refusal leaves the record
//! exactly as cancellable as it was, so the cancel that follows it still reaches the
//! terminal state — the wait a teardown, an IaC destroy or a "is this still billing?"
//! reconciliation makes always ends.
//!
//! The rows span what the rule generalises over: the plan family, the tier (the free one,
//! which carries no billing commitment, and paid ones), how many resources are covered,
//! the approval mode and so the status the cancellation acts on (`ACTIVE`, where a
//! `CANCELLATION` is scheduled first, and `PENDING_APPROVAL`, which is deleted outright),
//! and the form of the tag that is refused.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The HTTP harness and the paging walk are `subscription_wire_form`'s — the ONE harness of
// this crate's integration tests — and the wait for a cancelled subscription to be gone is
// the lifecycle suite's, so this suite adds no second copy of either.
use super::subscription_lifecycle::poll_until_absent;
use super::subscription_wire_form::{call, create, create_body, pages, router};

/// One subscription to cancel, and the shape of the cancellation it answers.
struct Case {
    family: &'static str,
    tier: &'static str,
    resources: usize,
    approval: Option<&'static str>,
    /// The status the subscription reports before any cancel is attempted, which is what
    /// decides whether the accepted cancellation schedules a change or deletes the record
    /// outright.
    status: &'static str,
    /// Whether the accepted cancellation leaves a pending `CANCELLATION` behind, which is
    /// what cancelling an already active subscription does.
    schedules: bool,
}

#[tokio::test]
async fn a_cancel_refused_on_its_if_match_still_leaves_the_next_one_able_to_complete() {
    let cases = [
        Case {
            family: "CloudFront",
            tier: "FREE",
            resources: 2,
            approval: None,
            status: "ACTIVE",
            schedules: true,
        },
        Case {
            family: "CloudFront",
            tier: "PREMIUM",
            resources: 1,
            approval: Some("IMMEDIATE"),
            status: "ACTIVE",
            schedules: true,
        },
        Case {
            // The free tier is "always activated immediately regardless of approval mode",
            // so this row is ACTIVE despite asking for MANUAL approval.
            family: "Route53",
            tier: "FREE",
            resources: 10,
            approval: Some("MANUAL"),
            status: "ACTIVE",
            schedules: true,
        },
        Case {
            family: "CloudFront",
            tier: "BUSINESS",
            resources: 3,
            approval: Some("MANUAL"),
            status: "PENDING_APPROVAL",
            schedules: false,
        },
    ];

    let router = router();
    for case in &cases {
        let label = format!(
            "{}/{}/{} resources/{:?}",
            case.family, case.tier, case.resources, case.approval
        );
        let body = create_body(case.family, case.tier, case.resources, None, case.approval);
        let covered = resource_arns(&body, &label);
        let created = create(&router, body).await;
        let arn = created.string("arn");
        let etag = created.require_etag(&label);
        assert_eq!(
            created.string("status"),
            case.status,
            "case {label}: unexpected status in {:?}",
            created.body
        );

        // Every refused cancel, each on a tag this service never issued for this record,
        // whose own tag is a small decimal revision.
        for stale in stale_tags(&etag) {
            let refused_label = format!("{label}/{stale:?}");
            assert_ne!(
                stale, etag,
                "case {refused_label}: the rejected tag must differ from the current one"
            );
            let refused = call(
                &router,
                "CancelSubscription",
                json!({"arn": arn.clone()}),
                Some(&stale),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {refused_label}: a stale If-Match must be refused 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ValidationException"),
                "case {refused_label}: unexpected error code for {:?}",
                refused.body
            );
            assert_eq!(
                refused.message(&refused_label),
                format!("Invalid or outdated ETag '{stale}': retrieve current ETag and retry."),
                "case {refused_label}: unexpected message for {:?}",
                refused.body
            );

            // Rule 1: the refusal wrote nothing, so the subscription is still exactly the
            // record the accepted cancel will act on.
            let unchanged = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
            assert_eq!(
                unchanged.status, 200,
                "case {refused_label}: a refused cancel must leave the subscription \
                 readable, got {} {:?}",
                unchanged.status, unchanged.body
            );
            assert_eq!(
                unchanged.body, created.body,
                "case {refused_label}: a refused cancel must not change the subscription"
            );
            assert_eq!(
                unchanged.require_etag(&refused_label),
                etag,
                "case {refused_label}: a refused cancel must not move the ETag"
            );
            assert!(
                unchanged.body.get("scheduledChange").is_none(),
                "case {refused_label}: a refused cancel must schedule nothing, got {:?}",
                unchanged.body
            );
        }

        // The cancel the current tag admits.
        let cancelled = call(
            &router,
            "CancelSubscription",
            json!({"arn": arn.clone()}),
            Some(&etag),
        )
        .await;
        assert_eq!(
            cancelled.status, 200,
            "case {label}: the current If-Match must be accepted, got {} {:?}",
            cancelled.status, cancelled.body
        );
        // Read off the cancelling call's OWN answer, so nothing here depends on how much
        // of the span has passed by the time a later call is made.
        let scheduled = cancelled
            .body
            .get("scheduledChange")
            .and_then(|change| change.get("changeType"))
            .and_then(Value::as_str);
        assert_eq!(
            scheduled,
            if case.schedules {
                Some("CANCELLATION")
            } else {
                None
            },
            "case {label}: unexpected pending change in {:?}",
            cancelled.body
        );

        // Rule 2: the wait a client written against AWS makes — look again until the
        // subscription is gone, with no call of its own ending it.
        let absent = poll_until_absent(&router, &arn, &label).await;
        assert_eq!(
            absent.status, 404,
            "case {label}: the terminal state is a 404, got {} {:?}",
            absent.status, absent.body
        );
        assert_eq!(
            absent.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label}: unexpected error class for {:?}",
            absent.body
        );
        assert_eq!(
            absent.message(&label),
            format!(
                "Subscription not found: {arn}. Verify the ARN is correct and the \
                 subscription exists in your account."
            ),
            "case {label}: unexpected message for {:?}",
            absent.body
        );
        assert_eq!(
            absent.opt_string("resourceId"),
            Some(arn.clone()),
            "case {label}: the refusal must name the subscription, got {:?}",
            absent.body
        );

        // ...and the listing has stopped reporting it, and the resources it covered with
        // it.
        let listed = listed_subscriptions(&router).await;
        assert!(
            !listed.arns.contains(&arn),
            "case {label}: a cancelled subscription must not be listed, got {:?}",
            listed.arns
        );
        for resource in &covered {
            assert!(
                !listed.resources.contains(resource),
                "case {label}: {resource} was covered by the cancelled subscription, so no \
                 summary may still report it, got {:?}",
                listed.resources
            );
        }
    }
}

/// The `If-Match` values this suite has refused for a record whose current tag is
/// `current`: empty, a revision below any this service issues, one far above, a
/// non-numeric token derived from the current tag, and a long one.
fn stale_tags(current: &str) -> Vec<String> {
    vec![
        String::new(),
        "0".to_string(),
        "4096".to_string(),
        format!("{current}-stale"),
        "e".repeat(64),
    ]
}

/// The resource ARNs a `CreateSubscription` body names.
fn resource_arns(body: &Value, case: &str) -> Vec<String> {
    body["resourceArns"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: resourceArns missing from {body:?}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: {value:?} is not a resource ARN"))
                .to_string()
        })
        .collect()
}

/// What `ListSubscriptions` reports over every page: the subscriptions and the resources
/// their summaries say are covered.
struct Listed {
    arns: BTreeSet<String>,
    resources: BTreeSet<String>,
}

async fn listed_subscriptions(router: &MultiServiceRouter) -> Listed {
    let mut listed = Listed {
        arns: BTreeSet::new(),
        resources: BTreeSet::new(),
    };
    for page in pages(router).await {
        for summary in page {
            if let Some(arn) = summary["arn"].as_str() {
                listed.arns.insert(arn.to_string());
            }
            for resource in summary["resourceArns"].as_array().into_iter().flatten() {
                if let Some(resource) = resource.as_str() {
                    listed.resources.insert(resource.to_string());
                }
            }
        }
    }
    listed
}
