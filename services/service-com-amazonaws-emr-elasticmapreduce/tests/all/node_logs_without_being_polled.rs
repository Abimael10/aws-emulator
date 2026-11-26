// SPDX-License-Identifier: CC0-1.0

//! A cluster's BRING-UP happens on the clock: its bootstrap actions run and the files its nodes
//! leave under its `LogUri` are written WITHOUT ANYTHING EVER REPORTING THE CLUSTER.
//!
//! THE RULE: "a list of bootstrap actions to run before Hadoop starts on the cluster nodes"
//! (`RunJobFlow$BootstrapActions` doc), each naming "[the] location in Amazon S3 of the script
//! to run during a bootstrap action" (`ScriptBootstrapActionConfig$Path` doc), and "LogUri — the
//! location in Amazon S3 to write the log files of the job flow. If a value is not provided,
//! logs are not created" (`RunJobFlow$LogUri` doc). Bringing the cluster up is therefore the
//! SERVICE's own work: once the launch has answered, each script is fetched and run on every
//! node, and the files those runs and those nodes produce are written under the launch's log
//! location — keyed by the node they are of, `<LogUri>/<cluster-id>/node/<instance-id>/`, with
//! one `bootstrap-actions/<position>/` directory per action — whether or not any later call asks
//! about the cluster. A client that launches a cluster and then watches only the object store
//! (because it deployed the cluster through CloudFormation, or because what it is waiting for is
//! the bootstrap output itself) sees exactly the same files as one that polls
//! `DescribeCluster`.
//!
//! So every row here makes ONE Amazon EMR call, `RunJobFlow`, and then calls Amazon EMR for
//! nothing at all: what it waits on is Amazon S3's own listing of the log location. A bring-up
//! performed only by the next operation that reports the cluster cannot pass that, because no
//! such operation is ever made — and the node ids in the keys are read out of the listing rather
//! than out of `ListInstances` for the same reason.
//!
//! The rows differ along what the rule generalises over: how many bootstrap actions the launch
//! carried (one, two), how many nodes it provisioned (one, two), the shape of the log location
//! (a prefix, the bucket root, a nested prefix), what the script wrote (standard output alone,
//! both streams), and whether the launch named a log location at all — the row that names none
//! still runs its action (it leaves a real object behind through the CLI its environment
//! provides) and still writes no log file.
//!
//! Amazon EC2 and Amazon S3 are mounted beside Amazon EMR on one engine, as the assembled
//! router mounts them: every node is an instance Amazon EC2 really launched, every script is an
//! object a client put with `PutObject`, and every file is read back with `GetObject` and
//! decompressed here.

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every script is fetched from and every log file is written into.
const BUCKET: &str = "emr-unpolled-node-logs";

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The key segments a node's files live under, spelled out here rather than imported from the
/// service crate so a wrong constant in the implementation cannot make this suite pass.
const NODE_SEGMENT: &str = "node";
const BOOTSTRAP_ACTIONS_SEGMENT: &str = "bootstrap-actions";
const INSTANCE_STATE_SEGMENT: &str = "instance-state";

/// The file of a run that holds what the script wrote to its standard output, and the one that
/// holds its standard error.
const STDOUT_FILE: &str = "stdout.gz";
const STDERR_FILE: &str = "stderr.gz";

/// How long a poll waits for the work the clock owes a cluster to have happened: both
/// provisioning phases of every row's cluster, each script's run and the writes they lead to,
/// with room to spare. The wait is on the CONDITION — it looks again until the files are there —
/// so a loaded host delays a row instead of breaking it, and nothing here asserts how long
/// anything took.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll sleeps between listings.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Amazon EMR, Amazon EC2 and Amazon S3 on one engine.
fn emulator() -> MultiServiceRouter {
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
    router
}

/// One `awsJson1_1` Amazon EMR call that must be served. The ONLY one this suite makes is
/// `RunJobFlow`.
async fn emr(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("elasticmapreduce.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261110/{REGION}/\
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
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One path-style Amazon S3 request: its status and the bytes it carried.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261110/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
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

/// Every key the store holds under `prefix`, as `ListObjectsV2` reports them.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    let text = String::from_utf8_lossy(&body).into_owned();
    assert!(
        (200..300).contains(&status),
        "case {case:?}: ListObjectsV2 under {prefix:?} must succeed, got {status} {text}"
    );
    text.split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The TEXT one delivered file holds: the object, which is a gzip stream, decompressed.
async fn log_text(router: &MultiServiceRouter, case: &str, key: &str) -> String {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: GetObject {key} must succeed, got {status}"
    );
    assert_eq!(
        body.get(..2),
        Some([0x1f, 0x8b].as_slice()),
        "case {case:?}: {key} must be a gzip stream, got {:?}",
        body.get(..2)
    );
    let mut text = String::new();
    ::flate2::read::GzDecoder::new(body.as_slice())
        .read_to_string(&mut text)
        .unwrap_or_else(|error| panic!("case {case:?}: {key} must decompress, got {error}"));
    text
}

/// The bytes of the plain object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).into_owned())
}

