// SPDX-License-Identifier: CC0-1.0

//! A headless Amazon EMR notebook execution really RUNS the notebook Amazon S3 holds, and
//! `ListNotebookExecutions` then partitions the executions by the status each of them ended
//! in — over the whole assembled router, as the binary serves it.
//!
//! The RULE under test: **an execution's end is what running its notebook answered. A
//! notebook whose code cells all succeed reaches `FINISHED` ("`FINISHED` indicates that the
//! execution has completed without error", `NotebookExecution$Status`) with its executed
//! notebook stored at the `OutputNotebookS3Location` the request named and
//! `OutputNotebookURI` naming that object, while only a notebook that RAISES reaches
//! `FAILED` ("`FAILED` indicates that the execution failed") with
//! `LastStateChangeReason` carrying the engine's own exception.
//! `ListNotebookExecutions$Status`, "the status filter for listing notebook executions",
//! then returns exactly the executions in that status, and two consecutive unfiltered
//! listings report the same order.**
//!
//! Only a cross-service suite can pin this: IAM holds the instance profile the launch names,
//! Amazon EMR runs the execution, and both the notebook that runs and the output notebook it
//! leaves are real objects of Amazon S3's own store — put there with `PutObject` and read
//! back with `GetObject`, never through anything this suite handed Amazon EMR. It therefore
//! runs against the WHOLE `build_router` service set, with every call made the way a client
//! makes it.
//!
//! The rows differ along what the rule generalises over: what the notebook's cells come to
//! (every cell runs, a cell raises), how many cells there are, whether the cells write
//! anything, and which status the listing is then filtered on.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
///
/// `pub(crate)` here and below: the sibling suite
/// [`crate::emr_notebook_execution_on_an_authenticating_router`] drives the same flow on a
/// router that VERIFIES signatures, so the two share the notebook, the trust policy and the
/// addressing rather than carrying copies that could drift apart.
pub(crate) const REGION: &str = "us-east-1";

/// The bucket every notebook of this suite is read from and every output notebook is stored
/// in.
pub(crate) const BUCKET: &str = "emr-headless-notebooks";

/// The prefix every row's output notebook is stored under — a key naming a location many
/// executions store under, which is how a client names one.
pub(crate) const OUTPUT_PREFIX: &str = "executed/";

/// The roles a launch and an execution name.
pub(crate) const SERVICE_ROLE: &str = "EMR_DefaultRole";
pub(crate) const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
pub(crate) const NOTEBOOK_ROLE: &str = "EMR_Notebooks_DefaultRole";

/// The service principal an Amazon EC2 instance assumes a role as.
pub(crate) const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The states a cluster reports once it is up.
pub(crate) const READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];

/// The statuses a notebook execution never leaves (`com.amazonaws.emr#NotebookExecutionStatus`),
/// spelled out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
pub(crate) const TERMINAL_STATUSES: [&str; 3] = ["FINISHED", "FAILED", "STOPPED"];

/// How long a poll waits for a cluster to come up and for an execution to settle: every
/// phase of both walks and the longest a notebook's own run may take, with room to spare.
/// The wait is on the CONDITION — it looks again until the state has settled — so a loaded
/// host delays a row instead of breaking it.
pub(crate) const DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll sleeps between reads.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon EMR call (`awsJson1_1`, `ElasticMapReduce.<Operation>`) that must be served.
async fn emr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("elasticmapreduce.{REGION}.amazonaws.com"))
        .header("authorization", authorization("elasticmapreduce"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One IAM `awsQuery` call, which must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, body: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body.to_owned()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM, got {status} with {text}"
    );
    text
}

/// One path-style Amazon S3 request: its status and body.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// Put `body` at `key`, which must be served.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &[u8]) {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: PutObject {key} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// A trust policy admitting `principal` and nothing else, in the form an `awsQuery` form body
/// carries it: its quotes and spaces percent-encoded.
pub(crate) fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
}

/// A notebook document in the Jupyter format, holding one code cell per entry of `sources`.
pub(crate) fn notebook(sources: &[&str]) -> Vec<u8> {
    let cells: Vec<Value> = sources
        .iter()
        .map(|source| json!({ "cell_type": "code", "source": source, "metadata": {}, "outputs": [] }))
        .collect();
    serde_json::to_vec(&json!({
        "cells": cells,
        "metadata": { "kernelspec": { "language": "python", "name": "python3" } },
        "nbformat": 4,
        "nbformat_minor": 5,
    }))
    .expect("the notebook serializes")
}

