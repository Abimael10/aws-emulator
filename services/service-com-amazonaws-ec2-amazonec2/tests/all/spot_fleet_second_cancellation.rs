// SPDX-License-Identifier: CC0-1.0

//! The SECOND cancellation of a Spot Fleet request, driven over the wire the service speaks
//! (`ec2Query`, through the crate's shared [`super::ec2_query`] harness).
//!
//! The rule under test: a Spot Fleet request can be cancelled while there is still something to
//! cancel, and `CancelSpotFleetRequests` reports the outcome PER FLEET — so a fleet whose first
//! cancellation left its instances running is cancelled again, its id appearing in
//! `SuccessfulFleetRequests` with the state it was in and the one `TerminateInstances` moves it
//! to. That is the two-step cancel the operation documents: "otherwise, the Spot Fleet request
//! enters the `cancelled_running` state and the instances continue to run until they are
//! interrupted or you terminate them manually", so the later cancel with `TerminateInstances`
//! set is what takes those instances down and drives the fleet to its terminal state.
//!
//! `fleetRequestNotInCancellableState` is reserved for a fleet whose cancellation HAS been
//! carried out: once the instances are being terminated, and once the fleet is `cancelled`,
//! there is nothing left to ask for.
//!
//! The rows span what the rule generalises over: which `TerminateInstances` the second cancel
//! carries (the same as the first, so nothing moves; and the other, so the fleet moves on), and
//! which already-cancelled state the refusal is reported from.

use std::time::Duration;

use ec2::state::{
    CANCEL_BATCH_FLEET_REQUEST_NOT_IN_CANCELLABLE_STATE, ComAmazonawsEc2Amazonec2State as Ec2State,
    SPOT_FLEET_REQUEST_CANCELLATION_NOT_FOUND_MESSAGE, SPOT_FLEET_STATE_CANCELLED,
    SPOT_FLEET_STATE_CANCELLED_RUNNING, SPOT_FLEET_STATE_CANCELLED_TERMINATING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, indexed};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The ARN of the IAM role a Spot Fleet request grants the fleet.
const FLEET_ROLE: &str = "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the deadline
/// the polls below give up at. Generous, because what is under test is the state a fleet SETTLES
/// in rather than how fast it gets there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll below waits between two looks at the fleet.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `SpotFleetRequestConfig.<member>` parameter.
fn c(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("SpotFleetRequestConfig.{member}"), value)
}

/// An AMI of the published catalog a fresh account can already see, read out of the service's
/// OWN state: a launch specification must name an image the account can resolve.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// Request one Spot Fleet of `target_capacity` units over one pool of `instance_type`, as the
/// fleet id it minted.
async fn requested_fleet(
    case: &str,
    router: &MultiServiceRouter,
    target_capacity: i64,
    instance_type: &str,
) -> String {
    let params: Params = vec![
        c("IamFleetRole", FLEET_ROLE),
        c("TargetCapacity", target_capacity.to_string()),
        c("LaunchSpecifications.1.ImageId", image_id()),
        c("LaunchSpecifications.1.InstanceType", instance_type),
    ];
    let (status, body) = call(router, REGION, "RequestSpotFleet", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RequestSpotFleet, got {status}: {body}"
    );
    let parsed = document(case, &body);
    child_text(parsed.root_element(), "spotFleetRequestId")
        .unwrap_or_else(|| panic!("case {case:?}: no spotFleetRequestId in {body}"))
}

/// One cancellation of `fleet_id`, as its status and body.
async fn cancel(
    router: &MultiServiceRouter,
    fleet_id: &str,
    terminate_instances: bool,
) -> (u16, String) {
    let mut params = indexed("SpotFleetRequestId", &[fleet_id]);
    params.push(p(
        "TerminateInstances",
        if terminate_instances { "true" } else { "false" },
    ));
    call(router, REGION, "CancelSpotFleetRequests", &params).await
}

/// One entry of `SuccessfulFleetRequests`: the fleet, the state it was in and the one it is in.
#[derive(Debug)]
struct Cancelled {
    id: String,
    previous: String,
    current: String,
}

