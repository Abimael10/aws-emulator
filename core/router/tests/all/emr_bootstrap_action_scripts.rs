// SPDX-License-Identifier: CC0-1.0

//! A cluster's BOOTSTRAP ACTIONS really run: each script is fetched from Amazon S3 and
//! performed before the cluster reports itself ready.
//!
//! The RULE under test: **"a list of bootstrap actions to run before Hadoop starts on the
//! cluster nodes" (`RunJobFlow$BootstrapActions`), each naming "[the] location in Amazon S3 of
//! the script to run during a bootstrap action" (`ScriptBootstrapActionConfig$Path`) — so a
//! cluster launched with bootstrap actions fetches each script from that location and RUNS it,
//! in launch order, and the script's own effects on Amazon S3 are in place by the time the
//! cluster reports `RUNNING`/`WAITING`. A script that exits non-zero takes the cluster to
//! `TERMINATED_WITH_ERRORS` with the modelled `BOOTSTRAP_FAILURE` state-change reason, and the
//! actions behind it never run — never a cluster reported ready for a script nothing
//! performed, and never a cluster left indefinitely short of ready.**
//!
//! Only a cross-service suite can pin this: IAM holds the instance profile whose role the
//! cluster's nodes assume, Amazon EMR runs the bootstrap phase, and every script and every
//! object a script writes is a real object of Amazon S3's own store — put there through
//! `PutObject` and read back through `GetObject`, never through anything this suite handed
//! Amazon EMR. It therefore runs against the WHOLE `build_router` service set, as the binary
//! serves it.
//!
//! The rows differ along what the rule generalises over: how many actions a launch carried
//! (none, one, three), WHAT a script does (writes a file and uploads it, pipes its bytes to
//! the CLI's stream operand, downloads an object and writes it back, reads back what an
//! earlier action of the same launch wrote), whether the script carries a shebang line, and
//! how it fails (a non-zero exit, a script the store holds nothing under).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every script of this suite is fetched from and writes to.
const BUCKET: &str = "emr-bootstrap-action-scripts";

/// The object the row that READS one reads, and the bytes it holds.
const SEED_KEY: &str = "in/seed.txt";
const SEED_BYTES: &str = "seeded bytes\n";

/// The service role every launch names — "the IAM role that Amazon EMR assumes in order to
/// access Amazon Web Services resources on your behalf" (`RunJobFlow$ServiceRole`).
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The instance profile every launch names as its `JobFlowRole`, carrying a role the cluster's
/// Amazon EC2 instances may assume — which is the credential a bootstrap action's own calls
/// present.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";

/// The service principal an Amazon EC2 instance assumes a role as.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The modelled `ClusterStateChangeReasonCode` a bootstrap failure carries, spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const BOOTSTRAP_FAILURE: &str = "BOOTSTRAP_FAILURE";

/// The states a cluster reports once it is up.
const READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];

/// The states a cluster never leaves (`com.amazonaws.emr#ClusterState`).
const TERMINAL_STATES: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// How long a poll waits for a cluster to settle: both provisioning phases, every script of
/// the longest row and the shutdown span, with room to spare. The wait is on the CONDITION —
/// it looks again until the cluster has settled — so a loaded host delays a row instead of
/// breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

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
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
}

