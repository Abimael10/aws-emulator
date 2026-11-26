// SPDX-License-Identifier: CC0-1.0

//! The S3 LOG PERMISSION BOUNDARY of a TRANSIENT Amazon EMR cluster, over the whole assembled
//! emulator: launch a cluster that does not stay up, wait for it to self-terminate with no
//! `TerminateJobFlows` call of any kind, and then decide from Amazon S3 alone whether its log
//! bundle landed.
//!
//! THE RULE under test: **a cluster launched with a `LogUri` ("the location in Amazon S3 to
//! write the log files of the job flow", `RunJobFlow$LogUri` doc) and
//! `Instances.KeepJobFlowAliveWhenNoSteps` false runs its queued steps to a terminal
//! `StepState`, then shuts ITSELF down — "RunJobFlow creates and starts running a new cluster.
//! The cluster runs the steps specified. After the steps complete, the cluster stops and the
//! HDFS partition is lost" (`RunJobFlow` doc) — reaching a terminal `ClusterState`
//! (`TERMINATED`, or `TERMINATED_WITH_ERRORS` for a shutdown whose
//! `ClusterStateChangeReasonCode` is a failure of the cluster itself) carrying the modelled
//! reason code for why it stopped; and its log bundle — one directory per step that ran, beside
//! the file each of its nodes leaves of its own — is written under `<LogUri>/<cluster-id>/`
//! exactly when the permissions in force on the role of the instance profile the launch named
//! ("Also called instance profile and Amazon EC2 role … The Amazon EC2 instances of the cluster
//! assume this role", `RunJobFlow$JobFlowRole` doc) do not deny `s3:PutObject` on the keys
//! those files take. A prefix the push role is denied stays EMPTY however many rounds pass, and
//! the step itself runs and ends either way: what the role governs is the delivery, not the
//! work.**
//!
//! Only a cross-service suite can pin this: IAM holds the service role and the per-row instance
//! profiles whose roles carry the statements, Amazon EC2 owns the VPC and the subnet the launch
//! is placed in AND provisions the nodes whose own files are half the bundle, Amazon EMR runs
//! the cluster and its steps, and every object is read back through Amazon S3's own
//! `ListObjectsV2` — never through anything this suite handed Amazon EMR. It therefore runs
//! against the whole `build_router` service set, as the binary serves it.
//!
//! The owning crate's `tests/all/step_log_delivery_authorization.rs` covers the authorization
//! half with Amazon EMR, Amazon S3 and IAM alone, and `all/emr_keep_alive_cluster_log_post_mortem.rs`
//! covers a KEPT-ALIVE cluster a caller shuts down. This suite's increment is the transient
//! launch nobody terminates — the cluster has to stop itself before anything can be listed —
//! and the WHOLE bundle: on this mount Amazon EC2 really provisions the nodes, so the files a
//! node leaves of its own are delivered beside the step directories, and a denied push role has
//! to suppress both.
//!
//! The rows differ along what the rule generalises over: the statement in force on the push role
//! (none at all, a grant, an explicit deny over the cluster's own log prefix, that deny beside a
//! grant of everything, a deny over ANOTHER cluster's prefix, and a deny of every action of the
//! service over the bucket), where the `LogUri` points (a prefix, a nested prefix, the bucket's
//! own root), how many steps the launch carries, how each step's work ENDED (a clean exit and a
//! non-zero status) and which shutdown the queue brings (the one that follows a run queue, and
//! the one a failed step's `ActionOnFailure` asks for).
//!
//! Nothing here waits a fixed time: every phase of a cluster, every step's span and each push
//! are stored due instants, so each wait LOOKS AGAIN until the condition holds or the generous
//! [`DEADLINE`] passes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The zone the subnet every cluster here is placed in lives in.
const ZONE: &str = "us-east-1a";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The bucket every cluster of this suite writes its logs into.
const BUCKET: &str = "emr-transient-cluster-log-boundary";

