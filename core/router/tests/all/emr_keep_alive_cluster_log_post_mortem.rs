// SPDX-License-Identifier: CC0-1.0

//! The post-mortem sequence a client runs against a keep-alive Amazon EMR cluster launched
//! with a `LogUri`, over the WHOLE assembled emulator: submit the cluster with its step, wait
//! for the step to end, wait for the cluster to settle, shut it down, and then read the
//! cluster's logs out of Amazon S3 with no further Amazon EMR call of any kind.
//!
//! THE RULE under test: **a cluster launched with a `LogUri` ("the location in Amazon S3 to
//! write the log files of the job flow", `RunJobFlow$LogUri` doc) converges on the clock —
//! its submitted step reaches a terminal `StepState` ("for a step to be considered complete,
//! the main function must exit with a zero exit code", `AddJobFlowSteps` doc), a cluster asked
//! to stay up rests in `WAITING` ("if the `JobFlowInstancesConfig`
//! `KeepJobFlowAliveWhenNoSteps` parameter is set to TRUE, the cluster transitions to the
//! `WAITING` state rather than shutting down after the steps have completed", `RunJobFlow`
//! doc) and `TerminateJobFlows` drives it to `TERMINATED` — and the log files written under
//! `<LogUri>/<cluster-id>/` are objects of the OBJECT STORE: they belong to the bucket rather
//! than to the cluster, so they are there, and keep answering the same listing, for a client
//! that only reads Amazon S3 from the moment the cluster is gone.**
//!
//! Only a cross-service suite can pin this: IAM holds the service role and the instance
//! profile the launch names, Amazon EC2 owns the VPC and the subnet the launch is placed in,
//! Amazon EMR runs the cluster and its steps, and every log file is read back through Amazon
//! S3's own `ListObjectsV2` and `GetObject` — never through anything this suite handed Amazon
//! EMR. It therefore runs against the whole `build_router` service set, as the binary serves
//! it. The owning crate's `tests/all/cluster_log_persistence_after_termination.rs` covers the
//! same rule with Amazon EMR beside Amazon S3 alone, and DRAINS the delivery with an Amazon
//! EMR call inside its own wait; this suite's increment is that the client makes no such call:
//! from the cluster's shutdown on, the only requests are Amazon S3 reads, and the launch is the
//! one a command-line client makes — its capacity, its instance profile and its subnet all
//! resolved in the services that own them.
//!
//! The rows differ along what the rule generalises over: how the launch states its capacity
//! (the uniform instance-type form and an explicit instance group), where the `LogUri` points
//! (a prefix, a nested prefix, the bucket's own root), how the step's work ENDED (a clean exit
//! and a non-zero status) and how many steps the launch carried. The last two axes are the
//! sibling suite's as well, and they are re-driven here rather than left to it because the
//! delivery path is not the same one: on this mount the push is authorized as the role of the
//! instance profile the launch named, resolved in the IAM this suite created it in, which a
//! mount without IAM cannot exercise at all.
//!
//! Nothing here waits a fixed time: every phase of a cluster, every step's span and the log
//! push are stored due instants, so each wait LOOKS AGAIN until the condition holds or the
//! generous [`DEADLINE`] passes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The zone the subnet every cluster here is placed in lives in.
const ZONE: &str = "us-east-1a";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The bucket every cluster of this suite writes its logs into.
const BUCKET: &str = "emr-log-post-mortem";

/// The roles every launch here names: the service role Amazon EMR assumes, and the instance
/// profile whose role the cluster's nodes — the ones that push the log files — assume.
const SERVICE_ROLE: &str = "EMR_DefaultRole";
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";

/// The release every launch here names, and the instance type its capacity is stated in.
const RELEASE_LABEL: &str = "emr-6.15.0";
const INSTANCE_TYPE: &str = "m5.xlarge";

/// What a row's completing step prints, which is the whole of its work.
const MARKER: &str = "emr-log-post-mortem-marker";

/// The status the command of a row that gives up exits with. CHOSEN: a status of the
/// command's OWN, so the step's failure cannot be read as the status a session gives a
/// command it could not run at all.
const GIVING_UP_STATUS: i32 = 4;

/// The files one step's log directory holds, spelled out here rather than imported from the
/// service crate so a wrong constant in the implementation cannot make a case below pass.
const STEP_LOG_FILES: [&str; 3] = ["controller.gz", "stderr.gz", "stdout.gz"];

/// The key segment under a cluster's own log prefix that keys the directories by step.
const STEPS_SEGMENT: &str = "steps";

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a cluster that is still coming up reports (`com.amazonaws.emr#ClusterState`): a
/// client waiting for a cluster to settle waits for the state to LEAVE these.
const COMING_UP: [&str; 3] = ["STARTING", "BOOTSTRAPPING", "RUNNING"];