/// The bucket, the seeded object, the service role and the instance profile every row shares,
/// each created through the service that owns it.
async fn seed(router: &MultiServiceRouter) {
    let case = "setup";
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    put_object(router, case, SEED_KEY, SEED_BYTES.as_bytes()).await;
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

/// What a row expects of its cluster.
enum Comes {
    /// Up: it reports itself ready, and by then every object its scripts wrote is there.
    Up,
    /// Down for a bootstrap failure, on the action at this position (counted from one), which
    /// ran and exited non-zero.
    DownOnExit(usize),
    /// Down for a bootstrap failure, on the action at this position, whose script the node
    /// could not fetch.
    DownOnFetch(usize),
}

/// One row: the scripts its launch carries, what the cluster comes to, and what the store must
/// hold afterwards.
struct Row {
    case: &'static str,
    /// One entry per bootstrap action, in launch order: the script's own text, or `None` for an
    /// action whose `Path` names an object nothing ever put there.
    scripts: Vec<Option<String>>,
    comes: Comes,
    /// `(key, contents)` the store must hold once the cluster has settled.
    holds: Vec<(String, String)>,
    /// Keys the store must hold NOTHING under once the cluster has settled.
    holds_nothing: Vec<String>,
}

/// The key one row's action writes its marker under.
fn marker_key(ordinal: usize, position: usize) -> String {
    format!("markers/{ordinal}-{position}.txt")
}

/// The key one row's script is put under.
fn script_key(ordinal: usize, position: usize) -> String {
    format!("scripts/{ordinal}-{position}.sh")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a launch that carried no bootstrap action at all",
            scripts: Vec::new(),
            comes: Comes::Up,
            holds: Vec::new(),
            holds_nothing: Vec::new(),
        },
        Row {
            case: "one action that writes a file and uploads it",
            scripts: vec![Some(format!(
                "#!/bin/bash\n\
                 echo -n bootstrap-executed > /tmp/marker.txt\n\
                 aws s3 cp /tmp/marker.txt s3://{BUCKET}/{}\n",
                marker_key(1, 1)
            ))],
            comes: Comes::Up,
            holds: vec![(marker_key(1, 1), "bootstrap-executed".to_string())],
            holds_nothing: Vec::new(),
        },
        Row {
            case: "one action that pipes its bytes to the CLI's stream operand",
            scripts: vec![Some(format!(
                "echo piped-through-the-cli | aws s3 cp - s3://{BUCKET}/{}\n",
                marker_key(2, 1)
            ))],
            comes: Comes::Up,
            holds: vec![(marker_key(2, 1), "piped-through-the-cli\n".to_string())],
            holds_nothing: Vec::new(),
        },
        Row {
            case: "one action that downloads an object and writes it back",
            scripts: vec![Some(format!(
                "#!/bin/bash\n\
                 aws s3 cp s3://{BUCKET}/{SEED_KEY} /tmp/seed.txt\n\
                 cat /tmp/seed.txt > /tmp/again.txt\n\
                 aws s3 cp /tmp/again.txt s3://{BUCKET}/{}\n",
                marker_key(3, 1)
            ))],
            comes: Comes::Up,
            holds: vec![(marker_key(3, 1), SEED_BYTES.to_string())],
            holds_nothing: Vec::new(),
        },
        Row {
            // The third action reads back what the FIRST one wrote, so it can only succeed if
            // the actions ran in launch order.
            case: "three actions, the last reading back what the first wrote",
            scripts: vec![
                Some(format!(
                    "echo -n first-ran > /tmp/one.txt\n\
                     aws s3 cp /tmp/one.txt s3://{BUCKET}/{}\n",
                    marker_key(4, 1)
                )),
                Some(format!(
                    "echo -n second-ran > /tmp/two.txt\n\
                     aws s3 cp /tmp/two.txt s3://{BUCKET}/{}\n",
                    marker_key(4, 2)
                )),
                Some(format!(
                    "#!/bin/bash\n\
                     aws s3 cp s3://{BUCKET}/{} /tmp/three.txt\n\
                     aws s3 cp /tmp/three.txt s3://{BUCKET}/{}\n",
                    marker_key(4, 1),
                    marker_key(4, 3)
                )),
            ],
            comes: Comes::Up,
            holds: vec![
                (marker_key(4, 1), "first-ran".to_string()),
                (marker_key(4, 2), "second-ran".to_string()),
                (marker_key(4, 3), "first-ran".to_string()),
            ],
            holds_nothing: Vec::new(),
        },
        Row {
            // The script writes its object and THEN exits non-zero: the write really happened,
            // and the cluster still goes down for the status the script exited with.
            case: "a script that writes its object and then exits non-zero",
            scripts: vec![
                Some(format!(
                    "echo -n wrote-then-failed > /tmp/marker.txt\n\
                     aws s3 cp /tmp/marker.txt s3://{BUCKET}/{}\n\
                     exit 3\n",
                    marker_key(5, 1)
                )),
                Some(format!(
                    "echo -n never-ran > /tmp/marker.txt\n\
                     aws s3 cp /tmp/marker.txt s3://{BUCKET}/{}\n",
                    marker_key(5, 2)
                )),
            ],
            comes: Comes::DownOnExit(1),
            holds: vec![(marker_key(5, 1), "wrote-then-failed".to_string())],
            // The action behind the failing one never runs.
            holds_nothing: vec![marker_key(5, 2)],
        },
        Row {
            case: "a second action whose script the store holds nothing under",
            scripts: vec![
                Some(format!(
                    "echo -n first-ran > /tmp/marker.txt\n\
                     aws s3 cp /tmp/marker.txt s3://{BUCKET}/{}\n",
                    marker_key(6, 1)
                )),
                None,
            ],
            comes: Comes::DownOnFetch(2),
            holds: vec![(marker_key(6, 1), "first-ran".to_string())],
            holds_nothing: Vec::new(),
        },
    ]
}

/// The `BootstrapActions` one row's launch carries: one entry per script, each naming the
/// object this suite put that script in.
fn bootstrap_actions(ordinal: usize, row: &Row) -> Value {
    let actions: Vec<Value> = (1..=row.scripts.len())
        .map(|position| {
            json!({
                "Name": format!("{}-{position}", row.case),
                "ScriptBootstrapAction": {
                    "Path": format!("s3://{BUCKET}/{}", script_key(ordinal, position)),
                },
            })
        })
        .collect();
    json!(actions)
}