/// The service role every launch names — "the IAM role that Amazon EMR assumes in order to
/// access Amazon Web Services resources on your behalf" (`RunJobFlow$ServiceRole` doc). It is no
/// instance profile, so it decides nothing about a node's push.
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The service principals the two kinds of fixture role admit.
const EMR_PRINCIPAL: &str = "elasticmapreduce.amazonaws.com";
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The release every launch names, and the instance type its capacity is stated in.
const RELEASE_LABEL: &str = "emr-6.15.0";
const INSTANCE_TYPE: &str = "m5.xlarge";

/// What a row's completing step prints, which is the whole of its work.
const MARKER: &str = "emr-log-boundary-marker";

/// The status the command of a row's failing step exits with. CHOSEN: a status of the command's
/// OWN, so the step's failure cannot be read as the status a session gives a command it could
/// not run at all.
const GIVING_UP_STATUS: i32 = 4;

/// The files one step's log directory holds, spelled out here rather than imported from the
/// service crate so a wrong constant in the implementation cannot make a case below pass.
const STEP_LOG_FILES: [&str; 3] = ["controller.gz", "stderr.gz", "stdout.gz"];

/// The key segment under a cluster's own log prefix that keys the directories by step, and the
/// one that keys the files a cluster's NODES leave by node.
const STEPS_SEGMENT: &str = "steps";
const NODE_SEGMENT: &str = "node";

/// How many nodes each row's launch asks for: one primary, so each cluster leaves ONE node's own
/// log file beside its steps' directories.
const NODES: usize = 1;

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a cluster never leaves (`com.amazonaws.emr#ClusterState` =
/// `STARTING | BOOTSTRAPPING | RUNNING | WAITING | TERMINATING | TERMINATED |
/// TERMINATED_WITH_ERRORS`).
const TERMINAL_CLUSTER: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// The `ClusterStateChangeReasonCode` values a row expects, spelled out here for the same reason
/// the file names are.
const ALL_STEPS_COMPLETED: &str = "ALL_STEPS_COMPLETED";
const STEP_FAILURE: &str = "STEP_FAILURE";

/// How many further rounds each DENIED prefix is read over once every permitted bundle has
/// landed. CHOSEN: more than one, so "the prefix is empty" is a repeated answer over rounds that
/// each drain whatever delivery was owed, rather than a single look that was simply early.
const EMPTY_REREADS: usize = 3;

/// How long a look for a clock-owed move keeps looking before giving up, and how often it looks.
/// Generous, and only here so a failure is a failure rather than a hang: every wait is on the
/// CONDITION, so a loaded host delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Send one request to the service `signing_name` addresses, and answer `(status, body)` with the
/// answer's BYTES as they came — a log file is a gzip stream, which no text reading of it
/// survives.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    target: Option<&str>,
    content_type: &str,
    body: Bytes,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request.body(body).expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// [`send`] where the answer is text: the same call with its body read as one.
async fn send_text(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    target: Option<&str>,
    content_type: &str,
    body: Bytes,
) -> (u16, String) {
    let (status, answered) =
        send(router, signing_name, method, uri, target, content_type, body).await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One Amazon EC2 action of the setup, which must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send_text(
        router,
        "ec2",
        "POST",
        "/",
        None,
        "application/x-www-form-urlencoded",
        Bytes::from(form(action, EC2_API_VERSION, params)),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One IAM action of the setup, which must succeed.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = send_text(
        router,
        "iam",
        "POST",
        "/",
        None,
        "application/x-www-form-urlencoded",
        Bytes::from(form(action, IAM_API_VERSION, params)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} with {body}"
    );
    body
}

/// One path-style Amazon S3 request: its status and the bytes it answered.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    send(
        router,
        "s3",
        method,
        uri,
        None,
        "application/octet-stream",
        Bytes::copy_from_slice(body),
    )
    .await
}

