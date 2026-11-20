// SPDX-License-Identifier: CC0-1.0

//! WHERE a scaling activity says the instance it moved is, and WHICH milestone it reports
//! the move at.
//!
//! Three rules are under test here, all about the `Activity` record a group writes:
//!
//! 1. An activity about ONE instance states that instance's placement in
//!    `Activity$Details` ("the details about the activity",
//!    `smithy_query(query="doc",shape="Activity",member="Details")`): a JSON document whose
//!    members are the id of the Availability Zone the instance is in, the subnet it was
//!    placed in and the name of that zone, taken from where Amazon EC2 — the service that
//!    owns the placement — says the instance actually is. An activity about the GROUP
//!    carries no such document: there is no one placement for it to report.
//! 2. The `StatusCode`/`Progress` pair an activity that is still running reports is the
//!    milestone of the transition it records, from the `ScalingActivityStatusCode`
//!    vocabulary (`smithy_query(query="shape",shape="ScalingActivityStatusCode")`) — a
//!    return from `Standby` is at the pre-service step, a step a lifecycle hook is holding
//!    is waiting mid lifecycle action, a park is merely under way — and every one of them
//!    reads `Successful` at 100 once the clock has completed it.
//! 3. A standby move's `Cause` is the sentence Amazon EC2 Auto Scaling words for it: when
//!    the move began, which instance moved, that a user asked for it, and the capacities
//!    the group moved between when it moved any. An activity of an instance the group's own
//!    CONVERGENCE moved states two such clauses: the request that changed the capacity, and
//!    the step the group took to meet it.
//!
//! Every table below spans what its rule generalises over — how the group states its
//! placement, how many instances move, in which direction, and whether the move changes the
//! group's capacity — and every row is driven over the wire against Auto Scaling and Amazon
//! EC2 on ONE engine, so the placement an assertion compares against is the account's own.

use std::time::{Duration, Instant, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, list_members, p, query, seed_subnets,
    tag_value, tag_values, xml_unescape, REGION,
};

/// How long this suite waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every loop below looks AGAIN until the condition holds, so this
/// only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How far either side of a call an instant named in a sentence it produced may fall for
/// this suite to accept it as that call's own instant. Generous on purpose: the assertion is
/// about the VALUE being the call's instant, never about how fast the host ran.
const INSTANT_SLACK: Duration = Duration::from_secs(120);

/// What one scaling activity reports, read out of one `<member>` of an `Activities` list.
#[derive(Debug, Clone)]
struct ReportedActivity {
    activity_id: String,
    description: String,
    cause: String,
    status_code: String,
    progress: String,
    /// `Details`, or the empty string when the member is absent.
    details: String,
    /// `EndTime`, or the empty string for an activity that has not finished.
    end_time: String,
}

impl ReportedActivity {
    /// The instance this activity is about, as its description names it.
    fn instance_id(&self) -> String {
        self.description
            .rsplit_once(": ")
            .map(|(_, instance_id)| instance_id.to_string())
            .unwrap_or_default()
    }
}

/// Every activity an answer reports, in document order.
fn reported_activities(body: &str) -> Vec<ReportedActivity> {
    list_members(body, "Activities")
        .iter()
        .map(|block| ReportedActivity {
            activity_id: tag_value(block, "ActivityId"),
            description: tag_value(block, "Description"),
            cause: xml_unescape(&tag_value(block, "Cause")),
            status_code: tag_value(block, "StatusCode"),
            progress: tag_value(block, "Progress"),
            details: xml_unescape(&tag_value(block, "Details")),
            end_time: tag_value(block, "EndTime"),
        })
        .collect()
}

/// Read one group's scaling activities, newest first.
async fn activities_of(router: &MultiServiceRouter, group: &str) -> Vec<ReportedActivity> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeScalingActivities for {group} expected 200, got {status}: {body}"
    );
    reported_activities(&body)
}

