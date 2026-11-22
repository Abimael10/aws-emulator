// SPDX-License-Identifier: CC0-1.0

//! The `Activity$Cause` of a termination a CALLER NAMED.
//!
//! THE RULE: `Activity$Cause` is "the reason the activity began"
//! (`smithy_query(query="doc",shape="Activity",member="Cause")`), and the activity of an
//! instance a REQUEST named states that instance and the move the caller asked for:
//! `At <instant> instance <instance-id> was taken out of service in response to a user
//! request`, with `, shrinking the capacity from <from> to <to>.` appended when the
//! group's desired capacity moved with the instance and the sentence ending at the period
//! when it did not. It is the same wording this service already answers a park a caller
//! named with (`instance <instance-id> was moved to standby in response to a user request,
//! shrinking the capacity from <from> to <to>.`, asserted in
//! `scaling_activity_placement_details.rs`) — one class of activity, an instance the
//! request named and moved at that request, so a termination reads like a park and differs
//! only in the move it states. What a group's own CONVERGENCE moved is a different class
//! and states the difference it was closing instead, which the suite above owns.
//!
//! The rows span what the rule generalises over: whether the request asked for the desired
//! capacity to be decremented (so whether the capacity clause is there at all), where the
//! capacity started and so which two numbers that clause names, how many instances the
//! request named and in which of the two forms it named them (`InstanceId`, and the
//! `InstanceIds` list, where EVERY activity names its own instance), and how many
//! instances the group holds beside the ones it names.
//!
//! Every row is driven over the awsQuery wire and reads the activity back through
//! `DescribeScalingActivities`, so what is asserted is the record the service STORED rather
//! than one call's answer.

use std::time::{Duration, Instant, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape,
};

/// The launch source every group here is created from: a launch TEMPLATE, because no
/// account can create a launch configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The zone every group here places its instances in.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// How long a row waits for the instances the group owes before calling it a failure.
///
/// A BACKSTOP, not a wait: the loop below looks AGAIN until the group reports them, so this
/// only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How far either side of a call an instant named in a sentence that call produced may fall
/// for this suite to accept it as that call's own instant.
///
/// Generous on purpose: the assertion is about the VALUE being the instant of the call,
/// never about how fast the host ran.
const INSTANT_SLACK: Duration = Duration::from_secs(300);

/// One row of the table: the group the row terminates an instance of, what the request
/// names and whether it asks for the decrement.
struct Row {
    case: &'static str,
    /// The capacity the group is created with, which is how many instances it holds.
    created: i32,
    /// How many of those instances the request names.
    named: usize,
    /// Whether it names them through `InstanceIds` (the list form) rather than through
    /// `InstanceId`.
    as_list: bool,
    /// Whether it asks for the group's desired capacity to be decremented.
    decrement: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the group's only instance, named by InstanceId, with the decrement",
        created: 1,
        named: 1,
        as_list: false,
        decrement: true,
    },
    Row {
        case: "the group's only instance, named by InstanceId, without the decrement",
        created: 1,
        named: 1,
        as_list: false,
        decrement: false,
    },
    Row {
        case: "one instance of three, named by InstanceId, with the decrement",
        created: 3,
        named: 1,
        as_list: false,
        decrement: true,
    },
    Row {
        case: "one instance named in the list form, with the decrement",
        created: 2,
        named: 1,
        as_list: true,
        decrement: true,
    },
    Row {
        case: "two instances of three named in the list form, with the decrement",
        created: 3,
        named: 2,
        as_list: true,
        decrement: true,
    },
    Row {
        case: "two instances of two named in the list form, without the decrement",
        created: 2,
        named: 2,
        as_list: true,
        decrement: false,
    },
];

/// The whole sentence AWS words the activity of an instance a caller named out of service
/// with, without the instant that precedes it.
///
/// Written out here rather than taken from the crate's own builder — a test that re-used the
/// implementation's literal would assert nothing. `moved` is the two capacities the group
/// went between, or `None` for a request that left its capacity where it was.
fn expected_sentence(instance_id: &str, moved: Option<(i32, i32)>) -> String {
    match moved {
        None => {
            format!("instance {instance_id} was taken out of service in response to a user request.")
        }
        Some((from, to)) => format!(
            "instance {instance_id} was taken out of service in response to a user request, \
             shrinking the capacity from {from} to {to}."
        ),
    }
}

/// The instant one clause of a cause names, and the sentence that follows it.
///
/// A clause reads `At <instant> <sentence>`: the instant is when that part of the cause
/// happened and the sentence is the fixed text plus the request's and the group's own
/// values, so the two are read apart.
fn clause_of(case: &str, cause: &str) -> (SystemTime, String) {
    let rest = cause.strip_prefix("At ").unwrap_or_else(|| {
        panic!("case {case:?}: a cause clause starts with the instant it happened at, got {cause:?}")
    });
    let (instant, sentence) = rest.split_once(' ').unwrap_or_else(|| {
        panic!("case {case:?}: a cause clause states an instant and then what happened, got {cause:?}")
    });
    let parsed = DateTime::from_str(instant, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {instant:?} is not an instant a client can read: {error}")
    });
    let seconds = u64::try_from(parsed.secs())
        .unwrap_or_else(|_| panic!("case {case:?}: {instant:?} predates the epoch"));
    (
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        sentence.to_string(),
    )
}

