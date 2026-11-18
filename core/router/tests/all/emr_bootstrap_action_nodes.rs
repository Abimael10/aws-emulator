// SPDX-License-Identifier: CC0-1.0

//! A cluster's BOOTSTRAP ACTIONS run ONCE ON EVERY PROVISIONED NODE, as a program of that
//! node — and a cluster that never became ready ran no step of its queue.
//!
//! The RULES under test:
//!
//! 1. **"A list of bootstrap actions to run before Hadoop starts on the cluster nodes"
//!    (`RunJobFlow$BootstrapActions`) — on the NODES: each action's script is performed once on
//!    every node the launch provisioned (primary and core alike), as a program of that node, so
//!    it reads that node's own identity from the instance metadata service and that node's own
//!    role from `/mnt/var/lib/info/instance.json`, and the per-node effects it leaves are all
//!    there by the time the cluster reports itself ready** (`WAITING` for a kept-alive launch,
//!    with `Status.Timeline.ReadyDateTime` set).
//! 2. **Only a script that really exited non-zero — or one whose object the node could not
//!    fetch — fails the cluster, and then the reason is the modelled `BOOTSTRAP_FAILURE` with
//!    the sentence Amazon EMR words it in, naming the primary node and the action's position.**
//!    A script the emulator performed nothing of is no such outcome: such a cluster stays short
//!    of ready and publishes no failure reason at all.
//! 3. **A step of a cluster that never reported `ReadyDateTime` never ran**: it stays `PENDING`
//!    while the cluster is short of ready, and the cluster's shutdown leaves it `CANCELLED`
//!    with no `Status.Timeline.StartDateTime` — `INTERRUPTED` is for a step that WAS running
//!    when its cluster was cut short.
//!
//! Only a cross-service suite can pin these: IAM holds the instance profile whose role the
//! nodes assume, Amazon EC2 provisions the nodes whose identities the scripts read, Amazon EMR
//! runs the bootstrap phase, and every object a script writes is a real object of Amazon S3's
//! own store — read back through `GetObject` and `ListObjectsV2`, never through anything this
//! suite handed Amazon EMR. It therefore runs against the whole `build_router` service set.
//!
//! The rows differ along what the rules generalise over: how a script reads its node's identity
//! (the IMDSv2 token request and then the read, or a plain read), where it reads its role from
//! (`grep -q` of the role member, `grep -c` of it), how it carries its bytes (a file it wrote,
//! the CLI's stream operand), whether the transfer names a Region, and how the phase ends (every
//! node answering, a script exiting non-zero, a script performed by nothing).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every script of this suite is fetched from and writes to.
const BUCKET: &str = "emr-bootstrap-action-nodes";

/// The service role every launch names ("the IAM role that Amazon EMR assumes in order to
/// access Amazon Web Services resources on your behalf" — `RunJobFlow$ServiceRole`).
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The instance profile every launch names as its `JobFlowRole`, carrying a role the cluster's
/// Amazon EC2 instances may assume — the credential a bootstrap action's own calls present.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";

/// The service principal an Amazon EC2 instance assumes a role as.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The instance type every node of every row is provisioned as.
const INSTANCE_TYPE: &str = "m5.xlarge";

/// How many CORE nodes a row's launch asks for, beside its one primary — so every row has more
/// than one node and "once per node" is a count this suite can tell from "once per cluster".
const CORE_NODES: usize = 2;

/// The modelled `ClusterStateChangeReasonCode` values this suite reads, spelled out here rather
/// than imported from the service crate, so a wrong constant in the implementation cannot make
/// the cases below pass.
const BOOTSTRAP_FAILURE: &str = "BOOTSTRAP_FAILURE";
const USER_REQUEST: &str = "USER_REQUEST";

/// The states a cluster reports once it is up, and the ones it never leaves
/// (`com.amazonaws.emr#ClusterState`).
const READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];
const TERMINAL_STATES: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// How long a poll waits for a cluster to settle: both provisioning phases, every script of
/// every node and the shutdown span, with room to spare. The wait is on the CONDITION — it
/// looks again until the cluster has settled — so a loaded host delays a row instead of
/// breaking it.
const DEADLINE: Duration = Duration::from_secs(240);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
async fn iam(router: &MultiServiceRouter, case: &str, body: &str) {
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

/// A trust policy admitting `principal` and nothing else, in the form an `awsQuery` form body
/// carries it: its quotes and spaces percent-encoded.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
}

