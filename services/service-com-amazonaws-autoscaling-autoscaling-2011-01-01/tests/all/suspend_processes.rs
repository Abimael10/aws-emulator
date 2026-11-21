// SPDX-License-Identifier: CC0-1.0

//! `SuspendProcesses`: the scaling processes one Auto Scaling group has turned off.
//!
//! The operation "suspends the specified auto scaling processes, or all processes, for the
//! specified Auto Scaling group"
//! (`smithy_query(query="operation",operation="SuspendProcesses")`), its modelled output is
//! `Unit`, and "if you omit this property, all processes are specified"
//! (`ScalingProcessQuery$ScalingProcesses`). The processes a request may name are the
//! service's own catalog — the one `DescribeScalingProcessTypes` publishes — so this suite
//! reads that catalog over the wire rather than writing it out, and a name outside it is
//! refused with a 400 `ValidationError` that leaves the group's suspensions as they were.
//!
//! What a suspension IS, as the group reports it: one `SuspendedProcess` per suspended
//! process, carrying the process name and the reason it is suspended for
//! (`smithy_query(query="shape",shape="SuspendedProcess")`), read back through
//! `DescribeAutoScalingGroups`. A process is either suspended or it is not, so naming one
//! twice — in one request or in a later one — leaves ONE suspension, and the instant its
//! reason states does not move when it is named again.
//!
//! What a suspension DOES: `Launch` is the process that adds instances to the group when it
//! scales out and `Terminate` is the one that removes them when it scales in, so while
//! either is suspended a capacity change records the capacity the caller asked for while
//! the group moves no instance for the half that process does. Suspending any OTHER process
//! leaves both halves running.
//!
//! The tables below span what those rules generalise over: how many processes a request
//! names (none, one, several, the same one twice, the whole published catalog), which ones,
//! the order it names them in, the kind of bad name, and the capacity trajectory a group
//! with each suspension is driven along. Every row goes over the awsQuery wire through one
//! router.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values,
    xml_unescape,
};

/// The launch source every group here is created from: a launch TEMPLATE, because no
/// account can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The fixed text of a suspension reason; what follows it is the instant the process was
/// suspended at.
const REASON_PREFIX: &str = "User suspended at ";

/// The processes the service publishes as its own catalog, read over the wire from
/// `DescribeScalingProcessTypes` — the same set a request may name — so no row here writes
/// that membership out.
async fn published_catalog(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeScalingProcessTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published process catalog: expected 200, got {status}: {body}"
    );
    let catalog = tag_values(&body, "ProcessName");
    assert!(
        catalog.len() > 1,
        "the published process catalog must name more than one process, got {catalog:?}"
    );
    catalog
}

/// Create one group placed in the zones of `subnet_ids`, holding `desired_capacity`
/// instances.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    subnet_ids: &str,
    desired_capacity: usize,
) {
    let desired = desired_capacity.to_string();
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", desired.as_str()),
            ("VPCZoneIdentifier", subnet_ids),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the group {name:?}: expected 200, got {status}: {body}"
    );
}

/// `SuspendProcesses` for one group, naming the processes `processes` lists (naming none
/// when it is empty).
async fn suspend(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
    processes: &[String],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            process.clone(),
        ));
    }
    query(router, "SuspendProcesses", &borrowed(&params)).await
}

/// The suspensions one group reports, as `(ProcessName, SuspensionReason)` pairs in the
/// order `DescribeAutoScalingGroups` reports them.
async fn reported_suspensions(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the group {group:?} back: expected 200, got {status}: {body}"
    );
    let names = tag_values(&body, "ProcessName");
    let reasons = tag_values(&body, "SuspensionReason");
    assert_eq!(
        names.len(),
        reasons.len(),
        "every suspension reports a process and a reason, got {names:?} and {reasons:?} \
         from {body}"
    );
    names.into_iter().zip(reasons).collect()
}