/// Create one group holding `capacity` instances and answer the ids it reports once every
/// one of them is in service.
async fn group_in_service(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    capacity: i32,
) -> Vec<String> {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", capacity.to_string()),
        p(ZONE.0, ZONE.1),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let wanted = usize::try_from(capacity).expect("a small capacity");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
        );
        let states = tag_values(&body, "LifecycleState");
        let ids = tag_values(&body, "InstanceId");
        if states.len() == wanted && states.iter().all(|state| state == "InService") {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} instances in service, got {states:?} for {ids:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `(description, cause)` of every activity an answer reports, in document order: a
/// request naming one instance answers with `Activity` and one naming a list with
/// `Activities`, and both are read here.
fn answered_activities(body: &str) -> Vec<(String, String)> {
    let blocks = list_members(body, "Activities");
    if blocks.is_empty() {
        return vec![(
            tag_value(body, "Description"),
            xml_unescape(&tag_value(body, "Cause")),
        )];
    }
    blocks
        .iter()
        .map(|block| {
            (
                tag_value(block, "Description"),
                xml_unescape(&tag_value(block, "Cause")),
            )
        })
        .collect()
}

/// The cause the activity log reports for `activity_id`, read through
/// `DescribeScalingActivities`.
async fn stored_cause(router: &MultiServiceRouter, case: &str, activity_id: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("ActivityIds.member.1", activity_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities for {activity_id} expected 200, got \
         {status}: {body}"
    );
    xml_unescape(&tag_value(&body, "Cause"))
}

#[tokio::test]
async fn a_termination_a_caller_named_states_that_instance_and_the_user_request() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One engine per row, so the instance ids and the activity log a row reads are its
        // own.
        let router = router_with(new_engine());
        let name = format!("asg-user-terminate-{index}");
        let held = group_in_service(&router, case, &name, row.created).await;
        let named: Vec<String> = held.iter().take(row.named).cloned().collect();
        assert_eq!(
            named.len(),
            row.named,
            "case {case:?}: the group must hold the {} instances the row names, got {held:?}",
            row.named
        );

        let mut params = vec![p(
            "ShouldDecrementDesiredCapacity",
            if row.decrement { "true" } else { "false" },
        )];
        if row.as_list {
            // The list form names the group as well: `InstanceIds` "requires that you also
            // specify AutoScalingGroupName".
            params.push(p("AutoScalingGroupName", name.clone()));
            for (position, instance_id) in named.iter().enumerate() {
                params.push(p(
                    &format!("InstanceIds.member.{}", position + 1),
                    instance_id.clone(),
                ));
            }
        } else {
            params.push(p("InstanceId", named[0].clone()));
        }

        let called_at = SystemTime::now();
        let (status, body) = query(
            &router,
            "TerminateInstanceInAutoScalingGroup",
            &borrowed(&params),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: TerminateInstanceInAutoScalingGroup expected 200, got {status}: {body}"
        );

        let answered = answered_activities(&body);
        assert_eq!(
            answered.len(),
            row.named,
            "case {case:?}: expected one activity per instance named, got {answered:?}"
        );
        // The capacity the request left the group with, which the sentence names only when
        // the request moved it.
        let taken = i32::try_from(row.named).expect("a small instance count");
        let moved = row
            .decrement
            .then_some((row.created, row.created - taken));

        for (instance_id, (description, cause)) in named.iter().zip(answered.iter()) {
            assert_eq!(
                *description,
                format!("Terminating EC2 instance: {instance_id}"),
                "case {case:?}: expected the activity of {instance_id}, got {description:?}"
            );
            // The termination is the step the request itself asked for, so its cause is ONE
            // clause: the group's own convergence took no step for it.
            assert!(
                !cause.contains("  "),
                "case {case:?}: a termination a caller named states the one step it is, got \
                 {cause:?}"
            );
            let (named_at, sentence) = clause_of(case, cause);
            assert!(
                named_at >= called_at - INSTANT_SLACK && named_at <= called_at + INSTANT_SLACK,
                "case {case:?}: expected the cause to name the instant of the call, got \
                 {named_at:?} for a call at {called_at:?}"
            );
            assert_eq!(
                sentence,
                expected_sentence(instance_id, moved),
                "case {case:?}: expected the sentence AWS words for an instance a user \
                 request took out of service, got {sentence:?}"
            );
            // It names the instance the request named and NOTHING else of the account: not
            // the group, and not another instance of it.
            assert!(
                !sentence.contains(name.as_str()),
                "case {case:?}: the sentence names no group, got {sentence:?}"
            );
            for other in &held {
                assert!(
                    other == instance_id || !sentence.contains(other.as_str()),
                    "case {case:?}: the activity of {instance_id} names only that instance, \
                     got {sentence:?}"
                );
            }
        }

        // And the log reports what the answer carried: the response reported the record the
        // service stored, so reading each activity back by its own id answers the same
        // cause.
        let ids = tag_values(&body, "ActivityId");
        assert_eq!(
            ids.len(),
            row.named,
            "case {case:?}: expected one activity id per instance named, got {ids:?}"
        );
        for (activity_id, (_, cause)) in ids.iter().zip(answered.iter()) {
            let stored = stored_cause(&router, case, activity_id).await;
            assert_eq!(
                stored.as_str(),
                cause.as_str(),
                "case {case:?}: the log must report the cause the answer carried for \
                 {activity_id}"
            );
        }
    }
}
