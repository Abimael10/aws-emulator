// SPDX-License-Identifier: CC0-1.0

//! A step's command line runs the cluster's HADOOP commands, over the cluster's own
//! filesystems, end to end over the wire.
//!
//! THE RULE: "command-runner.jar … lets you run commands on your cluster" (Amazon EMR
//! documentation) and "for a step to be considered complete, the main function must exit with a
//! zero exit code" (`AddJobFlowSteps` doc), so the commands the cluster's installed Hadoop
//! distribution ships — `hadoop fs` / `hdfs dfs` over the filesystems a path's scheme names,
//! `hdfs getconf` over the configuration Hadoop resolves on the nodes, and `hadoop version` —
//! are commands of that line, and what they do is the step's own work. The filesystems they
//! work over OUTLIVE the one program that wrote to them: what a bootstrap action left on a
//! node's local disk the first step reads, and what one step wrote into the cluster's HDFS the
//! next step reads. A command line this emulator performs nothing of keeps the failure that
//! says its work was not run, and a command the filesystem refused exits non-zero and FAILS its
//! step.
//!
//! The rows span what the rule generalises over: the FILESYSTEM a path names (the cluster's
//! HDFS under `hdfs://` and under the default filesystem, Amazon S3 under `s3://` through
//! EMRFS, the node's own local files), the command (`-mkdir`, `-put`, `-cat`, `-rm`, `-du`,
//! `-stat`, `-test`, `getconf`, `version`), where the bytes come from (a file of the node, the
//! command's own standard input, another filesystem), whether the path exists, and a form of
//! the filesystem shell the node performs nothing of.
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine, every object a row asserts is read
//! back through `GetObject`, and everything a step wrote into HDFS or onto a node is only ever
//! read back THROUGH ANOTHER STEP — so what is asserted is what the cluster really holds rather
//! than anything this suite handed Amazon EMR.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::STEP_NOT_EXECUTED_REASON;
use smithy_server_runtime::MultiServiceRouter;

/// The Region both services are addressed in: the one whose `CreateBucket` needs no location
/// constraint in its body.
pub(crate) const REGION: &str = "us-east-1";

/// The bucket every row reads from and writes to.
pub(crate) const BUCKET: &str = "emr-node-hadoop-filesystem";

/// The release every cluster of this suite runs, and the version of Hadoop its catalog ships —
/// spelled out here rather than imported, so a wrong version in the implementation cannot make
/// the version row pass.
pub(crate) const RELEASE_LABEL: &str = "emr-6.15.0";
const HADOOP_VERSION: &str = "3.3.6";

/// The class `fs.s3.impl` resolves to on an Amazon EMR cluster: EMRFS.
const EMRFS_CLASS: &str = "com.amazon.ws.emr.hadoop.fs.EmrFileSystem";

/// The roles a launch names.
pub(crate) const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
pub(crate) const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// How long a poll waits for a step or a cluster to settle: both provisioning phases, the
/// bootstrap phase, every step's own span and one command's transfers, with room to spare.
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll sleeps between reads.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
pub(crate) const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// Amazon EMR and Amazon S3 on one engine, the shape the router mounts them in.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One `awsJson1_1` Amazon EMR call that must be served.
pub(crate) async fn emr_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
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
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261118/{REGION}/\
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
        "{operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One path-style Amazon S3 request.
pub(crate) async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261118/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    (parts.status.as_u16(), answered.to_vec())
}

/// Create the bucket every row uses.
pub(crate) async fn create_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Put `body` at `key`, which must be served.
pub(crate) async fn put_object(router: &MultiServiceRouter, key: &str, body: &[u8]) {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "PutObject {key} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The object at `key`, or `None` when the store holds none.
pub(crate) async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// A command line wrapped in a shell program, which is the form the CLI's `--steps` writes.
pub(crate) fn shell(program: &str) -> Vec<String> {
    vec!["bash".to_string(), "-c".to_string(), program.to_string()]
}

/// The `StepConfig` a row submits.
pub(crate) fn step_config(name: &str, args: Vec<String>) -> Value {
    json!({
        "Name": name,
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
    })
}

/// A cluster launched with `steps`, kept alive so each step's own end is what is read.
pub(crate) async fn launch(router: &MultiServiceRouter, name: &str, request: Value) -> String {
    let launched = emr_ok(router, "RunJobFlow", request).await;
    launched["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: RunJobFlow answers a JobFlowId"))
        .to_string()
}