/// A suspension reason states that a caller suspended the process, and the instant they
/// did: RFC 3339 UTC to the second, which this re-parses to prove it is an instant rather
/// than text.
fn assert_reason_shape(case: &str, process: &str, reason: &str) {
    let instant = reason.strip_prefix(REASON_PREFIX).unwrap_or_else(|| {
        panic!("case {case:?}: {process} reports {reason:?}, which does not open with {REASON_PREFIX:?}")
    });
    assert!(
        ::aws_smithy_types::DateTime::from_str(
            instant,
            ::aws_smithy_types::date_time::Format::DateTime,
        )
        .is_ok(),
        "case {case:?}: {process} reports the instant {instant:?}, which is not an RFC 3339 \
         date-time"
    );
    assert!(
        !instant.contains('.'),
        "case {case:?}: {process} reports the instant {instant:?}, which carries a \
         sub-second part the member states to the second"
    );
}

/// What a row's request names in `ScalingProcesses`.
enum Named {
    /// The member is left out altogether, which names every process.
    Omitted,
    /// The processes written here, in this order.
    Processes(Vec<&'static str>),
    /// Every process the service publishes, named explicitly.
    WholePublishedCatalog,
}

/// One row of the suspension table.
struct SuspensionCase {
    case: &'static str,
    named: Named,
    /// The processes the group then reports suspended, in the ASCII order it reports them;
    /// `None` for a row that suspends the whole published catalog.
    expected: Option<Vec<&'static str>>,
}

/// A suspension is recorded against the group it names, for exactly the processes the
/// request named — or for the whole published catalog when it named none — and read back
/// through `DescribeAutoScalingGroups`.
#[tokio::test]
async fn suspend_processes_records_the_processes_the_request_names() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;

    let cases = [
        SuspensionCase {
            case: "no process named: every published process is suspended",
            named: Named::Omitted,
            expected: None,
        },
        SuspensionCase {
            case: "the whole published catalog named explicitly",
            named: Named::WholePublishedCatalog,
            expected: None,
        },
        SuspensionCase {
            case: "one process",
            named: Named::Processes(vec!["AZRebalance"]),
            expected: Some(vec!["AZRebalance"]),
        },
        SuspensionCase {
            case: "three processes, named out of the order they are reported in",
            named: Named::Processes(vec!["ScheduledActions", "AddToLoadBalancer", "HealthCheck"]),
            expected: Some(vec!["AddToLoadBalancer", "HealthCheck", "ScheduledActions"]),
        },
        SuspensionCase {
            case: "the same process twice: one suspension",
            named: Named::Processes(vec!["InstanceRefresh", "InstanceRefresh"]),
            expected: Some(vec!["InstanceRefresh"]),
        },
        SuspensionCase {
            case: "the two processes that move instances",
            named: Named::Processes(vec!["Terminate", "Launch"]),
            expected: Some(vec!["Launch", "Terminate"]),
        },
        SuspensionCase {
            case: "a process the group is left running beside one it suspends",
            named: Named::Processes(vec!["ReplaceUnhealthy"]),
            expected: Some(vec!["ReplaceUnhealthy"]),
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = format!("suspension-{index}");
        create_group(&router, &name, &subnets[0].id, 0).await;
        let before = reported_suspensions(&router, &name).await;
        assert!(
            before.is_empty(),
            "case {:?}: a new group runs every process, got {before:?}",
            case.case
        );

        let named: Vec<String> = match &case.named {
            Named::Omitted => Vec::new(),
            Named::Processes(processes) => processes
                .iter()
                .map(|process| (*process).to_string())
                .collect(),
            Named::WholePublishedCatalog => catalog.clone(),
        };
        let (status, body) = suspend(&router, &name, &named).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<SuspendProcessesResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );
        assert!(
            !body.contains("<ErrorResponse"),
            "case {:?}: a served suspension carries no error envelope, got {body}",
            case.case
        );

        let expected: Vec<String> = case.expected.as_ref().map_or_else(
            || {
                let mut all = catalog.clone();
                all.sort();
                all
            },
            |processes| {
                processes
                    .iter()
                    .map(|process| (*process).to_string())
                    .collect()
            },
        );
        let reported = reported_suspensions(&router, &name).await;
        assert_eq!(
            reported
                .iter()
                .map(|(process, _)| process.clone())
                .collect::<Vec<String>>(),
            expected,
            "case {:?}: expected the suspended processes {expected:?}, got {reported:?}",
            case.case
        );
        for (process, reason) in &reported {
            assert_reason_shape(case.case, process, reason);
        }
        // The same suspensions on a second read: a read reports state, it does not move it.
        let again = reported_suspensions(&router, &name).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second read reports the same suspensions, got {again:?} after \
             {reported:?}",
            case.case
        );
    }
}

