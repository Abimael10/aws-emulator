// SPDX-License-Identifier: CC0-1.0

//! `ResumeProcesses`: turning the scaling processes one Auto Scaling group has suspended
//! back on.
//!
//! The operation "resumes the specified suspended auto scaling processes, or all suspended
//! process, for the specified Auto Scaling group"
//! (`smithy_query(query="operation",operation="ResumeProcesses")`), its modelled output is
//! `Unit`, and "if you omit this property, all processes are specified"
//! (`ScalingProcessQuery$ScalingProcesses`) — the same reading of that member a suspension
//! takes. The processes a request may name are the service's own catalog, the one
//! `DescribeScalingProcessTypes` publishes, so this suite reads that catalog over the wire
//! rather than writing it out, and a name outside it is refused with a 400
//! `ValidationError` that leaves the group's suspensions as they were.
//!
//! What a resumption IS, as the group reports it: the `SuspendedProcess` entries
//! `DescribeAutoScalingGroups` carries for the resumed processes are gone, and the ones for
//! the processes still suspended are untouched — reasons included, since those processes
//! have been suspended since the instant they state. A process is either suspended or it is
//! not, so resuming one the group was already running changes nothing and is not an error.
//!
//! What a resumption DOES: `Launch` is the process that adds instances to the group when it
//! scales out and `Terminate` is the one that removes them when it scales in, so a group
//! that has either of them back does again what it did not do while the process was
//! suspended — it brings its instances to the capacity it holds, which is the capacity the
//! changes made while the process was suspended recorded. Resuming any OTHER process moves
//! no instance.
//!
//! The tables below span what those rules generalise over: how many processes the request
//! names (none, one, several, the whole published catalog, one that is not suspended), the
//! order it names them in, the kind of bad name, and the capacity trajectory a group with
//! each suspension is driven along before the resumption. Every row goes over the awsQuery
//! wire through one router.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values,
    xml_unescape,
};

/// The launch source every group here is created from: a launch TEMPLATE, because no
/// account can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// How long a case that waits for something the clock owes keeps looking before it calls the
/// wait a failure. Far above the work one capacity change takes, because what is waited for
/// is the CONDITION and this is only the backstop that turns a condition that never holds
/// into a failed assertion.
const DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);

/// How long such a case waits between looks.
const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(25);

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

/// One `SuspendProcesses` or `ResumeProcesses` call, naming the processes `processes` lists
/// (naming none when it is empty).
async fn process_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    action: &str,
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
    query(router, action, &borrowed(&params)).await
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