/// Where Amazon EC2 says one instance is: the zone it is in and the subnet it was placed
/// in, read through that service's own `DescribeInstances`.
async fn ec2_placement(router: &MultiServiceRouter, instance_id: &str) -> (String, String) {
    let (status, body) = ec2_query(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_id} expected 200, got {status}: {body}"
    );
    (
        tag_value(&tag_value(&body, "placement"), "availabilityZone"),
        tag_value(&body, "subnetId"),
    )
}

/// The id Amazon EC2's own catalog gives the zone named `zone_name`.
fn zone_id_of(zone_name: &str) -> String {
    ec2::availability_zones_in_region(REGION)
        .into_iter()
        .find(|zone| zone.zone_name == zone_name)
        .map(|zone| zone.zone_id)
        .unwrap_or_else(|| panic!("the Region {REGION} must hold a zone named {zone_name}"))
}

/// The placement document an activity about an instance with this placement must state.
///
/// SOURCE: the `Activity.Details` document Amazon EC2 Auto Scaling writes for a launch, a
/// termination and a standby move — a JSON object whose members are `Availability Zone ID`,
/// `Subnet ID` and `Availability Zone`, in that order. A member the placement does not have
/// (an instance in no subnet) is left out rather than written empty, and a placement with
/// nothing at all is no document.
fn expected_details(zone: &str, subnet_id: &str) -> String {
    let zone_id = if zone.is_empty() {
        String::new()
    } else {
        zone_id_of(zone)
    };
    let members: Vec<String> = [
        ("Availability Zone ID", zone_id.as_str()),
        ("Subnet ID", subnet_id),
        ("Availability Zone", zone),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| format!("\"{key}\":\"{value}\""))
    .collect();
    if members.is_empty() {
        return String::new();
    }
    format!("{{{}}}", members.join(","))
}

