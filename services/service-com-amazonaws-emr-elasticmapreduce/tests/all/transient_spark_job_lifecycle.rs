// SPDX-License-Identifier: CC0-1.0

//! A spark-submit step on a cluster that was NOT asked to stay up, end to end over the wire.
//!
//! THE RULE: a step a cluster accepted is work the cluster RUNS — it leaves the state it was
//! queued in on its own and settles in a terminal `StepState`
//! (`smithy_query(query="shape", shape="com.amazonaws.emr#StepState")`), and the objects the
//! application it submitted writes are objects a client's own `ListObjectsV2` and `GetObject`
//! find — and a cluster whose `JobFlowInstancesConfig$KeepJobFlowAliveWhenNoSteps` is false
//! ("specifies whether the cluster should remain available after completing all steps") then
//! shuts itself down once that queue has run: it reaches `TERMINATED` with
//! `Status.StateChangeReason.Code` `ALL_STEPS_COMPLETED`
//! (`smithy_query(query="shape", shape="com.amazonaws.emr#ClusterStateChangeReasonCode")`),
//! drops out of the live `ListClusters` state set, and the legacy job-flow surface then
//! refuses `AddJobFlowSteps` on it with that surface's fixed sentence — "you can only add
//! steps to a cluster that is in one of the following states: STARTING, BOOTSTRAPPING,
//! RUNNING, or WAITING" (`AddJobFlowSteps` doc).
//!
//! The rows span what the rule generalises over: the release the cluster runs (so the Spark
//! the step submits to is the one that release ships), the `--deploy-mode` the submit names
//! (client, cluster, and none at all), how many partitions the application slices its work
//! into — which is how many part files its output prefix then holds — and how much work it
//! computes.
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine through the harness
//! `tests/all/spark_step_application.rs` owns: every application is written through S3's own
//! `PutObject` and every output read back through `ListObjectsV2` and `GetObject`, so the code
//! that runs is the code the bucket holds and the output is a real object rather than anything
//! this suite handed Amazon EMR.
//!
//! Every wait here waits for the CONDITION a read reports, under a deadline far above every
//! span this service owes a cluster or a step, so a slow or loaded host delays a row rather
//! than failing it.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::input_refusals::error_message;
use crate::job_flow_lifecycle::assert_refused;
use crate::spark_step_application::{emr_ok, emulator, object, put_object, s3, BUCKET};

/// The Region both services are addressed in, as the shared harness addresses them.
const REGION: &str = "us-east-1";

/// How long a poll waits for a condition the clock owes: both provisioning phases, the step's
/// own span, one application run and the shutdown, with room to spare on a loaded host.
const DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP_STATES: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a step is in before it has settled (`com.amazonaws.emr#StepState`).
const PENDING_STEP_STATES: [&str; 2] = ["PENDING", "RUNNING"];

/// The states a cluster that is still LIVE is in: the set a client filters `ListClusters` by
/// to see the clusters it is paying for (`com.amazonaws.emr#ClusterState`).
const LIVE_CLUSTER_STATES: [&str; 4] = ["STARTING", "BOOTSTRAPPING", "RUNNING", "WAITING"];

/// The one sentence the legacy job-flow surface refuses a change to a cluster on its way out
/// or gone with. Entirely fixed text: no cluster id, and no state.
const NOT_MODIFIABLE_SENTENCE: &str =
    "A job flow that is shutting down, terminated, or finished may not be modified.";

/// The Hadoop marker a successful text output carries beside its part files.
const SUCCESS_MARKER: &str = "_SUCCESS";

/// One `awsJson1_1` Amazon EMR call, answered as `(status, body)` — the shared harness's own
/// caller asserts 200, and a refusal is what this suite's last assertion reads.
async fn emr_call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let host = format!("elasticmapreduce.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261120/{REGION}/\
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

/// The keys of every object under `prefix`, from `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let (status, listing) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    let listing = String::from_utf8_lossy(&listing).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing s3://{BUCKET}/{prefix} must succeed, got {status} with {listing}"
    );
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// One row: the cluster a launch asks for and the work its single step submits.
struct Case {
    label: &'static str,
    /// The release the cluster runs, which is the release whose Spark the step reaches.
    release: &'static str,
    /// The `--deploy-mode` the submit names, or `None` for a submit that names none.
    deploy_mode: Option<&'static str>,
    /// How many values the application squares.
    count: u32,
    /// How many partitions it slices them into, which is how many part files it writes.
    partitions: usize,
}