/// Create the bucket every row shares.
async fn seed(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// One row: what its launch carries, and what its nodes must leave behind.
struct Row {
    case: &'static str,
    /// The key prefix its `LogUri` names inside the bucket, with a trailing separator where it
    /// names one at all, and `None` for a launch that names no `LogUri`.
    log_prefix: Option<&'static str>,
    /// How many core nodes it asks for beside its one primary node.
    core_nodes: usize,
    /// One entry per bootstrap action, in launch order: the script, what its `stdout.gz` must
    /// hold exactly, and a fragment its `stderr.gz` must carry.
    actions: Vec<Action>,
    /// `(key, contents)` an action of this row really left in the object store through the CLI
    /// its environment provides — the evidence a row with no log location has that its script
    /// ran at all.
    wrote: Vec<(String, String)>,
}

/// One bootstrap action of a row.
struct Action {
    script: String,
    stdout: String,
    stderr: &'static str,
}

impl Row {
    /// How many nodes the launch provisions: the one primary node and its core nodes.
    fn nodes(&self) -> usize {
        self.core_nodes + 1
    }
}

/// The key one row's script is put under.
fn script_key(ordinal: usize, position: usize) -> String {
    format!("scripts/{ordinal}-{position}.sh")
}

/// The key the row that names no log location writes its object at.
fn marker_key() -> String {
    "markers/unlogged.txt".to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one action on one node, under a log prefix",
            log_prefix: Some("logs/"),
            core_nodes: 0,
            actions: vec![Action {
                script: "#!/bin/bash\necho unpolled-bootstrap-ran-ok\n".to_string(),
                stdout: "unpolled-bootstrap-ran-ok\n".to_string(),
                stderr: "Command exiting with ret '0'",
            }],
            wrote: Vec::new(),
        },
        Row {
            case: "two actions, with the log location at the bucket root",
            log_prefix: Some(""),
            core_nodes: 0,
            actions: vec![
                Action {
                    script: "echo first-unpolled-action\n".to_string(),
                    stdout: "first-unpolled-action\n".to_string(),
                    stderr: "Command exiting with ret '0'",
                },
                Action {
                    script: "echo second-unpolled-action\n".to_string(),
                    stdout: "second-unpolled-action\n".to_string(),
                    stderr: "Command exiting with ret '0'",
                },
            ],
            wrote: Vec::new(),
        },
        Row {
            case: "one action on each of two nodes, under a nested log prefix",
            log_prefix: Some("deep/nested/logs/"),
            core_nodes: 1,
            actions: vec![Action {
                script: "#!/bin/bash\necho on-every-node\necho noted-on-stderr 1>&2\n".to_string(),
                stdout: "on-every-node\n".to_string(),
                stderr: "noted-on-stderr",
            }],
            wrote: Vec::new(),
        },
        Row {
            // No log location: "if a value is not provided, logs are not created" — and the
            // action still RUNS, which the object it uploads is the proof of.
            case: "a launch that named no LogUri",
            log_prefix: None,
            core_nodes: 0,
            actions: vec![Action {
                script: format!(
                    "#!/bin/bash\n\
                     echo -n unlogged-action-ran > /tmp/marker.txt\n\
                     aws s3 cp /tmp/marker.txt s3://{BUCKET}/{}\n",
                    marker_key()
                ),
                stdout: String::new(),
                stderr: "Command exiting with ret '0'",
            }],
            wrote: vec![(marker_key(), "unlogged-action-ran".to_string())],
        },
    ]
}