/// The instant one clause of a cause names, and the sentence that follows it.
///
/// A clause reads `At <instant> <sentence>`, so the two are read apart: the sentence is
/// fixed text plus the request's and the group's own values, and the instant is when that
/// part of the cause happened.
fn cause_at_and_sentence(case: &str, cause: &str) -> (SystemTime, String) {
    let rest = cause
        .strip_prefix("At ")
        .unwrap_or_else(|| panic!("case {case:?}: a cause clause starts with the instant it \
                                  happened at, got {cause:?}"));
    let (instant, sentence) = rest.split_once(' ').unwrap_or_else(|| {
        panic!("case {case:?}: a cause clause states an instant and then what happened, got \
                {cause:?}")
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

/// Every clause of one cause, in the order it states them.
///
/// SOURCE: the recorded `Activity.Cause`, which states one clause per step and separates
/// two clauses with two spaces.
fn cause_clauses(case: &str, cause: &str) -> Vec<(SystemTime, String)> {
    cause
        .split("  ")
        .map(|clause| cause_at_and_sentence(case, clause))
        .collect()
}

/// Assert the instant a sentence names is the one the call that produced it ran at.
fn assert_instant_of_call(case: &str, named: SystemTime, call_started: SystemTime) {
    let earliest = call_started - INSTANT_SLACK;
    let latest = call_started + INSTANT_SLACK;
    assert!(
        named >= earliest && named <= latest,
        "case {case:?}: expected the cause to name the instant of the call it reports, got \
         {named:?} for a call at {call_started:?}"
    );
}

/// Create one Auto Scaling group over the wire.
async fn create_group(router: &MultiServiceRouter, params: &[(String, String)]) {
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Look again until the group reports exactly `wanted` instances in `state`, answering the
/// ids it last read.
async fn instances_in_state(
    router: &MultiServiceRouter,
    group: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let matching: Vec<String> = ids
            .iter()
            .zip(states.iter())
            .filter(|(_, held)| held.as_str() == state)
            .map(|(id, _)| id.clone())
            .collect();
        if matching.len() == wanted {
            return matching;
        }
        assert!(
            Instant::now() < deadline,
            "group {group} never reported {wanted} instance(s) in {state}; last read \
             {states:?} in {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the activity `activity_id` has finished, and answer it.
async fn finished_activity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    activity_id: &str,
) -> ReportedActivity {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = activities_of(router, group).await;
        let found = reported
            .iter()
            .find(|activity| activity.activity_id == activity_id)
            .cloned();
        if let Some(activity) = found.filter(|activity| !activity.end_time.is_empty()) {
            return activity;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the activity {activity_id} never finished; last read {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the placement table: how the group states where it launches.
struct PlacementRow {
    case: &'static str,
    /// How many subnets the group's `VPCZoneIdentifier` names; 0 states the zone instead.
    subnets: usize,
    /// How many instances the group holds.
    capacity: i32,
}

/// Every activity a group records for ONE of its instances states where Amazon EC2 put that
/// instance — for the launch that placed it and for the termination that took it away
/// alike, and for a group placed by subnet and one placed by zone alike.
#[tokio::test]
async fn an_activity_about_an_instance_states_where_that_instance_is() {
    let rows = [
        PlacementRow {
            case: "a group placed across two subnets in two zones",
            subnets: 2,
            capacity: 2,
        },
        PlacementRow {
            case: "a group placed by Availability Zone alone",
            subnets: 0,
            capacity: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the activities of one row
        // never explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let template = format!("lt-details-{index}");
        create_launch_template(&router, &template).await;

        let name = format!("asg-details-{index}");
        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", &template),
            p("MinSize", "0"),
            p("MaxSize", "4"),
            p("DesiredCapacity", row.capacity.to_string()),
        ];
        if row.subnets == 0 {
            params.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
        } else {
            let named: Vec<String> = subnets
                .iter()
                .take(row.subnets)
                .map(|subnet| subnet.id.clone())
                .collect();
            params.push(p("VPCZoneIdentifier", named.join(",")));
        }
        create_group(&router, &params).await;

        let held = usize::try_from(row.capacity).expect("a small capacity");
        let in_service = instances_in_state(&router, &name, "InService", held).await;

        // Where Amazon EC2 says each of them actually is, read before anything moves them.
        let mut placements: Vec<(String, String, String)> = Vec::new();
        for instance_id in &in_service {
            let (zone, subnet_id) = ec2_placement(&router, instance_id).await;
            assert!(
                !zone.is_empty(),
                "case {:?}: Amazon EC2 must report the zone it placed {instance_id} in",
                row.case
            );
            placements.push((instance_id.clone(), zone, subnet_id));
        }

        // The launch of each instance states that instance's own placement.
        let launches = activities_of(&router, &name).await;
        assert_eq!(
            launches.len(),
            held,
            "case {:?}: expected one activity per launched instance, got {launches:?}",
            row.case
        );
        for (instance_id, zone, subnet_id) in &placements {
            let activity = launches
                .iter()
                .find(|activity| activity.instance_id() == *instance_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: no activity names the launch of {instance_id}, got \
                         {launches:?}",
                        row.case
                    )
                });
            let expected = expected_details(zone, subnet_id);
            assert_eq!(
                activity.details, expected,
                "case {:?}: the launch of {instance_id} must state its placement, got {:?}",
                row.case, activity.details
            );
        }

        // And so does the termination a scale-in records for it.
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[("AutoScalingGroupName", name.as_str()), ("DesiredCapacity", "0")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: SetDesiredCapacity 0 expected 200, got {status}: {body}",
            row.case
        );
        let reported = activities_of(&router, &name).await;
        for (instance_id, zone, subnet_id) in &placements {
            let terminating = reported
                .iter()
                .filter(|activity| activity.description.starts_with("Terminating"))
                .find(|activity| activity.instance_id() == *instance_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: no activity names the termination of {instance_id}, got \
                         {reported:?}",
                        row.case
                    )
                });
            let expected = expected_details(zone, subnet_id);
            assert_eq!(
                terminating.details, expected,
                "case {:?}: the termination of {instance_id} must state its placement, got {:?}",
                row.case, terminating.details
            );
        }
    }
}

/// One row of the standby table.
struct StandbyRow {
    case: &'static str,
    /// How many instances the group holds before the park.
    capacity: i32,
    /// How many of them the request parks.
    parking: usize,
    /// Whether the park asks for the desired capacity to be decremented.
    decrement: bool,
    /// The capacity the group holds once the park is made, which is what the park's cause
    /// names as the capacity it moved TO and the return's cause as the one it moved FROM.
    after_park: i32,
    /// The capacity the return leaves the group with.
    after_return: i32,
}

/// A standby move reports the milestone of the move it is making, states where the instance
/// it moved is, and gives Amazon EC2 Auto Scaling's own reason for the move.
#[tokio::test]
async fn a_standby_move_reports_its_milestone_its_placement_and_its_cause() {
    let rows = [
        StandbyRow {
            case: "the only instance of the group, parked with the decrement",
            capacity: 1,
            parking: 1,
            decrement: true,
            after_park: 0,
            after_return: 1,
        },
        StandbyRow {
            case: "both instances of the group, parked with the decrement",
            capacity: 2,
            parking: 2,
            decrement: true,
            after_park: 0,
            after_return: 2,
        },
        StandbyRow {
            case: "the only instance of the group, parked WITHOUT the decrement",
            capacity: 1,
            parking: 1,
            decrement: false,
            // The capacity stands, so the park's cause names no change at all and the
            // return raises it from the capacity the replacement is holding.
            after_park: 1,
            after_return: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let template = format!("lt-standby-details-{index}");
        create_launch_template(&router, &template).await;

        let name = format!("asg-standby-details-{index}");
        create_group(
            &router,
            &[
                p("AutoScalingGroupName", &name),
                p("LaunchTemplate.LaunchTemplateName", &template),
                p("MinSize", "0"),
                p("MaxSize", "4"),
                p("DesiredCapacity", row.capacity.to_string()),
                p("VPCZoneIdentifier", subnets[0].id.clone()),
            ],
        )
        .await;
        let held = usize::try_from(row.capacity).expect("a small capacity");
        let in_service = instances_in_state(&router, &name, "InService", held).await;
        let parking: Vec<String> = in_service.iter().take(row.parking).cloned().collect();

        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p(
                "ShouldDecrementDesiredCapacity",
                if row.decrement { "true" } else { "false" },
            ),
        ];
        for (position, instance_id) in parking.iter().enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", position + 1),
                instance_id.clone(),
            ));
        }
        let parked_at = SystemTime::now();
        let (status, body) = query(&router, "EnterStandby", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: EnterStandby expected 200, got {status}: {body}",
            row.case
        );
        let parks = reported_activities(&body);
        assert_eq!(
            parks.len(),
            row.parking,
            "case {:?}: expected one activity per parked instance, got {parks:?}",
            row.case
        );
        for activity in &parks {
            let instance_id = activity.instance_id();
            // A park is under way and the service reports no finer step than that.
            assert_eq!(
                (activity.status_code.as_str(), activity.progress.as_str()),
                ("InProgress", "50"),
                "case {:?}: expected the park of {instance_id} to be under way, got {activity:?}",
                row.case
            );
            let (zone, subnet_id) = ec2_placement(&router, &instance_id).await;
            assert_eq!(
                activity.details,
                expected_details(&zone, &subnet_id),
                "case {:?}: the park of {instance_id} must state its placement, got {:?}",
                row.case,
                activity.details
            );
            // A park is the step the request itself asked for, so its cause is ONE clause.
            let clauses = cause_clauses(row.case, &activity.cause);
            assert_eq!(
                clauses.len(),
                1,
                "case {:?}: a park states the one step it is, got {:?}",
                row.case,
                activity.cause
            );
            let (named, sentence) = clauses[0].clone();
            assert_instant_of_call(row.case, named, parked_at);
            let expected = if row.after_park == row.capacity {
                format!("instance {instance_id} was moved to standby in response to a user request.")
            } else {
                format!(
                    "instance {instance_id} was moved to standby in response to a user request, \
                     shrinking the capacity from {} to {}.",
                    row.capacity, row.after_park
                )
            };
            assert_eq!(
                sentence, expected,
                "case {:?}: expected the standby sentence AWS words, got {sentence:?}",
                row.case
            );
        }

        // Every park reads back exactly as it was answered, and reaches the terminal pair
        // once the clock has completed it.
        for activity in &parks {
            let read = activities_of(&router, &name)
                .await
                .into_iter()
                .find(|reported| reported.activity_id == activity.activity_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the activity {} must be in the group's log",
                        row.case, activity.activity_id
                    )
                });
            assert_eq!(
                (read.details.as_str(), read.cause.as_str()),
                (activity.details.as_str(), activity.cause.as_str()),
                "case {:?}: a read of {} must report what the park answered, got {read:?}",
                row.case,
                activity.activity_id
            );
            let finished =
                finished_activity(&router, row.case, &name, &activity.activity_id).await;
            assert_eq!(
                (finished.status_code.as_str(), finished.progress.as_str()),
                ("Successful", "100"),
                "case {:?}: a completed park has succeeded, got {finished:?}",
                row.case
            );
        }

        // And the return: the instances come back, and each return reports the pre-service
        // milestone of an instance on its way into service.
        instances_in_state(&router, &name, "Standby", row.parking).await;
        let mut params = vec![p("AutoScalingGroupName", &name)];
        for (position, instance_id) in parking.iter().enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", position + 1),
                instance_id.clone(),
            ));
        }
        let returned_at = SystemTime::now();
        let (status, body) = query(&router, "ExitStandby", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: ExitStandby expected 200, got {status}: {body}",
            row.case
        );
        let returns = reported_activities(&body);
        assert_eq!(
            returns.len(),
            row.parking,
            "case {:?}: expected one activity per returned instance, got {returns:?}",
            row.case
        );
        for activity in &returns {
            let instance_id = activity.instance_id();
            assert_eq!(
                (activity.status_code.as_str(), activity.progress.as_str()),
                ("PreInService", "30"),
                "case {:?}: expected the return of {instance_id} to be pre-service, got \
                 {activity:?}",
                row.case
            );
            let (zone, subnet_id) = ec2_placement(&router, &instance_id).await;
            assert_eq!(
                activity.details,
                expected_details(&zone, &subnet_id),
                "case {:?}: the return of {instance_id} must state its placement, got {:?}",
                row.case,
                activity.details
            );
            let clauses = cause_clauses(row.case, &activity.cause);
            assert_eq!(
                clauses.len(),
                1,
                "case {:?}: a return states the one step it is, got {:?}",
                row.case,
                activity.cause
            );
            let (named, sentence) = clauses[0].clone();
            assert_instant_of_call(row.case, named, returned_at);
            assert_eq!(
                sentence,
                format!(
                    "instance {instance_id} was moved out of standby in response to a user \
                     request, increasing the capacity from {} to {}.",
                    row.after_park, row.after_return
                ),
                "case {:?}: expected the exit-standby sentence AWS words, got {sentence:?}",
                row.case
            );
        }

        // A return in the group's log reports the pre-service milestone until the clock
        // completes it, and the terminal pair after — never a milestone of another move.
        for activity in &returns {
            let read = activities_of(&router, &name)
                .await
                .into_iter()
                .find(|reported| reported.activity_id == activity.activity_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the activity {} must be in the group's log",
                        row.case, activity.activity_id
                    )
                });
            let expected = if read.end_time.is_empty() {
                ("PreInService", "30")
            } else {
                ("Successful", "100")
            };
            assert_eq!(
                (read.status_code.as_str(), read.progress.as_str()),
                expected,
                "case {:?}: a read of the return {} must report its own milestone, got {read:?}",
                row.case,
                activity.activity_id
            );
            let finished =
                finished_activity(&router, row.case, &name, &activity.activity_id).await;
            assert_eq!(
                (finished.status_code.as_str(), finished.progress.as_str()),
                ("Successful", "100"),
                "case {:?}: a completed return has succeeded, got {finished:?}",
                row.case
            );
        }
    }
}

