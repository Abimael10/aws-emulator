// SPDX-License-Identifier: CC0-1.0

//! A PROBE of the whole headless-notebook workflow a client drives within its own polling
//! budget, across the services that own each resource it names.
//!
//! It is named for what it diagnoses: every wait of this suite is paced like a client's own
//! ([`CLIENT_CADENCE`]) rather than read as fast as the process can, so a workflow leg that
//! only settles for a caller reading in a tight loop fails here.
//!
//! THE RULE this probes is the one [`crate::notebook_execution_client_cadence`] states for the
//! execution alone, driven here over the whole workflow a client really performs: the IAM
//! instance profile the launch names, the Amazon EC2 subnet it is placed in, the Amazon S3
//! object the notebook is, the cluster that runs it, the execution started on that cluster and
//! the `StopNotebookExecution` the client sends after it has READ the execution at its own
//! polling cadence. Every status on the way is one a client reaches: the cluster settles where
//! a launch that stays alive settles (`WAITING`), the execution is read being processed by that
//! cluster rather than already ended, and the stop takes it to `STOPPED` ("`STOPPED` indicates
//! that the execution stopped because of a `StopNotebookExecution` request",
//! `NotebookExecution$Status` doc). A sibling execution nobody stops ends in what its own
//! notebook answered.
//!
//! Each resource is created through the service that OWNS it — IAM's `CreateRole` /
//! `CreateInstanceProfile` / `AddRoleToInstanceProfile`, Amazon EC2's `CreateVpc` /
//! `CreateSubnet`, Amazon S3's `CreateBucket` / `PutObject` — all mounted on ONE engine, so
//! nothing here hands Amazon EMR a resource it has not resolved where it lives.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every call below is addressed in.
const REGION: &str = "us-east-1";

/// The zone the subnet the cluster is placed in lives in.
const ZONE: &str = "us-east-1a";

/// The bucket the notebooks of this suite live in.
const BUCKET: &str = "emr-workflow-notebooks";

/// The roles a launch and an execution name.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";
const NOTEBOOK_ROLE: &str = "EMR_Notebooks_DefaultRole";

/// The release the cluster runs, and the applications a headless notebook execution needs.
const RELEASE_LABEL: &str = "emr-7.9.0";

/// How long any wait of this suite looks for the state it waits on before giving up.
/// Deliberately far longer than the walks take: every wait is on a STATE, so a loaded host
/// delays this suite rather than failing it.
const DEADLINE: Duration = Duration::from_secs(180);

/// The cadence this suite's client reads a status at — a client waiting on an AWS status
/// reads seconds apart rather than as fast as it can.
const CLIENT_CADENCE: Duration = Duration::from_secs(5);

/// The statuses a notebook execution never leaves (`NotebookExecutionStatus`).
const TERMINAL: [&str; 3] = ["FINISHED", "FAILED", "STOPPED"];

/// Amazon EMR beside the services that own the resources a launch and an execution name,
/// all on one engine, plus the clients this suite creates those resources through.
struct Harness {
    router: MultiServiceRouter,
    ec2: ::aws_sdk_ec2::Client,
    iam: ::aws_sdk_iam::Client,
}

fn harness() -> Harness {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    let callback = EmbeddedEmulator::from_router(router.clone());
    let ec2 = ::aws_sdk_ec2::Client::from_conf(
        ::aws_sdk_ec2::Config::builder()
            .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build(),
    );
    let iam = ::aws_sdk_iam::Client::from_conf(
        ::aws_sdk_iam::Config::builder()
            .behavior_version(::aws_sdk_iam::config::BehaviorVersion::latest())
            .region(::aws_sdk_iam::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://iam.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_iam::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_iam::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_iam::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_iam::config::StalledStreamProtectionConfig::disabled(),
            )
            .build(),
    );
    Harness { router, ec2, iam }
}

/// One `awsJson1_1` Amazon EMR call, answering its status and payload whatever they are.
async fn emr_call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("elasticmapreduce.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261109/{REGION}/\
                 elasticmapreduce/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// One Amazon EMR call that must be served.
async fn emr(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, payload) = emr_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One path-style Amazon S3 `PUT` that must be served — the bucket, then each notebook.
async fn s3_put(router: &MultiServiceRouter, uri: &str, body: &[u8]) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("PUT")
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261109/{REGION}/\
                 s3/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    assert!(
        (200..300).contains(&parts.status.as_u16()),
        "S3 PUT {uri} must succeed, got {} {}",
        parts.status.as_u16(),
        String::from_utf8_lossy(&answered)
    );
}