/// The `(active, terminating)` instances one group reports, and the capacity it holds.
async fn reported_instances(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> (usize, usize, String) {
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
    let states = tag_values(&body, "LifecycleState");
    let terminating = states.iter().filter(|state| *state == "Terminating").count();
    let active = states.len() - terminating;
    (active, terminating, tag_value(&body, "DesiredCapacity"))
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

impl Named {
    /// The names one row sends, given the catalog the service publishes.
    fn names(&self, catalog: &[String]) -> Vec<String> {
        match self {
            Self::Omitted => Vec::new(),
            Self::Processes(processes) => processes
                .iter()
                .map(|process| (*process).to_string())
                .collect(),
            Self::WholePublishedCatalog => catalog.to_vec(),
        }
    }
}

/// One row of the resumption table.
struct ResumptionCase {
    case: &'static str,
    /// What the group has suspended before the resumption.
    suspend: Named,
    /// What the resumption names.
    resume: Named,
    /// The processes the group then still reports suspended, in the ASCII order it reports
    /// them.
    expected: Vec<&'static str>,
}

/// A resumption turns off exactly the suspensions the request names — or every one of them
/// when it names none — and leaves every other suspension, reason included, as it was.
#[tokio::test]
async fn resume_processes_turns_off_the_suspensions_the_request_names() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;

    let cases = [
        ResumptionCase {
            case: "every process suspended, every one resumed by naming none",
            suspend: Named::Omitted,
            resume: Named::Omitted,
            expected: Vec::new(),
        },
        ResumptionCase {
            case: "every process suspended, the whole published catalog resumed explicitly",
            suspend: Named::Omitted,
            resume: Named::WholePublishedCatalog,
            expected: Vec::new(),
        },
        ResumptionCase {
            case: "one of three resumed",
            suspend: Named::Processes(vec!["AZRebalance", "HealthCheck", "ScheduledActions"]),
            resume: Named::Processes(vec!["HealthCheck"]),
            expected: vec!["AZRebalance", "ScheduledActions"],
        },
        ResumptionCase {
            case: "two of three resumed, named out of the order they are reported in",
            suspend: Named::Processes(vec!["AZRebalance", "HealthCheck", "ScheduledActions"]),
            resume: Named::Processes(vec!["ScheduledActions", "AZRebalance"]),
            expected: vec!["HealthCheck"],
        },
        ResumptionCase {
            case: "the same process named twice in one resumption",
            suspend: Named::Processes(vec!["InstanceRefresh", "ReplaceUnhealthy"]),
            resume: Named::Processes(vec!["InstanceRefresh", "InstanceRefresh"]),
            expected: vec!["ReplaceUnhealthy"],
        },
        ResumptionCase {
            case: "a process that is not suspended: the suspensions are left alone",
            suspend: Named::Processes(vec!["Launch"]),
            resume: Named::Processes(vec!["Terminate"]),
            expected: vec!["Launch"],
        },
        ResumptionCase {
            case: "a group running every process: resuming every one changes nothing",
            suspend: Named::Processes(Vec::new()),
            resume: Named::Omitted,
            expected: Vec::new(),
        },
        ResumptionCase {
            case: "the two processes that move instances, resumed together",
            suspend: Named::Processes(vec!["Launch", "Terminate", "AlarmNotification"]),
            resume: Named::Processes(vec!["Terminate", "Launch"]),
            expected: vec!["AlarmNotification"],
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = format!("resumption-{index}");
        create_group(&router, &name, &subnets[0].id, 0).await;

        let suspended = case.suspend.names(&catalog);
        let suspends_something = !matches!(&case.suspend, Named::Processes(processes) if processes.is_empty());
        if suspends_something {
            let (status, body) = process_call(&router, "SuspendProcesses", &name, &suspended).await;
            assert_eq!(
                status, 200,
                "case {:?}: suspending expected 200, got {status}: {body}",
                case.case
            );
        }
        let before = reported_suspensions(&router, &name).await;

        let (status, body) =
            process_call(&router, "ResumeProcesses", &name, &case.resume.names(&catalog)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<ResumeProcessesResponse"),
            "case {:?}: expected the operation's own response envelope, got {body}",
            case.case
        );
        assert!(
            !body.contains("<ErrorResponse"),
            "case {:?}: a served resumption carries no error envelope, got {body}",
            case.case
        );

        let reported = reported_suspensions(&router, &name).await;
        let expected: Vec<String> = case
            .expected
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        assert_eq!(
            reported
                .iter()
                .map(|(process, _)| process.clone())
                .collect::<Vec<String>>(),
            expected,
            "case {:?}: expected the suspended processes {expected:?} to be left, got \
             {reported:?}",
            case.case
        );
        // A suspension the resumption did not name has been suspended since the instant it
        // was, so its reason is exactly the one it carried before.
        for (process, reason) in &reported {
            let earlier = before
                .iter()
                .find(|(suspended, _)| suspended == process)
                .map(|(_, reason)| reason.clone())
                .unwrap_or_default();
            assert_eq!(
                *reason, earlier,
                "case {:?}: {process} keeps the reason it carried, got {reason:?} after \
                 {earlier:?}",
                case.case
            );
        }
        // And a second read reports the same thing: a read reports state, it does not move
        // it.
        let again = reported_suspensions(&router, &name).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second read reports the same suspensions, got {again:?} after \
             {reported:?}",
            case.case
        );
    }
}

/// One row of the refusal table.
struct RefusalCase {
    case: &'static str,
    group: &'static str,
    named: Vec<&'static str>,
    /// `None` when the message is the one every group-scoped write answers an unheld group
    /// with; otherwise the process the message quotes.
    unknown_process: Option<&'static str>,
}

/// A resumption naming a process outside the published catalog, or a group the account does
/// not hold, is refused with a 400 `ValidationError` and writes nothing.
#[tokio::test]
async fn an_unresumable_request_is_refused_and_writes_nothing() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    let catalog = published_catalog(&router).await;
    create_group(&router, "held", &subnets[0].id, 0).await;
    let (status, body) =
        process_call(&router, "SuspendProcesses", "held", &["Launch".to_string()]).await;
    assert_eq!(status, 200, "the base suspension: got {status}: {body}");
    let suspended = reported_suspensions(&router, "held").await;

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
            case: "an unknown process beside a published one: neither is resumed",
            group: "held",
            named: vec!["Launch", "Nope"],
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
        let (status, body) = process_call(&router, "ResumeProcesses", case.group, &named).await;
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
                 are :[{}].",
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
        assert_eq!(
            reported, suspended,
            "case {:?}: a refused request leaves the group's suspensions as they were, got \
             {reported:?} after {suspended:?}",
            case.case
        );
    }
}