/// One row of the refusal table: what the request names, and what the group holds
/// afterwards.
struct RefusalCase {
    case: &'static str,
    group: &'static str,
    named: Vec<&'static str>,
    /// `None` when the message is the absent-group sentence THIS operation serves — the
    /// detail states that no such group exists, naming it; otherwise the process the
    /// message quotes.
    unknown_process: Option<&'static str>,
}

/// A request naming a process outside the published catalog, or a group the account does
/// not hold, is refused with a 400 `ValidationError` and writes nothing.
#[tokio::test]
async fn an_unsuspendable_request_is_refused_and_writes_nothing() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;
    create_group(&router, "held", &subnets[0].id, 0).await;

    let cases = [
        RefusalCase {
            case: "a process the catalog does not publish",
            group: "held",
            named: vec!["Launches"],
            unknown_process: Some("Launches"),
        },
        RefusalCase {
            case: "a published process spelled in another case",
            group: "held",
            named: vec!["launch"],
            unknown_process: Some("launch"),
        },
        RefusalCase {
            case: "a published process carrying surrounding space",
            group: "held",
            named: vec![" Launch"],
            unknown_process: Some(" Launch"),
        },
        RefusalCase {
            case: "an unknown process beside a published one: neither is suspended",
            group: "held",
            named: vec!["HealthCheck", "Nope"],
            unknown_process: Some("Nope"),
        },
        RefusalCase {
            case: "a group the account does not hold",
            group: "no-such-group",
            named: vec!["Launch"],
            unknown_process: None,
        },
    ];

    for case in &cases {
        let named: Vec<String> = case
            .named
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        let (status, body) = suspend(&router, case.group, &named).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<Code>ValidationError</Code>"),
            "case {:?}: expected the ValidationError code, got {body}",
            case.case
        );
        let expected_message = match case.unknown_process {
            // Every name the request wrote that the catalog does not hold, and the catalog
            // the caller has to choose from, read from what the service itself publishes.
            Some(process) => format!(
                "Request contained illegal suspendable process names: [{process}].Valid names \
                 are :[{}]",
                catalog.join(", ")
            ),
            None => format!(
                "AutoScalingGroup name not found - no such group: {}",
                case.group
            ),
        };
        assert!(
            xml_unescape(&body).contains(&expected_message),
            "case {:?}: expected the message {expected_message:?}, got {body}",
            case.case
        );

        let reported = reported_suspensions(&router, "held").await;
        assert!(
            reported.is_empty(),
            "case {:?}: a refused request leaves the group running every process, got \
             {reported:?}",
            case.case
        );
    }
}

/// One row of the capacity table: what the group has suspended, and what a capacity change
/// then moves.
struct EffectCase {
    case: &'static str,
    /// The processes suspended before the change; `None` for a row that suspends nothing.
    suspend: Option<Vec<&'static str>>,
    /// The capacity the group is created holding, and the one it is then set to.
    initial: usize,
    target: usize,
    /// The instances the group then holds that are not on their way out, and the ones that
    /// are.
    expected_active: usize,
    expected_terminating: usize,
}

