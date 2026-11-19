// SPDX-License-Identifier: CC0-1.0

//! Run Command FANNED OUT over several managed nodes, and the per-node scoping of the
//! invocation listing — the whole path a fleet client drives, from the launch that brings
//! an instance under Systems Manager management to the one invocation a `(CommandId,
//! InstanceId)` pair selects.
//!
//! RULE, in three parts, each the premise of the next:
//!
//! 1. an Amazon EC2 instance whose instance profile's role lets the agent perform the call
//!    it registers with IS a managed node once the registration span has passed, so
//!    `DescribeInstanceInformation` filtered on that node's id (the `InstanceIds` filter
//!    key) reports exactly one row for it, whose `PingStatus` is `Online` — a client that
//!    polls for its instance to come under management is ANSWERED rather than left waiting;
//! 2. "a command invocation is created for each requested managed node ID"
//!    (`CommandInvocation`), so one send naming N nodes mints ONE command id and exactly N
//!    invocations — one per named node, each reaching a terminal status of its own that
//!    `GetCommandInvocation` reports for that node;
//! 3. `ListCommandInvocations` narrowed by a command AND a node reports that node's one
//!    invocation of it and no sibling target's, for every target of the same send — the
//!    per-node scoping that makes the listing usable at all.
//!
//! The rows differ along what the rule generalises over: HOW MANY nodes one send fans out
//! to (one, two, three), and WHICH of them the listing is then scoped by — every target of
//! each send in turn, so the scoping is pinned symmetrically rather than for one chosen
//! target.
//!
//! Nothing is seeded into this service's state: the role, its policy and its instance
//! profile are created over IAM's own wire, the instances over Amazon EC2's `RunInstances`,
//! and every Systems Manager call goes through the front door (`POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>`), using the shared harness in
//! `all/ec2_instance_managed_nodes.rs`. The only thing a case reaches a store for is the
//! CLOCK — an instance's launch instant and the instant an invocation's outcome falls due —
//! so no case waits out a span or depends on how fast the host runs.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use crate::run_command_in_the_guest::finish_due;

/// The published document every send below runs, the one a client reaches for to run a
/// shell command on a Linux node.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The script every send supplies as that document's `commands` parameter.
const SCRIPT: &str = "echo fanout-ok";

/// The status an invocation of that script reaches: "the command or plugin ran
/// successfully" (`CommandInvocationStatus`), which for a shell step is the exit code the
/// guest's shell answered being zero.
const SUCCESS: &str = "Success";

/// The exit code a step that succeeded reports (`GetCommandInvocationResult$ResponseCode`:
/// "if the response code is -1, then the command hasn't started running on the managed
/// node").
const SUCCESS_RESPONSE_CODE: i64 = 0;

/// The statuses an invocation stops moving at, from the modelled
/// `CommandInvocationStatus` enum's terminal members ("Success", "Cancelled",
/// "TimedOut", "Failed") — the set a poll for "has it finished" is over.
const TERMINAL: [&str; 4] = ["Success", "Cancelled", "TimedOut", "Failed"];

/// How long a poll looks for a condition the clock owes, far longer than any span this
/// emulator owes one after, so no case is decided by how fast the host runs.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many nodes the widest send below fans out to.
const WIDEST_FAN_OUT: usize = 3;

/// Part 1: launch `count` instances that each become a managed node, and answer their ids
/// in launch order, having polled each one's own filtered node report to `Online`.
async fn registered_nodes(emulator: &Emulator, case: &str, count: usize) -> Vec<String> {
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "fan-out-node-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let mut instances = Vec::new();
    for _ in 0..count {
        let instance = emulator.launch(case, &image, Some(&profile)).await;
        // The registration is owed a span after the launch; this suite is about what the
        // registered nodes then do, so the launch instant is moved back past it rather
        // than waited out (the span itself is `ec2_managed_node_registration`'s table).
        emulator.launched_ago(&instance, launched_long_enough_ago());
        instances.push(instance);
    }
    for instance in &instances {
        online(emulator, case, instance).await;
    }
    instances
}