/// The launch request every cluster of this suite is built from, carrying `steps`.
pub(crate) fn launch_request(name: &str, steps: Value) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": RELEASE_LABEL,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Hadoop" }],
        "Steps": steps,
        "Instances": {
            "InstanceCount": 1,
            "MasterInstanceType": "m5.xlarge",
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    })
}

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering them in submission
/// order.
pub(crate) async fn drained(
    case: &str,
    router: &MultiServiceRouter,
    cluster_id: &str,
) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let listed = emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
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
        if !states.is_empty() && states.iter().all(|state| TERMINAL.contains(&state.as_str())) {
            return steps;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a row expects its step to end as.
enum Ends {
    /// Every command of the line was performed and exited zero.
    Completed,
    /// A command of the line exited non-zero: the step failed.
    Failed,
    /// The line carries an invocation this emulator performs nothing of: the step keeps the
    /// failure that says its work was not run.
    NotExecuted,
}

/// One row: the command line the step submits, the end it must reach, and what the object store
/// must hold afterwards.
struct Case {
    label: &'static str,
    program: String,
    ends: Ends,
    /// `(key, the text the object must hold once the step has ended)`.
    holds: Vec<(String, String)>,
    /// Keys the store must hold nothing under once the step has ended.
    holds_nothing: Vec<String>,
}

/// The objects this suite seeds before any cluster is launched: `(key, contents)`.
const SEEDED: [(&str, &str); 2] = [
    ("in/seed.txt", "seeded bytes\n"),
    ("in/removed.txt", "removed bytes\n"),
];

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the EMRFS implementation the cluster's configuration resolves",
            program: format!(
                "set -eu; hdfs getconf -confKey fs.s3.impl > /tmp/impl; \
                 aws s3 cp /tmp/impl s3://{BUCKET}/out/impl.txt"
            ),
            ends: Ends::Completed,
            holds: vec![("out/impl.txt".to_string(), format!("{EMRFS_CLASS}\n"))],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "an EMRFS write of a node file, read back in the same step",
            program: format!(
                "set -eu; printf %s 'emrfs-payload' > /tmp/payload; \
                 hadoop fs -mkdir -p s3://{BUCKET}/emrfs; \
                 hadoop fs -put /tmp/payload s3://{BUCKET}/emrfs/payload.txt; \
                 hadoop fs -cat s3://{BUCKET}/emrfs/payload.txt > /tmp/read; \
                 aws s3 cp /tmp/read s3://{BUCKET}/out/emrfs-read.txt"
            ),
            ends: Ends::Completed,
            holds: vec![
                (
                    "emrfs/payload.txt".to_string(),
                    "emrfs-payload".to_string(),
                ),
                (
                    "out/emrfs-read.txt".to_string(),
                    "emrfs-payload\n".to_string(),
                ),
            ],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "an HDFS write from the command's own standard input, read back by path",
            program: format!(
                "set -eu; printf %s 'hdfs-payload' | hadoop fs -put - hdfs:///sim2real/one.txt; \
                 hadoop fs -cat /sim2real/one.txt | aws s3 cp - s3://{BUCKET}/out/hdfs-read.txt"
            ),
            ends: Ends::Completed,
            holds: vec![(
                "out/hdfs-read.txt".to_string(),
                "hdfs-payload\n".to_string(),
            )],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "an object carried into HDFS and back out through the filesystem shell",
            program: format!(
                "set -eu; hadoop fs -mkdir -p hdfs:///landing; \
                 hadoop fs -cp s3://{BUCKET}/in/seed.txt hdfs:///landing/seed.txt; \
                 hadoop fs -cp hdfs:///landing/seed.txt s3://{BUCKET}/out/landed.txt"
            ),
            ends: Ends::Completed,
            holds: vec![("out/landed.txt".to_string(), "seeded bytes\n".to_string())],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "the replication the cluster keeps, reported by the file and by the \
                    configuration",
            program: format!(
                "set -eu; printf %s 'r' | hadoop fs -put - hdfs:///rep/one.txt; \
                 hadoop fs -stat '%r' hdfs:///rep/one.txt > /tmp/stat; \
                 hdfs getconf -confKey dfs.replication >> /tmp/stat; \
                 aws s3 cp /tmp/stat s3://{BUCKET}/out/replication.txt"
            ),
            ends: Ends::Completed,
            // One node, so the published band keeps one copy of each block — and the file's own
            // replication and the resolved configuration agree, which is the point of the row.
            holds: vec![("out/replication.txt".to_string(), "1\n1\n".to_string())],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "the version of Hadoop the release installed on the node",
            program: format!(
                "set -eu; hadoop version | head -n 1 | cut -d' ' -f2 > /tmp/version; \
                 aws s3 cp /tmp/version s3://{BUCKET}/out/version.txt"
            ),
            ends: Ends::Completed,
            holds: vec![(
                "out/version.txt".to_string(),
                format!("{HADOOP_VERSION}\n"),
            )],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "the bytes an object holds, as the filesystem shell reports them",
            program: format!(
                "set -eu; hadoop fs -du -s s3://{BUCKET}/in/seed.txt | cut -d' ' -f1 \
                 > /tmp/du; aws s3 cp /tmp/du s3://{BUCKET}/out/du.txt"
            ),
            ends: Ends::Completed,
            holds: vec![(
                "out/du.txt".to_string(),
                format!("{}\n", SEEDED[0].1.len()),
            )],
            holds_nothing: Vec::new(),
        },
        Case {
            label: "an object the filesystem shell removes",
            program: format!(
                "set -eu; hadoop fs -test -e s3://{BUCKET}/in/removed.txt; \
                 hadoop fs -rm s3://{BUCKET}/in/removed.txt; \
                 printf %s gone | aws s3 cp - s3://{BUCKET}/out/removed.txt"
            ),
            ends: Ends::Completed,
            holds: vec![("out/removed.txt".to_string(), "gone".to_string())],
            holds_nothing: vec!["in/removed.txt".to_string()],
        },
        Case {
            label: "a path the filesystem holds nothing at",
            program: format!(
                "set -eu; hadoop fs -cat hdfs:///absent/one.txt \
                 > /tmp/read; aws s3 cp /tmp/read s3://{BUCKET}/out/absent.txt"
            ),
            ends: Ends::Failed,
            holds: Vec::new(),
            holds_nothing: vec!["out/absent.txt".to_string()],
        },
        Case {
            label: "a property the cluster's configuration does not carry",
            program: "hdfs getconf -confKey emulator.property.nothing.sets".to_string(),
            ends: Ends::Failed,
            holds: Vec::new(),
            holds_nothing: Vec::new(),
        },
        Case {
            label: "a form of the filesystem shell the node performs nothing of",
            program: format!("hadoop fs -chmod 777 s3://{BUCKET}/in/seed.txt"),
            ends: Ends::NotExecuted,
            holds: Vec::new(),
            holds_nothing: Vec::new(),
        },
    ]
}

