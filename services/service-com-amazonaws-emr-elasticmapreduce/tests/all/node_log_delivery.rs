// SPDX-License-Identifier: CC0-1.0

//! The log files a cluster's NODES leave under its `LogUri`, end to end over the wire.
//!
//! THE RULE: "LogUri — the location in Amazon S3 to write the log files of the job flow"
//! (`RunJobFlow$LogUri` doc). Naming that location makes the files a cluster's NODES produce
//! EXIST there, keyed by the node they are of — `<LogUri>/<cluster-id>/node/<instance-id>/` —
//! so a client that waits for a cluster to come up and then lists its log location finds, for
//! every node Amazon EC2 launched for the cluster, that node's own gzipped instance-state file
//! and one gzipped directory per bootstrap action the node ran, holding what that script really
//! wrote to each of its streams and the controller's account of how it ended. A cluster
//! launched with no `LogUri` has nowhere to write them, so none is written.
//!
//! The rows span what the rule generalises over: how many bootstrap actions the launch carried
//! (none, one, two), what the script wrote (a line on standard output, a line on standard error
//! and a non-zero exit), whether the cluster came up or went down on the action, and whether
//! the launch named a `LogUri` at all. Every cluster carries two nodes, so each row also pins
//! that the files are PER NODE.
//!
//! Amazon EC2 and Amazon S3 are mounted BESIDE Amazon EMR on one engine, exactly as the
//! assembled router mounts them: every node a row asserts files for is an instance Amazon EC2
//! really launched and reports through `ListInstances`, every script is an object a client put
//! there with `PutObject`, and every file is read back with `GetObject` and decompressed here —
//! nothing reaches into any service's state struct.

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
const BUCKET: &str = "emr-node-logs";

/// The key prefix the launches' `LogUri` names inside it.
const LOG_PREFIX: &str = "logs";

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The nodes every launch of this suite asks for: one primary and one core node, so every
/// assertion below is over MORE than one node.
const CORE_NODES: i64 = 1;
const NODES: usize = 2;

/// The key segments a node's files live under, spelled out here rather than imported from the
/// service crate so a wrong constant in the implementation cannot make this suite pass.
const NODE_SEGMENT: &str = "node";
const BOOTSTRAP_ACTIONS_SEGMENT: &str = "bootstrap-actions";
const INSTANCE_STATE_SEGMENT: &str = "instance-state";

/// The three files one bootstrap-action run leaves.
const RUN_FILES: [&str; 3] = ["controller.gz", "stderr.gz", "stdout.gz"];

/// The states a cluster reports once it is up, and the ones it never leaves.
const READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];
const TERMINAL_STATES: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// How long a poll waits for a cluster to settle and for the files it owes to land: both
/// provisioning phases, every script of the longest row and the shutdown span, with room to
/// spare. The wait is on the CONDITION, so a loaded host delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll sleeps between reads.
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

/// One `awsJson1_1` Amazon EMR call that must be served.
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
        "case {case:?}: ListObjectsV2 under {prefix} must succeed, got {status} {text}"
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

/// Create the bucket every row shares.
async fn seed(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// What a row expects its cluster to come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comes {
    /// Up: it reports itself ready.
    Up,
    /// Down, because an action of it ran and exited non-zero.
    Down,
}

/// One row: the scripts its launch carries, whether it names a `LogUri`, and what the files its
/// nodes leave must hold.
struct Row {
    case: &'static str,
    /// One entry per bootstrap action, in launch order: the script's own text.
    scripts: Vec<String>,
    /// Whether the launch names a `LogUri` at all.
    logged: bool,
    comes: Comes,
    /// For each action, in launch order: what its `stdout.gz` and `stderr.gz` must hold, and
    /// the fragment its `controller.gz` must carry.
    expected: Vec<ExpectedRun>,
    /// How many of the cluster's nodes leave those directories: every node runs every action,
    /// unless an action FAILED — then it ran on the one node that got to it and the nodes the
    /// rest would have run on are going away with the cluster.
    nodes_running: usize,
}

/// What one action's three files must hold.
struct ExpectedRun {
    stdout: String,
    stderr: String,
    controller: String,
}