/// The bucket, the service role and the instance profile every row shares, each created through
/// the service that owns it.
async fn seed(router: &MultiServiceRouter) {
    let case = "setup";
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    for role in [SERVICE_ROLE, INSTANCE_PROFILE] {
        let principal = if role == SERVICE_ROLE {
            "elasticmapreduce.amazonaws.com"
        } else {
            EC2_PRINCIPAL
        };
        iam(
            router,
            case,
            &format!(
                "Action=CreateRole&Version=2010-05-08&RoleName={role}\
                 &AssumeRolePolicyDocument={}",
                trust_policy(principal)
            ),
        )
        .await;
    }
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

/// A kept-alive launch of one primary and [`CORE_NODES`] core nodes, carrying `actions` and
/// `steps`.
fn launch_request(name: &str, actions: Value, steps: Value) -> Value {
    let mut request = json!({
        "Name": name,
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Hadoop" }],
        "Instances": {
            "InstanceGroups": [
                {
                    "Name": "primary",
                    "InstanceRole": "MASTER",
                    "InstanceType": INSTANCE_TYPE,
                    "InstanceCount": 1,
                },
                {
                    "Name": "core",
                    "InstanceRole": "CORE",
                    "InstanceType": INSTANCE_TYPE,
                    "InstanceCount": CORE_NODES,
                },
            ],
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    });
    if !actions.as_array().is_none_or(Vec::is_empty) {
        request["BootstrapActions"] = actions;
    }
    if !steps.as_array().is_none_or(Vec::is_empty) {
        request["Steps"] = steps;
    }
    request
}

/// One bootstrap action naming the object this suite put `script` in.
fn bootstrap_action(name: &str, key: &str) -> Value {
    json!({
        "Name": name,
        "ScriptBootstrapAction": { "Path": format!("s3://{BUCKET}/{key}") },
    })
}

/// One step a launch carries: a `command-runner.jar` command line that would exit zero.
fn step() -> Value {
    json!([{
        "Name": "queued",
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": "command-runner.jar", "Args": ["bash", "-c", "exit 0"] },
    }])
}

/// Launch one cluster and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let answered = emr_ok(router, case, "RunJobFlow", request).await;
    answered["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: RunJobFlow must answer a JobFlowId, got {answered}"))
        .to_string()
}