/// A notebook document in the Jupyter format whose one code cell is `source`.
fn notebook(source: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "cells": [{
            "cell_type": "code",
            "execution_count": Value::Null,
            "metadata": {},
            "outputs": [],
            "source": source,
        }],
        "metadata": { "kernelspec": { "language": "python", "name": "python3" } },
        "nbformat": 4,
        "nbformat_minor": 5,
    }))
    .expect("the notebook serializes")
}

/// The roles and the instance profile the launch and the execution name, built the way a
/// client builds them, in the service that holds them.
async fn roles(harness: &Harness) {
    for role in [INSTANCE_PROFILE, SERVICE_ROLE, NOTEBOOK_ROLE] {
        harness
            .iam
            .create_role()
            .role_name(role)
            .assume_role_policy_document(
                "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}",
            )
            .send()
            .await
            .unwrap_or_else(|error| panic!("IAM must create {role}: {error:?}"));
    }
    harness
        .iam
        .create_instance_profile()
        .instance_profile_name(INSTANCE_PROFILE)
        .send()
        .await
        .expect("IAM must create the instance profile");
    harness
        .iam
        .add_role_to_instance_profile()
        .instance_profile_name(INSTANCE_PROFILE)
        .role_name(INSTANCE_PROFILE)
        .send()
        .await
        .expect("IAM must attach the role to the instance profile");
}

/// The subnet Amazon EC2 itself minted, which is where the cluster is placed.
async fn subnet(harness: &Harness) -> String {
    let vpc = harness
        .ec2
        .create_vpc()
        .cidr_block("10.0.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC");
    let vpc_id = vpc
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc reports the VPC it minted")
        .to_string();
    let created = harness
        .ec2
        .create_subnet()
        .vpc_id(&vpc_id)
        .cidr_block("10.0.1.0/24")
        .availability_zone(ZONE)
        .send()
        .await
        .expect("EC2 must create the subnet");
    created
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet reports the subnet it minted")
        .to_string()
}

/// The keep-alive cluster a headless notebook execution runs on, read at the client's own
/// cadence until it reports the state a launch that stays up settles in.
async fn notebook_cluster(harness: &Harness, subnet_id: &str) -> String {
    let launched = emr(
        &harness.router,
        "RunJobFlow",
        json!({
            "Name": "headless-notebooks",
            "ReleaseLabel": RELEASE_LABEL,
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [
                { "Name": "Hadoop" },
                { "Name": "Spark" },
                { "Name": "Livy" },
                { "Name": "JupyterEnterpriseGateway" },
            ],
            "Instances": {
                "Ec2SubnetId": subnet_id,
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    {
                        "Name": "primary",
                        "InstanceRole": "MASTER",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                    },
                    {
                        "Name": "core",
                        "InstanceRole": "CORE",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                    },
                ],
            },
        }),
    )
    .await;
    let cluster_id = launched["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("RunJobFlow answers a JobFlowId, got {launched}"))
        .to_string();

    let began = Instant::now();
    loop {
        let described = emr(
            &harness.router,
            "DescribeCluster",
            json!({ "ClusterId": cluster_id.clone() }),
        )
        .await;
        let state = described["Cluster"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "WAITING" {
            return cluster_id;
        }
        assert!(
            began.elapsed() < DEADLINE,
            "a cluster kept alive with no steps must reach WAITING, last state {state:?}"
        );
        ::tokio::time::sleep(CLIENT_CADENCE).await;
    }
}

/// `StartNotebookExecution` of the notebook at `key`, as the headless workflow sends it.
async fn start(harness: &Harness, case: &str, cluster_id: &str, key: &str) -> String {
    let answer = emr(
        &harness.router,
        "StartNotebookExecution",
        json!({
            "ExecutionEngine": { "Id": cluster_id, "Type": "EMR" },
            "ServiceRole": NOTEBOOK_ROLE,
            "NotebookExecutionName": case,
            "NotebookS3Location": { "Bucket": BUCKET, "Key": key },
            "OutputNotebookS3Location": { "Bucket": BUCKET, "Key": "notebooks/output/" },
            "OutputNotebookFormat": "HTML",
            "NotebookParams": "{}",
        }),
    )
    .await;
    answer["NotebookExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartNotebookExecution answers an id, got {answer}"))
        .to_string()
}

/// `DescribeNotebookExecution` for `id`.
async fn described(harness: &Harness, id: &str) -> Value {
    emr(
        &harness.router,
        "DescribeNotebookExecution",
        json!({ "NotebookExecutionId": id }),
    )
    .await["NotebookExecution"]
        .clone()
}

