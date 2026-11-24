// SPDX-License-Identifier: CC0-1.0

//! WHEN an Amazon EC2 instance becomes a Systems Manager managed node, read through the wire
//! a client reads it on: `AmazonSSM.DescribeInstanceInformation` over `awsJson1_1`, with the
//! instance launched over Amazon EC2's `ec2Query` wire and its role and instance profile built
//! over IAM's `awsQuery` wire.
//!
//! RULE under test: the SSM Agent is part of the MACHINE an instance's launch starts — it is
//! preinstalled in the image and started by that machine's guest — so an instance whose
//! instance profile carries a role Amazon EC2 may act as whose policies let the agent perform
//! the call it registers with is a managed node FROM THE LAUNCH: the FIRST
//! `DescribeInstanceInformation` after `RunInstances` reports exactly one
//! `InstanceInformation` for it, with `PingStatus` `Online`, `ResourceType` `EC2Instance`, a
//! `RegistrationDate` and a `LastPingDateTime`, with no wait of any kind in between. An
//! instance whose profile withholds that call, and one that runs as no instance profile at
//! all, is reported by none — while Amazon EC2 goes on reporting that very instance, which is
//! what makes the absence a contract rather than a race. The only nodes the operation then
//! withholds are the ones it documents: "this operation does not return information for nodes
//! that are either Stopped or Terminated".
//!
//! The SECOND half of the same rule is what a client does with such a node: the agent takes a
//! command sent to it when its guest is up, so an invocation sent the instant the node
//! registered finishes with the script's own output rather than timing out — the delivery is
//! owed to the clock, not to the send.
//!
//! The rows differ along what the rule generalises over: HOW the role is granted the
//! registration call (the AWS-managed policy a client attaches for this, an inline statement
//! naming the action, an inline wildcard over the service) and how it is WITHHELD (a role
//! carrying no policy at all, no instance profile at all). The wider table over the other
//! dimensions — an explicit deny, a role another service may act as, a profile IAM no longer
//! holds, a Windows image, an instance that stops being held running — is
//! `ec2_managed_node_registration`'s, and is not repeated here: what this suite adds beside it
//! is that the node is there on the FIRST read, with nothing moved through the store and
//! nothing waited for.
//!
//! Nothing here sleeps for a span or asserts on elapsed time: the reads that must answer at
//! once are made at once, and the two conditions another component's clock owes — the guest
//! coming up under a deferred delivery, and a terminated instance's node going with it — are
//! LOOKED AT AGAIN until they hold or a generous deadline passes.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::{finished, send};

/// An inline document granting exactly the call the agent registers its node with
/// (`ssm:UpdateInstanceInformation`, the action the AWS-managed `AmazonSSMManagedInstanceCore`
/// policy exists to grant).
const GRANTS_THE_REGISTRATION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"ssm:UpdateInstanceInformation","Resource":"*"}]}"#;

/// An inline document granting every action of this service, which covers that call too.
const GRANTS_EVERY_ACTION: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;

/// The `PingStatus` of a node whose agent is reporting (`InstanceInformation$PingStatus`,
/// whose enum is `Online | ConnectionLost | Inactive`).
const ONLINE: &str = "Online";

/// The `ResourceType` of a node that is an Amazon EC2 instance
/// (`InstanceInformation$ResourceType`, whose enum is `ManagedInstance | EC2Instance`).
const EC2_INSTANCE: &str = "EC2Instance";