/// `DescribeCluster`, as the cluster it reports.
async fn cluster(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Value {
    emr_ok(
        router,
        case,
        "DescribeCluster",
        &json!({ "ClusterId": cluster_id }),
    )
    .await["Cluster"]
        .clone()
}

/// Poll `DescribeCluster` until the cluster reports a state `holds` accepts, and answer it as
/// last read.
async fn cluster_where(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    wanted: &str,
    holds: impl Fn(&str) -> bool,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = cluster(router, case, cluster_id).await;
        let state = reported["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if holds(&state) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never reached {wanted}, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Ec2InstanceId` of every instance of the cluster, and of its primary node alone — which
/// is the node Amazon EMR names in the reason a bootstrap failure carries.
async fn node_ids(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
) -> (BTreeSet<String>, String) {
    let listed = emr_ok(
        router,
        case,
        "ListInstances",
        &json!({ "ClusterId": cluster_id }),
    )
    .await;
    let every: BTreeSet<String> = listed["Instances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|instance| instance["Ec2InstanceId"].as_str().map(str::to_string))
        .collect();
    let primary = emr_ok(
        router,
        case,
        "ListInstances",
        &json!({ "ClusterId": cluster_id, "InstanceGroupTypes": ["MASTER"] }),
    )
    .await;
    let primary_id = primary["Instances"][0]["Ec2InstanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the primary node reports its id, got {primary}"))
        .to_string();
    (every, primary_id)
}

/// Every step `ListSteps` reports for the cluster.
async fn steps(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Vec<Value> {
    emr_ok(
        router,
        case,
        "ListSteps",
        &json!({ "ClusterId": cluster_id }),
    )
    .await["Steps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One row of the per-node table: the script its one action runs, the prefix its objects land
/// under, and what the object of one node must hold.
struct Node {
    case: &'static str,
    script: String,
    prefix: String,
    /// What the object of a node must hold, given that node's id and whether it is the primary.
    held: fn(&str, bool) -> String,
}

fn node_rows() -> Vec<Node> {
    vec![
        Node {
            // The IMDSv2 form: a token request, then the read presenting it.
            case: "the token form of the metadata read, the role from a quiet grep, a file \
                   uploaded with the Region named",
            script: format!(
                "#!/bin/bash\n\
                 set -eu\n\
                 TOKEN=$(curl -s -X PUT \"http://169.254.169.254/latest/api/token\" \\\n\
                 \x20 -H \"X-aws-ec2-metadata-token-ttl-seconds: 120\")\n\
                 IID=$(curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \\\n\
                 \x20 http://169.254.169.254/latest/meta-data/instance-id)\n\
                 ROLE=core\n\
                 if grep -q '\"isMaster\": *true' /mnt/var/lib/info/instance.json; then\n\
                 \x20 ROLE=primary\n\
                 fi\n\
                 printf '%s' \"$ROLE\" > \"/tmp/node-$IID\"\n\
                 aws s3 cp \"/tmp/node-$IID\" \"s3://{BUCKET}/tokened/$IID\" --region {REGION}\n"
            ),
            prefix: "tokened/".to_string(),
            held: |_id, is_master| {
                if is_master { "primary" } else { "core" }.to_string()
            },
        },
        Node {
            // The plain form, the role counted rather than tested, and the bytes piped to the
            // CLI's stream operand instead of written to a file.
            case: "a plain metadata read, the role counted, the bytes piped to the CLI",
            script: format!(
                "IID=$(curl -s http://169.254.169.254/latest/meta-data/instance-id)\n\
                 MASTERS=$(grep -c '\"isMaster\": true' /mnt/var/lib/info/instance.json)\n\
                 echo \"$IID masters $MASTERS\" | aws s3 cp - s3://{BUCKET}/piped/$IID\n"
            ),
            prefix: "piped/".to_string(),
            held: |id, is_master| {
                format!("{id} masters {}\n", if is_master { 1 } else { 0 })
            },
        },
        Node {
            // What the metadata service publishes BESIDE the identity, read by the same client.
            case: "the node's own type read from the metadata service",
            script: format!(
                "#!/bin/bash\n\
                 IID=$(curl -s http://169.254.169.254/latest/meta-data/instance-id)\n\
                 TYPE=$(curl -s http://169.254.169.254/latest/meta-data/instance-type)\n\
                 echo \"$IID $TYPE\" > /tmp/facts\n\
                 aws s3 cp /tmp/facts \"s3://{BUCKET}/typed/$IID\"\n"
            ),
            prefix: "typed/".to_string(),
            held: |id, _is_master| format!("{id} {INSTANCE_TYPE}\n"),
        },
    ]
}

/// RULE 1: every row's action is performed once on every node its launch provisioned, as a
/// program of that node — so the store holds exactly one object per node, each carrying what
/// THAT node read of itself, by the time the cluster reports itself ready.
#[tokio::test]
async fn a_bootstrap_action_runs_once_on_every_provisioned_node() {
    let router = router();
    seed(&router).await;
    let rows = node_rows();

    // Every row is launched before any is polled, so the clusters' provisioning phases run
    // together rather than one after another.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let key = format!("scripts/{ordinal}.sh");
        put_object(&router, row.case, &key, row.script.as_bytes()).await;
        let request = launch_request(
            row.case,
            json!([bootstrap_action(row.case, &key)]),
            json!([]),
        );
        launched.push(launch(&router, row.case, &request).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let reported = cluster_where(&router, case, cluster_id, "a ready state", |state| {
            READY_STATES.contains(&state) || TERMINAL_STATES.contains(&state)
        })
        .await;
        assert_eq!(
            reported["Status"]["State"].as_str(),
            Some("WAITING"),
            "case {case:?}: a kept-alive cluster whose actions all ran comes up waiting, got \
             {reported}"
        );
        assert!(
            reported["Status"]["Timeline"]["ReadyDateTime"].as_f64().is_some(),
            "case {case:?}: a cluster that is up reports when it became ready, got {reported}"
        );
        let (every, primary) = node_ids(&router, case, cluster_id).await;
        assert_eq!(
            every.len(),
            CORE_NODES + 1,
            "case {case:?}: expected {} provisioned nodes, got {every:?}",
            CORE_NODES + 1
        );
        // THE RULE's own instant: the first read that reports the cluster ready is already past
        // every node's run, so every per-node object is there now rather than later.
        for id in &every {
            let key = format!("{}{id}", row.prefix);
            let held = object(&router, &key).await;
            let expected = (row.held)(id, *id == primary);
            assert_eq!(
                held.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: the action ran on node {id} and left {expected:?} at {key}, got \
                 {held:?}"
            );
        }
        // ONCE per node, and on no other node: the keys under the row's prefix are exactly the
        // ids of the nodes the launch provisioned.
        let written = keys_under(&router, case, &row.prefix).await;
        let wanted: BTreeSet<String> = every.iter().map(|id| format!("{}{id}", row.prefix)).collect();
        assert_eq!(
            written, wanted,
            "case {case:?}: expected one object per provisioned node, got {written:?}"
        );
    }
}

/// RULES 2 and 3: a script that really exits non-zero fails its cluster in Amazon EMR's own
/// wording, a script the emulator performed nothing of fails it in no wording at all, and the
/// step queue of either cluster — a cluster that never reported `ReadyDateTime` — ran nothing:
/// its step is `CANCELLED` with no start, never `INTERRUPTED`.
///
/// The control cluster is what makes the second half of this a real test rather than a race: it
/// carries no bootstrap action and is launched in the same breath as the others, so the instant
/// IT reports itself ready is an instant by which every other row's own ready instant has come
/// and gone — and their steps are still `PENDING`, because their clusters never became ready.
#[tokio::test]
async fn a_step_of_a_cluster_that_never_became_ready_is_cancelled() {
    let router = router();
    seed(&router).await;

    // A script that writes its object and THEN exits non-zero: the write really happened, and
    // the cluster still goes down for the status the script exited with.
    let failing = format!(
        "#!/bin/bash\n\
         IID=$(curl -s http://169.254.169.254/latest/meta-data/instance-id)\n\
         echo -n ran > /tmp/marker\n\
         aws s3 cp /tmp/marker \"s3://{BUCKET}/failed/$IID\"\n\
         exit 17\n"
    );
    // A script whose commands this emulator provides no program for: nothing of it is
    // performed, so nothing is known about what it would have come to.
    let unperformed = "#!/bin/bash\njq --raw-output .isMaster /mnt/var/lib/info/instance.json\n";

    put_object(&router, "setup", "scripts/failing.sh", failing.as_bytes()).await;
    put_object(
        &router,
        "setup",
        "scripts/unperformed.sh",
        unperformed.as_bytes(),
    )
    .await;

    let failed_id = launch(
        &router,
        "a script that exits non-zero",
        &launch_request(
            "exits-non-zero",
            json!([bootstrap_action("failing", "scripts/failing.sh")]),
            step(),
        ),
    )
    .await;
    let unperformed_id = launch(
        &router,
        "a script nothing performed",
        &launch_request(
            "performed-by-nothing",
            json!([bootstrap_action("unperformed", "scripts/unperformed.sh")]),
            step(),
        ),
    )
    .await;
    let control_id = launch(
        &router,
        "the control cluster",
        &launch_request("no-actions", json!([]), json!([])),
    )
    .await;

    // Rule 2's own half: the failing cluster goes down for the modelled reason, worded as
    // Amazon EMR words it, naming the primary node and the action's position.
    let case = "a script that exits non-zero";
    let reported = cluster_where(&router, case, &failed_id, "a terminal state", |state| {
        TERMINAL_STATES.contains(&state)
    })
    .await;
    assert_eq!(
        reported["Status"]["State"].as_str(),
        Some("TERMINATED_WITH_ERRORS"),
        "case {case:?}: a cluster whose bootstrap action failed ends in the modelled failure \
         state, got {reported}"
    );
    assert_eq!(
        reported["Status"]["StateChangeReason"]["Code"].as_str(),
        Some(BOOTSTRAP_FAILURE),
        "case {case:?}: the shutdown names the modelled reason, got {reported}"
    );
    let (_, primary) = node_ids(&router, case, &failed_id).await;
    let expected = format!(
        "On the master instance ({primary}), bootstrap action 1 returned a non-zero return code"
    );
    assert_eq!(
        reported["Status"]["StateChangeReason"]["Message"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the reason {expected:?}, got {reported}"
    );
    assert!(
        reported["Status"]["Timeline"]["ReadyDateTime"].is_null(),
        "case {case:?}: a cluster that never came up reports no ready instant, got {reported}"
    );
    // What the script DID before it failed is real, and it ran as a program of ONE node: the
    // first node whose turn came, which is the only one that ran before the cluster went down.
    let written = keys_under(&router, case, "failed/").await;
    let ran_on: BTreeSet<String> = written
        .iter()
        .filter_map(|key| key.strip_prefix("failed/").map(str::to_string))
        .collect();
    let (every, _) = node_ids(&router, case, &failed_id).await;
    assert_eq!(
        ran_on.len(),
        1,
        "case {case:?}: the failing action ran on one node before the cluster went down, got \
         {written:?} for the nodes {every:?}"
    );
    assert!(
        ran_on.is_subset(&every),
        "case {case:?}: the object the script wrote is keyed by the node it ran on, got \
         {written:?} for the nodes {every:?}"
    );
    for key in &written {
        let held = object(&router, key).await;
        assert_eq!(
            held.as_deref(),
            Some("ran"),
            "case {case:?}: the failing script's own write happened, got {held:?} at {key}"
        );
    }
    // Rule 3: the step of that cluster never ran.
    for reported in steps(&router, case, &failed_id).await {
        assert_eq!(
            reported["Status"]["State"].as_str(),
            Some("CANCELLED"),
            "case {case:?}: a step of a cluster that never became ready never ran, got {reported}"
        );
        assert!(
            reported["Status"]["Timeline"]["StartDateTime"].is_null(),
            "case {case:?}: a step that never ran reports no start, got {reported}"
        );
    }

    // The control cluster's ready instant is the clock the rest of this case reads: once it is
    // up, every cluster launched beside it is past its own ready instant too.
    let case = "the control cluster";
    let control = cluster_where(&router, case, &control_id, "a ready state", |state| {
        READY_STATES.contains(&state) || TERMINAL_STATES.contains(&state)
    })
    .await;
    assert_eq!(
        control["Status"]["State"].as_str(),
        Some("WAITING"),
        "case {case:?}: a kept-alive cluster that carried no action comes up waiting, got {control}"
    );

    // Rule 2's other half: a script nothing performed is no AWS outcome, so the cluster
    // publishes no reason at all — and it is not ready either.
    let case = "a script nothing performed";
    let reported = cluster(&router, case, &unperformed_id).await;
    assert_eq!(
        reported["Status"]["State"].as_str(),
        Some("BOOTSTRAPPING"),
        "case {case:?}: a cluster whose script nothing performed is neither ready nor failed, \
         got {reported}"
    );
    assert!(
        reported["Status"]["StateChangeReason"]["Code"].is_null()
            && reported["Status"]["StateChangeReason"]["Message"].is_null(),
        "case {case:?}: no reason is published for an outcome AWS has no wording for, got \
         {reported}"
    );
    assert!(
        reported["Status"]["Timeline"]["ReadyDateTime"].is_null(),
        "case {case:?}: such a cluster never became ready, got {reported}"
    );
    // Rule 3, before any shutdown: the step is still queued, because its cluster never became
    // ready — the bootstrap phase's own instant has passed by now (the control cluster is up).
    for reported in steps(&router, case, &unperformed_id).await {
        assert_eq!(
            reported["Status"]["State"].as_str(),
            Some("PENDING"),
            "case {case:?}: a step waits for a cluster that is not ready, got {reported}"
        );
        assert!(
            reported["Status"]["Timeline"]["StartDateTime"].is_null(),
            "case {case:?}: a step that has not started reports no start, got {reported}"
        );
    }

    // ... and the caller's own shutdown of it leaves that step CANCELLED, never INTERRUPTED.
    emr_ok(
        &router,
        case,
        "TerminateJobFlows",
        &json!({ "JobFlowIds": [&unperformed_id] }),
    )
    .await;
    let reported = cluster_where(&router, case, &unperformed_id, "a terminal state", |state| {
        TERMINAL_STATES.contains(&state)
    })
    .await;
    assert_eq!(
        reported["Status"]["State"].as_str(),
        Some("TERMINATED"),
        "case {case:?}: a cluster the caller shut down ends TERMINATED, got {reported}"
    );
    assert_eq!(
        reported["Status"]["StateChangeReason"]["Code"].as_str(),
        Some(USER_REQUEST),
        "case {case:?}: the shutdown names the caller's own reason, got {reported}"
    );
    for reported in steps(&router, case, &unperformed_id).await {
        assert_eq!(
            reported["Status"]["State"].as_str(),
            Some("CANCELLED"),
            "case {case:?}: a step that never ran is cancelled by its cluster's shutdown, got \
             {reported}"
        );
        assert!(
            reported["Status"]["Timeline"]["StartDateTime"].is_null(),
            "case {case:?}: a cancelled step that never ran reports no start, got {reported}"
        );
    }
}