/// THE RULE over the whole table: every row's step ends in the status its command line exited
/// with, and the filesystem each command named really carries what it was told to carry.
#[tokio::test]
async fn a_steps_command_line_runs_the_clusters_hadoop_commands() {
    let router = emulator();
    let cases = cases();

    create_bucket(&router).await;
    for (key, contents) in SEEDED {
        put_object(&router, key, contents.as_bytes()).await;
    }

    // One cluster per row, all launched before any is polled, so the rows' clusters come up
    // together.
    let mut launched: Vec<String> = Vec::new();
    for case in &cases {
        let request = launch_request(
            case.label,
            json!([step_config(case.label, shell(&case.program))]),
        );
        launched.push(launch(&router, case.label, request).await);
    }

    for (case, cluster_id) in cases.iter().zip(&launched) {
        let label = case.label;
        let steps = drained(label, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {label:?}: expected the submitted step back"));
        let state = step["Status"]["State"].as_str().unwrap_or_default();
        let reason = step["Status"]["FailureDetails"]["Reason"]
            .as_str()
            .unwrap_or_default();
        match case.ends {
            Ends::Completed => assert_eq!(
                state, "COMPLETED",
                "case {label:?}: a command line that exited zero completes its step, got {step}"
            ),
            Ends::Failed => {
                assert_eq!(
                    state, "FAILED",
                    "case {label:?}: a command that exited non-zero fails its step, got {step}"
                );
                assert_ne!(
                    reason, STEP_NOT_EXECUTED_REASON,
                    "case {label:?}: the command RAN and was refused, so the failure is the \
                     command's own, got {step}"
                );
            }
            Ends::NotExecuted => {
                assert_eq!(
                    state, "FAILED",
                    "case {label:?}: a command line nothing performed fails its step, got {step}"
                );
                assert_eq!(
                    reason, STEP_NOT_EXECUTED_REASON,
                    "case {label:?}: expected the not-executed failure, got {step}"
                );
            }
        }
        for (key, expected) in &case.holds {
            let held = object(&router, key).await;
            assert_eq!(
                held.as_deref(),
                Some(expected.as_str()),
                "case {label:?}: expected {key} to hold {expected:?}, got {held:?}"
            );
        }
        for key in &case.holds_nothing {
            let held = object(&router, key).await;
            assert_eq!(
                held, None,
                "case {label:?}: expected the store to hold nothing at {key}, got {held:?}"
            );
        }
    }
}

/// THE RULE's filesystem half: what one step wrote into the cluster's HDFS the NEXT step reads,
/// so the cluster's filesystem outlives the program that wrote to it.
///
/// The rows are the two ways the second step can name what the first wrote — the `hdfs://`
/// scheme and the default filesystem — and the payloads differ in bytes, so a reader that
/// answered anything but what was written fails.
#[tokio::test]
async fn hdfs_outlives_the_step_that_wrote_it() {
    let router = emulator();
    create_bucket(&router).await;

    // (case, the path the first step writes, the path the second step reads, the payload)
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "read back under the hdfs scheme",
            "hdfs:///boundary/scheme/payload.txt",
            "hdfs:///boundary/scheme/payload.txt",
            "survives-the-boundary",
        ),
        (
            "read back through the default filesystem",
            "hdfs:///boundary/default/payload.txt",
            "/boundary/default/payload.txt",
            "another-payload-entirely",
        ),
    ];

    let mut launched: Vec<(&str, String, String)> = Vec::new();
    for (case, written, read, payload) in rows {
        let output = format!("out/{}", payload);
        let steps = json!([
            step_config(
                "write-to-hdfs",
                shell(&format!(
                    "set -eu; printf %s '{payload}' | hadoop fs -put - {written}"
                )),
            ),
            step_config(
                "read-from-hdfs",
                shell(&format!(
                    "set -eu; hadoop fs -cat {read} | aws s3 cp - s3://{BUCKET}/{output}"
                )),
            ),
        ]);
        let cluster_id = launch(&router, case, launch_request(case, steps)).await;
        launched.push((case, cluster_id, output));
    }

    for ((case, cluster_id, output), (_, _, _, payload)) in launched.iter().zip(rows) {
        let steps = drained(case, &router, cluster_id).await;
        assert_eq!(
            steps.len(),
            2,
            "case {case:?}: both steps are reported, got {steps:?}"
        );
        for step in &steps {
            assert_eq!(
                step["Status"]["State"].as_str(),
                Some("COMPLETED"),
                "case {case:?}: both steps completed, got {step}"
            );
        }
        let held = object(&router, output).await;
        assert_eq!(
            held.as_deref().map(str::trim),
            Some(payload),
            "case {case:?}: the second step must read back what the first wrote into HDFS, got \
             {held:?}"
        );
    }
}

