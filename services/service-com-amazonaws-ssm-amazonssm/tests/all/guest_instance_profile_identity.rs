// SPDX-License-Identifier: CC0-1.0

//! What the GUEST of a managed node learns about the identity it runs as: the instance
//! profile its launch gave it and the NAME of the role that profile carries, read the way an
//! agent reads it — a Run Command invocation that curls the instance metadata service — with
//! Systems Manager, Amazon EC2 and IAM on one engine
//! ([`super::ec2_instance_managed_nodes`]).
//!
//! RULE under test: an instance's metadata service publishes the IAM instance profile the
//! instance runs as — `iam/info` carrying that profile's ARN and id, and
//! `iam/security-credentials/` listing the name of the ROLE it carries — and an instance that
//! runs as no profile publishes neither. The role's name is the one IAM holds, which is why a
//! profile and the role inside it may be named differently and the listing still names the
//! role.
//!
//! The rows differ along what the rule generalises over: the path read (the profile document
//! and the role listing) and whether the profile's name is the role's name or another name
//! entirely. What such a machine publishes when it runs as NO profile, and when nothing
//! resolved its role, is tabled in the EC2 crate's
//! `tests/all/instance_guest_iam_profile_metadata.rs`, where a machine can be launched
//! without one — an instance with no profile is no managed node, so no command of this
//! service reaches its guest.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The published document that runs a shell script on a Linux node.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The `CommandInvocationStatus` values that are terminal (`Command$StatusDetails`).
const TERMINAL: &[&str] = &["Success", "Cancelled", "Failed", "TimedOut"];

/// How long a case looks for an invocation to finish, far longer than the span the clock owes
/// that move after.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look-again cycle pauses.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The metadata service's own address, read from the service that publishes it to its guest
/// rather than written here, with the scheme a documented read names it under.
fn metadata() -> String {
    format!("http://{}", ec2::state::metadata_address())
}

/// RULE: the guest's metadata service publishes the profile the instance runs as and the name
/// of the role that profile carries — and publishes neither for an instance that runs as no
/// profile.
#[tokio::test]
async fn a_guest_reads_the_role_its_instance_profile_carries() {
    // (what the case is, the profile's name, the role's name)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a profile and role named alike, as a client that names both once creates them",
            "imds-alike",
            "imds-alike",
        ),
        (
            "a profile whose role is named differently, as a template's generated names are",
            "stack-instance-profile-1A2B3C",
            "stack-ssm-role-4D5E6F",
        ),
    ];
    for (case, profile_name, role_name) in rows {
        let emulator = Emulator::new();
        let image = emulator.image(case, AL2023_NAME_PATTERN).await;
        let profile = profile_carrying_role(&emulator, case, profile_name, role_name).await;
        let instance = emulator.launch(case, &image, Some(&profile)).await;
        emulator.launched_ago(&instance, launched_long_enough_ago());
        online(&emulator, case, &instance).await;

        let read = read_in_guest(
            &emulator,
            case,
            &instance,
            &[
                format!(
                    "TOKEN=$(curl -X PUT {}/latest/api/token \
                     -H \"X-aws-ec2-metadata-token-ttl-seconds: 21600\")",
                    metadata()
                ),
                format!(
                    "curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
                     {}/latest/meta-data/iam/security-credentials/",
                    metadata()
                ),
                format!(
                    "curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
                     {}/latest/meta-data/iam/info",
                    metadata()
                ),
            ],
        )
        .await;
        let mut lines = read.lines();
        let listed = lines.next().unwrap_or_default().trim().to_string();
        assert_eq!(
            listed, *role_name,
            "case {case:?}: expected the metadata service to list the role \
             {role_name:?}, got {listed:?} (whole read {read:?})"
        );
        let document = lines.next().unwrap_or_default().trim().to_string();
        let parsed: Value = serde_json::from_str(&document).unwrap_or_else(|error| {
            panic!("case {case:?}: the profile document must be JSON ({error}): {document:?}")
        });
        assert_eq!(
            parsed["Code"].as_str(),
            Some("Success"),
            "case {case:?}: expected the profile document's own code, got {parsed}"
        );
        let arn = parsed["InstanceProfileArn"].as_str().unwrap_or_default();
        assert!(
            arn.ends_with(&format!("instance-profile/{profile_name}")),
            "case {case:?}: expected the document to carry the profile's ARN, got {parsed}"
        );
        assert!(
            parsed["InstanceProfileId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the document to carry the profile's id, got {parsed}"
        );
    }
}

/// Create a role named `role_name` carrying the AWS-managed policy a managed node's profile
/// carries, an instance profile named `profile_name` holding it, and answer the profile's
/// name — the chain a template's generated names create, where the two names differ.
async fn profile_carrying_role(
    emulator: &Emulator,
    case: &str,
    profile_name: &str,
    role_name: &str,
) -> String {
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    emulator
        .iam(
            case,
            "CreateRole",
            &[("RoleName", role_name), ("AssumeRolePolicyDocument", trust)],
        )
        .await;
    emulator
        .iam(
            case,
            "AttachRolePolicy",
            &[("RoleName", role_name), ("PolicyArn", SSM_CORE_POLICY_ARN)],
        )
        .await;
    emulator
        .iam(
            case,
            "CreateInstanceProfile",
            &[("InstanceProfileName", profile_name)],
        )
        .await;
    emulator
        .iam(
            case,
            "AddRoleToInstanceProfile",
            &[
                ("InstanceProfileName", profile_name),
                ("RoleName", role_name),
            ],
        )
        .await;
    profile_name.to_string()
}

/// Look until this service reports `instance_id` as a managed node whose agent is responding.
async fn online(emulator: &Emulator, case: &str, instance_id: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = emulator
            .ssm(
                REGION,
                "DescribeInstanceInformation",
                &json!({"Filters": [{"Key": "InstanceIds", "Values": [instance_id]}]}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstanceInformation must be served, got {status}: {answered}"
        );
        let reported = answered["InstanceInformationList"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .and_then(|node| node["PingStatus"].as_str())
            .unwrap_or_default()
            .to_string();
        if reported == "Online" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {instance_id} to be a managed node, got {reported:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Send `commands` to the node's guest through Run Command, and answer what the guest wrote
/// once the invocation has succeeded.
async fn read_in_guest(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    commands: &[String],
) -> String {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": [instance_id],
                "Parameters": {"commands": commands},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status}: {answered}"
    );
    let command_id = answered["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint a command id: {answered}"))
        .to_string();
    // The invocation's move off `Pending` is owed a span after the send and a case about WHAT
    // the guest answered does not wait it out: the stored due instant is moved to now, the way
    // this crate's other guest-run suites do.
    {
        let state = emulator.ssm_state();
        let mut store = state.lock();
        let command = store
            .commands
            .get_mut(&(REGION.to_string(), command_id.clone()))
            .unwrap_or_else(|| panic!("case {case:?}: the store must hold the command"));
        for invocation in command.invocations.values_mut() {
            if let Some(outcome) = invocation.outcome.as_mut() {
                outcome.due_at = SystemTime::now();
            }
        }
    }
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, invocation) = emulator
            .ssm(
                REGION,
                "GetCommandInvocation",
                &json!({"CommandId": command_id, "InstanceId": instance_id}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation read must be served, got {status}: {invocation}"
        );
        let reached = invocation["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&reached.as_str()) {
            assert_eq!(
                reached, "Success",
                "case {case:?}: expected the read to succeed in the guest, got {invocation}"
            );
            return invocation["StandardOutputContent"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the invocation to finish, still {reached:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