/// The key one row's script is put under.
fn script_key(ordinal: usize, position: usize) -> String {
    format!("scripts/{ordinal}-{position}.sh")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a launch that carried no bootstrap action",
            scripts: Vec::new(),
            logged: true,
            comes: Comes::Up,
            expected: Vec::new(),
            nodes_running: 0,
        },
        Row {
            case: "one action that writes a line on standard output",
            scripts: vec!["#!/bin/bash\necho node-bootstrap-ran-ok\n".to_string()],
            logged: true,
            comes: Comes::Up,
            expected: vec![ExpectedRun {
                stdout: "node-bootstrap-ran-ok\n".to_string(),
                stderr: "Command exiting with ret '0'\n".to_string(),
                controller: "ended with exit code 0".to_string(),
            }],
            nodes_running: NODES,
        },
        Row {
            case: "two actions, each writing its own line",
            scripts: vec![
                "echo first-action-ran\n".to_string(),
                "echo second-action-ran\n".to_string(),
            ],
            logged: true,
            comes: Comes::Up,
            expected: vec![
                ExpectedRun {
                    stdout: "first-action-ran\n".to_string(),
                    stderr: "Command exiting with ret '0'\n".to_string(),
                    controller: "ended with exit code 0".to_string(),
                },
                ExpectedRun {
                    stdout: "second-action-ran\n".to_string(),
                    stderr: "Command exiting with ret '0'\n".to_string(),
                    controller: "ended with exit code 0".to_string(),
                },
            ],
            nodes_running: NODES,
        },
        Row {
            // A failing action takes the cluster down, and reading its files is how that is
            // diagnosed: they are delivered for it exactly as they are for one that succeeded.
            case: "an action that exits non-zero, which takes the cluster down",
            scripts: vec!["echo about-to-fail\nexit 3\n".to_string()],
            logged: true,
            comes: Comes::Down,
            expected: vec![ExpectedRun {
                stdout: "about-to-fail\n".to_string(),
                stderr: "Command exiting with ret '3'\n".to_string(),
                controller: "ended with exit code 3".to_string(),
            }],
            // The failure takes the cluster down, so the action ran on the one node that got
            // to it and never on the other.
            nodes_running: 1,
        },
        Row {
            case: "a launch that named no LogUri",
            scripts: vec!["echo unlogged-action-ran\n".to_string()],
            logged: false,
            comes: Comes::Up,
            expected: Vec::new(),
            nodes_running: 0,
        },
    ]
}