/// Every fleet a cancellation reports as cancelled, in document order.
fn cancelled(case: &str, body: &str) -> Vec<Cancelled> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "successfulFleetRequestSet")
        .into_iter()
        .map(|item| Cancelled {
            id: child_text(item, "spotFleetRequestId").unwrap_or_default(),
            previous: child_text(item, "previousSpotFleetRequestState").unwrap_or_default(),
            current: child_text(item, "currentSpotFleetRequestState").unwrap_or_default(),
        })
        .collect()
}

/// One entry of `UnsuccessfulFleetRequests`: the fleet that was not cancelled, and why.
#[derive(Debug)]
struct NotCancelled {
    id: String,
    code: String,
    message: String,
}

/// Every fleet a cancellation reports as not cancelled, in document order.
fn not_cancelled(case: &str, body: &str) -> Vec<NotCancelled> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "unsuccessfulFleetRequestSet")
        .into_iter()
        .map(|item| NotCancelled {
            id: child_text(item, "spotFleetRequestId").unwrap_or_default(),
            code: child(item, "error")
                .and_then(|error| child_text(error, "code"))
                .unwrap_or_default(),
            message: child(item, "error")
                .and_then(|error| child_text(error, "message"))
                .unwrap_or_default(),
        })
        .collect()
}