/// How long a look for a clock-owed move keeps looking before giving up, and how often it
/// looks. Generous, and only here so a failure is a failure rather than a hang: every wait is
/// on the CONDITION, so a loaded host delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(50);

/// How many times a client re-reads the destination once the cluster is gone, each round
/// fetching every file it listed before looking again. CHOSEN: more than one, so "the files are
/// still there" is a repeated answer over several rounds of real reads rather than a single
/// look.
const PERSISTENCE_REREADS: usize = 4;

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

/// Send one request to the service `signing_name` addresses, and answer `(status, body)` with
/// the answer's BYTES as they came — a log file is a gzip stream, which no text reading of it
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

/// One Amazon EC2 action that must succeed.
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

/// One IAM action that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
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
        "setup: IAM {action} must succeed, got {status} with {body}"
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

/// The bucket the logs land in, the roles the launch names and the subnet it is placed in —
/// each created through the service that owns it. Answers the subnet id Amazon EC2 minted.
async fn seed(router: &MultiServiceRouter) -> String {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
    for (role, principal) in [
        (SERVICE_ROLE, "elasticmapreduce.amazonaws.com"),
        (INSTANCE_PROFILE, "ec2.amazonaws.com"),
    ] {
        iam(
            router,
            "CreateRole",
            &[
                p("RoleName", role),
                p("AssumeRolePolicyDocument", trust_policy(principal)),
            ],
        )
        .await;
    }
    iam(
        router,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", INSTANCE_PROFILE), p("Path", "/")],
    )
    .await;
    iam(
        router,
        "AddRoleToInstanceProfile",
        &[
            p("InstanceProfileName", INSTANCE_PROFILE),
            p("RoleName", INSTANCE_PROFILE),
        ],
    )
    .await;
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.42.0.0/16")]).await,
        "vpcId",
    );
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc.as_str()),
                p("CidrBlock", "10.42.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    )
}

/// How a row's launch states the capacity it wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// The uniform form: one instance of a named type, as `--instance-type` and
    /// `--instance-count` state it.
    Uniform,
    /// One explicit `MASTER` instance group, as `--instance-groups` states it.
    Group,
}

/// What one of a row's steps submits as its whole work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// A line of the step's own output, which exits zero.
    Echo,
    /// A status of the command's own, which is not zero.
    GiveUp,
}

/// One row: the launch's shape, where its logs go and what its steps answered.
struct Row {
    case: &'static str,
    capacity: Capacity,
    /// The key prefix the row's `LogUri` names inside the bucket — empty for a URI that names
    /// the bucket alone.
    prefix: &'static str,
    /// The work each step submits, and the state its exit status ends that step in.
    steps: Vec<(Work, &'static str)>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the uniform capacity form, one step that exits zero",
            capacity: Capacity::Uniform,
            prefix: "logs",
            steps: vec![(Work::Echo, "COMPLETED")],
        },
        Row {
            case: "an explicit primary instance group, one step that exits zero",
            capacity: Capacity::Group,
            prefix: "nested/cluster-logs",
            steps: vec![(Work::Echo, "COMPLETED")],
        },
        Row {
            case: "a log destination at the bucket's root, one step that exits non-zero",
            capacity: Capacity::Uniform,
            prefix: "",
            steps: vec![(Work::GiveUp, "FAILED")],
        },
        Row {
            case: "two steps, one of each ending",
            capacity: Capacity::Group,
            prefix: "logs",
            steps: vec![(Work::Echo, "COMPLETED"), (Work::GiveUp, "FAILED")],
        },
    ]
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

