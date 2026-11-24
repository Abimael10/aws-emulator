// SPDX-License-Identifier: CC0-1.0

//! The suspend / record-capacity / resume cycle of an Auto Scaling group's `Launch` process,
//! driven against the emulator a customer STARTS: the built binary, on its own port, with
//! request authentication on, reached over TLS with SigV4-signed calls.
//!
//! RULE: a group's `DesiredCapacity` is the capacity it RECORDS and MAINTAINS, and `Launch` is
//! the process that adds instances to it — "if you suspend `Launch`, your Auto Scaling group
//! does not add instances" — so while that process is suspended the group accepts every
//! capacity change, reports the capacity asked for and adds NO member, reporting the suspension
//! itself as a `SuspendedProcess` of that process name with the reason it was suspended for;
//! and once the process is running again the group launches the shortfall between the capacity
//! it holds and the capacity its members provide, each new member coming to rest in the
//! `LifecycleState` of a member in service, with no further call from anybody. The fixed text
//! in these projections is the member and enum names themselves; the variable parts are the
//! group name the request carried and the instance ids and capacity numbers the group's own
//! state produced.
//!
//! The sibling suites in the service crate pin that rule in process, on a router assembled
//! inside the test binary with authentication off: `all/suspended_launch_capacity_intent.rs`
//! pins the capacity a withheld change records and `all/resumed_launch_brings_members_into_service.rs`
//! pins the members a resumption brings into service. The emulator a client RUNS is a different
//! configuration of the same code — the calls are signed and verified, the group's launch source
//! is an Amazon EC2 launch template this process created, its placement is made of subnets of
//! the account's own default VPC, and every lifecycle transition is settled on the binary's own
//! runtime by a task that outlives the request that armed the clock — so this suite is the one
//! that pins the whole cycle THERE: a cycle that completes in process and stalls on the started
//! emulator is a failure a test reports rather than one only a client polling the group sees.
//!
//! The table spans what the rule generalises over: HOW MANY units the withheld change records,
//! WHICH writer records them (`SetDesiredCapacity`, `UpdateAutoScalingGroup`), how WIDE the
//! group's placement is (one subnet, two), how the resumption NAMES the process (`Launch`
//! itself, no process at all — which names every process) and a second suspension the
//! resumption must leave exactly where it is. Starting the binary costs what starting the
//! emulator costs, so every row is driven against ONE started emulator, each row with a group of
//! its own. Every wait is on a CONDITION under a generous deadline; nothing here asserts how
//! fast the emulator answers.

use std::time::{Duration, Instant};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, elements, items_of, opt_element};

/// The launch template every group of this suite launches from, and the image and instance type
/// it names.
const TEMPLATE: &str = "emu-started-asg-suspend";
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The process that adds instances to a group, and the `LifecycleState` of a member that has
/// come into service, spelled out here rather than imported from the service crate so a wrong
/// constant in the implementation cannot make this suite pass.
const LAUNCH: &str = "Launch";
const IN_SERVICE: &str = "InService";

/// How long any one condition is waited for, and how often it is looked at. Generous next to the
/// span one launch of this emulator takes, because what is waited for is the CONDITION and this
/// is only the backstop that turns a condition which never holds into a failed assertion.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How many times a row LOOKS AGAIN at a group whose `Launch` process is suspended before it
/// accepts that no member is appearing. A re-read loop rather than a measurement of time: each
/// look reads the group again and asserts what it reports, so the row fails on the first member
/// the group adds while the process is suspended.
const WITHHELD_LOOKS: usize = 10;

/// One Amazon EC2 call over its ec2Query wire, which must answer 200.
fn ec2(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("ec2", "2016-11-15", action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}"
    );
    body
}

/// One Auto Scaling call over its awsQuery wire, which must answer 200 — the empty success the
/// model gives `SuspendProcesses`, `ResumeProcesses` and `SetDesiredCapacity`.
fn autoscaling(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("autoscaling", "2011-01-01", action, params);
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got HTTP {status} with {body}; the emulator's \
         log:\n{}",
        emulator.log()
    );
    body
}

/// Two subnets of the account's own default VPC, discovered the way a client that created no
/// network of its own discovers them.
fn default_vpc_subnets(emulator: &Started) -> Vec<String> {
    let vpcs = ec2(
        emulator,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
    );
    let vpc = element(&vpcs, "vpcId");
    let subnets = ec2(
        emulator,
        "DescribeSubnets",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
    );
    items_of(&subnets)
        .into_iter()
        .filter_map(|item| elements(item, "subnetId").into_iter().next())
        .collect()
}

