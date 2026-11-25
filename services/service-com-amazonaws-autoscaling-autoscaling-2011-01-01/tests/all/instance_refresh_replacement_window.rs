// SPDX-License-Identifier: CC0-1.0

//! The WINDOW an instance refresh replaces a group's instances inside.
//!
//! RULE under test: a refresh replaces the instances of its group ONE at a time, and the
//! healthy-percentage window it runs under decides the ORDER of the two moves each
//! replacement is made of. `MaxHealthyPercentage` "specifies the maximum percentage of the
//! group that can be in service and healthy, or pending, to support your workload when
//! replacing instances. The value is expressed as a percentage of the desired capacity of
//! the Auto Scaling group. Value range is 100 to 200"
//! (`smithy_query(query="doc",shape="RefreshPreferences",member="MaxHealthyPercentage")`),
//! so a refresh running under a ceiling ABOVE its group's desired capacity launches each
//! replacement before taking out the instance it replaces — the group, and the account's
//! own Amazon EC2 instances, SURGE above that capacity while the two exist side by side —
//! while a refresh with no such room takes the instance out first and launches its
//! replacement after, so the group DIPS below its desired capacity instead. Either way the
//! refresh ends `Successful` with the group serving its capacity from the replacements
//! alone.
//!
//! The rows span what that rule generalises over: how many instances the group holds (one,
//! two, three), where the ceiling comes from (the refresh's own preferences, or the
//! instance maintenance policy of the GROUP, which states the same pair of thresholds), the
//! ceiling's value (the 200 percent top of the published range, 150 in the middle, the 100
//! percent bottom that opens no room at all, and none stated) and the floor the refresh
//! runs under.
//!
//! Both services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling, ec2Query for Amazon EC2 — so what these assertions count is
//! the account's own instances. Nothing here sleeps for a span: every wait looks again
//! until the condition it needs holds, and the observations the assertions are made on are
//! taken by looking as often as a client can.

use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{ec2_query, emulator, p, query, tag_value, tag_values};

/// The launch template the groups in this suite launch from.
const LAUNCH_TEMPLATE: &str = "lt-refresh";

/// The zone the groups in this suite place their instances in.
const ZONE: &str = "us-west-2a";

/// How often a client that watches a refresh as closely as it can looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A BACKSTOP for every wait below, not a schedule: nothing in the emulator sleeps for a
/// due instant, so each wait looks again until the condition it needs holds. Generous on
/// purpose — these rows run beside every other suite — and derived from the service's own
/// spans, so this and the implementation cannot drift apart.
fn patience() -> Duration {
    autoscaling::state::INSTANCE_REFRESH_REPLACEMENT
        .max(autoscaling::state::LIFECYCLE_TRANSITION)
        * 120
}

/// The lifecycle states of a group's own members, as the model spells them.
const PENDING: &str = "Pending";
const IN_SERVICE: &str = "InService";

/// The Amazon EC2 instance states an instance the account is RUNNING carries: one on its
/// way up and one up. An instance Amazon EC2 reports any other way — `shutting-down`,
/// `terminated` — is one the account is no longer running.
const EC2_LIVE_STATES: &[&str] = &["pending", "running"];

/// Where the ceiling a refresh runs under comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ceiling {
    /// Nothing states one: neither the request nor the group.
    Unstated,
    /// `Preferences.MaxHealthyPercentage` on the request.
    Requested(i32),
    /// `InstanceMaintenancePolicy.MaxHealthyPercentage` on the GROUP, with the request
    /// stating no preferences at all.
    GroupPolicy(i32),
}

/// What a refresh under that window must be observed doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Window {
    /// It holds MORE instances than the group's desired capacity while it replaces, and
    /// never fewer in service than that capacity.
    Surge,
    /// It holds FEWER, exactly one instance out at a time, and never more than the
    /// capacity.
    InPlace,
}

/// One row of the window table.
struct Row {
    case: &'static str,
    /// How many instances the group holds when the refresh starts.
    capacity: i32,
    /// `Preferences.MinHealthyPercentage`, when the request states one.
    floor: Option<i32>,
    /// Where the ceiling comes from.
    ceiling: Ceiling,
    /// What the refresh must be observed doing.
    window: Window,
}