/// Launch one row's cluster — keep-alive, placed in `subnet`, logging to its own destination —
/// and answer the cluster id Amazon EMR minted for it.
async fn launch(router: &MultiServiceRouter, row: &Row, ordinal: usize, subnet: &str) -> String {
    let steps: Vec<Value> = row
        .steps
        .iter()
        .enumerate()
        .map(|(at, (work, _))| {
            json!({
                "Name": format!("{} #{at}", row.case),
                // "CONTINUE — Continues to the next step in the queue"
                // (`StepConfig$ActionOnFailure` doc): a step that fails neither cancels the
                // queue behind it nor takes the cluster down.
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": {
                    "Jar": "command-runner.jar",
                    "Args": command_of(*work, at),
                },
            })
        })
        .collect();
    let mut instances = json!({
        "KeepJobFlowAliveWhenNoSteps": true,
        "Ec2SubnetId": subnet,
    });
    match row.capacity {
        Capacity::Uniform => {
            instances["InstanceCount"] = json!(1);
            instances["MasterInstanceType"] = json!(INSTANCE_TYPE);
        }
        Capacity::Group => {
            instances["InstanceGroups"] = json!([{
                "Name": "Primary",
                "InstanceRole": "MASTER",
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
            }]);
        }
    }
    let answered = emr(
        router,
        row.case,
        "RunJobFlow",
        &json!({
            "Name": format!("post-mortem-{ordinal}"),
            "ReleaseLabel": RELEASE_LABEL,
            "Applications": [{ "Name": "Hadoop" }],
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "LogUri": log_uri(row),
            "Steps": steps,
            "Instances": instances,
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

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering them in
/// submission order.
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
        if !states.is_empty()
            && states
                .iter()
                .all(|state| TERMINAL_STEP.contains(&state.as_str()))
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

/// Poll `DescribeCluster` until the cluster's state satisfies `settled`, answering that state.
async fn awaited_state<F: Fn(&str) -> bool>(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    wanted: &str,
    settled: F,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emr(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id }),
        )
        .await;
        let state = described["Cluster"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if settled(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cluster to reach {wanted}, got {state:?}"
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

/// Poll `ListObjectsV2` until the cluster's log destination holds EXACTLY the files `expected`
/// names — making NO Amazon EMR call while it waits, which is what a client that has shut its
/// cluster down does.
///
/// The wait is for the whole directory rather than for the first file to appear: one step's
/// files are written one object at a time, so a listing taken mid-delivery holds some of them,
/// and a reader that stopped at the first non-empty answer would be comparing a half-written
/// directory.
async fn delivered_keys(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    expected: &BTreeSet<String>,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, prefix).await;
        if &keys == expected {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected exactly {expected:?} under {prefix}, got {keys:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every row, end to end: the steps settle, the cluster rests in `WAITING`, the caller shuts
/// it down, and the log files under its `LogUri` are there — and stay there, read after read —
/// for a client that asks Amazon EMR nothing more.
#[tokio::test]
async fn a_terminated_keep_alive_clusters_logs_are_read_out_of_amazon_s3() {
    let router = router();
    let subnet = seed(&router).await;
    let rows = rows();

    // Every row's cluster is launched FIRST, so the phases the clock owes them all run
    // together rather than one row's waiting standing in front of the next row's launch.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        launched.push(launch(&router, row, ordinal, &subnet).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let prefix = cluster_prefix(row, cluster_id);

        // (1) Every submitted step reaches the terminal state its own work answered.
        let settled = settled_steps(&router, case, cluster_id).await;
        assert_eq!(
            settled.len(),
            row.steps.len(),
            "case {case:?}: expected one step back per submitted step, got {settled:?}"
        );
        for ((work, expected), step) in row.steps.iter().zip(&settled) {
            assert_eq!(
                step["Status"]["State"].as_str(),
                Some(*expected),
                "case {case:?}: expected the {work:?} step to end {expected}, got {step}"
            );
        }

        // (2) A cluster asked to stay up rests in WAITING once its queue has run.
        let resting = awaited_state(&router, case, cluster_id, "WAITING", |state| {
            !COMING_UP.contains(&state)
        })
        .await;
        assert_eq!(
            resting, "WAITING",
            "case {case:?}: expected the kept-alive cluster to rest in WAITING, got {resting:?}"
        );

        // (3) The caller shuts the cluster down and waits for it to be gone.
        emr(
            &router,
            case,
            "TerminateJobFlows",
            &json!({ "JobFlowIds": [cluster_id.clone()] }),
        )
        .await;
        let ended = awaited_state(&router, case, cluster_id, "TERMINATED", |state| {
            state == "TERMINATED" || state == "TERMINATED_WITH_ERRORS"
        })
        .await;
        assert_eq!(
            ended, "TERMINATED",
            "case {case:?}: expected the terminated cluster to end TERMINATED, got {ended:?}"
        );

        // (4) From here on the client asks Amazon EMR nothing: the log files under the
        // cluster's own prefix are there, one directory per step that ran, and every re-read
        // answers the same set — with each file still a gzip stream Amazon S3's own reader
        // serves, fetched between one re-read and the next.
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for step in &settled {
            let step_id = step["Id"].as_str().unwrap_or_default();
            for name in STEP_LOG_FILES {
                expected.insert(format!("{prefix}{STEPS_SEGMENT}/{step_id}/{name}"));
            }
        }
        let delivered = delivered_keys(&router, case, &prefix, &expected).await;
        for round in 0..PERSISTENCE_REREADS {
            let after = keys_under(&router, case, &prefix).await;
            assert_eq!(
                after, delivered,
                "case {case:?}: re-read {round} after termination must answer the same files, \
                 expected {delivered:?}, got {after:?}"
            );
            for key in &delivered {
                let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
                assert!(
                    (200..300).contains(&status),
                    "case {case:?}: {key} must still be served after termination, got {status}"
                );
                assert_eq!(
                    body.get(..2),
                    Some([0x1f, 0x8b].as_slice()),
                    "case {case:?}: {key} must still be a gzip stream, got {:?}",
                    body.get(..2)
                );
            }
        }
    }
}