/// What one group reports: the capacity it holds, the `LifecycleState` of each member, the
/// members' ids, the processes it has suspended and the reason given for the first of them.
struct Reported {
    capacity: String,
    states: Vec<String>,
    ids: Vec<String>,
    suspended: Vec<String>,
    first_reason: Option<String>,
}

/// Read `group` back over the wire.
fn reported(emulator: &Started, case: &str, group: &str) -> Reported {
    let body = autoscaling(
        emulator,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    );
    Reported {
        capacity: element(&body, "DesiredCapacity"),
        states: elements(&body, "LifecycleState"),
        ids: elements(&body, "InstanceId"),
        suspended: elements(&body, "ProcessName"),
        first_reason: opt_element(&body, "SuspensionReason"),
    }
}

/// How one row's withheld capacity is recorded while `Launch` is suspended.
enum Recorded {
    /// `SetDesiredCapacity`, naming the capacity.
    DesiredCapacity(i32),
    /// `UpdateAutoScalingGroup`, naming the capacity.
    UpdatedCapacity(i32),
}

/// One row: what the group suspends, how the withheld capacity is recorded, how wide its
/// placement is, what the resumption names, and what the group then holds.
struct Row {
    case: &'static str,
    /// The processes suspended before the capacity change, in request order.
    suspend: Vec<&'static str>,
    recorded: Recorded,
    /// How many of the discovered subnets the group is placed over.
    subnets: usize,
    /// The processes the resumption names — empty for a request that names none, which names
    /// every process.
    resume: Vec<&'static str>,
    /// The members the group comes to rest holding, every one of them in service.
    expected_members: usize,
    /// The capacity the group holds throughout, stated rather than derived from the member
    /// count.
    expected_capacity: i32,
    /// The processes the group still reports suspended after the resumption.
    still_suspended: Vec<&'static str>,
}

/// One `SuspendProcesses` or `ResumeProcesses` call naming `processes`.
fn process_call(
    emulator: &Started,
    case: &str,
    action: &str,
    group: &str,
    processes: &[&str],
) {
    let members: Vec<(String, String)> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| {
            (
                format!("ScalingProcesses.member.{}", index + 1),
                (*process).to_string(),
            )
        })
        .collect();
    let mut params: Vec<(&str, &str)> = vec![("AutoScalingGroupName", group)];
    for (name, value) in &members {
        params.push((name.as_str(), value.as_str()));
    }
    autoscaling(emulator, case, action, &params);
}