/// The status `id` reports once it has left `START_PENDING`, read at the client's cadence.
async fn past_start_pending(harness: &Harness, case: &str, id: &str) -> String {
    let began = Instant::now();
    loop {
        let execution = described(harness, id).await;
        let status = execution["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: an execution reports a Status, got {execution}"))
            .to_string();
        if status != "START_PENDING" {
            return status;
        }
        assert!(
            began.elapsed() < DEADLINE,
            "case {case:?}: expected the execution to leave START_PENDING, got {execution}"
        );
        ::tokio::time::sleep(CLIENT_CADENCE).await;
    }
}

/// `id` once it has reached a status it never leaves.
async fn settled(harness: &Harness, case: &str, id: &str) -> Value {
    let began = Instant::now();
    loop {
        let execution = described(harness, id).await;
        let status = execution["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&status.as_str()) {
            return execution;
        }
        assert!(
            began.elapsed() < DEADLINE,
            "case {case:?}: expected the execution to settle, last status {status:?}"
        );
        ::tokio::time::sleep(CLIENT_CADENCE).await;
    }
}

#[tokio::test]
async fn the_headless_notebook_workflow_a_client_drives_settles() {
    let harness = harness();
    roles(&harness).await;
    let subnet_id = subnet(&harness).await;
    s3_put(&harness.router, &format!("/{BUCKET}"), &[]).await;
    s3_put(
        &harness.router,
        &format!("/{BUCKET}/notebooks/stopped.ipynb"),
        &notebook("total = sum(range(10))"),
    )
    .await;
    s3_put(
        &harness.router,
        &format!("/{BUCKET}/notebooks/finished.ipynb"),
        &notebook("print(1 + 1)"),
    )
    .await;

    let cluster_id = notebook_cluster(&harness, &subnet_id).await;

    // The execution this client STOPS: it reads the execution at its own cadence first, so
    // the stop is the one a client that polls before deciding really sends.
    let stopped_case = "an execution the client stops after reading it";
    let stopped_id = start(&harness, stopped_case, &cluster_id, "notebooks/stopped.ipynb").await;
    let reading = past_start_pending(&harness, stopped_case, &stopped_id).await;
    assert!(
        !TERMINAL.contains(&reading.as_str()),
        "case {stopped_case:?}: an execution this client has only just started is still being \
         processed, read {reading:?}"
    );
    emr(
        &harness.router,
        "StopNotebookExecution",
        json!({ "NotebookExecutionId": stopped_id.clone() }),
    )
    .await;
    let stopped = settled(&harness, stopped_case, &stopped_id).await;
    assert_eq!(
        stopped["Status"].as_str(),
        Some("STOPPED"),
        "case {stopped_case:?}: an execution stopped from {reading:?} reaches STOPPED, got \
         {stopped}"
    );
    assert_eq!(
        stopped["NotebookExecutionId"].as_str(),
        Some(stopped_id.as_str()),
        "case {stopped_case:?}: the stopped execution is the one that was started, got {stopped}"
    );

    // And the sibling nobody stops ends in what its own notebook answered, on the same
    // cluster, with its output notebook where the request asked for it.
    let finished_case = "an execution nobody stops";
    let finished_id = start(
        &harness,
        finished_case,
        &cluster_id,
        "notebooks/finished.ipynb",
    )
    .await;
    let finished = settled(&harness, finished_case, &finished_id).await;
    assert_eq!(
        finished["Status"].as_str(),
        Some("FINISHED"),
        "case {finished_case:?}: a notebook whose cells all ran finishes, got {finished}"
    );
    assert!(
        finished["OutputNotebookURI"]
            .as_str()
            .is_some_and(|uri| uri.starts_with(&format!("s3://{BUCKET}/notebooks/output/"))),
        "case {finished_case:?}: a finished execution reports its output file under the \
         location the request named, got {finished}"
    );

    // The two executions of this cluster are partitioned by the status each reached.
    for (status, expected) in [("STOPPED", &stopped_id), ("FINISHED", &finished_id)] {
        let listed = emr(
            &harness.router,
            "ListNotebookExecutions",
            json!({ "ExecutionEngineId": cluster_id.clone(), "Status": status }),
        )
        .await;
        let ids: Vec<String> = listed["NotebookExecutions"]
            .as_array()
            .map(|executions| {
                executions
                    .iter()
                    .map(|execution| {
                        execution["NotebookExecutionId"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            ids,
            vec![expected.clone()],
            "case {status:?}: the status filter selects the execution that reached it, got \
             {listed}"
        );
    }
}