/// Every row of the table, each driven by its own test below so the rows run beside each
/// other rather than one after another.
const ROWS: &[Row] = &[
    Row {
        case: "two instances under the 200 percent top of the published ceiling range",
        capacity: 2,
        floor: Some(100),
        ceiling: Ceiling::Requested(200),
        window: Window::Surge,
    },
    Row {
        case: "two instances with no ceiling stated at all",
        capacity: 2,
        floor: Some(100),
        ceiling: Ceiling::Unstated,
        window: Window::InPlace,
    },
    Row {
        case: "three instances under a 75 percent floor and no ceiling",
        capacity: 3,
        floor: Some(75),
        ceiling: Ceiling::Unstated,
        window: Window::InPlace,
    },
    Row {
        case: "one instance whose GROUP states the ceiling the refresh runs under",
        capacity: 1,
        floor: None,
        ceiling: Ceiling::GroupPolicy(200),
        window: Window::Surge,
    },
    Row {
        case: "two instances under a 150 percent ceiling, the middle of the range",
        capacity: 2,
        floor: Some(50),
        ceiling: Ceiling::Requested(150),
        window: Window::Surge,
    },
    Row {
        case: "two instances under the 100 percent bottom of the range, which opens no room",
        capacity: 2,
        floor: Some(100),
        ceiling: Ceiling::Requested(100),
        window: Window::InPlace,
    },
];

/// What one look at the group and at the account's instances saw.
#[derive(Debug, Clone, Copy, Default)]
struct Observation {
    /// How many instances the group holds that count towards its capacity: the ones it
    /// reports `Pending` or `InService`.
    held: usize,
    /// How many of those are serving.
    serving: usize,
    /// How many of the instances this group has ever had Amazon EC2 is still running.
    live: usize,
}