/// The `BatchState` a describe reports for one fleet.
async fn described_state(case: &str, router: &MultiServiceRouter, fleet_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSpotFleetRequests",
        &indexed("SpotFleetRequestId", &[fleet_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSpotFleetRequests, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let item = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "spotFleetRequestState").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no fleet reported for {fleet_id} in {body}"));
    child_text(item, "spotFleetRequestState").unwrap_or_default()
}

/// Look again until `fleet_id` is reported in `wanted`, or a generous deadline passes. The
/// CONDITION is what is waited for, never a span of the clock.
async fn poll_until_state(case: &str, router: &MultiServiceRouter, fleet_id: &str, wanted: &str) {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let state = described_state(case, router, fleet_id).await;
        if state == wanted {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {fleet_id} must reach {wanted:?}, and it reported {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `fleet_id` reports at least one instance of its own, or a generous deadline
/// passes — the state in which a cancellation that leaves the instances running has something
/// left to cancel. Answers the instances the fleet is running.
async fn poll_until_running(case: &str, router: &MultiServiceRouter, fleet_id: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = call(
            router,
            REGION,
            "DescribeSpotFleetInstances",
            &[p("SpotFleetRequestId", fleet_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSpotFleetInstances, got {status}: \
             {body}"
        );
        let parsed = document(case, &body);
        let instances: Vec<String> = parsed
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "instanceId")
            .map(|node| node.text().unwrap_or_default().to_string())
            .collect();
        if !instances.is_empty() {
            return instances;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {fleet_id} must place an instance, and it reports none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many `fleetRequestChange` events the fleet's own history carries — one per `BatchState`
/// it ENTERS, so a call that asks it for the state it is already in must add none.
///
/// The whole history is read, from the Unix epoch on, so nothing the fleet has done is outside
/// the window.
async fn fleet_change_events(case: &str, router: &MultiServiceRouter, fleet_id: &str) -> usize {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSpotFleetRequestHistory",
        &[
            p("SpotFleetRequestId", fleet_id),
            p("StartTime", "1970-01-01T00:00:00Z"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSpotFleetRequestHistory, got {status}: \
         {body}"
    );
    elements(&body, "eventType")
        .into_iter()
        .filter(|event_type| event_type == "fleetRequestChange")
        .count()
}

/// One row of the second-cancellation table.
struct Row {
    case: &'static str,
    target_capacity: i64,
    instance_type: &'static str,
    /// What the SECOND cancel asks for.
    terminate_instances: bool,
    /// The state that cancel must report the fleet moving to.
    expected_current: &'static str,
}

/// A fleet whose cancellation left its instances running is cancellable again, and the second
/// cancel reports the state it was in beside the one it moves to.
#[tokio::test]
async fn a_cancelled_running_fleet_is_cancelled_again() {
    let rows = [
        Row {
            case: "the second cancel leaves the instances running too",
            target_capacity: 1,
            instance_type: "t3.micro",
            terminate_instances: false,
            expected_current: SPOT_FLEET_STATE_CANCELLED_RUNNING,
        },
        Row {
            case: "the second cancel terminates the instances",
            target_capacity: 2,
            instance_type: "t3.small",
            terminate_instances: true,
            expected_current: SPOT_FLEET_STATE_CANCELLED_TERMINATING,
        },
    ];

    for row in &rows {
        let Row {
            case,
            target_capacity,
            instance_type,
            terminate_instances,
            expected_current,
        } = row;
        let (router, _engine) = super::ec2_query::emulator();
        let fleet = requested_fleet(case, &router, *target_capacity, instance_type).await;
        // The fleet has to be RUNNING instances for the first cancellation to leave anything
        // behind, so it is polled for rather than waited out.
        poll_until_running(case, &router, &fleet).await;

        let (status, body) = cancel(&router, &fleet, false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first cancellation must succeed, got {status}: {body}"
        );
        let first = cancelled(case, &body);
        assert_eq!(
            first.len(),
            1,
            "case {case:?}: the first cancellation must report the fleet cancelled, got {first:?}"
        );
        assert_eq!(
            first[0].current, SPOT_FLEET_STATE_CANCELLED_RUNNING,
            "case {case:?}: a cancellation that leaves the instances running moves the fleet to \
             {SPOT_FLEET_STATE_CANCELLED_RUNNING:?}, got {first:?}"
        );

        // The fleet keeps its instances, so it stays in that state: it is the second cancel
        // that moves it, which is what this row is about.
        let changes_before = fleet_change_events(case, &router, &fleet).await;
        let (status, body) = cancel(&router, &fleet, *terminate_instances).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second cancellation must succeed, got {status}: {body}"
        );
        let failed = not_cancelled(case, &body);
        assert!(
            failed.is_empty(),
            "case {case:?}: expected no unsuccessful cancellation, got {failed:?}"
        );
        let second = cancelled(case, &body);
        assert_eq!(
            second.len(),
            1,
            "case {case:?}: the second cancellation must report the fleet, got {second:?}"
        );
        assert_eq!(
            second[0].id, fleet,
            "case {case:?}: the entry must name the fleet the request named, got {second:?}"
        );
        assert_eq!(
            second[0].previous, SPOT_FLEET_STATE_CANCELLED_RUNNING,
            "case {case:?}: the entry must report the state the fleet was in, got {second:?}"
        );
        assert_eq!(
            second[0].current, *expected_current,
            "case {case:?}: expected the fleet moved to {expected_current:?}, got {second:?}"
        );
        assert_eq!(
            described_state(case, &router, &fleet).await,
            *expected_current,
            "case {case:?}: a describe must report the state the cancellation answered"
        );

        // One event per state the fleet ENTERS: the cancel that moved it records one, and the
        // one that asked for the state it was already in records none — so a caller repeating
        // the call cannot grow what the fleet retains.
        let changes_after = fleet_change_events(case, &router, &fleet).await;
        let expected_changes = changes_before + usize::from(*terminate_instances);
        assert_eq!(
            changes_after, expected_changes,
            "case {case:?}: expected {expected_changes} fleet-change events after the second \
             cancel, got {changes_after}"
        );
        // And repeating the no-op cancel any number of times still records nothing.
        if !*terminate_instances {
            for repeat in 0..3 {
                let (status, body) = cancel(&router, &fleet, false).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: repeat {repeat} must be accepted, got {status}: {body}"
                );
                let reported = cancelled(case, &body);
                assert_eq!(
                    reported.len(),
                    1,
                    "case {case:?}: repeat {repeat} must report the fleet, got {reported:?}"
                );
            }
            let after_repeats = fleet_change_events(case, &router, &fleet).await;
            assert_eq!(
                after_repeats, changes_after,
                "case {case:?}: a cancel that changes nothing must record nothing, got \
                 {after_repeats} events where the fleet had {changes_after}"
            );
        }

        // A cancel that terminates the instances drives the fleet to its TERMINAL state once
        // they are gone, which is what the documented second step is for.
        if *terminate_instances {
            poll_until_state(case, &router, &fleet, SPOT_FLEET_STATE_CANCELLED).await;
        }
    }
}

/// An id the account holds no fleet for is the OTHER per-fleet outcome, and it carries AWS's own
/// fixed sentence for a cancellation that found no fleet — which names no id, the entry's own
/// `SpotFleetRequestId` doing that.
#[tokio::test]
async fn an_id_the_account_holds_no_fleet_for_is_reported_per_fleet() {
    let case = "an id the account holds no fleet for";
    let (router, _engine) = super::ec2_query::emulator();
    // One fleet that CAN be cancelled, named in the same batch: the per-fleet error must not
    // stop it being cancelled.
    let held = requested_fleet(case, &router, 1, "t3.micro").await;

    for unknown in [
        "sfr-00000000-0000-0000-0000-00000000ffff",
        "sfr-0000ffff-0000-0000-0000-000000000001",
    ] {
        let mut params = indexed("SpotFleetRequestId", &[unknown, held.as_str()]);
        params.push(p("TerminateInstances", "false"));
        let (status, body) = call(&router, REGION, "CancelSpotFleetRequests", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-fleet error does not refuse the call, got {status}: {body}"
        );
        let failed = not_cancelled(case, &body);
        assert_eq!(
            failed.len(),
            1,
            "case {case:?}: expected exactly one unsuccessful fleet for {unknown}, got {failed:?}"
        );
        assert_eq!(
            failed[0].id, unknown,
            "case {case:?}: the entry must name the id the request sent, got {failed:?}"
        );
        assert_eq!(
            failed[0].message, SPOT_FLEET_REQUEST_CANCELLATION_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the fixed sentence a cancellation that found no fleet \
             carries, got {failed:?}"
        );
        assert!(
            !failed[0].message.contains(unknown),
            "case {case:?}: the sentence names no id, got {failed:?}"
        );
    }
}

/// The other half of the rule: once the cancellation HAS been carried out there is nothing left
/// to ask for, so a further cancel is reported per fleet with
/// `fleetRequestNotInCancellableState` and the state the fleet is in.
#[tokio::test]
async fn a_fleet_whose_cancellation_was_carried_out_is_reported_not_cancellable() {
    let case = "a fleet cancelled with its instances";
    let (router, _engine) = super::ec2_query::emulator();
    let fleet = requested_fleet(case, &router, 1, "t3.micro").await;
    // An instance to terminate, so the fleet really passes through the state in which its
    // termination is under way rather than going straight to the terminal one.
    poll_until_running(case, &router, &fleet).await;
    let (status, body) = cancel(&router, &fleet, true).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the cancellation must succeed, got {status}: {body}"
    );

    // Both states the cancellation passes through once it has been carried out: the instances
    // being terminated, and the fleet with none left.
    for expected_state in [
        SPOT_FLEET_STATE_CANCELLED_TERMINATING,
        SPOT_FLEET_STATE_CANCELLED,
    ] {
        poll_until_state(case, &router, &fleet, expected_state).await;
        let (status, body) = cancel(&router, &fleet, true).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-fleet error does not refuse the call, got {status}: {body}"
        );
        assert!(
            cancelled(case, &body).is_empty(),
            "case {case:?}: nothing was cancelled, got {body}"
        );
        let failed = not_cancelled(case, &body);
        assert_eq!(
            failed.len(),
            1,
            "case {case:?}: expected exactly one unsuccessful fleet, got {failed:?}"
        );
        assert_eq!(
            failed[0].id, fleet,
            "case {case:?}: the entry must name the fleet the request named, got {failed:?}"
        );
        assert_eq!(
            failed[0].code, CANCEL_BATCH_FLEET_REQUEST_NOT_IN_CANCELLABLE_STATE,
            "case {case:?}: expected the code \
             {CANCEL_BATCH_FLEET_REQUEST_NOT_IN_CANCELLABLE_STATE:?}, got {failed:?}"
        );
        assert_eq!(
            failed[0].message,
            format!(
                "The spot fleet request '{fleet}' is in the state '{expected_state}' and cannot \
                 be canceled"
            ),
            "case {case:?}: the message names the fleet and the state it is in, got {failed:?}"
        );
    }
}
