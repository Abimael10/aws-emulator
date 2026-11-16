// SPDX-License-Identifier: CC0-1.0

//! WHERE the agent version a container instance reports comes from, and what
//! `UpdateContainerAgent` answers for an instance no version is recorded for. Driven end
//! to end over the shared wire harness in [`crate::ecs_wire`], with the Amazon EC2 service
//! mounted beside this one for the launch half, so every value asserted is read back
//! through `RegisterContainerInstance`, `DescribeContainerInstances` and
//! `UpdateContainerAgent`.
//!
//! # The rules under test
//!
//! > `ContainerInstance$versionInfo` is "the version information for the Amazon ECS
//! > container agent and Docker daemon RUNNING on the container instance": it is recorded
//! > by the service from the agent it observes, never taken from the request, so a
//! > registration's own `versionInfo` is discarded — the response and every later read
//! > carry a version only where this service provisioned the agent itself.
//!
//! > An error's wire `message` is the sentence the service sends for that error class, not
//! > the documentation of the error shape: an instance no agent version is recorded for is
//! > refused with `MissingVersionException` and the fixed text `Unknown current version`.
//!
//! # What the rows span
//!
//! The registration table varies the `versionInfo` the request carries along what the rule
//! generalises over: whether the member is sent at all, which of its three members are
//! filled, a version far ABOVE the one this emulator publishes and one EQUAL to it (so a
//! caller cannot claim to be current either), a blank version, and an empty structure.
//! The recorded divergence — a full, synthetic `versionInfo` echoed back — is one row. The
//! contrast is the other half of the rule: an instance whose agent this emulator
//! provisioned, launched through Amazon EC2's own API, DOES report a version, and that
//! version is none of the ones a caller claimed.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2_element, ec2_form, emulator_with_ec2, identity_document,
    identity_document_signature, list, ok, refusal, service, string, SIGNED_REGION,
};

/// The cluster every registration below joins.
const CLUSTER: &str = "agentversions";

/// `MissingVersionException`'s wire message: fixed text, with no variable part.
const UNKNOWN_CURRENT_VERSION: &str = "Unknown current version";

/// How often the launch half re-reads the cluster's capacity, and how long it keeps
/// trying: the registration the launch owes is clock-owed, so the condition is polled for
/// rather than slept on, with generous room for a loaded machine.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// The AMI the launch boots — well-formed (`ami-<17 hexadecimal digits>`), which is all a
/// launch needs.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// One row of the registration table: the case, and the `versionInfo` the request carries.
struct Row {
    case: &'static str,
    version_info: Option<Value>,
}

/// Every `versionInfo` a registration may present. The versions named here are what a
/// CALLER claims; none of them may become what an instance reports.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no versionInfo at all",
            version_info: None,
        },
        Row {
            case: "a full synthetic versionInfo",
            version_info: Some(json!({
                "agentVersion": "1.0.0",
                "agentHash": "0".repeat(40),
                "dockerVersion": "DockerVersion: 1.0.0",
            })),
        },
        Row {
            case: "a version far above the published one",
            version_info: Some(json!({ "agentVersion": "9.99.99" })),
        },
        Row {
            case: "a version equal to the published one",
            version_info: Some(json!({ "agentVersion": PUBLISHED_AGENT_VERSION })),
        },
        Row {
            case: "a blank agentVersion",
            version_info: Some(json!({ "agentVersion": "   " })),
        },
        Row {
            case: "a dockerVersion only",
            version_info: Some(json!({ "dockerVersion": "DockerVersion: 25.0.3" })),
        },
        Row {
            case: "an agentHash only",
            version_info: Some(json!({ "agentHash": "a".repeat(40) })),
        },
        Row {
            case: "an empty versionInfo",
            version_info: Some(json!({})),
        },
    ]
}

/// The agent version this emulator publishes as the update target
/// (`state::LATEST_CONTAINER_AGENT_VERSION`). A row claims it to prove a caller cannot
/// make an instance look current either.
const PUBLISHED_AGENT_VERSION: &str = "1.89.3";

/// Register one container instance into `CLUSTER` presenting `version_info`, and answer
/// the `containerInstance` the registration reported.
async fn register(
    router: &MultiServiceRouter,
    ec2_instance_id: &str,
    version_info: Option<&Value>,
    case: &str,
) -> Value {
    let mut request = json!({
        "cluster": CLUSTER,
        "instanceIdentityDocument": identity_document(ec2_instance_id),
        "instanceIdentityDocumentSignature": identity_document_signature(),
    });
    if let Some(version_info) = version_info {
        request["versionInfo"] = version_info.clone();
    }
    let body = ok(router, "RegisterContainerInstance", request, case).await;
    body["containerInstance"].clone()
}

/// Read ONE container instance back through `DescribeContainerInstances` — the stored
/// record, not the answer of the call that wrote it.
async fn describe(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": [arn] }),
        case,
    )
    .await;
    list(&body, "containerInstances", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one instance back, got {body}"))
}