/// Launch one row's cluster, after putting every script its actions name in the store.
async fn launch(router: &MultiServiceRouter, ordinal: usize, row: &Row) -> String {
    for (index, script) in row.scripts.iter().enumerate() {
        put_object(
            router,
            row.case,
            &script_key(ordinal, index + 1),
            script.as_bytes(),
        )
        .await;
    }
    let mut request = json!({
        "Name": format!("node-logs-{ordinal}"),
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Hadoop" }],
        "Instances": {
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
                    "InstanceCount": CORE_NODES,
                },
            ],
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    });
    if row.logged {
        request["LogUri"] = json!(format!("s3://{BUCKET}/{LOG_PREFIX}/"));
    }
    if !row.scripts.is_empty() {
        request["BootstrapActions"] = json!((1..=row.scripts.len())
            .map(|position| json!({
                "Name": format!("action-{position}"),
                "ScriptBootstrapAction": {
                    "Path": format!("s3://{BUCKET}/{}", script_key(ordinal, position)),
                },
            }))
            .collect::<Vec<Value>>());
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

/// Poll `DescribeCluster` until the cluster has SETTLED — ready, or in a state it never leaves.
async fn settled(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let cluster = emr(
            router,
            case,
            "DescribeCluster",
            json!({ "ClusterId": cluster_id }),
        )
        .await["Cluster"]
            .clone();
        let state = cluster["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if READY_STATES.contains(&state.as_str()) || TERMINAL_STATES.contains(&state.as_str()) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never settled, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The Amazon EC2 instance ids of every node of the cluster, as `ListInstances` reports them.
async fn node_ids(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Vec<String> {
    let listed = emr(
        router,
        case,
        "ListInstances",
        json!({ "ClusterId": cluster_id }),
    )
    .await;
    let instances = listed["Instances"].as_array().cloned().unwrap_or_default();
    let ids: Vec<String> = instances
        .iter()
        .filter_map(|instance| instance["Ec2InstanceId"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        ids.len(),
        NODES,
        "case {case:?}: expected {NODES} nodes back, got {listed}"
    );
    ids
}

/// The node one key's file belongs to: the segment after `node/`.
fn node_of(key: &str) -> Option<&str> {
    key.split_once(&format!("{NODE_SEGMENT}/"))
        .and_then(|(_, rest)| rest.split('/').next())
}

/// Poll the listing under `prefix` until every node of the cluster has left its one
/// instance-state file and `nodes_running` of them have left a directory of [`RUN_FILES`] per
/// action, answering the whole set.
///
/// The wait is for the WHOLE set rather than for the first key to appear: a node's files are
/// written one object at a time, so a listing taken mid-delivery holds some of them. The
/// instance-state file carries the instant it reports in its NAME, so it is matched by its
/// directory rather than spelled out — and WHICH nodes ran an action that took their cluster
/// down is not fixed, so they are counted rather than named.
async fn delivered_keys(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    actions: usize,
    nodes_running: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, prefix).await;
        let states = keys
            .iter()
            .filter(|key| key.contains(INSTANCE_STATE_SEGMENT))
            .count();
        let runs: Vec<&String> = keys
            .iter()
            .filter(|key| key.contains(BOOTSTRAP_ACTIONS_SEGMENT))
            .collect();
        let ran: BTreeSet<&str> = runs.iter().filter_map(|key| node_of(key)).collect();
        if states == NODES
            && ran.len() == nodes_running
            && runs.len() == nodes_running * actions * RUN_FILES.len()
        {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one instance-state file for each of {NODES} nodes and \
             {actions} action directory(ies) on {nodes_running} of them under {prefix}, got \
             {keys:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE, over every row: each node of a cluster launched with a `LogUri` leaves its own
/// instance-state file and one directory per bootstrap action it ran, holding what the script
/// really wrote — and a cluster launched without one leaves nothing anywhere.
#[tokio::test]
async fn every_node_of_a_logged_cluster_leaves_its_own_log_files() {
    let router = emulator();
    seed(&router).await;
    let rows = rows();

    // Every row's cluster is launched first, so the phases the clock owes them run together.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        launched.push(launch(&router, ordinal, row).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let state = settled(&router, case, cluster_id).await;
        match row.comes {
            Comes::Up => assert!(
                READY_STATES.contains(&state.as_str()),
                "case {case:?}: expected the cluster to come up, got {state:?}"
            ),
            Comes::Down => assert_eq!(
                state, "TERMINATED_WITH_ERRORS",
                "case {case:?}: an action that exited non-zero takes the cluster down, got \
                 {state:?}"
            ),
        }

        let nodes = node_ids(&router, case, cluster_id).await;
        let cluster_prefix = format!("{LOG_PREFIX}/{cluster_id}/");
        if !row.logged {
            // Nowhere to write them: the whole cluster's prefix stays empty, however many
            // actions its nodes ran.
            let keys = keys_under(&router, case, &cluster_prefix).await;
            assert!(
                keys.is_empty(),
                "case {case:?}: a cluster that named no LogUri writes no log file, got {keys:?}"
            );
            continue;
        }

        // Every node leaves its own instance-state file; the nodes that RAN an action leave a
        // directory per action beside it. An action that takes its cluster down runs on the
        // first node that got to it and on no other, because the nodes the actions behind it
        // would have run on are going away with the cluster.
        let delivered = delivered_keys(
            &router,
            case,
            &cluster_prefix,
            row.expected.len(),
            row.nodes_running,
        )
        .await;

        for node in &nodes {
            let node_prefix = format!("{cluster_prefix}{NODE_SEGMENT}/{node}");
            // The node's own instance-state file names the node and its cluster.
            let state_key = delivered
                .iter()
                .find(|key| {
                    key.starts_with(&format!("{node_prefix}/{INSTANCE_STATE_SEGMENT}/"))
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: node {node} is owed an instance-state file, got \
                         {delivered:?}"
                    )
                })
                .clone();
            let text = log_text(&router, case, &state_key).await;
            for fragment in [node.as_str(), cluster_id.as_str(), "m5.xlarge"] {
                assert!(
                    text.contains(fragment),
                    "case {case:?}: the instance-state file of {node} must name {fragment:?}, \
                     got {text:?}"
                );
            }
            // And one directory per action it ran, holding what that script wrote. A node that
            // ran none has none.
            let ran = delivered
                .iter()
                .any(|key| key.starts_with(&format!("{node_prefix}/{BOOTSTRAP_ACTIONS_SEGMENT}/")));
            if !ran {
                continue;
            }
            for (index, expected_run) in row.expected.iter().enumerate() {
                let run_prefix =
                    format!("{node_prefix}/{BOOTSTRAP_ACTIONS_SEGMENT}/{}", index + 1);
                let stdout = log_text(&router, case, &format!("{run_prefix}/stdout.gz")).await;
                assert_eq!(
                    stdout, expected_run.stdout,
                    "case {case:?}: node {node} action {}: expected {:?} on standard output, \
                     got {stdout:?}",
                    index + 1,
                    expected_run.stdout
                );
                let stderr = log_text(&router, case, &format!("{run_prefix}/stderr.gz")).await;
                assert_eq!(
                    stderr, expected_run.stderr,
                    "case {case:?}: node {node} action {}: expected {:?} on standard error, \
                     got {stderr:?}",
                    index + 1,
                    expected_run.stderr
                );
                let controller =
                    log_text(&router, case, &format!("{run_prefix}/controller.gz")).await;
                assert!(
                    controller.contains(&expected_run.controller)
                        && controller.contains(node.as_str()),
                    "case {case:?}: node {node} action {}: the controller file must carry {:?} \
                     and name the node, got {controller:?}",
                    index + 1,
                    expected_run.controller
                );
            }
        }
    }
}

/// The delivery is owed ONCE: a client that keeps reporting the cluster afterwards never makes
/// the set of files grow, and the files outlive the cluster the nodes belonged to.
#[tokio::test]
async fn a_nodes_files_are_written_once_and_outlive_its_cluster() {
    let router = emulator();
    seed(&router).await;
    let case = "written once";
    let row = Row {
        case,
        scripts: vec!["echo once\n".to_string()],
        logged: true,
        comes: Comes::Up,
        expected: Vec::new(),
        nodes_running: NODES,
    };
    let cluster_id = launch(&router, 9, &row).await;
    let state = settled(&router, case, &cluster_id).await;
    assert!(
        READY_STATES.contains(&state.as_str()),
        "case {case:?}: expected the cluster to come up, got {state:?}"
    );
    let cluster_prefix = format!("{LOG_PREFIX}/{cluster_id}/");
    // Every node's instance-state file and its one action directory, waited for as a whole
    // set; that set is what must not change afterwards.
    let settled_keys = delivered_keys(&router, case, &cluster_prefix, 1, NODES).await;

    // Reporting the cluster again writes nothing more.
    for round in 0..3 {
        emr(
            &router,
            case,
            "DescribeCluster",
            json!({ "ClusterId": &cluster_id }),
        )
        .await;
        emr(
            &router,
            case,
            "ListInstances",
            json!({ "ClusterId": &cluster_id }),
        )
        .await;
        let after = keys_under(&router, case, &cluster_prefix).await;
        assert_eq!(
            after, settled_keys,
            "case {case:?}: read {round} must answer the same files, expected \
             {settled_keys:?}, got {after:?}"
        );
    }

    // And they outlive the cluster: the caller shuts it down and the files are all still there.
    emr(
        &router,
        case,
        "TerminateJobFlows",
        json!({ "JobFlowIds": [&cluster_id] }),
    )
    .await;
    let ended = settled(&router, case, &cluster_id).await;
    assert_eq!(
        ended, "TERMINATED",
        "case {case:?}: expected the terminated cluster to end TERMINATED, got {ended:?}"
    );
    let after = keys_under(&router, case, &cluster_prefix).await;
    assert_eq!(
        after, settled_keys,
        "case {case:?}: a terminated cluster's node files are still there, expected \
         {settled_keys:?}, got {after:?}"
    );
}
