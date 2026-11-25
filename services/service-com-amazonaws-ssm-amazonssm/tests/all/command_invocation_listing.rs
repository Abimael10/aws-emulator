// SPDX-License-Identifier: CC0-1.0

//! The INVOCATIONS of a Run Command command, listed: `ListCommandInvocations` reports the
//! copy of a command each node it was sent to holds, narrowed by the criteria the request
//! names.
//!
//! RULE: "an invocation is [a] copy of a command sent to a specific managed node … a command
//! invocation is created for each requested managed node ID", so the listing reports one
//! invocation per node of every command of the Region the request was signed for — the
//! invocations of the one command a `CommandId` names, the one an `InstanceId` holds, and the
//! ones the filters select, which narrow together — and "if set this returns the response of
//! the command executions and any command output" is what decides whether each invocation
//! carries the per-step results of what the node's agent ran.
//!
//! Every case goes through the front door: `AmazonSSM.SendCommand` and
//! `AmazonSSM.ListCommandInvocations` over `awsJson1_1`, with the instances launched over
//! EC2's `ec2Query` wire and their roles, policies and instance profiles created over IAM's
//! `awsQuery` wire (the harness in `all/ec2_instance_managed_nodes.rs`). The nodes themselves
//! are never seeded: they are the managed nodes the launched instances became.
//!
//! The rows differ along what the rule generalises over: which criterion narrows the listing
//! (a command, a node, both, each documented filter key), how many nodes a command was sent
//! to, whether the command has finished, and whether the per-step results were asked for.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::finish_due;

/// The published command document every case runs.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The step of that document a Linux node runs, which is what the per-step results name:
/// the document is published at schema 1.2, whose steps are its `runtimeConfig` plugin
/// entries, so the step's name is the plugin's own.
const LINUX_STEP: &str = "aws:runShellScript";

/// How long a case looks for an invocation to finish, far longer than the span the clock owes
/// that move after — so the case does not depend on how fast the host runs.
const FINISH_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Launch an instance that becomes a managed node and answer its id.
async fn node(emulator: &Emulator, case: &str, role: &str) -> String {
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, role, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );
    instance
}

/// Send `script` to `instances` and answer the command id.
async fn send(emulator: &Emulator, case: &str, instances: &[&str], script: &str) -> String {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": instances,
                "Parameters": {"commands": [script]},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status}: {answered}"
    );
    answered["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint a command id: {answered}"))
        .to_string()
}

/// `ListCommandInvocations` with `body`.
async fn list(emulator: &Emulator, body: &Value) -> (u16, Value) {
    emulator.ssm(REGION, "ListCommandInvocations", body).await
}