/// A step a lifecycle hook is holding is at a milestone of its own: the activity reports
/// that it is waiting at the hold rather than that the transition is simply under way, and
/// reaches the terminal pair once the hold resolves and the step completes.
#[tokio::test]
async fn a_step_a_lifecycle_hook_holds_reports_the_mid_lifecycle_milestone() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_launch_template(&router, "lt-hook-milestone").await;

    let name = "asg-hook-milestone";
    create_group(
        &router,
        &[
            p("AutoScalingGroupName", name),
            p("LaunchTemplate.LaunchTemplateName", "lt-hook-milestone"),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "1"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
        ],
    )
    .await;
    let in_service = instances_in_state(&router, name, "InService", 1).await;
    let instance_id = in_service[0].clone();
    let (zone, subnet_id) = ec2_placement(&router, &instance_id).await;

    // A hook on the termination of an instance, so the scale-in below is held at it.
    let (status, body) = query(
        &router,
        "PutLifecycleHook",
        &[
            ("AutoScalingGroupName", name),
            ("LifecycleHookName", "hook-milestone"),
            (
                "LifecycleTransition",
                "autoscaling:EC2_INSTANCE_TERMINATING",
            ),
            ("HeartbeatTimeout", "300"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutLifecycleHook expected 200, got {status}: {body}"
    );

    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", name), ("DesiredCapacity", "0")],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetDesiredCapacity 0 expected 200, got {status}: {body}"
    );
    // The hook holds the instance at the step, which is what the activity is waiting on.
    instances_in_state(&router, name, "Terminating:Wait", 1).await;
    let reported = activities_of(&router, name).await;
    let held = reported
        .iter()
        .find(|activity| activity.description.starts_with("Terminating"))
        .unwrap_or_else(|| {
            panic!("the scale-in must have recorded a termination, got {reported:?}")
        });
    assert_eq!(
        (held.status_code.as_str(), held.progress.as_str()),
        ("MidLifecycleAction", "50"),
        "a step a hook is holding waits at the hold, got {held:?}"
    );
    assert_eq!(
        held.details,
        expected_details(&zone, &subnet_id),
        "the held termination must state the placement of {instance_id}, got {:?}",
        held.details
    );

    // And once the hold resolves the step runs on and completes.
    let (status, body) = query(
        &router,
        "CompleteLifecycleAction",
        &[
            ("AutoScalingGroupName", name),
            ("LifecycleHookName", "hook-milestone"),
            ("InstanceId", instance_id.as_str()),
            ("LifecycleActionResult", "CONTINUE"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CompleteLifecycleAction expected 200, got {status}: {body}"
    );
    let finished = finished_activity(
        &router,
        "the held termination, once the hold resolved",
        name,
        &held.activity_id,
    )
    .await;
    assert_eq!(
        (finished.status_code.as_str(), finished.progress.as_str()),
        ("Successful", "100"),
        "a termination whose hold resolved has succeeded, got {finished:?}"
    );
    assert_eq!(
        finished.details,
        expected_details(&zone, &subnet_id),
        "a completed termination still states the placement it reported, got {:?}",
        finished.details
    );
}

/// One row of the convergence-cause table: the door that asks the group for a capacity, and
/// the sentence Amazon EC2 Auto Scaling words for that request.
struct CauseRow {
    case: &'static str,
    /// The capacity the group is created with.
    created: i32,
    /// What the row then asks the group for, and the request clause that asking records.
    then: Asked,
}

/// How a row asks the group for its capacity after the create.
enum Asked {
    /// Nothing further: the create itself is the only capacity change.
    Nothing,
    /// `SetDesiredCapacity`, with the capacity it asks for.
    DesiredCapacity(i32),
    /// `UpdateAutoScalingGroup`, with the bounds and capacity it leaves the group with.
    Constraints { min: i32, max: i32, desired: i32 },
}

/// The clause AWS words for the request that created the group.
///
/// SOURCE: the recorded `Activity.Cause` of a launch, whose first clause names the request
/// that changed the capacity and the capacities it changed it between.
fn created_clause(to: i32) -> String {
    format!("a user request created an AutoScalingGroup changing the desired capacity from 0 to {to}.")
}

/// The clause AWS words for the step the group's own convergence took to meet the capacity
/// it was asked for.
///
/// SOURCE: the recorded `Activity.Cause` of a launch and of a termination, whose second
/// clause names the step and the difference it was closing.
fn convergence_clause(launched: bool, from: i32, to: i32) -> String {
    let (step, direction) = if launched {
        ("an instance was started", "increasing")
    } else {
        ("an instance was taken out of service", "shrinking")
    };
    format!(
        "{step} in response to a difference between desired and actual capacity, {direction} \
         the capacity from {from} to {to}."
    )
}

/// The activity of an instance the group's own convergence moved states BOTH clauses: what
/// asked for the capacity, and the step the group took to meet it.
#[tokio::test]
async fn a_convergence_activity_states_the_request_and_the_step_it_took() {
    let rows = [
        CauseRow {
            case: "the create itself launched the group's instance",
            created: 1,
            then: Asked::Nothing,
        },
        CauseRow {
            case: "a later request set the capacity higher",
            created: 1,
            then: Asked::DesiredCapacity(2),
        },
        CauseRow {
            case: "a later request set the capacity lower",
            created: 2,
            then: Asked::DesiredCapacity(1),
        },
        CauseRow {
            case: "a later request narrowed the group's own constraints",
            created: 2,
            then: Asked::Constraints {
                min: 0,
                max: 1,
                desired: 1,
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let template = format!("lt-cause-{index}");
        create_launch_template(&router, &template).await;

        let name = format!("asg-cause-{index}");
        let created_at = SystemTime::now();
        create_group(
            &router,
            &[
                p("AutoScalingGroupName", &name),
                p("LaunchTemplate.LaunchTemplateName", &template),
                p("MinSize", "0"),
                p("MaxSize", "4"),
                p("DesiredCapacity", row.created.to_string()),
                p("VPCZoneIdentifier", subnets[0].id.clone()),
            ],
        )
        .await;
        let held = usize::try_from(row.created).expect("a small capacity");
        instances_in_state(&router, &name, "InService", held).await;

        // Every instance the create launched states the create's clause and the step the
        // convergence took for it.
        for activity in activities_of(&router, &name).await {
            let clauses = cause_clauses(row.case, &activity.cause);
            assert_eq!(
                clauses.len(),
                2,
                "case {:?}: a convergence states the request and its own step, got {:?}",
                row.case,
                activity.cause
            );
            assert_instant_of_call(row.case, clauses[0].0, created_at);
            assert_instant_of_call(row.case, clauses[1].0, created_at);
            assert_eq!(
                clauses[0].1,
                created_clause(row.created),
                "case {:?}: expected the create's own clause, got {:?}",
                row.case,
                clauses[0].1
            );
            assert_eq!(
                clauses[1].1,
                convergence_clause(true, 0, row.created),
                "case {:?}: expected the launch step's clause, got {:?}",
                row.case,
                clauses[1].1
            );
        }

        let (asked_at, expected_request, expected_step) = match row.then {
            Asked::Nothing => continue,
            Asked::DesiredCapacity(desired) => {
                let at = SystemTime::now();
                let (status, body) = query(
                    &router,
                    "SetDesiredCapacity",
                    &[
                        ("AutoScalingGroupName", name.as_str()),
                        ("DesiredCapacity", desired.to_string().as_str()),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: SetDesiredCapacity expected 200, got {status}: {body}",
                    row.case
                );
                (
                    at,
                    format!(
                        "a user request explicitly set group desired capacity changing the \
                         desired capacity from {} to {desired}.",
                        row.created
                    ),
                    convergence_clause(desired > row.created, row.created, desired),
                )
            }
            Asked::Constraints { min, max, desired } => {
                let at = SystemTime::now();
                let (status, body) = query(
                    &router,
                    "UpdateAutoScalingGroup",
                    &[
                        ("AutoScalingGroupName", name.as_str()),
                        ("MinSize", min.to_string().as_str()),
                        ("MaxSize", max.to_string().as_str()),
                        ("DesiredCapacity", desired.to_string().as_str()),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}",
                    row.case
                );
                (
                    at,
                    format!(
                        "a user request update of AutoScalingGroup constraints to min: {min}, \
                         max: {max}, desired: {desired} changed the desired capacity from {} to \
                         {desired}.",
                        row.created
                    ),
                    convergence_clause(desired > row.created, row.created, desired),
                )
            }
        };

        // The newest activity is the step that change made, and it states the clause of the
        // request that asked for it beside the step's own.
        let reported = activities_of(&router, &name).await;
        let newest = reported
            .first()
            .unwrap_or_else(|| panic!("case {:?}: the change must record an activity", row.case));
        let clauses = cause_clauses(row.case, &newest.cause);
        assert_eq!(
            clauses.len(),
            2,
            "case {:?}: a convergence states the request and its own step, got {:?}",
            row.case,
            newest.cause
        );
        assert_instant_of_call(row.case, clauses[0].0, asked_at);
        assert_eq!(
            clauses[0].1, expected_request,
            "case {:?}: expected the request's own clause, got {:?}",
            row.case, clauses[0].1
        );
        assert_eq!(
            clauses[1].1, expected_step,
            "case {:?}: expected the convergence step's clause, got {:?}",
            row.case, clauses[1].1
        );
    }
}

/// The activity a group's own delete records is about the GROUP, so it states no placement:
/// there is no one instance for it to report the placement of.
#[tokio::test]
async fn the_activity_of_a_group_delete_states_no_placement() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_launch_template(&router, "lt-delete-details").await;

    let name = "asg-delete-details";
    create_group(
        &router,
        &[
            p("AutoScalingGroupName", name),
            p("LaunchTemplate.LaunchTemplateName", "lt-delete-details"),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "1"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
        ],
    )
    .await;
    instances_in_state(&router, name, "InService", 1).await;

    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", name), ("ForceDelete", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteAutoScalingGroup expected 200, got {status}: {body}"
    );

    let (status, body) = query(
        &router,
        "DescribeScalingActivities",
        &[
            ("AutoScalingGroupName", name),
            ("IncludeDeletedGroups", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeScalingActivities expected 200, got {status}: {body}"
    );
    let reported = reported_activities(&body);
    let delete = reported
        .iter()
        .find(|activity| activity.description.starts_with("Deleting the Auto Scaling group"))
        .unwrap_or_else(|| panic!("the delete must have recorded an activity, got {reported:?}"));
    assert!(
        delete.details.is_empty(),
        "the delete of the group states no placement, got {:?}",
        delete.details
    );
    // While the launch it recorded for the instance the group held does state that
    // instance's placement, so the empty document above is this activity's own subject and
    // not a member this service never writes. (A forced delete reports the whole group in
    // ONE activity; the terminations it performed are not activities of their own.)
    let launches: Vec<&ReportedActivity> = reported
        .iter()
        .filter(|activity| activity.description.starts_with("Launching"))
        .collect();
    assert!(
        !launches.is_empty(),
        "the group must have recorded the launch of the instance it held, got {reported:?}"
    );
    for activity in launches {
        let (zone, subnet_id) = ec2_placement(&router, &activity.instance_id()).await;
        assert_eq!(
            activity.details,
            expected_details(&zone, &subnet_id),
            "the launch of {} must state its placement, got {:?}",
            activity.instance_id(),
            activity.details
        );
    }
}