/// Launch one row's cluster, after putting every script its actions name in the store.
async fn launch(router: &MultiServiceRouter, ordinal: usize, row: &Row) -> String {
    for (index, script) in row.scripts.iter().enumerate() {
        if let Some(script) = script {
            put_object(
                router,
                row.case,
                &script_key(ordinal, index + 1),
                script.as_bytes(),
            )
            .await;
        }
    }
    let mut request = json!({
        "Name": format!("bootstrap-{ordinal}"),
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Hadoop" }],
        "Instances": {
            "InstanceGroups": [{
                "Name": "primary",
                "InstanceRole": "MASTER",
                "InstanceType": "m5.xlarge",
                "InstanceCount": 1,
            }],
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    });
    if !row.scripts.is_empty() {
        request["BootstrapActions"] = bootstrap_actions(ordinal, row);
    }
    let answered = emr_ok(router, row.case, "RunJobFlow", &request).await;
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

/// Poll `DescribeCluster` until the cluster has SETTLED — it reports itself ready or it has
/// reached a state it never leaves — and answer it as last read.
async fn settled(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let cluster = emr_ok(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id }),
        )
        .await["Cluster"]
            .clone();
        let state = cluster["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if READY_STATES.contains(&state.as_str()) || TERMINAL_STATES.contains(&state.as_str()) {
            return cluster;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never settled, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The id of the cluster's primary node, as `ListInstances` reports it — which is the node
/// Amazon EMR names in the reason a bootstrap failure carries.
async fn primary_instance_id(router: &MultiServiceRouter, case: &str, cluster_id: &str) -> String {
    let listed = emr_ok(
        router,
        case,
        "ListInstances",
        &json!({ "ClusterId": cluster_id, "InstanceGroupTypes": ["MASTER"] }),
    )
    .await;
    let instances = listed["Instances"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: expected the one primary node back, got {listed}"
    );
    instances[0]["Ec2InstanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the primary node reports its id, got {listed}"))
        .to_string()
}

/// Every row: the cluster runs each script its launch named before it reports itself ready, and
/// a script that did not succeed takes the cluster down for the modelled reason.
#[tokio::test]
async fn a_launchs_bootstrap_scripts_run_before_the_cluster_is_ready() {
    let router = router();
    let rows = rows();

    seed(&router).await;

    // Every row is launched before any is polled, so the clusters' provisioning phases run
    // together rather than one after another.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        launched.push(launch(&router, ordinal, row).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let cluster = settled(&router, case, cluster_id).await;
        let state = cluster["Status"]["State"].as_str().unwrap_or_default();
        match row.comes {
            Comes::Up => {
                assert!(
                    READY_STATES.contains(&state),
                    "case {case:?}: expected the cluster to come up, got {cluster}"
                );
                // THE RULE's own instant: the FIRST read that reports the cluster ready is
                // already past every one of its scripts, so each object a script wrote is
                // there now rather than later.
                for (key, expected) in &row.holds {
                    let held = object(&router, key).await;
                    assert_eq!(
                        held.as_deref(),
                        Some(expected.as_str()),
                        "case {case:?}: a cluster that reports itself ready has run its \
                         scripts, so {key} holds {expected:?} by now, got {held:?}"
                    );
                }
                assert!(
                    cluster["Status"]["Timeline"]["ReadyDateTime"]
                        .as_f64()
                        .is_some(),
                    "case {case:?}: a cluster that is up reports when it became ready, got \
                     {cluster}"
                );
            }
            Comes::DownOnExit(position) | Comes::DownOnFetch(position) => {
                assert_eq!(
                    state, "TERMINATED_WITH_ERRORS",
                    "case {case:?}: a cluster whose bootstrap action did not succeed ends in \
                     the modelled failure state, got {cluster}"
                );
                assert_eq!(
                    cluster["Status"]["StateChangeReason"]["Code"].as_str(),
                    Some(BOOTSTRAP_FAILURE),
                    "case {case:?}: the shutdown names the modelled reason, got {cluster}"
                );
                let node = primary_instance_id(&router, case, cluster_id).await;
                let expected = match row.comes {
                    Comes::DownOnExit(_) => format!(
                        "On the master instance ({node}), bootstrap action {position} returned \
                         a non-zero return code"
                    ),
                    _ => format!(
                        "Master instance ({node}) failed attempting to download bootstrap \
                         action {position} file from S3"
                    ),
                };
                assert_eq!(
                    cluster["Status"]["StateChangeReason"]["Message"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the reason {expected:?}, got {cluster}"
                );
                assert!(
                    cluster["Status"]["Timeline"]["ReadyDateTime"].is_null(),
                    "case {case:?}: a cluster that never came up reports no ready instant, got \
                     {cluster}"
                );
                // The effects the scripts that DID run left are real, and the ones behind the
                // failing action left none.
                for (key, expected) in &row.holds {
                    let held = object(&router, key).await;
                    assert_eq!(
                        held.as_deref(),
                        Some(expected.as_str()),
                        "case {case:?}: expected {key} to hold {expected:?}, got {held:?}"
                    );
                }
            }
        }
        for key in &row.holds_nothing {
            let held = object(&router, key).await;
            assert_eq!(
                held, None,
                "case {case:?}: expected the store to hold nothing at {key}, got {held:?}"
            );
        }
    }
}