/// Create one group holding `capacity` instances, and look again until every one of them is
/// serving.
async fn create_group(router: &MultiServiceRouter, name: &str, row: &Row) {
    let capacity = row.capacity.to_string();
    // The group's maximum leaves room for the replacement a surging refresh launches
    // before it takes out the instance that replacement stands in for.
    let max_size = (row.capacity + 1).to_string();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", max_size.as_str()),
        p("DesiredCapacity", capacity.as_str()),
        p("AvailabilityZones.member.1", ZONE),
    ];
    if let Ceiling::GroupPolicy(ceiling) = row.ceiling {
        // The pair is stated together: a maximum "requires MinHealthyPercentage"
        // (`smithy_query(query="doc",shape="RefreshPreferences",member="MaxHealthyPercentage")`),
        // and the group's own policy carries that same pair.
        params.push(p(
            "InstanceMaintenancePolicy.MinHealthyPercentage",
            "100".to_string(),
        ));
        params.push(p(
            "InstanceMaintenancePolicy.MaxHealthyPercentage",
            ceiling.to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
        row.case
    );
    let wanted = usize::try_from(row.capacity).expect("a capacity is a count");
    let deadline = tokio::time::Instant::now() + patience();
    loop {
        let members = group_members(router, name).await;
        let serving = members.iter().filter(|(_, state)| state == IN_SERVICE).count();
        if serving == wanted {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {:?}: the group must serve the capacity it was created with, last saw \
             {members:?}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The instances one group reports, each with the lifecycle state it is in.
async fn group_members(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
        .into_iter()
        .zip(tag_values(&body, "LifecycleState"))
        .collect()
}

/// What Amazon EC2 reports as the state of each of `instance_ids`, in the order it answers.
async fn ec2_states(router: &MultiServiceRouter, instance_ids: &[String]) -> Vec<String> {
    if instance_ids.is_empty() {
        return Vec::new();
    }
    let params: Vec<(String, String)> = instance_ids
        .iter()
        .enumerate()
        .map(|(index, instance_id)| p(&format!("InstanceId.{}", index + 1), instance_id.clone()))
        .collect();
    let (status, body) = ec2_query(router, "DescribeInstances", &params).await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_ids:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "instanceState")
        .iter()
        .map(|state| tag_value(state, "name"))
        .collect()
}

/// The `Status` of the newest refresh of one group.
async fn refresh_status(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceRefreshes for {name} expected 200, got {status}: {body}"
    );
    tag_value(&body, "Status")
}

/// Start the refresh the row asks for and answer the id it is tracked by.
async fn start_refresh(router: &MultiServiceRouter, name: &str, row: &Row) -> String {
    let mut params = vec![p("AutoScalingGroupName", name)];
    if let Some(floor) = row.floor {
        params.push(p("Preferences.MinHealthyPercentage", floor.to_string()));
    }
    if let Ceiling::Requested(ceiling) = row.ceiling {
        params.push(p("Preferences.MaxHealthyPercentage", ceiling.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(router, "StartInstanceRefresh", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {:?}: StartInstanceRefresh expected 200, got {status}: {body}",
        row.case
    );
    let instance_refresh_id = tag_value(&body, "InstanceRefreshId");
    assert!(
        !instance_refresh_id.is_empty(),
        "case {:?}: the answer must carry the refresh's id, got {body}",
        row.case
    );
    instance_refresh_id
}

/// Every row: the window the refresh runs under decides whether the group surges above its
/// desired capacity while it replaces or dips below it, and the refresh succeeds either
/// way.
async fn the_window_rule_holds_for(index: usize, row: &Row) {
    // One emulator per row: each row is its own account, so the instances of one row
    // never explain another's.
    let (router, _engine) = emulator();
    let name = format!("asg-refresh-window-{index}");
    create_group(&router, &name, row).await;
    let before: Vec<String> = group_members(&router, &name)
        .await
        .into_iter()
        .map(|(instance_id, _)| instance_id)
        .collect();

    let started = start_refresh(&router, &name, row).await;

    // What the refresh did, looked at as often as a client can until it reaches a
    // status it never leaves. Every id the group has ever reported is kept, so the
    // count of the account's own running instances covers the ones the refresh
    // launched and the ones it took out.
    let mut seen_ids: Vec<String> = before.clone();
    let mut observations: Vec<Observation> = Vec::new();
    let deadline = tokio::time::Instant::now() + patience();
    let status = loop {
        let members = group_members(&router, &name).await;
        for (instance_id, _) in &members {
            if !seen_ids.contains(instance_id) {
                seen_ids.push(instance_id.clone());
            }
        }
        let live = ec2_states(&router, &seen_ids)
            .await
            .iter()
            .filter(|state| EC2_LIVE_STATES.contains(&state.as_str()))
            .count();
        let status = refresh_status(&router, &name).await;
        if status == autoscaling::state::REFRESH_SUCCESSFUL {
            break status;
        }
        observations.push(Observation {
            held: members
                .iter()
                .filter(|(_, state)| state == PENDING || state == IN_SERVICE)
                .count(),
            serving: members.iter().filter(|(_, state)| state == IN_SERVICE).count(),
            live,
        });
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {:?}: expected the refresh {started} to succeed, last saw {status:?} \
             after {observations:?}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        status,
        autoscaling::state::REFRESH_SUCCESSFUL,
        "case {:?}: expected the refresh to have succeeded, got {status:?}",
        row.case
    );

    let capacity = usize::try_from(row.capacity).expect("a capacity is a count");
    let max_held = observations.iter().map(|seen| seen.held).max().unwrap_or(0);
    let min_held = observations
        .iter()
        .map(|seen| seen.held)
        .min()
        .unwrap_or(capacity);
    let max_live = observations.iter().map(|seen| seen.live).max().unwrap_or(0);
    let min_live = observations
        .iter()
        .map(|seen| seen.live)
        .min()
        .unwrap_or(capacity);
    let min_serving = observations
        .iter()
        .map(|seen| seen.serving)
        .min()
        .unwrap_or(capacity);
    match row.window {
        Window::Surge => {
            assert!(
                max_held > capacity,
                "case {:?}: a ceiling above the desired capacity lets the group hold more \
                 than {capacity} instances while it replaces, got at most {max_held} \
                 in {observations:?}",
                row.case
            );
            assert!(
                max_live > capacity,
                "case {:?}: and the account's own instances surge with it, got at most \
                 {max_live} running in {observations:?}",
                row.case
            );
            assert!(
                min_serving >= capacity,
                "case {:?}: a replacement launched before its instance is taken out keeps \
                 {capacity} serving throughout, got as few as {min_serving} \
                 in {observations:?}",
                row.case
            );
        }
        Window::InPlace => {
            assert_eq!(
                max_held, capacity,
                "case {:?}: with no room to surge into the group never holds more than \
                 {capacity} instances, got at most {max_held} in {observations:?}",
                row.case
            );
            assert_eq!(
                min_held,
                capacity - 1,
                "case {:?}: it takes exactly one instance out at a time, so it holds \
                 {} while a replacement is in flight, got as few as {min_held} \
                 in {observations:?}",
                row.case,
                capacity - 1
            );
            assert_eq!(
                min_live,
                capacity - 1,
                "case {:?}: and the account's own instances dip with it, got as few as \
                 {min_live} running in {observations:?}",
                row.case
            );
        }
    }

    // However it made room, the refresh leaves the group serving its capacity from the
    // replacements alone, and every instance it replaced is terminated where it lives.
    let after = group_members(&router, &name).await;
    let serving: Vec<String> = after
        .iter()
        .filter(|(_, state)| state == IN_SERVICE)
        .map(|(instance_id, _)| instance_id.clone())
        .collect();
    assert_eq!(
        serving.len(),
        capacity,
        "case {:?}: the group serves its capacity after the refresh, got {after:?}",
        row.case
    );
    assert!(
        serving.iter().all(|instance_id| !before.contains(instance_id)),
        "case {:?}: every instance of {before:?} had to be replaced, got {serving:?}",
        row.case
    );
    for (instance_id, state) in before.iter().zip(ec2_states(&router, &before).await) {
        assert_eq!(
            state, "terminated",
            "case {:?}: {instance_id} was replaced, so Amazon EC2 must report it \
             terminated, got {state:?}",
            row.case
        );
    }
}

/// One test per row, so the rows run beside each other: each drives the same rule over the
/// row it names.
#[tokio::test]
async fn the_top_of_the_ceiling_range_surges_the_group() {
    the_window_rule_holds_for(0, &ROWS[0]).await;
}

#[tokio::test]
async fn no_ceiling_at_all_replaces_in_place() {
    the_window_rule_holds_for(1, &ROWS[1]).await;
}

#[tokio::test]
async fn a_floor_below_the_capacity_still_replaces_in_place_with_no_ceiling() {
    the_window_rule_holds_for(2, &ROWS[2]).await;
}

#[tokio::test]
async fn the_ceiling_the_group_itself_states_surges_the_group() {
    the_window_rule_holds_for(3, &ROWS[3]).await;
}

#[tokio::test]
async fn the_middle_of_the_ceiling_range_surges_the_group() {
    the_window_rule_holds_for(4, &ROWS[4]).await;
}

#[tokio::test]
async fn the_bottom_of_the_ceiling_range_opens_no_room_and_replaces_in_place() {
    the_window_rule_holds_for(5, &ROWS[5]).await;
}

/// What STOPPED the refresh while one of its replacements was in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// `RollbackInstanceRefresh`: the group goes back to the configuration it was using,
    /// so the surge — capacity the group does not have to hold — goes back out and the
    /// instance it stood in for stays.
    Rollback,
    /// `CancelInstanceRefresh`: the refresh stops where it stands, so the replacement it was
    /// part way through is finished and the instance it stood in for goes out.
    Cancel,
}

impl Stop {
    /// The status the refresh reaches once the stop has come due.
    fn terminal_status(self) -> &'static str {
        match self {
            Self::Rollback => autoscaling::state::REFRESH_ROLLBACK_SUCCESSFUL,
            Self::Cancel => autoscaling::state::REFRESH_CANCELLED,
        }
    }

    /// The operation that stops it.
    fn action(self) -> &'static str {
        match self {
            Self::Rollback => "RollbackInstanceRefresh",
            Self::Cancel => "CancelInstanceRefresh",
        }
    }
}

/// The capacity the group in the stop rows is configured for.
const STOPPED_CAPACITY: i32 = 2;

/// RULE: stopping a surging refresh leaves the group holding EXACTLY the capacity it is
/// configured for. The replacement a surge has already launched is capacity the group holds
/// twice — the instance it stands in for is still in service — so a rollback, which is
/// putting the group back, takes that surge out again, while a cancellation finishes the
/// replacement and takes the older of the two out. Neither leaves the account running an
/// instance the group does not count.
async fn the_stop_leaves_the_capacity_exact(index: usize, stop: Stop) {
    let case = format!("{stop:?} mid-surge");
    let (router, _engine) = emulator();
    // The group names a NUMBERED launch-template version: a rollback restores the
    // configuration the group was using, and a floating version pins none to go back to.
    let template = format!("lt-refresh-stop-{index}");
    let (_, version) = crate::support::create_launch_template(&router, &template).await;
    let capacity = STOPPED_CAPACITY.to_string();
    let max_size = (STOPPED_CAPACITY + 1).to_string();
    let params = [
        p("AutoScalingGroupName", format!("asg-refresh-stop-{index}")),
        p("LaunchTemplate.LaunchTemplateName", template.clone()),
        p("LaunchTemplate.Version", version.clone()),
        p("MinSize", "0"),
        p("MaxSize", max_size.as_str()),
        p("DesiredCapacity", capacity.as_str()),
        p("AvailabilityZones.member.1", ZONE),
    ];
    let name = format!("asg-refresh-stop-{index}");
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let wanted = usize::try_from(STOPPED_CAPACITY).expect("a capacity is a count");
    let deadline = tokio::time::Instant::now() + patience();
    let before = loop {
        let members = group_members(&router, &name).await;
        let serving: Vec<String> = members
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .map(|(instance_id, _)| instance_id.clone())
            .collect();
        if serving.len() == wanted {
            break serving;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the group must serve the capacity it was created with, last saw \
             {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };

    // A refresh that surges, carrying the desired configuration a rollback needs to have
    // something to go back FROM.
    let refresh = [
        p("AutoScalingGroupName", name.clone()),
        p("Preferences.MinHealthyPercentage", "100"),
        p("Preferences.MaxHealthyPercentage", "200"),
        p(
            "DesiredConfiguration.LaunchTemplate.LaunchTemplateName",
            template.clone(),
        ),
        p("DesiredConfiguration.LaunchTemplate.Version", version),
    ];
    let borrowed: Vec<(&str, &str)> = refresh
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(&router, "StartInstanceRefresh", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartInstanceRefresh expected 200, got {status}: {body}"
    );

    // Stop it exactly while the surge is in flight: the group holds one instance more than
    // its capacity, and the instance that extra one stands in for is still serving.
    let deadline = tokio::time::Instant::now() + patience();
    let surged = loop {
        let members = group_members(&router, &name).await;
        let held: Vec<String> = members
            .iter()
            .filter(|(_, state)| state == PENDING || state == IN_SERVICE)
            .map(|(instance_id, _)| instance_id.clone())
            .collect();
        if held.len() == wanted + 1 {
            break held;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the refresh must surge above {wanted} instances, last saw \
             {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    let (status, body) = query(
        &router,
        stop.action(),
        &[("AutoScalingGroupName", name.as_str())],
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} expected 200, got {status}: {body}",
        stop.action()
    );

    // Once the stop has come due, the group holds exactly its capacity — never the surge as
    // well — and the account runs exactly that many of the instances this group ever had.
    let deadline = tokio::time::Instant::now() + patience();
    let mut seen_ids = surged.clone();
    for instance_id in &before {
        if !seen_ids.contains(instance_id) {
            seen_ids.push(instance_id.clone());
        }
    }
    loop {
        let members = group_members(&router, &name).await;
        for (instance_id, _) in &members {
            if !seen_ids.contains(instance_id) {
                seen_ids.push(instance_id.clone());
            }
        }
        let status = refresh_status(&router, &name).await;
        let serving = members
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .count();
        if status == stop.terminal_status() && members.len() == wanted && serving == wanted {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected the refresh {:?} with {wanted} instance(s) serving, \
             last saw {status:?} and {members:?}",
            stop.terminal_status()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let live = ec2_states(&router, &seen_ids)
        .await
        .iter()
        .filter(|state| EC2_LIVE_STATES.contains(&state.as_str()))
        .count();
    assert_eq!(
        live, wanted,
        "case {case:?}: the account runs exactly the {wanted} instance(s) the group holds, \
         got {live} of {seen_ids:?} running"
    );
    // And a stopped refresh is terminal, so the group is free to be refreshed again.
    let (status, body) = query(
        &router,
        "StartInstanceRefresh",
        &[("AutoScalingGroupName", name.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be refreshable again, got {status}: {body}"
    );
}

#[tokio::test]
async fn rolling_back_a_surging_refresh_takes_the_surge_out_again() {
    the_stop_leaves_the_capacity_exact(0, Stop::Rollback).await;
}

#[tokio::test]
async fn cancelling_a_surging_refresh_finishes_the_replacement_in_flight() {
    the_stop_leaves_the_capacity_exact(1, Stop::Cancel).await;
}

/// The instance type version 1 of the template below launches, and the one version 2
/// launches: two members of Amazon EC2's own instance-type catalog, so which configuration
/// an instance was launched from is a fact EC2 reports rather than a string this suite
/// invents.
const FIRST_TYPE: &str = "t3.micro";
const NEXT_TYPE: &str = "t3.small";

/// The AMI the versions below carry — one of the account's published AMIs, built for the
/// architecture those instance types run.
const IMAGE_ID: &str = "ami-00000000000000031";

/// Create a launch template running `FIRST_TYPE` and add a second version running
/// `NEXT_TYPE`, over Amazon EC2's own wire, and answer the two version numbers EC2 minted.
async fn two_version_template(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", FIRST_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate for {name} expected 200, got {status}: {body}"
    );
    let first = tag_value(&body, "latestVersionNumber");
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateName", name),
            p("SourceVersion", first.clone()),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", NEXT_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplateVersion for {name} expected 200, got {status}: {body}"
    );
    let next = tag_value(&body, "versionNumber");
    assert!(
        !first.is_empty() && !next.is_empty(),
        "Amazon EC2 must report both version numbers of {name}, got {first:?} and {next:?}"
    );
    (first, next)
}

/// The instance type Amazon EC2 reports for one instance.
async fn ec2_instance_type(router: &MultiServiceRouter, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_id} expected 200, got {status}: {body}"
    );
    tag_value(&body, "instanceType")
}

/// RULE, the other half of the launch-source half of the plan: a ROLLBACK replaces the
/// refresh's replacements back from the GROUP's own launch source — the configuration it
/// was using before the refresh started, since a desired configuration reaches the group's
/// settings only once the refresh has succeeded — and a cancellation of that rollback,
/// which `CancelInstanceRefresh` serves ("cancels an instance refresh or rollback that is
/// in progress"), finishes the replacement the ROLLBACK was part way through: from that same
/// restored launch source, never from the configuration the rollback is undoing.
#[tokio::test]
async fn cancelling_a_rollback_finishes_its_replacement_from_the_restored_launch_source() {
    let case = "cancel of a rollback mid-replacement";
    let (router, _engine) = emulator();
    let template = "lt-refresh-cancelled-rollback";
    let (first_version, next_version) = two_version_template(&router, template).await;
    let name = "asg-refresh-cancelled-rollback";
    // The group is PINNED to the first version, which is the configuration a rollback
    // restores; the refresh moves it to the second.
    let params = [
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("LaunchTemplate.Version", first_version.clone()),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "1"),
        p("AvailabilityZones.member.1", ZONE),
    ];
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let deadline = tokio::time::Instant::now() + patience();
    let before = loop {
        let members = group_members(&router, name).await;
        if let Some((instance_id, _)) = members.iter().find(|(_, state)| state == IN_SERVICE) {
            break instance_id.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the group must serve its one instance, last saw {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    let launched_type = ec2_instance_type(&router, &before).await;
    assert_eq!(
        launched_type, FIRST_TYPE,
        "case {case:?}: the group's instance runs the version it is pinned to, got \
         {launched_type:?}"
    );

    // A refresh moving the group to the second version. It states no ceiling, so it replaces
    // IN PLACE: the instance goes out first and its replacement is launched after, which is
    // the gap the stop below has to fill.
    let refresh = [
        p("AutoScalingGroupName", name),
        p("Preferences.MinHealthyPercentage", "100"),
        p(
            "DesiredConfiguration.LaunchTemplate.LaunchTemplateName",
            template.to_string(),
        ),
        p(
            "DesiredConfiguration.LaunchTemplate.Version",
            next_version.clone(),
        ),
    ];
    let borrowed: Vec<(&str, &str)> = refresh
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = query(&router, "StartInstanceRefresh", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartInstanceRefresh expected 200, got {status}: {body}"
    );

    // Once the refresh has made its one replacement, the group serves an instance running
    // the configuration the refresh was moving it to — the instance a rollback has to
    // replace BACK.
    let deadline = tokio::time::Instant::now() + patience();
    let replaced_by = loop {
        let members = group_members(&router, name).await;
        let serving = members
            .iter()
            .find(|(instance_id, state)| state == IN_SERVICE && *instance_id != before)
            .map(|(instance_id, _)| instance_id.clone());
        if let Some(instance_id) = serving {
            break instance_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the refresh must replace the group's instance, last saw \
             {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    let replaced_type = ec2_instance_type(&router, &replaced_by).await;
    assert_eq!(
        replaced_type, NEXT_TYPE,
        "case {case:?}: the refresh's replacement runs the configuration it is moving the \
         group to, got {replaced_type:?}"
    );

    let (status, body) = query(
        &router,
        "RollbackInstanceRefresh",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RollbackInstanceRefresh expected 200, got {status}: {body}"
    );

    // Cancel it exactly while ITS replacement is in flight: the rollback has taken the
    // refresh's replacement out and has not launched the instance that puts the group back
    // yet, so the group holds nothing that counts towards its capacity.
    let deadline = tokio::time::Instant::now() + patience();
    loop {
        let members = group_members(&router, name).await;
        let held = members
            .iter()
            .filter(|(_, state)| state == PENDING || state == IN_SERVICE)
            .count();
        if held == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the rollback must take the refresh's replacement out, last saw \
             {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let (status, body) = query(
        &router,
        "CancelInstanceRefresh",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CancelInstanceRefresh expected 200, got {status}: {body}"
    );

    // The cancellation finishes the replacement the ROLLBACK was making: the group serves
    // its capacity again from an instance running the version it is pinned to, not the one
    // the refresh was taking it to.
    let deadline = tokio::time::Instant::now() + patience();
    let settled = loop {
        let members = group_members(&router, name).await;
        let serving: Vec<String> = members
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .map(|(instance_id, _)| instance_id.clone())
            .collect();
        let status = refresh_status(&router, name).await;
        if status == autoscaling::state::REFRESH_CANCELLED && serving.len() == 1 {
            break serving;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected the cancelled rollback to leave one instance serving, \
             last saw {status:?} and {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    let restored = settled
        .first()
        .cloned()
        .expect("the group's one serving instance");
    assert_ne!(
        restored, replaced_by,
        "case {case:?}: the instance the rollback took out is not the one the group serves"
    );
    let restored_type = ec2_instance_type(&router, &restored).await;
    assert_eq!(
        restored_type, FIRST_TYPE,
        "case {case:?}: the replacement the cancelled rollback launched comes from the \
         group's own launch source, got {restored_type:?}"
    );
}