/// THE RULE's node half: a bootstrap action runs before the cluster leaves its bootstrap phase,
/// so what it leaves on the node's local disk is there for the first step to read — the node's
/// storage outlives the program that wrote to it too.
///
/// The rows differ along the directory the action writes into and the token's bytes, and each
/// token is only ever read back through the STEP's own upload.
#[tokio::test]
async fn a_bootstrap_action_leaves_the_node_a_file_the_first_step_reads() {
    let router = emulator();
    create_bucket(&router).await;

    // (case, the path on the node, the token the action writes)
    let rows: [(&str, &str, &str); 2] = [
        (
            "a token under the node's temporary directory",
            "/tmp/bootstrap-token",
            "bootstrap-token-one",
        ),
        (
            "a token under the node's own state directory",
            "/mnt/var/lib/emulator/token",
            "bootstrap-token-two",
        ),
    ];

    let mut launched: Vec<(&str, String, String)> = Vec::new();
    for (case, path, token) in rows {
        let script_key = format!("scripts/{token}.sh");
        put_object(
            &router,
            &script_key,
            format!("#!/bin/bash\nmkdir -p /mnt/var/lib/emulator\nprintf %s '{token}' > {path}\n")
                .as_bytes(),
        )
        .await;
        let output = format!("steps/observed-{token}");
        let mut request = launch_request(
            case,
            json!([step_config(
                "read-the-bootstrap-token",
                shell(&format!(
                    "set -eu; cat {path} | aws s3 cp - s3://{BUCKET}/{output}"
                )),
            )]),
        );
        request["BootstrapActions"] = json!([{
            "Name": "leave-a-token",
            "ScriptBootstrapAction": { "Path": format!("s3://{BUCKET}/{script_key}") },
        }]);
        let cluster_id = launch(&router, case, request).await;
        launched.push((case, cluster_id, output));
    }

    for ((case, cluster_id, output), (_, _, token)) in launched.iter().zip(rows) {
        let steps = drained(case, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        assert_eq!(
            step["Status"]["State"].as_str(),
            Some("COMPLETED"),
            "case {case:?}: the step that read the token completed, got {step}"
        );
        let held = object(&router, output).await;
        assert_eq!(
            held.as_deref().map(str::trim),
            Some(token),
            "case {case:?}: the step must have read the token the bootstrap action left on the \
             node's local disk, got {held:?}"
        );
    }
}