/// The bucket, the service role and the instance profile every row shares, each created
/// through the service that owns it.
async fn seed(router: &MultiServiceRouter) {
    let case = "setup";
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={SERVICE_ROLE}\
             &AssumeRolePolicyDocument={}",
            trust_policy("elasticmapreduce.amazonaws.com")
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={INSTANCE_PROFILE}\
             &AssumeRolePolicyDocument={}",
            trust_policy(EC2_PRINCIPAL)
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=CreateInstanceProfile&Version=2010-05-08\
             &InstanceProfileName={INSTANCE_PROFILE}&Path=/"
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=AddRoleToInstanceProfile&Version=2010-05-08\
             &InstanceProfileName={INSTANCE_PROFILE}&RoleName={INSTANCE_PROFILE}"
        ),
    )
    .await;
}

/// The keep-alive cluster a headless notebook execution runs on: the applications an Amazon
/// EMR notebook needs, on the release a client pins.
async fn notebook_cluster(router: &MultiServiceRouter) -> String {
    let case = "setup";
    let launched = emr_ok(
        router,
        case,
        "RunJobFlow",
        &json!({
            "Name": "headless-notebooks",
            "ReleaseLabel": "emr-7.9.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [
                { "Name": "Hadoop" },
                { "Name": "Spark" },
                { "Name": "Livy" },
                { "Name": "JupyterEnterpriseGateway" },
            ],
            "Instances": {
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
        .unwrap_or_else(|| panic!("case {case:?}: RunJobFlow answers a JobFlowId, got {launched}"))
        .to_string();

    // A client starts an execution on a cluster that is up, so this waits for the state the
    // cluster reports when it is.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let cluster = emr_ok(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id.clone() }),
        )
        .await["Cluster"]
            .clone();
        let state = cluster["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if READY_STATES.contains(&state.as_str()) {
            return cluster_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never came up, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeNotebookExecution` until the execution reaches a status it never leaves, and
/// answer it as last read.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let execution = emr_ok(
            router,
            case,
            "DescribeNotebookExecution",
            &json!({ "NotebookExecutionId": id }),
        )
        .await["NotebookExecution"]
            .clone();
        let status = execution["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return execution;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never settled, last status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a row's notebook comes to.
enum Ends {
    /// Every cell ran: the execution finishes, and its executed notebook is stored.
    Finished,
    /// A cell raised: the execution fails, and its reason carries each of these.
    Failed(&'static [&'static str]),
}

/// One row: the cells of the notebook this execution runs, and what it must come to.
struct Row {
    case: &'static str,
    cells: &'static [&'static str],
    ends: Ends,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one cell that computes and writes",
            cells: &["print(1 + 1)"],
            ends: Ends::Finished,
        },
        Row {
            case: "cells sharing one namespace, writing nothing",
            cells: &["answer = 6 * 7", "assert answer == 42, answer"],
            ends: Ends::Finished,
        },
        Row {
            case: "a cell that raises",
            cells: &["raise ValueError('the notebook failed')"],
            ends: Ends::Failed(&["ValueError", "the notebook failed"]),
        },
    ]
}

/// The key one row's notebook is put under.
pub(crate) fn notebook_key(ordinal: usize) -> String {
    format!("in/notebook-{ordinal}.ipynb")
}

/// The executions of one cluster the listing reports for `status`, newest first.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    status: Option<&str>,
) -> Vec<String> {
    let mut request = json!({ "ExecutionEngineId": cluster_id });
    if let Some(status) = status {
        request["Status"] = json!(status);
    }
    let answered = emr_ok(router, case, "ListNotebookExecutions", &request).await;
    answered["NotebookExecutions"]
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
        .unwrap_or_default()
}

#[tokio::test]
async fn a_notebook_execution_runs_its_s3_notebook_and_the_listing_partitions_by_status() {
    let router = router();
    let rows = rows();

    seed(&router).await;
    let cluster_id = notebook_cluster(&router).await;

    // Every row's notebook is put first and every execution started before any is polled, so
    // the clock walks them together rather than one after another.
    for (ordinal, row) in rows.iter().enumerate() {
        put_object(
            &router,
            row.case,
            &notebook_key(ordinal),
            &notebook(row.cells),
        )
        .await;
    }
    let mut started: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let answered = emr_ok(
            &router,
            row.case,
            "StartNotebookExecution",
            &json!({
                "ExecutionEngine": { "Id": cluster_id.clone(), "Type": "EMR" },
                "ServiceRole": NOTEBOOK_ROLE,
                "NotebookExecutionName": row.case,
                "NotebookS3Location": { "Bucket": BUCKET, "Key": notebook_key(ordinal) },
                "OutputNotebookS3Location": { "Bucket": BUCKET, "Key": OUTPUT_PREFIX },
            }),
        )
        .await;
        started.push(
            answered["NotebookExecutionId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: StartNotebookExecution answers an id, got {answered}",
                        row.case
                    )
                })
                .to_string(),
        );
    }

    let mut finished: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for (row, id) in rows.iter().zip(&started) {
        let case = row.case;
        let execution = settled(&router, case, id).await;
        let status = execution["Status"].as_str().unwrap_or_default().to_string();
        let reason = execution["LastStateChangeReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match row.ends {
            Ends::Finished => {
                assert_eq!(
                    status, "FINISHED",
                    "case {case:?}: a notebook whose cells all ran finishes, got {execution}"
                );
                finished.push(id.clone());
                // The output notebook is a real object of the store, at the location the
                // execution reports it at.
                let uri = execution["OutputNotebookURI"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a finished execution reports its output file, \
                                got {execution}")
                    })
                    .to_string();
                let key = uri.strip_prefix(&format!("s3://{BUCKET}/")).unwrap_or_else(|| {
                    panic!("case {case:?}: the output file is an object of {BUCKET}, got {uri:?}")
                });
                let stored = object(&router, key).await;
                let stored = stored.unwrap_or_else(|| {
                    panic!("case {case:?}: the store holds the output notebook at {key:?}")
                });
                let document: Value = serde_json::from_str(&stored).unwrap_or_else(|error| {
                    panic!("case {case:?}: the output notebook is a notebook document: {error}")
                });
                let cells = document["cells"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    cells.len(),
                    row.cells.len(),
                    "case {case:?}: the executed notebook holds the notebook's own cells, got \
                     {document}"
                );
                for (position, cell) in cells.iter().enumerate() {
                    assert_eq!(
                        cell["execution_count"].as_u64(),
                        Some(position as u64 + 1),
                        "case {case:?}: cell {position} records which execution ran it, got \
                         {cell}"
                    );
                }
            }
            Ends::Failed(expected) => {
                assert_eq!(
                    status, "FAILED",
                    "case {case:?}: only a notebook that raised fails, got {execution}"
                );
                failed.push(id.clone());
                for part in expected {
                    assert!(
                        reason.contains(part),
                        "case {case:?}: expected the engine's own reason to carry {part:?}, \
                         got {reason:?}"
                    );
                }
                assert!(
                    execution["OutputNotebookURI"].is_null(),
                    "case {case:?}: a notebook that did not run through leaves no output \
                     notebook, got {execution}"
                );
            }
        }
    }

    // The Status filter partitions the executions of this cluster: each one is reported for
    // the status it ended in and for no other.
    for (status, expected) in [("FINISHED", &finished), ("FAILED", &failed)] {
        let mut reported = listed(&router, status, &cluster_id, Some(status)).await;
        reported.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {status:?}: expected the executions in {status} back, got {reported:?}"
        );
    }

    // And an unfiltered listing reports them in one order, whichever call reads it.
    let first = listed(&router, "unfiltered", &cluster_id, None).await;
    let again = listed(&router, "unfiltered", &cluster_id, None).await;
    assert_eq!(
        first, again,
        "case \"unfiltered\": two consecutive listings report one order, got {first:?} then \
         {again:?}"
    );
    let mut every = started.clone();
    every.sort();
    let mut reported = first.clone();
    reported.sort();
    assert_eq!(
        reported, every,
        "case \"unfiltered\": expected every execution of the cluster back, got {first:?}"
    );
}