/// One row of the capacity table: what the group had suspended while its capacity was
/// changed, what the resumption names, and what the group's instances then are.
struct EffectCase {
    case: &'static str,
    /// The processes suspended before the capacity change.
    suspend: Vec<&'static str>,
    /// The capacity the group is created holding, and the one it is then set to.
    initial: usize,
    target: usize,
    /// What the resumption names.
    resume: Named,
    /// The instances the group holds once the move the resumption made has SETTLED: an
    /// instance on its way out is gone once the transition the clock owes is due, so the row
    /// states where the group comes to rest rather than what it reports mid-transition.
    expected_settled: usize,
    /// Whether the resumption moves an instance at all: a row that moves none is asserted
    /// straight after the call as well, since nothing can move the group afterwards.
    moves: bool,
}

/// A group that has `Launch` back adds the instances the scale-out it could not make is
/// short of, one that has `Terminate` back removes the ones the scale-in it could not make
/// left, and the capacity the group holds is the one the change recorded either way.
/// Resuming any other process moves no instance.
#[tokio::test]
async fn resuming_a_process_that_moves_instances_brings_the_group_to_its_capacity() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    let cases = [
        EffectCase {
            case: "Launch resumed: the scale-out it blocked is made",
            suspend: vec!["Launch"],
            initial: 0,
            target: 2,
            resume: Named::Processes(vec!["Launch"]),
            expected_settled: 2,
            moves: true,
        },
        EffectCase {
            case: "Terminate resumed: the scale-in it blocked is made",
            suspend: vec!["Terminate"],
            initial: 2,
            target: 0,
            resume: Named::Processes(vec!["Terminate"]),
            expected_settled: 0,
            moves: true,
        },
        EffectCase {
            case: "every process resumed by naming none: the scale-out is made",
            suspend: vec!["Launch", "Terminate"],
            initial: 0,
            target: 3,
            resume: Named::Omitted,
            expected_settled: 3,
            moves: true,
        },
        EffectCase {
            case: "the whole published catalog resumed: the scale-in is made",
            suspend: vec!["Launch", "Terminate"],
            initial: 2,
            target: 1,
            resume: Named::WholePublishedCatalog,
            expected_settled: 1,
            moves: true,
        },
        EffectCase {
            case: "another process resumed: the blocked scale-out stays blocked",
            suspend: vec!["Launch", "AlarmNotification"],
            initial: 0,
            target: 2,
            resume: Named::Processes(vec!["AlarmNotification"]),
            expected_settled: 0,
            moves: false,
        },
        EffectCase {
            case: "Terminate resumed while Launch stays suspended: the scale-out stays blocked",
            suspend: vec!["Launch", "Terminate"],
            initial: 0,
            target: 2,
            resume: Named::Processes(vec!["Terminate"]),
            expected_settled: 0,
            moves: false,
        },
        EffectCase {
            case: "nothing was blocked: a resumption moves nothing",
            suspend: vec!["Launch"],
            initial: 1,
            target: 1,
            resume: Named::Processes(vec!["Launch"]),
            expected_settled: 1,
            moves: false,
        },
    ];

    let catalog = published_catalog(&router).await;
    for (index, case) in cases.iter().enumerate() {
        let name = format!("resumed-capacity-{index}");
        create_group(&router, &name, &subnets[0].id, case.initial).await;
        let named: Vec<String> = case
            .suspend
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        let (status, body) = process_call(&router, "SuspendProcesses", &name, &named).await;
        assert_eq!(
            status, 200,
            "case {:?}: suspending expected 200, got {status}: {body}",
            case.case
        );

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

        let (status, body) =
            process_call(&router, "ResumeProcesses", &name, &case.resume.names(&catalog)).await;
        assert_eq!(
            status, 200,
            "case {:?}: resuming expected 200, got {status}: {body}",
            case.case
        );

        // A resumption that moves no instance cannot move one later either, so such a row is
        // asserted straight after the call: the group is left exactly as the blocked change
        // left it.
        if !case.moves {
            let (active, terminating, desired) = reported_instances(&router, &name).await;
            assert_eq!(
                desired, target,
                "case {:?}: the group holds the capacity the change recorded, got {desired:?}",
                case.case
            );
            assert_eq!(
                (active, terminating),
                (case.initial, 0),
                "case {:?}: the resumption must move nothing, got {active} active and \
                 {terminating} on their way out",
                case.case
            );
        }

        // What the group comes to REST at: an instance a scale-in took out of service is
        // gone once the transition the clock owes is due, so this looks AGAIN until the
        // group has settled rather than reading once and assuming it had.
        let deadline = std::time::Instant::now() + DEADLINE;
        loop {
            let (active, terminating, desired) = reported_instances(&router, &name).await;
            if desired == target && active == case.expected_settled && terminating == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {:?}: expected the group to settle at DesiredCapacity {target} with {} \
                 instance(s), got {desired:?} with {active} active and {terminating} on their \
                 way out",
                case.case,
                case.expected_settled
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A capacity change the group is already waiting on is left waiting by a resumption: the
/// resumption does not apply the capacity the group currently holds over it, and the
/// deferred change converges the group itself — through the process the resumption gave
/// back — when it falls due.
#[tokio::test]
async fn a_resumption_leaves_a_deferred_capacity_change_waiting() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);

    // A one-second cooldown, so the change deferred until the cooldown has elapsed falls
    // due on the clock rather than at the group's documented five-minute default. The
    // condition this test waits for is the group settling, never a fixed wait.
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "deferred"),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ("DefaultCooldown", "1"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "the create expected 200, got {status}: {body}");

    let (status, body) =
        process_call(&router, "SuspendProcesses", "deferred", &["Launch".to_string()]).await;
    assert_eq!(status, 200, "suspending Launch: got {status}: {body}");
    // Applied at once, and launches nothing while `Launch` is suspended; it is also what
    // puts the group in the cooldown the next change is deferred by.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", "deferred"),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;
    assert_eq!(status, 200, "the first capacity change: got {status}: {body}");
    // Deferred: the caller asked for the cooldown to be honoured, so this capacity is
    // applied when the cooldown has elapsed rather than now.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", "deferred"),
            ("DesiredCapacity", "3"),
            ("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the deferred capacity change: got {status}: {body}"
    );

    let (status, body) =
        process_call(&router, "ResumeProcesses", "deferred", &["Launch".to_string()]).await;
    assert_eq!(status, 200, "resuming Launch: got {status}: {body}");

    // The deferred change is the only thing that asked for three, so a group that reaches
    // three — with three instances, which only the resumed `Launch` process can place — is
    // one whose deferred change the resumption neither discarded nor overrode.
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let (active, terminating, desired) = reported_instances(&router, "deferred").await;
        if desired == "3" && active == 3 && terminating == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the deferred capacity change must still converge the group once it falls due: \
             the group reports DesiredCapacity {desired:?} with {active} active and \
             {terminating} terminating instance(s)"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The instances a resumption launches are the account's OWN Amazon EC2 instances: the ids
