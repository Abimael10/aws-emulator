// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::EKS::Cluster` with control plane
//! LOGGING gets a cluster that exports exactly the log types it enabled.
//!
//! The RULE under test: **every property an `AWS::EKS::Cluster` declaration states is
//! sent to `CreateCluster` on the member that takes it — the type's
//! `Logging.ClusterLogging.EnabledTypes` entries as the `types` of one enabled
//! `LogSetup` of `logging.clusterLogging` (`smithy_query(query="shape",
//! shape="Logging,LogSetup")`) — so the cluster a stack creates reports the log types
//! the template enabled and exports each of them to the cluster's own log group in
//! Amazon CloudWatch Logs, while one that enables none has no log group at all.** A
//! declared property CloudFormation cannot send is refused rather than created
//! without, which fails the resource and rolls the stack back: a stack reporting
//! `CREATE_COMPLETE` names a cluster configured as it was declared.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the call,
//! Amazon EKS serves it and provisions the export, and Amazon CloudWatch Logs owns
//! the log group — so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: how many log types the
//! template enables (none at all, one, two, and every type the model declares), which
//! is exactly what the mapped property carries.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::Duration;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// How long one poll waits before reading again, and how many times: a cluster's
/// activation and the stack's completion are both owed from an instant no fixed sleep
/// can name, so the suite polls rather than sleeping once.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS: usize = 300;

/// The `StackStatus` a completed create reports, and the one a rolled-back create
/// settles at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Every control plane log type `LogType` declares
/// (`smithy_query(query="shape", shape="LogType")`), which is the set a template can
/// enable and the set a cluster reports.
const LOG_TYPES: [&str; 5] = [
    "api",
    "audit",
    "authenticator",
    "controllerManager",
    "scheduler",
];

/// One answer, as this suite reads it.
#[derive(Debug)]
struct Answer {
    status: u16,
    text: String,
    body: Value,
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is
/// what routes the request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Drive one request at `signing_name`'s wire.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    target: Option<&str>,
    body: String,
) -> Answer {
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, text, body }
}

/// Percent-encode one value: the test crate's single encoder.
fn encoded(value: &str) -> String {
    crate::query_wire::form_encode(value)
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&encoded(key));
        body.push('=');
        body.push_str(&encoded(value));
    }
    send(
        router,
        "cloudformation",
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        None,
        body,
    )
    .await
}

/// Call one Amazon EKS operation over its restJson1 wire.
async fn eks(router: &MultiServiceRouter, method: &str, uri: &str) -> Answer {
    send(
        router,
        "eks",
        method,
        uri,
        "application/json",
        None,
        String::new(),
    )
    .await
}

/// Call one Amazon CloudWatch Logs operation over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    send(
        router,
        "logs",
        "POST",
        "/",
        "application/x-amz-json-1.1",
        Some(&format!("Logs_20140328.{operation}")),
        body.to_string(),
    )
    .await
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(xml: &str, name: &str) -> Option<String> {
    crate::query_wire::elements(xml, name).into_iter().next()
}

/// Poll the stack `stack` until it reports a status the create has SETTLED in, and
/// answer that status.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut last = String::new();
    for _ in 0..POLLS {
        let answer = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: DescribeStacks must answer, got {answer:?}"
        );
        last = element(&answer.text, "StackStatus").unwrap_or_default();
        if !last.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// One row: how many log types the template enables, and so which ones the cluster
/// must report and export.
struct Row {
    case: &'static str,
    stack: &'static str,
    cluster: &'static str,
    /// The log types the template's `Logging.ClusterLogging.EnabledTypes` names, and
    /// the empty slice for a template that declares the property not at all.
    enabled: &'static [&'static str],
    /// Whether the template declares the `Logging` property at all.
    declares_logging: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cluster declaring no logging at all",
        stack: "eks-logging-none",
        cluster: "logging-none",
        enabled: &[],
        declares_logging: false,
    },
    Row {
        case: "a cluster enabling one log type",
        stack: "eks-logging-api",
        cluster: "logging-api",
        enabled: &["api"],
        declares_logging: true,
    },
    Row {
        case: "a cluster enabling two log types",
        stack: "eks-logging-pair",
        cluster: "logging-pair",
        enabled: &["api", "audit"],
        declares_logging: true,
    },
    Row {
        case: "a cluster enabling every log type the model declares",
        stack: "eks-logging-all",
        cluster: "logging-all",
        enabled: &LOG_TYPES,
        declares_logging: true,
    },
    Row {
        case: "a cluster declaring the property with no type enabled",
        stack: "eks-logging-empty",
        cluster: "logging-empty",
        enabled: &[],
        declares_logging: true,
    },
];

