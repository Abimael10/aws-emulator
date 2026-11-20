// SPDX-License-Identifier: CC0-1.0

//! The whole path a client takes to get Run Command output off an Amazon EC2 instance it
//! launched with an SSM-core instance profile — driven on the REAL clock, with nothing
//! seeded and nothing reached in any store.
//!
//! RULE under test, in two halves the second rests on the first:
//!
//! 1. an instance Amazon EC2 holds `running` whose instance profile carries a role Amazon
//!    EC2 may act as and whose policies permit the call the agent registers with becomes a
//!    managed node a span after its launch WITHOUT anybody asking for it, so a
//!    `DescribeInstanceInformation` narrowed to that instance's id eventually reports exactly
//!    one entry for it whose `PingStatus` is `Online`; the same read narrowed to a
//!    well-formed node id this endpoint holds nothing for is SERVED an empty list rather
//!    than refused ("if you specify a node ID that isn't valid … you receive an error" is
//!    about a malformed id), which is why waiting is all a client can do until the
//!    registration falls due;
//! 2. a send to that node reaches a terminal status on the clock too, carrying the
//!    document's own stdout verbatim in `StandardOutputContent` — the words the script
//!    wrote and the newline its `echo` ended each line with — and an empty
//!    `StandardErrorContent`, because the guest's session reports one stream.
//!
//! Every call goes through a front door: the role, its policy attachment and the instance
//! profile over IAM's `awsQuery` wire, the image lookup and the launch over Amazon EC2's
//! `ec2Query` wire, and every Systems Manager call as `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>`. No case here moves a stored due instant: the
//! registration and the invocation both fall due with nothing but the clock running, which is
//! what a client polling for `Online` depends on. Nothing waits a fixed time either — each
//! case looks again until the condition holds or a generous deadline passes.
//!
//! The filter rows differ along what the rule generalises over: which of the named ids the
//! endpoint holds, and how many ids one filter carries. The script rows differ along the
//! output's form and size: one word, several words, a long token, and several lines.

use serde_json::{json, Value};

