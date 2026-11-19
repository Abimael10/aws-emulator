// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyInstanceEventStartTime`, driven over the wire the service
//! actually speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test is that the operation acts on an event THE INSTANCE HAS: the instance
//! is resolved first, through the malformed-before-not-found pair this service reads an
//! instance id with, and the event id is then resolved against the scheduled events that
//! instance reports — the one reading of them this crate has, the one
//! `DescribeInstanceStatus` answers from. Amazon EC2 schedules those events itself, for
//! maintenance it has planned, and no API of this service creates one, so an instance of this
//! store has none: every event id therefore names no event of the instance it was asked
//! about, and that is what the refusal says.
//!
//! The rows differ along what the rule generalises over — which id cannot be resolved, which
//! form it takes, and whether the request is a dry run — so no row is the specification.

use ec2::state::TRANSITION_DELAY;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The ec2Query action under test.
const ACTION: &str = "ModifyInstanceEventStartTime";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// The HTTP status EC2 answers a `DryRun` request with.
const DRY_RUN_STATUS: u16 = 412;

/// An instance event id of the form EC2 issues one: no instance of this store has an event
/// scheduled against it, so this names none of them.
const EVENT_ID: &str = "instance-event-0abcdef1234567890";

/// The new start time every call here asks for, well past any event's own deadline.
const NOT_BEFORE: &str = "2030-01-01T00:00:00Z";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl AsRef<str>) -> (String, String) {
    (key.to_string(), value.as_ref().to_string())
}

/// Launch one instance and answer its id.
async fn launch(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// Rule: an event id names no event of an instance this store holds, because nothing
/// schedules one — the same answer `DescribeInstanceStatus` gives by reporting the instance
/// with no event at all.
#[tokio::test]
async fn an_event_the_instance_does_not_have_is_refused() {
    let case = "no scheduled event";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;

    // What the one reading of an instance's events reports: none, which is why the id below
    // resolves to nothing.
    let (status, body) = call(
        &router,
        REGION,
        "DescribeInstanceStatus",
        &[p("InstanceId.1", &instance_id), p("IncludeAllInstances", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceStatus must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let reported_event = parsed
        .descendants()
        .find(|node| node.is_element() && child_text(*node, "instanceEventId").is_some());
    assert!(
        reported_event.is_none(),
        "case {case:?}: no event is scheduled against an instance of this store: {body}"
    );

    // The rows differ in WHICH id cannot be resolved and in the form it takes: an event of an
    // instance that is there, a well-formed instance id the account holds nothing for, a
    // value that is no instance id at all, and the all-zero placeholder.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "an event id the instance has no event under",
            instance_id.clone(),
            EVENT_ID,
            "InvalidInstanceEventIDNotFound",
        ),
        (
            "a well-formed instance id the account holds no instance for",
            "i-0123456789abcdef0".to_string(),
            EVENT_ID,
            "InvalidInstanceID.NotFound",
        ),
        (
            "a value that is no instance id at all",
            "instance-one".to_string(),
            EVENT_ID,
            "InvalidInstanceID.Malformed",
        ),
        (
            "the all-zero instance id",
            "i-00000000000000000".to_string(),
            EVENT_ID,
            "InvalidInstanceID.Malformed",
        ),
    ];

    for (label, instance, event, code_wanted) in rows {
        let (status, body) = call(
            &router,
            REGION,
            ACTION,
            &[
                p("InstanceId", &instance),
                p("InstanceEventId", event),
                p("NotBefore", NOT_BEFORE),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {label:?}: expected {code_wanted}, got {code:?} in {body}"
        );
    }
}

/// Rule: the refusal of an event the instance does not have is AWS's fixed sentence for that
/// class — it names neither the event nor the instance, so the same call made for two
/// different events reads the same way.
#[tokio::test]
async fn the_refusal_is_the_fixed_sentence_for_that_class() {
    let case = "the sentence";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;

    for event in [EVENT_ID, "instance-event-00000000000000000"] {
        let (status, body) = call(
            &router,
            REGION,
            ACTION,
            &[
                p("InstanceId", &instance_id),
                p("InstanceEventId", event),
                p("NotBefore", NOT_BEFORE),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {event}, got {status} with body {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, "Cannot find event",
            "case {case:?}: expected the fixed sentence for {event}, got {message:?}"
        );
        assert!(
            !message.contains(event) && !message.contains(instance_id.as_str()),
            "case {case:?}: the sentence names neither the event nor the instance, got \
             {message:?}"
        );
    }
}

/// Rule: a dry run asks whether the request WOULD have succeeded, so one that would have been
/// refused is refused with that reason instead — the event the instance does not have — while
/// one whose instance id cannot be resolved is refused for the id.
#[tokio::test]
async fn a_dry_run_carries_the_reason_the_request_would_have_been_refused_with() {
    let case = "dry run";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(case, &router).await;
    // The span this crate's clock-owed moves take, waited for by polling nothing: the
    // instance is usable as soon as the launch answered, so there is no condition to wait on
    // here — named only so the row below reads as deliberate rather than racing a transition.
    let _ = TRANSITION_DELAY;

    for (label, instance, code_wanted) in [
        ("the instance it holds", instance_id.clone(), "InvalidInstanceEventIDNotFound"),
        (
            "an instance it does not hold",
            "instance-one".to_string(),
            "InvalidInstanceID.Malformed",
        ),
    ] {
        let (status, body) = call(
            &router,
            REGION,
            ACTION,
            &[
                p("InstanceId", &instance),
                p("InstanceEventId", EVENT_ID),
                p("NotBefore", NOT_BEFORE),
                p("DryRun", "true"),
            ],
        )
        .await;
        assert_ne!(
            status, DRY_RUN_STATUS,
            "case {label:?}: a request that would have been refused must carry that reason, \
             got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {label:?}: expected {code_wanted}, got {code:?} in {body}"
        );
    }
}