/// How long a case looks for a condition another component's clock owes. Generous on purpose:
/// every span crossed here is a stored due instant and nothing sleeps for one, so all this
/// bound is for is that a move that never comes fails instead of hanging.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look-again cycle pauses. Not a bound on anything.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// What one row's role carries about the registration call.
#[derive(Debug, Clone, Copy)]
enum Carries {
    /// A managed policy attached by ARN — AWS's own or the account's.
    Attached(&'static str),
    /// An inline document of the account's own.
    Inline(&'static str),
    /// No policy at all.
    Nothing,
    /// No instance profile at all: the instance runs as none, so its agent has no credentials
    /// to register with.
    NoProfile,
}

/// One row: a role carrying `carries`, and whether the instance running as it is a managed
/// node.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The name of the role and of the instance profile built for this row.
    name: &'static str,
    carries: Carries,
    /// Whether the FIRST read after the launch must report the node.
    registers: bool,
}

/// The rows.
const ROWS: &[Row] = &[
    Row {
        case: "the AWS-managed policy a client attaches for this",
        name: "launch-core",
        carries: Carries::Attached(SSM_CORE_POLICY_ARN),
        registers: true,
    },
    Row {
        case: "an inline statement naming the call the agent registers with",
        name: "launch-inline",
        carries: Carries::Inline(GRANTS_THE_REGISTRATION),
        registers: true,
    },
    Row {
        case: "an inline wildcard over every action of this service",
        name: "launch-wildcard",
        carries: Carries::Inline(GRANTS_EVERY_ACTION),
        registers: true,
    },
    Row {
        case: "a role carrying no policy at all",
        name: "launch-ungranted",
        carries: Carries::Nothing,
        registers: false,
    },
    Row {
        case: "an instance running as no instance profile",
        name: "launch-none",
        carries: Carries::NoProfile,
        registers: false,
    },
];

/// Build the role and instance profile one row launches with, and answer the profile name a
/// launch names — `None` for the row that runs as none.
async fn profile_of(emulator: &Emulator, row: &Row) -> Option<String> {
    match row.carries {
        Carries::NoProfile => None,
        Carries::Attached(policy_arn) => Some(
            emulator
                .profile(row.case, row.name, &Grant::Attached(policy_arn))
                .await,
        ),
        Carries::Nothing => Some(emulator.profile(row.case, row.name, &Grant::Nothing).await),
        Carries::Inline(document) => {
            let name = emulator.profile(row.case, row.name, &Grant::Nothing).await;
            emulator
                .iam(
                    row.case,
                    "PutRolePolicy",
                    &[
                        ("RoleName", row.name),
                        ("PolicyName", "registration"),
                        ("PolicyDocument", document),
                    ],
                )
                .await;
            Some(name)
        }
    }
}

/// The one `InstanceInformation` this service reports for `instance`, or `None` when it
/// reports none — read through the filter a client narrows the report with.
async fn node_of(emulator: &Emulator, case: &str, instance: &str) -> Option<Value> {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "DescribeInstanceInformation",
            &json!({"Filters": [{"Key": "InstanceIds", "Values": [instance]}]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceInformation must be served, got {status}: {answered}"
    );
    let listed = answered["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.len() <= 1,
        "case {case:?}: a report narrowed to {instance} carries at most one node, got {answered}"
    );
    listed.first().cloned()
}

/// Whether Amazon EC2 still holds the instance `instance` names, read through its own
/// `DescribeInstances`.
async fn ec2_holds(emulator: &Emulator, case: &str, instance: &str) -> bool {
    let body = emulator
        .ec2(case, "DescribeInstances", &[("InstanceId.1", instance)])
        .await;
    body.contains(instance)
}

/// RULE: an instance whose instance profile lets the agent register is a managed node from the
/// launch, so the FIRST read after `RunInstances` reports it `Online` — and one whose profile
/// withholds that call is reported by none, while Amazon EC2 goes on holding the instance.
#[tokio::test]
async fn a_launched_instance_is_a_managed_node_on_the_first_read_after_the_launch() {
    let emulator = Emulator::new();
    let image = emulator.image("the image every row launches", AL2023_NAME_PATTERN).await;

    for row in ROWS {
        let profile = profile_of(&emulator, row).await;
        // The launch, and then the read — with nothing in between: no poll, no span moved
        // through the store, no wait.
        let launched_at = SystemTime::now();
        let instance = emulator
            .launch(row.case, &image, profile.as_deref())
            .await;
        let reported = node_of(&emulator, row.case, &instance).await;

        if !row.registers {
            assert!(
                reported.is_none(),
                "case {:?}: expected no managed node for {instance}, got {reported:?}",
                row.case
            );
            // The absence is a contract rather than a race: the instance itself is there.
            assert!(
                ec2_holds(&emulator, row.case, &instance).await,
                "case {:?}: expected Amazon EC2 to hold {instance} all the same",
                row.case
            );
            continue;
        }

        let node = reported.unwrap_or_else(|| {
            panic!(
                "case {:?}: expected {instance} to be a managed node on the first read",
                row.case
            )
        });
        assert_eq!(
            node["InstanceId"].as_str(),
            Some(instance.as_str()),
            "case {:?}: expected the node of {instance}, got {node}",
            row.case
        );
        assert_eq!(
            node["PingStatus"].as_str(),
            Some(ONLINE),
            "case {:?}: expected PingStatus {ONLINE} for {instance}, got {node}",
            row.case
        );
        assert_eq!(
            node["ResourceType"].as_str(),
            Some(EC2_INSTANCE),
            "case {:?}: expected ResourceType {EC2_INSTANCE} for {instance}, got {node}",
            row.case
        );
        assert_eq!(
            node["IamRole"].as_str(),
            Some(row.name),
            "case {:?}: expected the node to report the role its profile carries, got {node}",
            row.case
        );
        // The registration instant is the launch's own: at or after the instant this case
        // launched the instance, and not in the future.
        let registered = seconds_of(row.case, &node, "RegistrationDate");
        let launched = epoch_seconds(launched_at);
        assert!(
            registered >= launched - 1.0 && registered <= epoch_seconds(SystemTime::now()) + 1.0,
            "case {:?}: expected a RegistrationDate at the launch ({launched}), got \
             {registered}: {node}",
            row.case
        );
        assert!(
            seconds_of(row.case, &node, "LastPingDateTime") >= launched - 1.0,
            "case {:?}: expected a LastPingDateTime of this node's own ping, got {node}",
            row.case
        );
    }
}

/// RULE (the second half): the agent takes a command sent to its node when that node's guest
/// is up, so an invocation sent the INSTANT the node registered — before any guest has come up
/// — finishes with the script's own output rather than reporting a command nothing was
/// delivered to.
#[tokio::test]
async fn a_command_sent_the_instant_a_node_registers_runs_in_its_guest() {
    let case = "a send that reaches the node before its guest is up";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "sent-at-launch", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    // The node is there at once, which is what lets a client send to it at once.
    assert!(
        node_of(&emulator, case, &instance).await.is_some(),
        "case {case:?}: expected {instance} to be a managed node on the first read"
    );
    let command = send(&emulator, case, &instance, &["echo registered"]).await;

    // Nothing is slept for: the invocation is looked at again until the clock has brought the
    // guest up, the agent has taken the command and the run has finished.
    let answered = finished(&emulator, case, &command, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case {case:?}: expected the invocation to finish on the guest's own exit code, got \
         {answered}"
    );
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("registered\n"),
        "case {case:?}: expected what the guest wrote, got {answered}"
    );
}

/// RULE (the documented exclusion): the operation "does not return information for nodes that
/// are either Stopped or Terminated", so the node of an instance a client terminates goes with
/// it — while every other node of the Region stays.
#[tokio::test]
async fn the_node_of_a_terminated_instance_stops_being_reported() {
    let case = "the terminated instance";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "terminated-node", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let going = emulator.launch(case, &image, Some(&profile)).await;
    let staying = emulator.launch(case, &image, Some(&profile)).await;
    for instance in [&going, &staying] {
        assert!(
            node_of(&emulator, case, instance).await.is_some(),
            "case {case:?}: expected {instance} to be a managed node on the first read"
        );
    }

    emulator
        .ec2(case, "TerminateInstances", &[("InstanceId.1", going.as_str())])
        .await;
    // The move to `terminated` is a stored due instant, so the node is looked for again until
    // it has gone rather than waited out.
    let deadline = Instant::now() + DEADLINE;
    while node_of(&emulator, case, &going).await.is_some() {
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the node of the terminated {going} to stop being reported"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
    assert!(
        node_of(&emulator, case, &staying).await.is_some(),
        "case {case:?}: expected the node of {staying} to stay reported"
    );
}

/// One instant member of a reported node, as the epoch seconds the `awsJson1_1` wire carries a
/// modelled timestamp as.
fn seconds_of(case: &str, node: &Value, member: &str) -> f64 {
    node[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected the node to report {member}, got {node}")
    })
}

/// `at` as the epoch seconds a modelled timestamp is carried as on that wire.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}