/// the group reports after the resumption resolve in Amazon EC2's own state.
#[tokio::test]
async fn the_instances_a_resumption_launches_are_real_ec2_instances() {
    use crate::support::{ec2_query, emulator};

    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_group(&router, "backed", &subnets[0].id, 0).await;

    let (status, body) =
        process_call(&router, "SuspendProcesses", "backed", &["Launch".to_string()]).await;
    assert_eq!(status, 200, "suspending Launch: got {status}: {body}");
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", "backed"),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;
    assert_eq!(status, 200, "the capacity change: got {status}: {body}");

    let (status, body) =
        process_call(&router, "ResumeProcesses", "backed", &["Launch".to_string()]).await;
    assert_eq!(status, 200, "resuming Launch: got {status}: {body}");

    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", "backed")],
    )
    .await;
    assert_eq!(status, 200, "reading the group back: got {status}: {body}");
    let ids = tag_values(&body, "InstanceId");
    assert_eq!(
        ids.len(),
        2,
        "the resumed group holds the two instances its capacity needs, got {ids:?} from \
         {body}"
    );

    for id in &ids {
        let (status, body) = ec2_query(
            &router,
            "DescribeInstances",
            &[p("InstanceId.1", id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "Amazon EC2 must resolve the instance {id:?}, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "instanceId"),
            *id,
            "Amazon EC2 must report the instance {id:?} the group launched, got {body}"
        );
    }
}
