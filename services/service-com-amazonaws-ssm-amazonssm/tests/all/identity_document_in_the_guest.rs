// SPDX-License-Identifier: CC0-1.0

//! The one Run Command invocation every ECS-on-EC2 agent makes before it can register the
//! machine it is in: reading that machine's SIGNED INSTANCE IDENTITY DOCUMENT out of the
//! instance metadata service, driven end to end over this service's own `awsJson1_1` wire
//! with Amazon EC2 and IAM on one engine ([`super::ec2_instance_managed_nodes`]).
//!
//! RULE under test: AN INVOCATION IS THE DOCUMENT'S SCRIPT PERFORMED IN THE NODE'S GUEST, and
//! that guest publishes its own machine's facts at the link-local metadata address — so the
//! documented read ("this document can be found by running the following command from the
//! instance", followed by a `curl` of that address, Amazon ECS's
//! `RegisterContainerInstanceRequest$instanceIdentityDocument`) SUCCEEDS and answers
//! the identity document of the node the command was sent to, with the signature its sibling
//! path publishes beside it.
//!
//! What the guest publishes, over the whole catalog of metadata categories and in both the
//! IMDSv1 and the IMDSv2 form, is tabled in the EC2 crate's
//! `tests/all/instance_guest_metadata_service.rs`; this suite is about reaching it the way an
//! agent does — through Run Command — and so has one case rather than a table.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The published document that runs a shell script on a Linux node.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The address the metadata service is published at, as the documented read spells it.
/// The metadata service's own address, read from the service that publishes it to its guest
/// rather than written here, with the scheme a documented read names it under.
fn metadata() -> String {
    format!("http://{}", ec2::state::metadata_address())
}

/// How long the case looks for the invocation to finish, far longer than the span the clock
/// owes that move after.
const FINISH_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `CommandInvocationStatus` values that are terminal (`Command$StatusDetails`).
const TERMINAL: &[&str] = &["Success", "Cancelled", "Failed", "TimedOut"];

/// How many bytes a signature under a 2048-bit key is, which is what the published
/// signature's base64 has to decode to.
const SIGNATURE_BYTES: usize = 256;

/// RULE: the agent's own read of the machine's identity reaches the guest and answers that
/// machine's signed document.
#[tokio::test]
async fn an_invocation_reads_the_node_s_own_identity_document() {
    let case = "the identity document read";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "identity-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );

    let (status, answered) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": [instance],
                "Parameters": {"commands": [
                    // An Amazon Linux 2023 AMI is IMDSv2-only (`Image$ImdsSupport` is
                    // `v2.0`, which launches the instance with `HttpTokens` `required`), so
                    // the agent asks for a session token first and presents it on each read.
                    format!(
                        "TOKEN=$(curl -X PUT {}/latest/api/token \
                         -H \"X-aws-ec2-metadata-token-ttl-seconds: 21600\")",
                        metadata()
                    ),
                    format!(
                        "curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
                         {}/latest/dynamic/instance-identity/document/",
                        metadata()
                    ),
                    format!(
                        "curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
                         {}/latest/dynamic/instance-identity/signature",
                        metadata()
                    ),
                ]},
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
    finish_due(&emulator, &command_id);
    let finished = finished(&emulator, case, &command_id, &instance).await;

    assert_eq!(
        finished["Status"].as_str(),
        Some("Success"),
        "case {case:?}: expected the documented read to succeed in the guest, got {finished}"
    );
    assert_eq!(
        finished["ResponseCode"].as_i64(),
        Some(0),
        "case {case:?}: expected the read's own exit code, got {finished}"
    );
    let output = finished["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut lines = output.lines();
    let document = lines
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the document, got {output:?}"));
    let parsed: Value = serde_json::from_str(document.trim())
        .unwrap_or_else(|error| panic!("case {case:?}: {error}: {document:?}"));
    assert_eq!(
        parsed["instanceId"].as_str(),
        Some(instance.as_str()),
        "case {case:?}: the document must name the node the command was sent to, got {parsed}"
    );
    assert_eq!(
        parsed["region"].as_str(),
        Some(REGION),
        "case {case:?}: the document must name the Region the node runs in, got {parsed}"
    );
    assert_eq!(
        parsed["accountId"].as_str(),
        Some(engine::emulator_account_id()),
        "case {case:?}: the document must name the account that holds it, got {parsed}"
    );

    let signature = lines
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the signature, got {output:?}"));
    let decoded = aws_smithy_types::base64::decode(signature.trim()).unwrap_or_else(|error| {
        panic!("case {case:?}: the signature must be base64 ({error}): {signature:?}")
    });
    assert_eq!(
        decoded.len(),
        SIGNATURE_BYTES,
        "case {case:?}: expected a signature of the document's own key size, got {} byte(s)",
        decoded.len()
    );
}

/// Move the instant the clock owes `command_id`'s invocations their terminal status to now,
/// so a case about WHAT the guest answered does not wait out the span — the same way this
/// crate's other clock-owed suites move an owed instant through the store.
fn finish_due(emulator: &Emulator, command_id: &str) {
    let state = emulator.ssm_state();
    let mut store = state.lock();
    let command = store
        .commands
        .get_mut(&(REGION.to_string(), command_id.to_string()))
        .unwrap_or_else(|| panic!("the store must hold the command {command_id}"));
    for invocation in command.invocations.values_mut() {
        if let Some(outcome) = invocation.outcome.as_mut() {
            outcome.due_at = SystemTime::now();
        }
    }
}

/// Look at the invocation until it reaches a terminal status, up to a deadline far longer
/// than the span that move is owed after.
async fn finished(emulator: &Emulator, case: &str, command_id: &str, instance: &str) -> Value {
    let deadline = Instant::now() + FINISH_DEADLINE;
    loop {
        let (status, answered) = emulator
            .ssm(
                REGION,
                "GetCommandInvocation",
                &json!({"CommandId": command_id, "InstanceId": instance}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status}: {answered}"
        );
        let reached = answered["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&reached.as_str()) {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the invocation to finish with nothing but the clock \
             running, still {reached:?}: {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