/// One Amazon EMR call (`awsJson1_1`, `ElasticMapReduce.<Operation>`) that must be served.
async fn emr(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = send_text(
        router,
        "elasticmapreduce",
        "POST",
        "/",
        Some(&format!("ElasticMapReduce.{operation}")),
        "application/x-amz-json-1.1",
        Bytes::from(body.to_string()),
    )
    .await;
    let parsed: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A trust policy admitting `principal` and nothing else.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// One statement, as an inline identity policy document.
fn statement(effect: &str, action: &str, resource: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"{effect}\",\
         \"Action\":\"{action}\",\"Resource\":\"{resource}\"}}]}}"
    )
}

/// A grant of every object write over the bucket, beside a deny of the one over `prefix`: IAM's
/// order, where an explicit deny is final.
fn granted_then_denied(prefix: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":\"s3:PutObject\",\
         \"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}},\
         {{\"Effect\":\"Deny\",\"Action\":\"s3:PutObject\",\
         \"Resource\":\"arn:aws:s3:::{BUCKET}/{prefix}/*\"}}]}}"
    )
}

/// The bucket every cluster logs into, the service role every launch names and the subnet they
/// are placed in — each created through the service that owns it. Answers the subnet id Amazon
/// EC2 minted.
async fn seed(router: &MultiServiceRouter) -> String {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
    iam(
        router,
        "setup",
        "CreateRole",
        &[
            p("RoleName", SERVICE_ROLE),
            p("AssumeRolePolicyDocument", trust_policy(EMR_PRINCIPAL)),
        ],
    )
    .await;
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.43.0.0/16")]).await,
        "vpcId",
    );
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc.as_str()),
                p("CidrBlock", "10.43.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    )
}

/// What one of a row's steps submits as its whole work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// A line of the step's own output, which exits zero.
    Echo,
    /// A status of the command's own, which is not zero.
    GiveUp,
}

