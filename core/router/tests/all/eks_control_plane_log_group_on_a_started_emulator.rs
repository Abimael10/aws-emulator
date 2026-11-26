// SPDX-License-Identifier: CC0-1.0

//! The control plane log group an Amazon EKS cluster's `logging` owes CloudWatch Logs, on the
//! emulator a client actually runs: the BUILT BINARY, on its own port, with request
//! authentication on, reached over TLS by a signing client — and with the cluster's inputs
//! resolved against the EC2 and IAM that binary serves.
//!
//! RULE: `LogSetup$enabled` is "If a log type is enabled, that log type exports its control
//! plane logs to CloudWatch Logs. If a log type isn't enabled, that log type doesn't export its
//! control plane logs", so the state of a cluster's log types is observable in CloudWatch Logs
//! and nowhere else: while EVERY type is disabled the derived group
//! `/aws/eks/<cluster name>/cluster` does not exist, and once an `UpdateClusterConfig` enabling
//! a type has reached `Successful` the service itself serves that group, found the way a client
//! finds it — `DescribeLogGroups` on that name as a prefix.
//!
//! The sibling suite [`crate::eks_disabled_logging_then_enabled_reaches_cloudwatch_logs`] pins
//! this same rule over the assembled router in process, and over a table of configurations;
//! what only THIS suite reaches is the emulator as it is served:
//!
//! * the export Amazon EKS makes is a call it makes INTO ITS OWN BINARY, over the in-process
//!   callback, on that binary's multi-thread runtime and under its own request authentication —
//!   none of which an in-process `dispatch` exercises;
//! * every call here crosses a real TLS connection to a real port, as a client's does;
//! * and when the group does not appear, the emulator's own log is what says why, so this suite
//!   quotes it rather than reporting an absence with no cause.
//!
//! One row, because starting the binary costs what starting a binary costs: the contrast itself
//! — all five types disabled at create, then `api` enabled — which is the one walk the sibling
//! suite's whole table generalises.

use std::time::Duration;

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};
use crate::query_wire::opt_element;

/// The API version a client sends in each query-protocol form body.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The `X-Amz-Target` prefix CloudWatch Logs names an operation under, and the media type its
/// `awsJson1_1` wire carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// Every member of `LogType`, in the order the model declares them
/// (`smithy_query(query="shape",shape="LogType",
/// model_path="services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json")`).
const DECLARED_LOG_TYPES: [&str; 5] = [
    "api",
    "audit",
    "authenticator",
    "controllerManager",
    "scheduler",
];

/// The type this row turns on, and the control plane component whose stream carries it.
const ENABLED_LOG_TYPE: &str = "api";
const ENABLED_COMPONENT_PREFIX: &str = "kube-apiserver-";

/// The `UpdateStatus` an update still running reports, and the terminal one a configuration
/// change that was applied settles on.
const UPDATE_IN_PROGRESS: &str = "InProgress";
const UPDATE_SUCCESSFUL: &str = "Successful";

/// The cluster status an update may be started on.
const CLUSTER_ACTIVE: &str = "ACTIVE";

/// The cluster this row walks, and the role it is created with.
const CLUSTER: &str = "started-logging-contrast";
const ROLE: &str = "eksStartedLoggingRole";

/// The managed policy AWS publishes for a cluster's service role.
const CLUSTER_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonEKSClusterPolicy";

/// A trust policy letting the Amazon EKS service principal assume the cluster's role.
const EKS_TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"eks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// How long to wait between two looks at a condition the clock owes, and how many looks bound
/// such a wait. Generous on purpose: what is asserted is that the condition holds at all, never
/// how fast the host runs.
const LOOK_INTERVAL: Duration = Duration::from_millis(200);
const LOOKS: usize = 300;

/// The log group one cluster's control plane logs are exported to.
fn cluster_log_group(cluster: &str) -> String {
    format!("/aws/eks/{cluster}/cluster")
}

/// One `logging.clusterLogging` payload: one entry per `(types, enabled)` pair.
fn cluster_logging(types: &[&str], enabled: bool) -> Value {
    json!([{ "types": types, "enabled": enabled }])
}