/// RULE: on the emulator a client starts, a group whose `Launch` process is suspended records
/// every capacity change and adds no member, and fills the capacity it recorded once that
/// process is running again, each member coming to rest in service.
#[test]
fn a_suspended_launch_group_records_its_capacity_and_fills_it_once_resumed() {
    let emulator = Started::start();
    let subnets = default_vpc_subnets(&emulator);
    assert!(
        subnets.len() >= 2,
        "the account's default VPC must publish two subnets to place a group over, got \
         {subnets:?}"
    );
    ec2(
        &emulator,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    );

    let rows = [
        Row {
            case: "one unit set outright, Launch itself resumed, two subnets",
            suspend: vec![LAUNCH],
            recorded: Recorded::DesiredCapacity(1),
            subnets: 2,
            resume: vec![LAUNCH],
            expected_members: 1,
            expected_capacity: 1,
            still_suspended: Vec::new(),
        },
        Row {
            case: "two units updated, the resumption naming no process, one subnet",
            suspend: vec![LAUNCH],
            recorded: Recorded::UpdatedCapacity(2),
            subnets: 1,
            resume: Vec::new(),
            expected_members: 2,
            expected_capacity: 2,
            still_suspended: Vec::new(),
        },
        Row {
            case: "Launch alone resumed beside a suspension the request does not name",
            suspend: vec![LAUNCH, "Terminate"],
            recorded: Recorded::DesiredCapacity(1),
            subnets: 1,
            resume: vec![LAUNCH],
            expected_members: 1,
            expected_capacity: 1,
            still_suspended: vec!["Terminate"],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let group = format!("emu-started-asg-suspend-{index}");
        let placement = subnets[..row.subnets].join(",");
        autoscaling(
            &emulator,
            case,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", &group),
                ("MinSize", "0"),
                ("MaxSize", "2"),
                ("DesiredCapacity", "0"),
                ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
                ("VPCZoneIdentifier", &placement),
                ("HealthCheckGracePeriod", "0"),
            ],
        );

        // ---- the suspension the group reports ----
        process_call(&emulator, case, "SuspendProcesses", &group, &row.suspend);
        let suspended = reported(&emulator, case, &group);
        let mut expected_suspended: Vec<String> =
            row.suspend.iter().map(|name| (*name).to_string()).collect();
        expected_suspended.sort();
        let mut named = suspended.suspended.clone();
        named.sort();
        assert_eq!(
            named, expected_suspended,
            "case {case:?}: expected the group to report the suspended processes \
             {expected_suspended:?}, got {:?}",
            suspended.suspended
        );
        assert!(
            suspended
                .first_reason
                .as_ref()
                .is_some_and(|reason| !reason.is_empty()),
            "case {case:?}: a suspended process is reported with the reason it was suspended \
             for, got {:?}",
            suspended.first_reason
        );

        // ---- the capacity it records while the process is suspended, and the members it does
        // not add for it ----
        let expected_capacity = row.expected_capacity.to_string();
        match &row.recorded {
            Recorded::DesiredCapacity(capacity) => autoscaling(
                &emulator,
                case,
                "SetDesiredCapacity",
                &[
                    ("AutoScalingGroupName", &group),
                    ("DesiredCapacity", &capacity.to_string()),
                ],
            ),
            Recorded::UpdatedCapacity(capacity) => autoscaling(
                &emulator,
                case,
                "UpdateAutoScalingGroup",
                &[
                    ("AutoScalingGroupName", &group),
                    ("DesiredCapacity", &capacity.to_string()),
                ],
            ),
        };
        // Looked at AGAIN rather than read once: the group is STEADY while the process is
        // suspended, so every look reports the capacity it recorded and no member at all.
        for look in 0..WITHHELD_LOOKS {
            let withheld = reported(&emulator, case, &group);
            assert_eq!(
                withheld.capacity, expected_capacity,
                "case {case:?}, look {look}: expected the group to hold the capacity the \
                 withheld change recorded, got {:?} with the members {:?}",
                withheld.capacity, withheld.ids
            );
            assert!(
                withheld.states.is_empty() && withheld.ids.is_empty(),
                "case {case:?}, look {look}: a group whose {LAUNCH} process is suspended adds \
                 no member, got the states {:?} for {:?}",
                withheld.states,
                withheld.ids
            );
            std::thread::sleep(LOOK_INTERVAL);
        }

        // ---- and the capacity it fills once the process is running again ----
        process_call(&emulator, case, "ResumeProcesses", &group, &row.resume);
        let giving_up_at = Instant::now() + DEADLINE;
        let settled = loop {
            let settling = reported(&emulator, case, &group);
            if settling.states.len() == row.expected_members
                && settling.states.iter().all(|state| state == IN_SERVICE)
            {
                break settling;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected the resumed group to hold {} member(s), every one of \
                 them {IN_SERVICE}, inside {DEADLINE:?}, got the states {:?} for {:?} at \
                 capacity {:?}; the emulator's log:\n{}",
                row.expected_members,
                settling.states,
                settling.ids,
                settling.capacity,
                emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        };

        assert_eq!(
            settled.capacity, expected_capacity,
            "case {case:?}: the resumption leaves the recorded capacity where it was, got {:?} \
             with the states {:?}",
            settled.capacity, settled.states
        );
        assert_eq!(
            settled.ids.len(),
            row.expected_members,
            "case {case:?}: expected {} member id(s), got {:?}",
            row.expected_members,
            settled.ids
        );
        let mut distinct = settled.ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            settled.ids.len(),
            "case {case:?}: every member of the group is its own instance, got {:?}",
            settled.ids
        );
        let mut still = settled.suspended.clone();
        still.sort();
        let mut expected_still: Vec<String> = row
            .still_suspended
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        expected_still.sort();
        assert_eq!(
            still, expected_still,
            "case {case:?}: the resumption turns off the suspensions it names and leaves the \
             others, got {:?}",
            settled.suspended
        );

        // The members the group reports are the account's OWN Amazon EC2 instances: every id it
        // answered resolves in the account's Amazon EC2 state.
        let described = ec2(
            &emulator,
            "DescribeInstances",
            &[("InstanceId.1", &settled.ids[0])],
        );
        assert!(
            described.contains(&settled.ids[0]),
            "case {case:?}: the member {:?} the group reports must resolve in Amazon EC2, got \
             {described}",
            settled.ids[0]
        );
    }
}