/// The template one row deploys: the VPC and the two subnets the cluster is placed in,
/// both created by the stack itself, and the cluster with the row's logging.
fn template(row: &Row) -> String {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "Name".to_string(),
        json!(row.cluster),
    );
    properties.insert(
        "RoleArn".to_string(),
        json!(format!(
            "arn:aws:iam::{}:role/eksClusterRole",
            engine::emulator_account_id()
        )),
    );
    properties.insert(
        "ResourcesVpcConfig".to_string(),
        json!({ "SubnetIds": [{ "Ref": "SubnetA" }, { "Ref": "SubnetB" }] }),
    );
    if row.declares_logging {
        properties.insert(
            "Logging".to_string(),
            json!({
                "ClusterLogging": {
                    "EnabledTypes": row
                        .enabled
                        .iter()
                        .map(|log_type| json!({ "Type": log_type }))
                        .collect::<Vec<Value>>(),
                }
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Net": { "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.91.0.0/16" } },
            "SubnetA": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Net" },
                    "CidrBlock": "10.91.1.0/24",
                    "AvailabilityZone": "us-west-2a",
                },
            },
            "SubnetB": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Net" },
                    "CidrBlock": "10.91.2.0/24",
                    "AvailabilityZone": "us-west-2b",
                },
            },
            "Cluster": {
                "Type": "AWS::EKS::Cluster",
                "Properties": Value::Object(properties),
            },
        },
        "Outputs": {
            "ClusterName": { "Value": { "Ref": "Cluster" } },
        },
    })
    .to_string()
}

/// The log types the cluster `name` reports as ENABLED, read off `DescribeCluster`'s
/// own `logging` the way a client does.
fn enabled_types(cluster: &Value) -> Vec<String> {
    cluster["logging"]["clusterLogging"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|setup| setup["enabled"] == json!(true))
        .flat_map(|setup| {
            setup["types"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|log_type| log_type.as_str().map(str::to_string))
        })
        .collect()
}

#[tokio::test]
async fn a_stacks_cluster_exports_the_control_plane_log_types_its_template_enabled() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);

    for row in ROWS {
        let case = row.case;
        let created = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack), ("TemplateBody", &template(row))],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateStack must be accepted, got {created:?}"
        );
        let status = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: a declaration CloudFormation can satisfy completes, got {status:?} \
             (events: {:?})",
            cloudformation(
                &router,
                "DescribeStackEvents",
                &[("StackName", row.stack)]
            )
            .await
            .text
        );

        // THE CLUSTER AMAZON EKS HOLDS: it reports exactly the log types the template
        // enabled, and nothing else.
        let described = eks(&router, "GET", &format!("/clusters/{}", row.cluster)).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the cluster must be readable through Amazon EKS, got {described:?}"
        );
        let reported = enabled_types(&described.body["cluster"]);
        let expected: Vec<String> = row.enabled.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: the cluster must report the enabled types the template declared, \
             got {reported:?} from {}",
            described.text
        );
        // Every type the template did NOT enable is reported as disabled rather than
        // left out, which is how a client reads a type that exports nothing.
        let disabled: Vec<String> = described.body["cluster"]["logging"]["clusterLogging"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|setup| setup["enabled"] == json!(false))
            .flat_map(|setup| {
                setup["types"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|log_type| log_type.as_str().map(str::to_string))
            })
            .collect();
        for log_type in LOG_TYPES {
            let enabled = row.enabled.contains(&log_type);
            assert_eq!(
                !enabled,
                disabled.contains(&log_type.to_string()),
                "case {case:?}: {log_type} must be reported {}, got {}",
                if enabled { "enabled" } else { "disabled" },
                described.text
            );
        }

        // WHAT THE EXPORT CREATED: the cluster's own log group in CloudWatch Logs,
        // with one stream per enabled type — and no group at all for a cluster that
        // enabled none.
        let group = format!("/aws/eks/{}/cluster", row.cluster);
        let groups = logs(
            &router,
            "DescribeLogGroups",
            &json!({ "logGroupNamePrefix": group }),
        )
        .await;
        assert_eq!(
            groups.status, 200,
            "case {case:?}: DescribeLogGroups must answer, got {groups:?}"
        );
        let names: Vec<String> = groups.body["logGroups"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|held| {
                held["logGroupName"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            names.contains(&group),
            !row.enabled.is_empty(),
            "case {case:?}: the cluster's log group must exist exactly when a type is \
             enabled, got {names:?}"
        );
        if row.enabled.is_empty() {
            continue;
        }
        let streams = logs(
            &router,
            "DescribeLogStreams",
            &json!({ "logGroupName": group }),
        )
        .await;
        assert_eq!(
            streams.status, 200,
            "case {case:?}: DescribeLogStreams must answer, got {streams:?}"
        );
        let stream_names: Vec<String> = streams.body["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|held| held["logStreamName"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            stream_names.len(),
            row.enabled.len(),
            "case {case:?}: one stream per enabled type, got {stream_names:?}"
        );
    }
}