/// The `(command id, node id, status)` of every invocation one listing reports, in the order
/// it reports them.
fn reported(answered: &Value) -> Vec<(String, String, String)> {
    answered["CommandInvocations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|invocation| {
            (
                invocation["CommandId"].as_str().unwrap_or_default().to_string(),
                invocation["InstanceId"].as_str().unwrap_or_default().to_string(),
                invocation["Status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Look at the listing until every invocation of `command_id` has reached a terminal status.
async fn finished(emulator: &Emulator, case: &str, command_id: &str, nodes: usize) {
    let terminal = ["Success", "Cancelled", "Failed", "TimedOut"];
    let deadline = Instant::now() + FINISH_DEADLINE;
    loop {
        let (status, answered) = list(emulator, &json!({ "CommandId": command_id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must be served, got {status}: {answered}"
        );
        let held = reported(&answered);
        if held.len() == nodes
            && held
                .iter()
                .all(|(_, _, status)| terminal.contains(&status.as_str()))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every invocation to finish with nothing but the clock \
             running, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the criteria table: what a failing assertion calls it, the criteria the
/// request carries, and the `(command, node)` pairs the listing must then report.
type CriteriaRow<'a> = (&'a str, Value, Vec<(&'a String, &'a String)>);

/// RULE: the listing reports one invocation per node of the commands the criteria select, and
/// a criterion no invocation meets selects none.
#[tokio::test]
async fn the_listing_reports_the_invocations_the_criteria_select() {
    let emulator = Emulator::new();
    let case = "the criteria table";
    let first = node(&emulator, case, "list-invocations-first").await;
    let second = node(&emulator, case, "list-invocations-second").await;

    // Three sends: one to each node on its own, and one to both.
    let to_first = send(&emulator, case, &[first.as_str()], "echo first").await;
    let to_second = send(&emulator, case, &[second.as_str()], "echo second").await;
    let to_both = send(
        &emulator,
        case,
        &[first.as_str(), second.as_str()],
        "echo both",
    )
    .await;
    for (command, nodes) in [(&to_first, 1), (&to_second, 1), (&to_both, 2)] {
        finish_due(&emulator, command);
        finished(&emulator, case, command, nodes).await;
    }

    // (case, the request's criteria, the (command, node) pairs it must report)
    let rows: Vec<CriteriaRow<'_>> = vec![
        (
            "one command, sent to one node",
            json!({ "CommandId": to_first }),
            vec![(&to_first, &first)],
        ),
        (
            "one command, sent to two nodes",
            json!({ "CommandId": to_both }),
            vec![(&to_both, &first), (&to_both, &second)],
        ),
        (
            "one node, across every command sent to it",
            json!({ "InstanceId": second }),
            vec![(&to_both, &second), (&to_second, &second)],
        ),
        (
            "one command and one node together",
            json!({ "CommandId": to_both, "InstanceId": second }),
            vec![(&to_both, &second)],
        ),
        (
            "the status every invocation reached",
            json!({ "Filters": [{ "key": "Status", "value": "Success" }] }),
            vec![
                (&to_both, &first),
                (&to_both, &second),
                (&to_second, &second),
                (&to_first, &first),
            ],
        ),
        (
            "a status no invocation holds",
            json!({ "Filters": [{ "key": "Status", "value": "Cancelled" }] }),
            vec![],
        ),
        (
            "the document every command ran",
            json!({
                "CommandId": to_second,
                "Filters": [{ "key": "DocumentName", "value": RUN_SHELL_SCRIPT }],
            }),
            vec![(&to_second, &second)],
        ),
        (
            "a document no command ran",
            json!({ "Filters": [{ "key": "DocumentName", "value": "AWS-RunPowerShellScript" }] }),
            vec![],
        ),
    ];
    for (why, body, expected) in rows {
        let (status, answered) = list(&emulator, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the listing must be served, got {status}: {answered}"
        );
        let held: Vec<(String, String)> = reported(&answered)
            .into_iter()
            .map(|(command, node, _)| (command, node))
            .collect();
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(command, node)| (command.clone(), node.clone()))
            .collect();
        assert_eq!(
            held, expected,
            "case {why:?}: expected {expected:?}, got {held:?}"
        );
    }
}

/// RULE: "if set this returns the response of the command executions and any command output" —
/// the per-step results are reported only when the request asks for them, and they are what
/// the node's agent really ran.
#[tokio::test]
async fn the_details_switch_decides_whether_the_per_step_results_are_reported() {
    let emulator = Emulator::new();
    let case = "the details case";
    let instance = node(&emulator, case, "list-invocations-details").await;
    let command_id = send(
        &emulator,
        case,
        &[instance.as_str()],
        "echo the-detailed-output",
    )
    .await;
    finish_due(&emulator, &command_id);
    finished(&emulator, case, &command_id, 1).await;

    // (case, whether the request asks for the details)
    for (why, details) in [("asked for", true), ("not asked for", false)] {
        let (status, answered) = list(
            &emulator,
            &json!({ "CommandId": command_id, "Details": details }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {why:?}: the listing must be served, got {status}: {answered}"
        );
        let invocation = answered["CommandInvocations"][0].clone();
        let plugins = invocation["CommandPlugins"].clone();
        if !details {
            assert!(
                plugins.is_null(),
                "case {why:?}: expected no per-step results, got {invocation}"
            );
            continue;
        }
        let plugins = plugins.as_array().cloned().unwrap_or_default();
        assert_eq!(
            plugins.len(),
            1,
            "case {why:?}: expected the one step the node's agent ran, got {invocation}"
        );
        let plugin = &plugins[0];
        assert_eq!(
            plugin["Name"].as_str(),
            Some(LINUX_STEP),
            "case {why:?}: expected the step the document declares, got {plugin}"
        );
        assert_eq!(
            plugin["Status"].as_str(),
            Some("Success"),
            "case {why:?}: expected the status the run ended in, got {plugin}"
        );
        assert_eq!(
            plugin["ResponseCode"].as_i64(),
            Some(0),
            "case {why:?}: expected the exit code the guest's shell answered, got {plugin}"
        );
        assert_eq!(
            plugin["Output"].as_str(),
            Some("the-detailed-output\n"),
            "case {why:?}: expected what the guest wrote, got {plugin}"
        );
        for member in ["ResponseStartDateTime", "ResponseFinishDateTime"] {
            assert!(
                !plugin[member].is_null(),
                "case {why:?}: expected {member} on a step that ran, got {plugin}"
            );
        }
    }
}

/// RULE: a criterion the operation cannot serve is refused rather than ignored — the
/// `ExecutionStage` filter is the one key this operation does not take ("can't be used with
/// the `ListCommandInvocations` operation, only with `ListCommands`"), a filter value the key
/// cannot carry is no value to compare, and a `NextToken` this read did not hand out is not a
/// page of it. A criterion that is merely met by no stored invocation is NOT one of them.
#[tokio::test]
async fn a_criterion_the_operation_cannot_serve_is_refused() {
    let emulator = Emulator::new();
    let case = "the refusal table";
    let instance = node(&emulator, case, "list-invocations-refusals").await;
    let command_id = send(&emulator, case, &[instance.as_str()], "echo refusals").await;

    // (case, the request, the status and error code it is refused with)
    let rows: Vec<(&str, Value, (u16, &str))> = vec![
        (
            "the ExecutionStage filter, which only ListCommands takes",
            json!({ "Filters": [{ "key": "ExecutionStage", "value": "Complete" }] }),
            (400, "InvalidFilterKey"),
        ),
        (
            "a token this read did not hand out",
            json!({ "NextToken": "not-a-token" }),
            (400, "InvalidNextToken"),
        ),
        (
            "an instant no timestamp filter can read",
            json!({ "Filters": [{ "key": "InvokedAfter", "value": "yesterday" }] }),
            (400, "ValidationException"),
        ),
    ];
    for (why, body, (expected_status, expected_code)) in rows {
        let (status, answered) = list(&emulator, &body).await;
        let code = answered["__type"]
            .as_str()
            .map(|named| named.rsplit('#').next().unwrap_or(named).to_string())
            .unwrap_or_default();
        assert_eq!(
            (status, code.as_str()),
            (expected_status, expected_code),
            "case {why:?}: expected {expected_status} {expected_code}, got {status}: {answered}"
        );
    }

    // Every member of the input is CRITERIA rather than an identity, so one no stored
    // invocation meets selects none instead of being refused: a node id nothing was sent to,
    // a command id no send of this Region minted, and the two together.
    let selects_none: Vec<(&str, Value)> = vec![
        (
            "a node nothing was sent to",
            json!({ "CommandId": command_id, "InstanceId": "i-0abcdef0123456789" }),
        ),
        (
            "a command id no send of this Region minted",
            json!({ "CommandId": "00000000-0000-0000-0000-000000000000" }),
        ),
        (
            "an unminted command id beside a node nothing was sent to",
            json!({
                "CommandId": "00000000-0000-0000-0000-000000000000",
                "InstanceId": "i-0abcdef0123456789",
            }),
        ),
    ];
    for (why, body) in selects_none {
        let (status, answered) = list(&emulator, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the listing must be served, got {status}: {answered}"
        );
        assert!(
            answered["CommandInvocations"].is_array(),
            "case {why:?}: expected the modelled list on the answer, got {answered}"
        );
        assert!(
            reported(&answered).is_empty(),
            "case {why:?}: expected no invocation, got {answered}"
        );
    }
}

/// RULE: a page holds `MaxResults` invocations and the `NextToken` resumes the listing at the
/// invocation the page stopped at, so a walk over the pages reports each one once.
#[tokio::test]
async fn a_page_resumes_at_the_invocation_it_stopped_at() {
    let emulator = Emulator::new();
    let case = "the paging case";
    let first = node(&emulator, case, "list-invocations-page-first").await;
    let second = node(&emulator, case, "list-invocations-page-second").await;
    let command_id = send(
        &emulator,
        case,
        &[first.as_str(), second.as_str()],
        "echo paged",
    )
    .await;
    finish_due(&emulator, &command_id);
    finished(&emulator, case, &command_id, 2).await;

    let mut walked: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "CommandId": command_id, "MaxResults": 1 });
        if let Some(next) = &token {
            body["NextToken"] = Value::String(next.clone());
        }
        let (status, answered) = list(&emulator, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: each page must be served, got {status}: {answered}"
        );
        let page = reported(&answered);
        assert!(
            page.len() <= 1,
            "case {case:?}: a page holds at most MaxResults invocations, got {page:?}"
        );
        walked.extend(page.into_iter().map(|(command, node, _)| (command, node)));
        match answered["NextToken"].as_str() {
            Some(next) if !next.is_empty() => token = Some(next.to_string()),
            _ => break,
        }
    }
    let expected: Vec<(String, String)> = vec![
        (command_id.clone(), first.clone()),
        (command_id.clone(), second.clone()),
    ];
    assert_eq!(
        walked, expected,
        "case {case:?}: expected the walk to report each invocation once, in node-id order"
    );
}