#[tokio::test]
async fn a_registration_records_no_agent_version_of_its_own() {
    let router = service();
    create_cluster(&router, CLUSTER, "setup").await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let ec2_instance_id = format!("i-0{index:016x}");
        let registered =
            register(&router, &ec2_instance_id, row.version_info.as_ref(), case).await;
        assert_eq!(
            registered["versionInfo"],
            Value::Null,
            "case {case:?}: expected the registration to record no versionInfo, got \
             {registered}"
        );
        let arn = string(&registered, "containerInstanceArn", case);

        // The stored record is what a later reader sees, so the discard reached the store
        // rather than only the response of the call that made it.
        let read = describe(&router, CLUSTER, &arn, case).await;
        assert_eq!(
            read["versionInfo"],
            Value::Null,
            "case {case:?}: expected a later read to report no versionInfo, got {read}"
        );

        // With no version recorded, Amazon ECS cannot tell what an update would start
        // from: the condition `MissingVersionException` names, carrying the message the
        // service sends for that class.
        let (code, message) = refusal(
            &router,
            "UpdateContainerAgent",
            json!({ "cluster": CLUSTER, "containerInstance": arn.clone() }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("MissingVersionException", UNKNOWN_CURRENT_VERSION),
            "case {case:?}: expected the missing-version refusal, got {code:?} with \
             {message:?}"
        );

        // The refused update wrote nothing either: no stage of an update is reported.
        let after = describe(&router, CLUSTER, &arn, case).await;
        assert_eq!(
            after["agentUpdateStatus"],
            Value::Null,
            "case {case:?}: a refused update reports no stage, got {after}"
        );

        // A RE-registration of the same machine presenting a version does not record one
        // either, whichever row it repeats.
        let again = register(
            &router,
            &ec2_instance_id,
            Some(&json!({ "agentVersion": "2.3.4", "dockerVersion": "DockerVersion: 24.0.0" })),
            case,
        )
        .await;
        assert_eq!(
            again["versionInfo"],
            Value::Null,
            "case {case:?}: expected a re-registration to record no versionInfo, got {again}"
        );
    }
}

#[tokio::test]
async fn an_instance_whose_agent_this_service_provisioned_reports_its_version() {
    let router = emulator_with_ec2();
    let case = "an instance launched into the cluster";
    create_cluster(&router, CLUSTER, case).await;

    // The network the launch is placed in, created through Amazon EC2's own API.
    let vpc = ec2_element(
        &ec2_form(&router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let zone = format!("{SIGNED_REGION}a");
    let subnet = ec2_element(
        &ec2_form(
            &router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.90.0.0/20"),
                ("AvailabilityZone", zone.as_str()),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );

    // An instance whose user data configures the container agent for this cluster: the
    // agent this emulator provisions is the one it publishes, so the version the
    // registration records is the SERVICE's, not any caller's.
    use ::base64::Engine as _;
    let user_data = ::base64::engine::general_purpose::STANDARD
        .encode(format!("#!/bin/bash\necho ECS_CLUSTER={CLUSTER} >> /etc/ecs/ecs.config\n"));
    ec2_form(
        &router,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", "t3.micro"),
            ("SubnetId", subnet.as_str()),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", user_data.as_str()),
        ],
        case,
    )
    .await;

    // The registration the launch owes is the clock's, so the condition is polled for.
    let deadline = Instant::now() + POLL_BUDGET;
    let arn = loop {
        let listed = ok(
            &router,
            "ListContainerInstances",
            json!({ "cluster": CLUSTER }),
            case,
        )
        .await;
        if let Some(arn) = list(&listed, "containerInstanceArns", case)
            .first()
            .and_then(|value| value.as_str())
        {
            break arn.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the launched instance to register, got {listed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let read = describe(&router, CLUSTER, &arn, case).await;
    let reported = read["versionInfo"]["agentVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !reported.trim().is_empty(),
        "case {case:?}: expected the service's own agent version to be recorded, got {read}"
    );
    // And it is none of the versions a CALLER may claim through a registration: the value
    // is the service's own.
    let claimed: BTreeSet<String> = rows()
        .iter()
        .filter_map(|row| row.version_info.as_ref())
        .filter_map(|info| info["agentVersion"].as_str())
        .filter(|version| *version != PUBLISHED_AGENT_VERSION)
        .map(str::to_string)
        .collect();
    assert!(
        !claimed.contains(&reported),
        "case {case:?}: the recorded version must not be one a caller claimed, got \
         {reported:?} among {claimed:?}"
    );

    // An agent already on the version this service publishes has nothing to update to —
    // the other half of the agent-update rule, reached because a version IS recorded.
    let (code, _) = refusal(
        &router,
        "UpdateContainerAgent",
        json!({ "cluster": CLUSTER, "containerInstance": arn }),
        case,
    )
    .await;
    assert_eq!(
        code, "NoUpdateAvailableException",
        "case {case:?}: expected the no-update refusal for an agent at the published \
         version, got {code:?}"
    );
}