/// Launch one row's cluster, after putting every script its actions name in the store.
async fn launch(router: &MultiServiceRouter, ordinal: usize, row: &Row) -> String {
    for (index, action) in row.actions.iter().enumerate() {
        put_object(
            router,
            row.case,
            &script_key(ordinal, index + 1),
            action.script.as_bytes(),
        )
        .await;
    }
    let mut groups = vec![json!({
        "Name": "primary",
        "InstanceRole": "MASTER",
        "InstanceType": "m5.xlarge",
        "InstanceCount": 1,
    })];
    if row.core_nodes > 0 {
        groups.push(json!({
            "Name": "core",
            "InstanceRole": "CORE",
            "InstanceType": "m5.xlarge",
            "InstanceCount": row.core_nodes,
        }));
    }
    let mut request = json!({
        "Name": format!("unpolled-node-logs-{ordinal}"),
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Hadoop" }],
        "Instances": {
            "InstanceGroups": groups,
            "KeepJobFlowAliveWhenNoSteps": true,
        },
        "BootstrapActions": (1..=row.actions.len())
            .map(|position| json!({
                "Name": format!("action-{position}"),
                "ScriptBootstrapAction": {
                    "Path": format!("s3://{BUCKET}/{}", script_key(ordinal, position)),
                },
            }))
            .collect::<Vec<Value>>(),
    });
    if let Some(prefix) = row.log_prefix {
        request["LogUri"] = json!(format!("s3://{BUCKET}/{prefix}"));
    }
    let answered = emr(router, row.case, "RunJobFlow", request).await;
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

/// The node one key's file belongs to: the segment after `node/`.
fn node_of(key: &str) -> Option<&str> {
    key.split_once(&format!("{NODE_SEGMENT}/"))
        .and_then(|(_, rest)| rest.split('/').next())
}

/// Poll Amazon S3's own listing of `prefix` — making no Amazon EMR call at all — until every
/// node of the cluster has left its instance-state file and one directory per action, and answer
/// the whole set.
///
/// The wait is for the WHOLE set rather than for the first key: a node's files are written one
/// object at a time, so a listing taken mid-delivery holds some of them.
async fn delivered_without_polling(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    nodes: usize,
    actions: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, prefix).await;
        let states = keys
            .iter()
            .filter(|key| key.contains(INSTANCE_STATE_SEGMENT))
            .count();
        let stdouts = keys
            .iter()
            .filter(|key| key.contains(BOOTSTRAP_ACTIONS_SEGMENT) && key.ends_with(STDOUT_FILE))
            .count();
        if states == nodes && stdouts == nodes * actions {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: with nothing ever reporting the cluster, {nodes} instance-state \
             file(s) and {} bootstrap-action stdout file(s) must appear under {prefix:?}, got \
             {keys:?}",
            nodes * actions
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE, over every row: a launched cluster runs each bootstrap action on each of its nodes
/// and writes the files those nodes leave under its `LogUri`, with nothing ever reporting the
/// cluster — and a launch that named no `LogUri` writes none of them while its action still
/// runs.
#[tokio::test]
async fn a_launchs_bootstrap_output_reaches_s3_without_anything_reporting_the_cluster() {
    let router = emulator();
    seed(&router).await;
    let rows = rows();

    // Every row's cluster is launched first, so the phases the clock owes them run together.
    // This is the last Amazon EMR call this test makes.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        launched.push(launch(&router, ordinal, row).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let Some(log_prefix) = row.log_prefix else {
            // Nowhere to write them. The action still ran, which the object it uploaded is the
            // proof of, and not one key of the store belongs to this cluster.
            for (key, expected) in &row.wrote {
                let deadline = Instant::now() + DEADLINE;
                loop {
                    let held = object(&router, key).await;
                    if held.as_deref() == Some(expected.as_str()) {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "case {case:?}: the action of a cluster nobody reports still runs, so \
                         {key} must hold {expected:?}, got {held:?}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
            let everything = keys_under(&router, case, "").await;
            let its_own: Vec<&String> = everything
                .iter()
                .filter(|key| key.contains(cluster_id.as_str()))
                .collect();
            assert!(
                its_own.is_empty(),
                "case {case:?}: a cluster that named no LogUri has nowhere to write its files, \
                 got {its_own:?}"
            );
            continue;
        };

        let cluster_prefix = format!("{log_prefix}{cluster_id}/");
        let delivered = delivered_without_polling(
            &router,
            case,
            &cluster_prefix,
            row.nodes(),
            row.actions.len(),
        )
        .await;

        // The nodes the files belong to are read out of the listing: nothing here asks Amazon
        // EMR which instances the cluster holds.
        let nodes: BTreeSet<&str> = delivered.iter().filter_map(|key| node_of(key)).collect();
        assert_eq!(
            nodes.len(),
            row.nodes(),
            "case {case:?}: expected the files of {} node(s) under {cluster_prefix:?}, got \
             {delivered:?}",
            row.nodes()
        );

        for node in &nodes {
            let node_prefix = format!("{cluster_prefix}{NODE_SEGMENT}/{node}");
            assert!(
                delivered
                    .iter()
                    .any(|key| key.starts_with(&format!("{node_prefix}/{INSTANCE_STATE_SEGMENT}/"))),
                "case {case:?}: node {node} is owed its own instance-state file, got {delivered:?}"
            );
            for (index, action) in row.actions.iter().enumerate() {
                let position = index + 1;
                let run_prefix =
                    format!("{node_prefix}/{BOOTSTRAP_ACTIONS_SEGMENT}/{position}");
                let stdout_key = format!("{run_prefix}/{STDOUT_FILE}");
                assert!(
                    delivered.contains(&stdout_key),
                    "case {case:?}: expected {stdout_key} among the delivered files, got \
                     {delivered:?}"
                );
                let stdout = log_text(&router, case, &stdout_key).await;
                assert_eq!(
                    stdout, action.stdout,
                    "case {case:?}: node {node} action {position}: expected {:?} on standard \
                     output, got {stdout:?}",
                    action.stdout
                );
                let stderr = log_text(&router, case, &format!("{run_prefix}/{STDERR_FILE}")).await;
                assert!(
                    stderr.contains(action.stderr),
                    "case {case:?}: node {node} action {position}: expected {:?} in the \
                     standard-error file, got {stderr:?}",
                    action.stderr
                );
            }
        }
    }
}