/// Poll the node report FILTERED on one node's id — the read a client waiting for its
/// instance to come under management makes — until that node is reported `Online`, and
/// assert the filtered report holds that node and nothing else; answer the row it reported.
///
/// `pub(crate)`, like [`finish_due`]: every suite that waits for a launch to come under
/// management makes this same filtered read, and a second copy of the poll would be a second
/// answer to "when is an instance a managed node".
pub(crate) async fn online(emulator: &Emulator, case: &str, instance: &str) -> Value {
    let body = json!({"Filters": [{"Key": "InstanceIds", "Values": [instance]}]});
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = emulator
            .ssm(REGION, "DescribeInstanceInformation", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the node report for {instance} must be served, got {status}: \
             {answered}"
        );
        let reported: Vec<&Value> = answered["InstanceInformationList"]
            .as_array()
            .map(|rows| rows.iter().collect())
            .unwrap_or_default();
        if let [row] = reported.as_slice() {
            assert_eq!(
                row["InstanceId"].as_str(),
                Some(instance),
                "case {case:?}: expected the filter to select {instance} alone, got {row}"
            );
            if row["PingStatus"].as_str() == Some("Online") {
                return (*row).clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instance} reported as one Online managed node with \
             nothing but the clock running, got {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Part 2: send the script to every node of `targets` at once and answer the one command id
/// the send minted.
async fn send(emulator: &Emulator, case: &str, targets: &[String]) -> String {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": targets,
                "Parameters": {"commands": [SCRIPT]},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a send naming {} registered node(s) must be served, got {status}: \
         {answered}",
        targets.len()
    );
    let command = &answered["Command"];
    assert_eq!(
        command["InstanceIds"],
        json!(targets),
        "case {case:?}: expected the command to name every node it was sent to, got {answered}"
    );
    command["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint one command id: {answered}"))
        .to_string()
}

/// Poll `GetCommandInvocation` for one `(command, node)` pair until it reports a terminal
/// status, and answer what it then reported.
async fn invocation_of(emulator: &Emulator, case: &str, command: &str, node: &str) -> Value {
    let body = json!({"CommandId": command, "InstanceId": node});
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = emulator
            .ssm(REGION, "GetCommandInvocation", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation read for {node} must be served, got {status}: \
             {answered}"
        );
        let reached = answered["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&reached.as_str()) {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {node}'s invocation of {command} to finish with nothing \
             but the clock running, still {reached:?}: {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `ListCommandInvocations` call, which must be served, as the `(command, node)` pairs
/// it reported.
async fn listed(emulator: &Emulator, case: &str, body: &Value) -> Vec<(String, String)> {
    let (status, answered) = emulator
        .ssm(REGION, "ListCommandInvocations", body)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation listing must be served, got {status}: {answered}"
    );
    answered["CommandInvocations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|invocation| {
            (
                invocation["CommandId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                invocation["InstanceId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The whole rule, one row per fan-out width: a send to N registered nodes mints one
/// command with exactly one invocation per node, each reaching `Success` of its own, and
/// the listing scoped by each of those nodes in turn reports that node's one invocation and
/// no sibling target's.
#[tokio::test]
async fn one_send_to_many_nodes_gives_each_node_its_own_scoped_invocation() {
    for width in 1..=WIDEST_FAN_OUT {
        let case = format!("a send fanned out to {width} node(s)");
        let case = case.as_str();
        // A fresh emulator per row, so each row's listings hold only its own send.
        let emulator = Emulator::new();
        let nodes = registered_nodes(&emulator, case, width).await;
        let command = send(&emulator, case, &nodes).await;
        // The move each invocation's outcome is owed is made due rather than waited out.
        finish_due(&emulator, &command);

        // Part 2: one invocation per named node, each terminal and successful, reported
        // for the node it belongs to.
        for node in &nodes {
            let answered = invocation_of(&emulator, case, &command, node).await;
            assert_eq!(
                answered["Status"].as_str(),
                Some(SUCCESS),
                "case {case:?}: expected {node}'s invocation to reach {SUCCESS}, got {answered}"
            );
            assert_eq!(
                answered["ResponseCode"].as_i64(),
                Some(SUCCESS_RESPONSE_CODE),
                "case {case:?}: expected {node}'s successful step to report exit code \
                 {SUCCESS_RESPONSE_CODE}, got {answered}"
            );
            assert_eq!(
                (
                    answered["CommandId"].as_str(),
                    answered["InstanceId"].as_str()
                ),
                (Some(command.as_str()), Some(node.as_str())),
                "case {case:?}: expected the read to answer for the pair it was asked about, \
                 got {answered}"
            );
        }

        // The command's whole invocation set: one per node it was sent to, and no other.
        let whole = listed(&emulator, case, &json!({"CommandId": command})).await;
        let held: BTreeSet<(String, String)> = whole.iter().cloned().collect();
        let expected: BTreeSet<(String, String)> = nodes
            .iter()
            .map(|node| (command.clone(), node.clone()))
            .collect();
        assert_eq!(
            held, expected,
            "case {case:?}: expected one invocation per node the send named, got {whole:?}"
        );

        // Part 3: scoped by each target in turn — exactly that node's one invocation, and
        // nothing of any sibling target of the same send.
        for node in &nodes {
            let scoped = format!("{case}, scoped by {node}");
            let scoped = scoped.as_str();
            let reported = listed(
                &emulator,
                scoped,
                &json!({"CommandId": command, "InstanceId": node}),
            )
            .await;
            assert_eq!(
                reported,
                vec![(command.clone(), node.clone())],
                "case {scoped:?}: expected exactly this node's one invocation, got {reported:?}"
            );
            let siblings: Vec<&String> = nodes.iter().filter(|held| *held != node).collect();
            for sibling in siblings {
                assert!(
                    reported.iter().all(|(_, listed)| listed != sibling),
                    "case {scoped:?}: expected no invocation of the sibling target \
                     {sibling}, got {reported:?}"
                );
            }
        }
    }
}