/// One Amazon EKS call (restJson1), answered as parsed JSON; the status must be 200.
fn eks(emulator: &Started, method: &str, path: &str, body: Option<Value>, case: &str) -> Value {
    let payload = body.map(|json| json.to_string()).unwrap_or_default();
    let (status, text) = emulator.call(
        "eks",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: eks {method} {path} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One CloudWatch Logs call, answered as parsed JSON; the status must be 200.
fn logs(emulator: &Started, operation: &str, body: &Value, case: &str) -> Value {
    let (status, text) = emulator.rpc(
        "logs",
        &format!("{LOGS_TARGET_PREFIX}.{operation}"),
        LOGS_MEDIA_TYPE,
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: logs {operation} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Whether CloudWatch Logs serves the cluster's log group, found as a client finds it.
fn holds_log_group(emulator: &Started, case: &str) -> bool {
    let group = cluster_log_group(CLUSTER);
    let answer = logs(
        emulator,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": &group }),
        case,
    );
    answer["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|held| held["logGroupName"] == json!(group))
}

/// The streams the cluster's group holds, by name.
fn log_streams(emulator: &Started, case: &str) -> Vec<String> {
    let answer = logs(
        emulator,
        "DescribeLogStreams",
        &json!({ "logGroupName": cluster_log_group(CLUSTER) }),
        case,
    );
    answer["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_started_emulator_serves_the_log_group_an_enabled_log_type_owes() {
    let emulator = Started::start();
    let case = "api enabled on a cluster created with every type disabled";

    // ---------------------------------------------------------------------
    // Setup, through the services that OWN these inputs: a VPC with two
    // subnets in two Availability Zones, and a role IAM holds whose trust
    // policy admits the Amazon EKS service principal.
    // ---------------------------------------------------------------------
    let (status, created) = emulator.form_call(
        "ec2",
        EC2_API_VERSION,
        "CreateVpc",
        &[("CidrBlock", "10.42.0.0/16")],
    );
    assert_eq!(
        status, 200,
        "setup: CreateVpc must succeed, got {status} with {created}"
    );
    let vpc_id = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("setup: no <vpcId> in {created}"));
    let mut subnets: Vec<String> = Vec::new();
    for (cidr_block, zone) in [
        ("10.42.1.0/24", format!("{REGION}a")),
        ("10.42.2.0/24", format!("{REGION}b")),
    ] {
        let (status, answer) = emulator.form_call(
            "ec2",
            EC2_API_VERSION,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", cidr_block),
                ("AvailabilityZone", zone.as_str()),
            ],
        );
        assert_eq!(
            status, 200,
            "setup: CreateSubnet {cidr_block} must succeed, got {status} with {answer}"
        );
        subnets.push(
            opt_element(&answer, "subnetId")
                .unwrap_or_else(|| panic!("setup: no <subnetId> in {answer}")),
        );
    }
    let (status, role) = emulator.form_call(
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", EKS_TRUST_POLICY),
        ],
    );
    assert_eq!(
        status, 200,
        "setup: CreateRole must succeed, got {status} with {role}"
    );
    let (status, attached) = emulator.form_call(
        "iam",
        IAM_API_VERSION,
        "AttachRolePolicy",
        &[("RoleName", ROLE), ("PolicyArn", CLUSTER_POLICY_ARN)],
    );
    assert_eq!(
        status, 200,
        "setup: AttachRolePolicy must succeed, got {status} with {attached}"
    );
    let role_arn = opt_element(&role, "Arn")
        .unwrap_or_else(|| panic!("setup: no <Arn> in the created role {role}"));

    // ---------------------------------------------------------------------
    // The NEGATIVE arm: a cluster created with every declared log type
    // explicitly disabled exports nothing, so no group of its name exists.
    // ---------------------------------------------------------------------
    eks(
        &emulator,
        "POST",
        "/clusters",
        Some(json!({
            "name": CLUSTER,
            "roleArn": role_arn,
            "resourcesVpcConfig": { "subnetIds": subnets },
            "logging": { "clusterLogging": cluster_logging(&DECLARED_LOG_TYPES, false) },
        })),
        case,
    );
    let mut reported = String::new();
    let mut active = false;
    for _ in 0..LOOKS {
        reported = eks(&emulator, "GET", &format!("/clusters/{CLUSTER}"), None, case)["cluster"]
            ["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == CLUSTER_ACTIVE {
            active = true;
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert!(
        active,
        "case {case:?}: the cluster must reach {CLUSTER_ACTIVE}, last reported {reported:?}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    assert!(
        !holds_log_group(&emulator, case),
        "case {case:?}: with every log type disabled, CloudWatch Logs must serve no {}",
        cluster_log_group(CLUSTER)
    );

    // ---------------------------------------------------------------------
    // The POSITIVE control: enabling one type, polled the way a client polls
    // it, and then the group that type's export owes.
    // ---------------------------------------------------------------------
    let started = eks(
        &emulator,
        "POST",
        &format!("/clusters/{CLUSTER}/update-config"),
        Some(json!({
            "logging": { "clusterLogging": cluster_logging(&[ENABLED_LOG_TYPE], true) },
        })),
        case,
    );
    let update_id = started["update"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the update must carry an id, got {started}"))
        .to_string();
    let mut status = UPDATE_IN_PROGRESS.to_string();
    for _ in 0..LOOKS {
        status = eks(
            &emulator,
            "GET",
            &format!("/clusters/{CLUSTER}/updates/{update_id}"),
            None,
            case,
        )["update"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status != UPDATE_IN_PROGRESS {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        status, UPDATE_SUCCESSFUL,
        "case {case:?}: the logging update must settle on {UPDATE_SUCCESSFUL}, got {status:?}; \
         the emulator's log:\n{}",
        emulator.log()
    );

    let mut held = false;
    for _ in 0..LOOKS {
        if holds_log_group(&emulator, case) {
            held = true;
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert!(
        held,
        "case {case:?}: once {ENABLED_LOG_TYPE:?} is enabled CloudWatch Logs must serve {}, got \
         no such group; the emulator's log:\n{}",
        cluster_log_group(CLUSTER),
        emulator.log()
    );
    let streams = log_streams(&emulator, case);
    assert!(
        streams
            .iter()
            .any(|stream| stream.starts_with(ENABLED_COMPONENT_PREFIX)),
        "case {case:?}: the enabled type must export under a {ENABLED_COMPONENT_PREFIX:?} \
         stream, got {streams:?}; the emulator's log:\n{}",
        emulator.log()
    );
}