/// One row: the log destination its launch names, the statement in force on the role its nodes
/// push with, the queue it carries and what the whole run has to come to.
struct Row {
    case: &'static str,
    /// The name the row's role and instance profile take, which is also what makes each row's
    /// push role its own.
    role: &'static str,
    /// The key prefix the row's `LogUri` names inside the bucket — empty for a URI that names
    /// the bucket alone.
    prefix: &'static str,
    /// The inline identity policy the push role carries, or `None` for a role carrying none.
    policy: Option<String>,
    /// Whether the cluster's log bundle has to appear under its prefix.
    delivered: bool,
    /// The `ActionOnFailure` every step of the row carries.
    on_failure: &'static str,
    /// The work each step submits, and the state its own answer ends that step in.
    steps: Vec<(Work, &'static str)>,
    /// The terminal `ClusterState` the launch's own shutdown brings, and the modelled
    /// `ClusterStateChangeReasonCode` it carries.
    cluster_state: &'static str,
    reason_code: &'static str,
}

/// The table, in the order the clusters are launched.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an unrestricted instance role, one step that exits zero",
            role: "emr-ec2-allowed",
            prefix: "logs-allowed",
            policy: None,
            delivered: true,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
        Row {
            case: "a role denied the object write over its own log prefix",
            role: "emr-ec2-denied",
            prefix: "logs-denied",
            policy: Some(statement(
                "Deny",
                "s3:PutObject",
                &format!("arn:aws:s3:::{BUCKET}/logs-denied/*"),
            )),
            delivered: false,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
        Row {
            case: "a role granted every object write and denied its own nested log prefix",
            role: "emr-ec2-denied-nested",
            prefix: "nested/logs-denied",
            policy: Some(granted_then_denied("nested/logs-denied")),
            delivered: false,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
        Row {
            case: "a role denied the object write over ANOTHER cluster's log prefix, two steps \
                   of each ending",
            role: "emr-ec2-denied-elsewhere",
            prefix: "logs-elsewhere",
            policy: Some(statement(
                "Deny",
                "s3:PutObject",
                &format!("arn:aws:s3:::{BUCKET}/logs-denied/*"),
            )),
            delivered: true,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED"), (Work::GiveUp, "FAILED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
        Row {
            case: "a granted role logging at the bucket's own root",
            role: "emr-ec2-root",
            prefix: "",
            policy: Some(statement(
                "Allow",
                "s3:PutObject",
                &format!("arn:aws:s3:::{BUCKET}/*"),
            )),
            delivered: true,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
        Row {
            case: "a failing step whose ActionOnFailure shuts the cluster down",
            role: "emr-ec2-step-failure",
            prefix: "logs-step-failure",
            policy: None,
            delivered: true,
            on_failure: "TERMINATE_CLUSTER",
            steps: vec![(Work::GiveUp, "FAILED")],
            cluster_state: "TERMINATED_WITH_ERRORS",
            reason_code: STEP_FAILURE,
        },
        Row {
            case: "a role denied every action of the service over the bucket",
            role: "emr-ec2-denied-wholesale",
            prefix: "logs-denied-wholesale",
            policy: Some(statement(
                "Deny",
                "s3:*",
                &format!("arn:aws:s3:::{BUCKET}/*"),
            )),
            delivered: false,
            on_failure: "CONTINUE",
            steps: vec![(Work::Echo, "COMPLETED")],
            cluster_state: "TERMINATED",
            reason_code: ALL_STEPS_COMPLETED,
        },
    ]
}

/// Build the instance profile one row's cluster runs as, the way a client builds one, and answer
/// the name the launch names it by.
async fn instance_profile(router: &MultiServiceRouter, row: &Row) -> String {
    iam(
        router,
        row.case,
        "CreateRole",
        &[
            p("RoleName", row.role),
            p("AssumeRolePolicyDocument", trust_policy(EC2_PRINCIPAL)),
        ],
    )
    .await;
    if let Some(document) = row.policy.as_ref() {
        iam(
            router,
            row.case,
            "PutRolePolicy",
            &[
                p("RoleName", row.role),
                p("PolicyName", "log-delivery"),
                p("PolicyDocument", document.clone()),
            ],
        )
        .await;
    }
    iam(
        router,
        row.case,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", row.role), p("Path", "/")],
    )
    .await;
    iam(
        router,
        row.case,
        "AddRoleToInstanceProfile",
        &[p("InstanceProfileName", row.role), p("RoleName", row.role)],
    )
    .await;
    row.role.to_string()
}

/// The command line one of a row's steps submits, in the `command-runner.jar` form a
/// command-line client's `--steps` writes.
fn command_of(work: Work, ordinal: usize) -> Vec<String> {
    let program = match work {
        Work::Echo => format!("echo {MARKER}-{ordinal}"),
        Work::GiveUp => format!("exit {GIVING_UP_STATUS}"),
    };
    vec!["bash".to_string(), "-c".to_string(), program]
}

/// The `LogUri` one row names.
fn log_uri(row: &Row) -> String {
    if row.prefix.is_empty() {
        format!("s3://{BUCKET}")
    } else {
        format!("s3://{BUCKET}/{}/", row.prefix)
    }
}

/// The key prefix one row's cluster writes its own files under.
fn cluster_prefix(row: &Row, cluster_id: &str) -> String {
    if row.prefix.is_empty() {
        format!("{cluster_id}/")
    } else {
        format!("{}/{cluster_id}/", row.prefix)
    }
}

/// Launch one row's cluster — transient, placed in `subnet`, running as `profile`, logging to its
/// own destination — and answer the cluster id Amazon EMR minted for it.
async fn launch(
    router: &MultiServiceRouter,
    row: &Row,
    ordinal: usize,
    subnet: &str,
    profile: &str,
) -> String {
    let steps: Vec<Value> = row
        .steps
        .iter()
        .enumerate()
        .map(|(at, (work, _))| {
            json!({
                "Name": format!("{} #{at}", row.case),
                "ActionOnFailure": row.on_failure,
                "HadoopJarStep": {
                    "Jar": "command-runner.jar",
                    "Args": command_of(*work, at),
                },
            })
        })
        .collect();
    let answered = emr(
        router,
        row.case,
        "RunJobFlow",
        &json!({
            "Name": format!("log-boundary-{ordinal}"),
            "ReleaseLabel": RELEASE_LABEL,
            "Applications": [{ "Name": "Hadoop" }],
            "JobFlowRole": profile,
            "ServiceRole": SERVICE_ROLE,
            "LogUri": log_uri(row),
            "Steps": steps,
            "Instances": {
                "InstanceCount": NODES,
                "MasterInstanceType": INSTANCE_TYPE,
                // "Specifies whether the cluster should remain available after completing all
                // steps" (`JobFlowInstancesConfig$KeepJobFlowAliveWhenNoSteps` doc): false is
                // the transient launch whose shutdown nobody asks for.
                "KeepJobFlowAliveWhenNoSteps": false,
                "Ec2SubnetId": subnet,
            },
        }),
    )
    .await;
    answered["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: RunJobFlow must answer a JobFlowId, got {answered}",
                row.case
            )
        })
        .to_string()
}

/// Poll `DescribeCluster` until `cluster_id` is terminal, answering the cluster it reports then.
///
/// Describing the cluster is what a client does while it waits, and what a due delivery is
/// drained by, so this is the traffic the real wait makes.
async fn settled_cluster(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emr(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id }),
        )
        .await;
        let cluster = described["Cluster"].clone();
        let state = cluster["Status"]["State"].as_str().unwrap_or_default();
        if TERMINAL_CLUSTER.contains(&state) {
            return cluster;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the transient cluster to terminate itself, got {state:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering them in submission
/// order.
async fn settled_steps(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = emr(router, case, "ListSteps", &json!({ "ClusterId": cluster_id })).await;
        // "The list of steps for the cluster … in reverse order" (`ListSteps` doc): the
        // submission order is that listing read backwards.
        let mut steps: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
        steps.reverse();
        let states: Vec<String> = steps
            .iter()
            .map(|step| {
                step["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if !states.is_empty() && states.iter().all(|state| TERMINAL_STEP.contains(&state.as_str()))
        {
            return steps;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The keys of every object under `prefix`, from `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let (status, listing) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={}", form_encode(prefix)),
        &[],
    )
    .await;
    let listing = String::from_utf8_lossy(&listing).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing s3://{BUCKET}/{prefix} must succeed, got {status} with {listing}"
    );
    elements(&listing, "Key").into_iter().collect()
}

/// Poll until the log destination of a PERMITTED row holds exactly the step files `expected`
/// names beside one file per node, draining whatever push is owed by reading the cluster each
/// round, and answer the keys it holds then.
///
/// A node's own file carries the instant it reports in its NAME, so the node files are counted
/// under their own key segment rather than spelled out; the step directories are exact. The wait
/// is for the WHOLE bundle rather than for the first file: one directory is written one object at
/// a time, so a listing taken mid-delivery holds some of them.
async fn delivered_keys(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    prefix: &str,
    expected: &BTreeSet<String>,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, prefix).await;
        let steps: BTreeSet<String> = keys
            .iter()
            .filter(|key| !key.contains(&format!("/{NODE_SEGMENT}/")))
            .cloned()
            .collect();
        let node_files = keys.len() - steps.len();
        if &steps == expected && node_files == NODES {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected exactly {expected:?} and one file per node for {NODES} \
             node(s) under {prefix}, got {keys:?}"
        );
        // A client learns the files are there by reading the cluster, which is what the delivery
        // is owed by; the listing is then what reports them.
        emr(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id }),
        )
        .await;
        tokio::time::sleep(POLL).await;
    }
}

/// Every row, end to end: a transient cluster runs its queue, stops itself, and its log bundle is
/// under its `LogUri` exactly when the push role is not denied the object write.
#[tokio::test]
async fn a_transient_clusters_logs_land_only_where_its_push_role_may_write_them() {
    let router = router();
    let subnet = seed(&router).await;
    let rows = rows();

    // Every row's cluster is launched FIRST, so the phases the clock owes them all run together
    // rather than one row's waiting standing in front of the next row's launch — which is also
    // what gives each refused delivery at least as many rounds as a permitted one.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let profile = instance_profile(&router, row).await;
        launched.push(launch(&router, row, ordinal, &subnet, &profile).await);
    }

    // (1) and (2): the queue runs and the cluster stops ITSELF — nothing here calls
    // `TerminateJobFlows` — reaching the terminal state and the modelled reason its own shutdown
    // brings. The step's own answer decides its state whatever the push role says.
    let mut settled: Vec<Vec<Value>> = Vec::new();
    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let steps = settled_steps(&router, case, cluster_id).await;
        assert_eq!(
            steps.len(),
            row.steps.len(),
            "case {case:?}: expected one step back per submitted step, got {steps:?}"
        );
        for ((work, expected), step) in row.steps.iter().zip(&steps) {
            assert_eq!(
                step["Status"]["State"].as_str(),
                Some(*expected),
                "case {case:?}: expected the {work:?} step to end {expected}, got {step}"
            );
        }
        let cluster = settled_cluster(&router, case, cluster_id).await;
        assert_eq!(
            cluster["Status"]["State"].as_str(),
            Some(row.cluster_state),
            "case {case:?}: expected the cluster to end {}, got {}",
            row.cluster_state,
            cluster["Status"]
        );
        assert_eq!(
            cluster["Status"]["StateChangeReason"]["Code"].as_str(),
            Some(row.reason_code),
            "case {case:?}: expected the shutdown reason {}, got {}",
            row.reason_code,
            cluster["Status"]
        );
        assert_eq!(
            cluster["AutoTerminate"].as_bool(),
            Some(true),
            "case {case:?}: a transient launch reports AutoTerminate true, got {cluster}"
        );
        settled.push(steps);
    }

    // (3) The permitted rows are waited for FIRST: by the time the last of them holds its whole
    // bundle, every refused delivery has had at least as many rounds.
    let mut bundles: Vec<BTreeSet<String>> = vec![BTreeSet::new(); rows.len()];
    for (at, (row, cluster_id)) in rows.iter().zip(&launched).enumerate() {
        if !row.delivered {
            continue;
        }
        let prefix = cluster_prefix(row, cluster_id);
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for step in &settled[at] {
            let step_id = step["Id"].as_str().unwrap_or_default();
            for name in STEP_LOG_FILES {
                expected.insert(format!("{prefix}{STEPS_SEGMENT}/{step_id}/{name}"));
            }
        }
        let keys = delivered_keys(&router, row.case, cluster_id, &prefix, &expected).await;
        for key in &keys {
            let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
            assert!(
                (200..300).contains(&status),
                "case {:?}: {key} must be served by Amazon S3, got {status}",
                row.case
            );
            assert_eq!(
                body.get(..2),
                Some([0x1f, 0x8b].as_slice()),
                "case {:?}: {key} must be a gzip stream, got {:?}",
                row.case,
                body.get(..2)
            );
        }
        bundles[at] = keys;
    }

    // (4) A denied push role leaves its prefix EMPTY — both the step directories and the node's
    // own file — over further rounds that each drain whatever delivery was owed.
    for (row, cluster_id) in rows.iter().zip(&launched) {
        if row.delivered {
            continue;
        }
        let prefix = cluster_prefix(row, cluster_id);
        for round in 0..EMPTY_REREADS {
            emr(
                &router,
                row.case,
                "DescribeCluster",
                &json!({ "ClusterId": cluster_id }),
            )
            .await;
            let keys = keys_under(&router, row.case, &prefix).await;
            assert!(
                keys.is_empty(),
                "case {:?}: round {round} expected nothing under {prefix}, got {keys:?}",
                row.case
            );
        }
    }

    // (5) Nor does a refused delivery leak anywhere else: the bucket holds exactly the bundles of
    // the rows whose push was not denied.
    let whole: BTreeSet<String> = keys_under(&router, "every row", "").await;
    let permitted: BTreeSet<String> = bundles.into_iter().flatten().collect();
    assert_eq!(
        whole, permitted,
        "expected the bucket to hold exactly the permitted rows' bundles, got {whole:?}"
    );
}