// The ONE poll this crate keeps for "when is an instance a managed node": the filtered node
// report, looked at until the launch is reported Online.
use crate::command_fan_out_per_node_invocations::online;
use crate::ec2_instance_managed_nodes::{
    Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use crate::run_command_in_the_guest::{finished, send};

/// The filter key a client narrows the managed-node report by an instance id with
/// (`DescribeInstanceInformationRequest$Filters`, the `InstanceIds` key of
/// `service_com_amazonaws_ssm_amazonssm::state::INSTANCE_INFORMATION_FILTER_KEYS`).
const INSTANCE_IDS_KEY: &str = "InstanceIds";

/// A well-formed managed-node id this endpoint holds nothing for: `i-` and 17 hexadecimal
/// digits, which is what the node-id constraint accepts
/// (`service_com_amazonaws_ssm_amazonssm::state::MANAGED_NODE_ID_PATTERN`). Two of them, so
/// a filter can name several ids none of which is held.
const ABSENT_NODE_IDS: [&str; 2] = ["i-0123456789abcdef0", "i-00112233445566778"];

/// `InstanceInformation$PingStatus` of a node whose agent is responding, from the modelled
/// enum (`smithy_query(query="shape",shape="PingStatus")` — Online, ConnectionLost,
/// Inactive).
const ONLINE: &str = "Online";

/// `InstanceInformation$ResourceType` of a node that is an Amazon EC2 instance, from the
/// modelled enum (`smithy_query(query="shape",shape="ResourceType")` — ManagedInstance,
/// EC2Instance).
const EC2_INSTANCE: &str = "EC2Instance";

/// The name of the role the instance profile below carries, which is also what the node it
/// registers reports as its `IamRole`.
const ROLE: &str = "ssm-core-node-role";

/// Launch one instance whose profile's role carries the AWS-managed SSM-core policy and
/// answer its id. The launch instant is left ALONE: the registration this suite is about is
/// the one the clock owes on its own.
async fn launched_with_ssm_core(emulator: &Emulator, case: &str, role: &str) -> String {
    let profile = emulator
        .profile(case, role, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    emulator.launch(case, &image, Some(&profile)).await
}

/// One `DescribeInstanceInformation` narrowed to `ids`, which must be SERVED, as the entries
/// it reported.
///
/// One CALL rather than a poll: what the rows below are about is which of the named ids the
/// report matches at an instant when the registration has already been observed — the waiting
/// is [`online`]'s, and nothing here is a second answer to when an instance is managed.
async fn nodes_named(emulator: &Emulator, case: &str, ids: &[&str]) -> Vec<Value> {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "DescribeInstanceInformation",
            &json!({"Filters": [{"Key": INSTANCE_IDS_KEY, "Values": ids}]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a filter naming well-formed node ids must be served, got {status}: \
         {answered}"
    );
    answered["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE half 1: the instance becomes a managed node on its own, the filtered report names it
/// `Online`, and the same filter naming ids the endpoint holds nothing for is served an empty
/// list rather than refused.
#[tokio::test]
async fn an_instance_with_an_ssm_core_profile_is_reported_online_and_an_absent_id_matches_nothing()
{
    let case = "the registration path";
    let emulator = Emulator::new();
    let instance = launched_with_ssm_core(&emulator, case, ROLE).await;

    let entry = online(&emulator, case, &instance).await;
    assert_eq!(
        entry["InstanceId"].as_str(),
        Some(instance.as_str()),
        "case {case:?}: expected the filtered report to name the instance it filtered on, got \
         {entry}"
    );
    assert_eq!(
        entry["PingStatus"].as_str(),
        Some(ONLINE),
        "case {case:?}: expected a registered node to report PingStatus {ONLINE}, got {entry}"
    );
    assert_eq!(
        entry["ResourceType"].as_str(),
        Some(EC2_INSTANCE),
        "case {case:?}: expected the node of an EC2 instance, got {entry}"
    );
    assert_eq!(
        entry["IamRole"].as_str(),
        Some(ROLE),
        "case {case:?}: expected the node to report the role its profile carries, got {entry}"
    );

    // One row per shape of the filter's value list: which of the named ids the endpoint
    // holds, and how many ids one filter carries.
    let absent_one = ABSENT_NODE_IDS[0];
    let absent_two = ABSENT_NODE_IDS[1];
    let rows: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("the node's own id", vec![instance.as_str()], vec![instance.as_str()]),
        ("one id the endpoint holds nothing for", vec![absent_one], vec![]),
        ("two ids the endpoint holds nothing for", vec![absent_one, absent_two], vec![]),
        (
            "the node's id beside an absent one",
            vec![instance.as_str(), absent_one],
            vec![instance.as_str()],
        ),
        (
            "an absent id before the node's",
            vec![absent_one, instance.as_str()],
            vec![instance.as_str()],
        ),
    ];
    for (row_case, ids, expected) in rows {
        let reported = nodes_named(&emulator, row_case, &ids).await;
        let named: Vec<&str> = reported
            .iter()
            .map(|entry| entry["InstanceId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            named, expected,
            "case {row_case:?}: expected the filter on {ids:?} to match {expected:?}, got \
             {named:?}"
        );
    }
}

/// One script row: what the send carries and the stdout the guest's run of it writes.
struct ScriptRow {
    case: &'static str,
    script: Vec<String>,
    stdout: String,
}

/// RULE half 2: an invocation on that node reaches `Success` on the clock, carrying the
/// script's own stdout and no second stream.
#[tokio::test]
async fn an_invocation_on_that_node_carries_the_scripts_stdout_and_an_empty_error_stream() {
    let case = "the command-output path";
    let emulator = Emulator::new();
    let instance = launched_with_ssm_core(&emulator, case, "ssm-core-output-role").await;
    online(&emulator, case, &instance).await;

    // A token long enough that what comes back is not one short word: the rule is about the
    // output being the guest's own, whatever its size.
    let long_token = "a1b2c3d4e5f6a7b8".repeat(4);
    let rows = vec![
        ScriptRow {
            case: "one word echoed",
            script: vec!["echo mf-7f3c91".to_string()],
            stdout: "mf-7f3c91\n".to_string(),
        },
        ScriptRow {
            case: "several words echoed",
            script: vec!["echo mf token 7f3c91".to_string()],
            stdout: "mf token 7f3c91\n".to_string(),
        },
        ScriptRow {
            case: "a long token echoed",
            script: vec![format!("echo {long_token}")],
            stdout: format!("{long_token}\n"),
        },
        ScriptRow {
            case: "several lines echoed",
            script: vec![
                "echo first-token".to_string(),
                "echo second-token".to_string(),
            ],
            stdout: "first-token\nsecond-token\n".to_string(),
        },
    ];

    // Every send is made first and every invocation is then waited for: each one's terminal
    // status is owed from its OWN send, so the waits overlap instead of adding up.
    let mut sent = Vec::new();
    for row in &rows {
        let script: Vec<&str> = row.script.iter().map(String::as_str).collect();
        sent.push(send(&emulator, row.case, &instance, &script).await);
    }

    for (row, command_id) in rows.iter().zip(&sent) {
        let answered = finished(&emulator, row.case, command_id, &instance).await;
        assert_eq!(
            answered["Status"].as_str(),
            Some("Success"),
            "case {:?}: expected the invocation to succeed, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StatusDetails"].as_str(),
            Some("Success"),
            "case {:?}: expected the detailed status to be the same word, got {answered}",
            row.case
        );
        assert_eq!(
            answered["ResponseCode"].as_i64(),
            Some(0),
            "case {:?}: expected the exit code of a script the guest performed, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(row.stdout.as_str()),
            "case {:?}: expected the script's own stdout {:?}, got {answered}",
            row.case,
            row.stdout
        );
        assert_eq!(
            answered["StandardErrorContent"].as_str(),
            Some(""),
            "case {:?}: expected no second stream, got {answered}",
            row.case
        );
    }
}