/// The rows: a release of each of the two lines the catalog publishes Spark on, every form of
/// `--deploy-mode` a submit can take, one partition up to four, and a count from the smallest
/// there is up to a hundred.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "client deploy mode, four partitions",
            release: "emr-6.15.0",
            deploy_mode: Some("client"),
            count: 100,
            partitions: 4,
        },
        Case {
            label: "cluster deploy mode, two partitions",
            release: "emr-7.9.0",
            deploy_mode: Some("cluster"),
            count: 10,
            partitions: 2,
        },
        Case {
            label: "no deploy mode at all, one partition",
            release: "emr-7.9.0",
            deploy_mode: None,
            count: 1,
            partitions: 1,
        },
        Case {
            label: "client deploy mode, three partitions on a newer release",
            release: "emr-7.9.0",
            deploy_mode: Some("client"),
            count: 7,
            partitions: 3,
        },
    ]
}

/// The application every row submits: it squares the values it is asked for, slices them into
/// the partitions it is asked for, and saves them as text under the prefix it is given — so
/// the objects the prefix then holds are this program's own output.
const APPLICATION: &str = r#"
import sys
from pyspark import SparkContext

count = int(sys.argv[1])
partitions = int(sys.argv[2])
sc = SparkContext.getOrCreate()
sc.parallelize(range(1, count + 1), partitions).map(lambda value: value * value).saveAsTextFile(
    sys.argv[3]
)
sc.stop()
"#;

/// The key one row's application is written under.
fn application_key(ordinal: usize) -> String {
    format!("job/sum_squares-{ordinal}.py")
}

/// The prefix one row's application writes its output under.
fn output_prefix(ordinal: usize) -> String {
    format!("out/{ordinal}")
}

/// The `StepConfig` one row submits: `command-runner.jar` running a `spark-submit` of the
/// application in Amazon S3, with the row's own deploy mode and arguments.
fn step_config(case: &Case, ordinal: usize) -> Value {
    let mut args: Vec<String> = vec!["spark-submit".to_string()];
    if let Some(mode) = case.deploy_mode {
        args.push("--deploy-mode".to_string());
        args.push(mode.to_string());
    }
    args.push(format!("s3://{BUCKET}/{}", application_key(ordinal)));
    args.push(case.count.to_string());
    args.push(case.partitions.to_string());
    args.push(format!("s3://{BUCKET}/{}", output_prefix(ordinal)));
    json!({
        "Name": case.label,
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
    })
}