/// A group that has suspended `Launch` adds no instance on a scale-out, one that has
/// suspended `Terminate` removes none on a scale-in, and either way the group holds the
/// capacity the caller asked for. A group that suspended some other process moves its
/// instances as it always would.
#[tokio::test]
async fn a_suspended_process_stops_the_half_of_a_capacity_change_it_does() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    let cases = [
        EffectCase {
            case: "Launch suspended: a scale-out adds nothing",
            suspend: Some(vec!["Launch"]),
            initial: 0,
            target: 2,
            expected_active: 0,
            expected_terminating: 0,
        },
        EffectCase {
            case: "Terminate suspended: a scale-in removes nothing",
            suspend: Some(vec!["Terminate"]),
            initial: 2,
            target: 0,
            expected_active: 2,
            expected_terminating: 0,
        },
        EffectCase {
            case: "both suspended: a scale-out moves nothing",
            suspend: Some(vec!["Launch", "Terminate"]),
            initial: 1,
            target: 3,
            expected_active: 1,
            expected_terminating: 0,
        },
        EffectCase {
            case: "both suspended: a scale-in moves nothing either",
            suspend: Some(vec!["Terminate", "Launch"]),
            initial: 1,
            target: 0,
            expected_active: 1,
            expected_terminating: 0,
        },
        EffectCase {
            case: "every process suspended: nothing moves",
            suspend: Some(Vec::new()),
            initial: 0,
            target: 2,
            expected_active: 0,
            expected_terminating: 0,
        },
        EffectCase {
            case: "another process suspended: the scale-out still launches",
            suspend: Some(vec!["AlarmNotification"]),
            initial: 0,
            target: 2,
            expected_active: 2,
            expected_terminating: 0,
        },
        EffectCase {
            case: "another process suspended: the scale-in still terminates",
            suspend: Some(vec!["AZRebalance"]),
            initial: 2,
            target: 1,
            expected_active: 1,
            expected_terminating: 1,
        },
        EffectCase {
            case: "nothing suspended: the scale-out launches",
            suspend: None,
            initial: 0,
            target: 2,
            expected_active: 2,
            expected_terminating: 0,
        },
        EffectCase {
            case: "nothing suspended: the scale-in terminates",
            suspend: None,
            initial: 2,
            target: 1,
            expected_active: 1,
            expected_terminating: 1,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = format!("capacity-{index}");
        create_group(&router, &name, &subnets[0].id, case.initial).await;
        if let Some(processes) = &case.suspend {
            let named: Vec<String> = processes
                .iter()
                .map(|process| (*process).to_string())
                .collect();
            let (status, body) = suspend(&router, &name, &named).await;
            assert_eq!(
                status, 200,
                "case {:?}: suspending expected 200, got {status}: {body}",
                case.case
            );
        }

        let target = case.target.to_string();
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("DesiredCapacity", target.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the capacity change expected 200, got {status}: {body}",
            case.case
        );

        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            tag_value(&body, "DesiredCapacity"),
            target,
            "case {:?}: the group holds the capacity the request set, got {body}",
            case.case
        );
        let states = tag_values(&body, "LifecycleState");
        let terminating = states.iter().filter(|state| *state == "Terminating").count();
        let active = states.len() - terminating;
        assert_eq!(
            active, case.expected_active,
            "case {:?}: expected {} instances in the group, got {states:?} from {body}",
            case.case, case.expected_active
        );
        assert_eq!(
            terminating, case.expected_terminating,
            "case {:?}: expected {} instances on their way out, got {states:?} from {body}",
            case.case, case.expected_terminating
        );
    }
}

/// Suspending a process that is already suspended leaves ONE suspension, and leaves the
/// instant its reason states where it was: the process has been suspended since then.
#[tokio::test]
async fn suspending_again_adds_no_second_suspension() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;
    create_group(&router, "twice", &subnets[0].id, 0).await;

    let first = ["Launch".to_string()];
    let (status, body) = suspend(&router, "twice", &first).await;
    assert_eq!(status, 200, "the first suspension: got {status}: {body}");
    let after_first = reported_suspensions(&router, "twice").await;
    assert_eq!(
        after_first.len(),
        1,
        "one process was suspended, so the group reports one suspension, got {after_first:?}"
    );
    let launch_reason = after_first[0].1.clone();

    // Every process, which names the one already suspended among the rest.
    let (status, body) = suspend(&router, "twice", &[]).await;
    assert_eq!(status, 200, "suspending every process: got {status}: {body}");
    let after_all = reported_suspensions(&router, "twice").await;
    assert_eq!(
        after_all.len(),
        catalog.len(),
        "every published process is suspended once, got {after_all:?} for the catalog \
         {catalog:?}"
    );
    let relaunched = after_all
        .iter()
        .find(|(process, _)| process == "Launch")
        .map(|(_, reason)| reason.clone())
        .unwrap_or_default();
    assert_eq!(
        relaunched, launch_reason,
        "the process already suspended keeps the instant it was suspended at, got \
         {relaunched:?} after {launch_reason:?}"
    );

    // And naming it on its own again changes nothing either.
    let (status, body) = suspend(&router, "twice", &first).await;
    assert_eq!(status, 200, "the third suspension: got {status}: {body}");
    let after_third = reported_suspensions(&router, "twice").await;
    assert_eq!(
        after_third, after_all,
        "naming a suspended process again leaves the group's suspensions as they were, got \
         {after_third:?} after {after_all:?}"
    );
}