/// A cluster that does NOT stay up, launched with one step and nothing else.
async fn launch_transient(router: &MultiServiceRouter, case: &Case, ordinal: usize) -> String {
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": case.label,
            "ReleaseLabel": case.release,
            "JobFlowRole": "EMR_EC2_DefaultRole",
            "ServiceRole": "EMR_DefaultRole",
            "Applications": [{ "Name": "Spark" }],
            "Steps": [step_config(case, ordinal)],
            "Instances": {
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                // The member the rule is about.
                "KeepJobFlowAliveWhenNoSteps": false,
                "TerminationProtected": false,
            },
        }),
    )
    .await;
    launched["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// The id of the one step `ListSteps` reports for `cluster_id`.
async fn only_step_id(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> String {
    let listed = emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
    let steps = listed["Steps"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        steps.len(),
        1,
        "case {case:?}: the launch submitted one step, got {listed}"
    );
    steps[0]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a listed step carries an Id, got {listed}"))
        .to_string()
}

/// `DescribeStep` for one step.
async fn describe_step(router: &MultiServiceRouter, cluster_id: &str, step_id: &str) -> Value {
    emr_ok(
        router,
        "DescribeStep",
        json!({ "ClusterId": cluster_id, "StepId": step_id }),
    )
    .await["Step"]
        .clone()
}

/// Poll `DescribeStep` until the step reports a terminal `StepState`, answering that read.
async fn settled_step(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    step_id: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let step = describe_step(router, cluster_id, step_id).await;
        let state = step["Status"]["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STEP_STATES.contains(&state.as_str()) {
            return step;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: expected the step to reach a terminal state, got {state:?} in {step}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeCluster` until the cluster reports `TERMINATED`, answering that read.
async fn terminated_cluster(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Value {
    let started = Instant::now();
    loop {
        let cluster = emr_ok(router, "DescribeCluster", json!({ "ClusterId": cluster_id })).await
            ["Cluster"]
            .clone();
        let state = cluster["Status"]["State"].as_str().unwrap_or_default().to_string();
        if state == "TERMINATED" {
            return cluster;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: expected the cluster to terminate itself, got {state:?} in {cluster}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The cluster ids `ListClusters` reports under `states`.
async fn listed_cluster_ids(
    router: &MultiServiceRouter,
    case: &str,
    states: &[&str],
) -> BTreeSet<String> {
    let listed = emr_ok(router, "ListClusters", json!({ "ClusterStates": states })).await;
    let clusters = listed["Clusters"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: ListClusters answers Clusters, got {listed}"));
    clusters
        .iter()
        .filter_map(|cluster| cluster["Id"].as_str().map(str::to_string))
        .collect()
}

/// The keys one row's output prefix must hold: one part file per partition, and the Hadoop
/// success marker beside them.
fn expected_keys(case: &Case, ordinal: usize) -> BTreeSet<String> {
    let prefix = output_prefix(ordinal);
    let mut keys: BTreeSet<String> = (0..case.partitions)
        .map(|index| format!("{prefix}/part-{index:05}"))
        .collect();
    keys.insert(format!("{prefix}/{SUCCESS_MARKER}"));
    keys
}

/// The squares the application computes, as this suite's own arithmetic.
fn expected_values(case: &Case) -> Vec<u64> {
    (1..=u64::from(case.count)).map(|value| value * value).collect()
}

#[tokio::test]
async fn a_transient_clusters_spark_step_runs_writes_its_output_and_ends_the_cluster() {
    let router = emulator();
    let cases = cases();

    // The bucket and one application per row, written through Amazon S3's own calls.
    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    for (ordinal, case) in cases.iter().enumerate() {
        put_object(
            &router,
            case.label,
            &application_key(ordinal),
            APPLICATION.as_bytes(),
        )
        .await;
    }

    // Every row's cluster is launched before any is waited on, and each step is read straight
    // after its launch: a step a cluster has accepted but not yet run is readable, and has not
    // settled yet.
    let mut launched: Vec<(String, String)> = Vec::new();
    for (ordinal, case) in cases.iter().enumerate() {
        let cluster_id = launch_transient(&router, case, ordinal).await;
        let step_id = only_step_id(&router, case.label, &cluster_id).await;
        let queued = describe_step(&router, &cluster_id, &step_id).await;
        let state = queued["Status"]["State"].as_str().unwrap_or_default();
        assert!(
            PENDING_STEP_STATES.contains(&state),
            "case {:?}: a step that has not run yet is still queued, got {queued}",
            case.label
        );
        launched.push((cluster_id, step_id));
    }

    for ((ordinal, case), (cluster_id, step_id)) in cases.iter().enumerate().zip(&launched) {
        let label = case.label;

        // The step settles on its own, and it settles in the state a clean run ends in.
        let step = settled_step(&router, label, cluster_id, step_id).await;
        assert_eq!(
            step["Status"]["State"].as_str(),
            Some("COMPLETED"),
            "case {label:?}: an application that exited cleanly completes its step, got {step}"
        );
        assert!(
            step["Status"]["FailureDetails"].is_null(),
            "case {label:?}: a completed step reports no failure, got {step}"
        );
        for member in ["CreationDateTime", "StartDateTime", "EndDateTime"] {
            assert!(
                step["Status"]["Timeline"][member].as_f64().is_some(),
                "case {label:?}: a step that ran reports {member}, got {step}"
            );
        }

        // The output prefix holds exactly the objects the run wrote, as a client's own listing
        // reports them.
        let prefix = output_prefix(ordinal);
        let keys = keys_under(&router, label, &format!("{prefix}/")).await;
        let expected = expected_keys(case, ordinal);
        assert_eq!(
            keys, expected,
            "case {label:?}: expected s3://{BUCKET}/{prefix}/ to hold {expected:?}, got {keys:?}"
        );

        // The part files hold what the job computed: the squares, in partition order.
        let mut written: Vec<u64> = Vec::new();
        for index in 0..case.partitions {
            let key = format!("{prefix}/part-{index:05}");
            let held = object(&router, &key).await.unwrap_or_else(|| {
                panic!("case {label:?}: the store must hold {key}")
            });
            for line in held.lines() {
                written.push(line.parse::<u64>().unwrap_or_else(|error| {
                    panic!("case {label:?}: {key} holds the values as text, got {line:?}: {error}")
                }));
            }
        }
        let values = expected_values(case);
        assert_eq!(
            written, values,
            "case {label:?}: expected the part files to hold {values:?}, got {written:?}"
        );
        let marker = object(&router, &format!("{prefix}/{SUCCESS_MARKER}")).await;
        assert_eq!(
            marker.as_deref(),
            Some(""),
            "case {label:?}: the success marker is an empty object, got {marker:?}"
        );

        // Nothing but the clock is called from here on: no TerminateJobFlows.
        let cluster = terminated_cluster(&router, label, cluster_id).await;
        assert_eq!(
            cluster["Status"]["StateChangeReason"]["Code"].as_str(),
            Some("ALL_STEPS_COMPLETED"),
            "case {label:?}: the self-shutdown names the modelled reason, got {cluster}"
        );
        assert!(
            cluster["Status"]["Timeline"]["EndDateTime"].as_f64().is_some(),
            "case {label:?}: a terminated cluster records EndDateTime, got {cluster}"
        );
        assert_eq!(
            cluster["AutoTerminate"].as_bool(),
            Some(true),
            "case {label:?}: a cluster that was not kept alive terminates after its steps, \
             got {cluster}"
        );

        // A client watching what it is paying for no longer sees it, and the record is still
        // there under the state it ended in.
        let live = listed_cluster_ids(&router, label, &LIVE_CLUSTER_STATES).await;
        assert!(
            !live.contains(cluster_id),
            "case {label:?}: a terminated cluster leaves the live listing, got {live:?}"
        );
        let gone = listed_cluster_ids(&router, label, &["TERMINATED"]).await;
        assert!(
            gone.contains(cluster_id),
            "case {label:?}: the terminated cluster is still reported under its own state, \
             got {gone:?}"
        );

        // And the job-flow surface refuses further work on it, with its fixed sentence.
        let refusal = format!("AddJobFlowSteps on the terminated cluster of {label:?}");
        let (status, payload) = emr_call(
            &router,
            "AddJobFlowSteps",
            json!({
                "JobFlowId": cluster_id,
                "Steps": [step_config(case, ordinal)],
            }),
        )
        .await;
        assert_refused(&refusal, status, &payload, "ValidationException");
        let said = error_message(&payload);
        assert_eq!(
            said, NOT_MODIFIABLE_SENTENCE,
            "case {refusal:?}: expected {NOT_MODIFIABLE_SENTENCE:?}, got {payload}"
        );
        assert!(
            !said.contains(cluster_id.as_str()) && !said.contains("TERMINATED"),
            "case {refusal:?}: the sentence is fixed text, got {said:?}"
        );
        // The refusal added nothing: the cluster still reports the one step it ran.
        let listed = emr_ok(&router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
        assert_eq!(
            listed["Steps"].as_array().map(Vec::len),
            Some(1),
            "case {refusal:?}: a refused submission queues nothing, got {listed}"
        );
    }
}
