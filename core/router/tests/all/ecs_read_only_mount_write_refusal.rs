// SPDX-License-Identifier: CC0-1.0

//! WHERE the account of a write a mount REFUSED goes, and what the program around it reads.
//!
//! RULE under test: a `mountPoints` entry whose `readOnly` is `true` gives the container
//! read-only access to the volume ("If this value is `true`, the container has read-only
//! access to the volume"), so the kernel refuses every write through it with EROFS, whose text
//! is `Read-only file system`. Such a write DOES NOT HAPPEN — the volume keeps exactly what it
//! held, a truncating write included — the command that carried the redirection answers
//! FAILURE, so the program's own branch over it takes the refused path, and the shell's
//! diagnostic about it goes wherever that command's standard error is directed: delivered
//! beside the container's output when nothing redirects it, and into the FILE a `2> path`
//! redirection names, where the program can read it back. The program runs to its end either
//! way, so the container exits with a status of its own and the task it is the essential
//! container of reaches `STOPPED` with `EssentialContainerExited` — which is what makes a
//! client that polls such a run converge rather than wait for ever.
//!
//! The rows differ along what the rule generalises over: where the command's standard error is
//! directed (nowhere, so it is delivered beside the output; into a file the program then reads
//! back), which write form carries it (an append and a truncation, because a refused
//! truncation must not empty the file either) and whether the mount point the write goes
//! through refuses one at all — the read-write mount of the same volume is the control, where
//! the same forms succeed and nothing is reported.
//!
//! Each row's volume is a `host` volume, whose data belongs to the task, mounted TWICE into
//! the one container — read-write at `/data` and read-only at `/readonly` — so one run writes
//! the bytes through one mount point and has its write through the other refused. The Amazon
//! EFS file system such a volume can be backed by instead, which is the namespace two TASKS
//! share, is `all/ecs_efs_volume_mounts.rs` and `all/ecs_efs_shared_volume_handover.rs`.
//!
//! Why this suite lives beside the router: what the container wrote is read back with
//! CloudWatch Logs' own `GetLogEvents` and the task with Amazon ECS's own `DescribeTasks`, the
//! two services mounted over ONE shared engine the way the production router mounts them, so
//! the output travels the real in-process delivery rather than being read out of the
//! producer's own buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "writerefusals";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The log group every container below delivers to, created through CloudWatch Logs' own API
/// before any task is launched — the way a client sets one up.
const LOG_GROUP: &str = "/emulator/ecs/write-refusals";

/// The `awslogs-stream-prefix` the task definitions below configure, and so the first segment
/// of every stream name they deliver to.
const STREAM_PREFIX: &str = "refusal";

/// The EC2 instance the cluster's capacity is registered as, so the tasks have somewhere to
/// run.
const INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c0";

/// The account id every emulator ARN carries, which a container-instance registration's
/// identity document has to name.
const ACCOUNT: &str = "000000000000";

/// The bytes each row's program writes through the read-write mount point before it tries the
/// read-only one.
const MARKER: &str = "refusal-marker-0123456789";

/// The text the kernel's EROFS carries, which is what a shell reports a write through a
/// read-only mount with. Spelled out here rather than imported from the service crate, so a
/// wrong wording in the implementation cannot make this suite pass.
const READ_ONLY_FILE_SYSTEM: &str = "Read-only file system";

/// How a shell prefixes a diagnostic of its own, which is what tells the container's own
/// report of a refusal apart from the shell's delivered beside the output.
const SHELL_DIAGNOSTIC: &str = "sh: ";

/// The line every program below ends with, which is what makes the lines before it the whole
/// of what the program wrote.
const DONE: &str = "DONE";

/// How long a row waits for a condition before giving up, and how often it looks again while
/// it waits. Generous on purpose: every wait below is for the CONDITION (a line delivered, a
/// task stopped), so a loaded host cannot fail a row the emulator satisfies.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// A router carrying Amazon ECS and CloudWatch Logs over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsJson1_1 call, answered with its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "ecs",
        &format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecs {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "logs", &format!("Logs_20140328.{operation}"), body).await
}

/// The messages CloudWatch Logs holds in `stream`, oldest first. An unknown stream reads as no
/// messages, which is what a stream nothing was delivered to holds.
async fn delivered(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming that driver's own
/// `awslogs-stream-prefix` documentation states.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// Create the cluster and register ONE container instance into it through the agent's own
/// operation, so the tasks below have capacity to run on.
async fn cluster_with_capacity(router: &MultiServiceRouter, case: &str) {
    ecs(router, case, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let identity = json!({
        "accountId": ACCOUNT,
        "instanceId": INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string();
    ecs(
        router,
        case,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity,
            // A registration presents the document's signature beside it; nothing here signs
            // anything, so this is base64 text of the right shape.
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
}

/// One row: the program's own statement between the write that seeds the volume and the read
/// that checks what the volume kept.
struct Row {
    case: &'static str,
    /// The statement under test, which writes through one of the two mount points.
    statement: &'static str,
    /// Whether the volume must still hold exactly the bytes the seeding write left.
    volume_unchanged: bool,
    /// Whether the container's log must carry the kernel's EROFS text — which is where a
    /// diagnostic nothing redirected is delivered.
    refusal_in_the_log: bool,
    /// The `NAME=` line the program reports what the file its diagnostic was redirected into
    /// holds, for a row that redirects one.
    reported_under: Option<&'static str>,
    /// Whether the write the statement carried must have succeeded.
    wrote: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an append through the read-only mount, its diagnostic delivered beside the \
               output",
        statement: "if echo appended >> /readonly/payload; then echo WROTE; else echo DENIED; fi",
        volume_unchanged: true,
        refusal_in_the_log: true,
        reported_under: None,
        wrote: false,
    },
    Row {
        // The shape the recorded shared-EFS scenario's read-only arm drives: the diagnostic is
        // sent to a file and the program reports what that file holds.
        case: "an append through the read-only mount, its diagnostic redirected into a file \
               the program reads back",
        statement: "if echo x 2>/tmp/err >> /readonly/payload; then echo WROTE; \
                    else echo DENIED; echo \"DENIED-ERR=$(tr \"\\n\" \" \" < /tmp/err)\"; fi",
        volume_unchanged: true,
        refusal_in_the_log: false,
        reported_under: Some("DENIED-ERR"),
        wrote: false,
    },
    Row {
        case: "a TRUNCATING write through the read-only mount, which must not empty the file \
               either",
        statement: "if echo replaced > /readonly/payload; then echo WROTE; else echo DENIED; fi",
        volume_unchanged: true,
        refusal_in_the_log: true,
        reported_under: None,
        wrote: false,
    },
    Row {
        case: "the same forms through the READ-WRITE mount point of the same volume",
        statement: "if echo appended >> /data/payload 2>/tmp/err; then \
                    echo \"DENIED-ERR=$(tr \"\\n\" \" \" < /tmp/err)\"; echo WROTE; \
                    else echo DENIED; fi",
        volume_unchanged: false,
        refusal_in_the_log: false,
        reported_under: Some("DENIED-ERR"),
        wrote: true,
    },
];

impl Row {
    /// The whole program the row's container runs: seed the volume through the read-write
    /// mount point, take the statement under test, report what the volume kept as read back
    /// through the read-only mount point, and end with [`DONE`].
    fn program(&self) -> String {
        format!(
            "echo \"$MARKER\" > /data/payload; {}; echo \"KEPT=$(cat /readonly/payload)\"; \
             echo {DONE}",
            self.statement
        )
    }
}

/// The value of the `NAME=` line `name` names, or `None` when the program reported none.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// Look again at what `probe` answers until it answers `Some`, and hand that back.
///
/// Every wait here is for a CONDITION the clock materialises; the deadline only turns a
/// transition that never happens into a failure instead of a hang.
async fn poll_until<Answer, Probe, Reading>(probe: Probe, what: &str) -> Answer
where
    Probe: Fn() -> Reading,
    Reading: std::future::Future<Output = Option<Answer>>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(answered) = probe().await {
            return answered;
        }
        assert!(Instant::now() < deadline, "{what} never happened");
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// RULE: a write a read-only mount point refuses does not happen, answers failure to the
/// program around it, and is accounted for on the command's standard error — wherever that is
/// directed — while the program runs to its end and the task reaches its own stop.
#[tokio::test]
async fn a_refused_write_is_reported_where_the_commands_standard_error_goes() {
    let router = emulator();
    cluster_with_capacity(&router, "the cluster the rows run in").await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLogGroup must answer 200, got {status} with {created}"
    );

    // Every row is launched before any wait, so the rows' clocks run together rather than one
    // after another.
    let mut placed: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("writerefusal{index}");
        ecs(
            &router,
            row.case,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "volumes": [{ "name": "shared", "host": {} }],
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": IMAGE,
                    "memory": 128,
                    "essential": true,
                    "command": ["sh", "-c", row.program()],
                    "environment": [{ "name": "MARKER", "value": MARKER }],
                    "mountPoints": [
                        { "sourceVolume": "shared", "containerPath": "/data", "readOnly": false },
                        {
                            "sourceVolume": "shared",
                            "containerPath": "/readonly",
                            "readOnly": true,
                        },
                    ],
                    "logConfiguration": {
                        "logDriver": "awslogs",
                        "options": {
                            "awslogs-group": LOG_GROUP,
                            "awslogs-region": REGION,
                            "awslogs-stream-prefix": STREAM_PREFIX,
                        },
                    },
                }],
            }),
        )
        .await;
        let body = ecs(
            &router,
            row.case,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": family,
                "count": 1,
                "launchType": "EC2",
            }),
        )
        .await;
        let task_arn = body["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: RunTask answers a task ARN, got {body}",
                    row.case
                )
            })
            .to_string();
        placed.push((row, task_arn));
    }

    for (row, task_arn) in placed {
        let case = row.case;
        let stream = stream_of(&task_arn);
        // The CONDITION each row waits on: the program ran to its end, so the lines before it
        // are the whole of what it wrote.
        let lines = poll_until(
            || async {
                let events = delivered(&router, &stream).await;
                events.iter().any(|line| line == DONE).then_some(events)
            },
            &format!("case {case:?}: the container's own lines in {stream}"),
        )
        .await;

        assert_eq!(
            lines.iter().any(|line| line == "WROTE"),
            row.wrote,
            "case {case:?}: expected the write reported as done = {}, got {lines:?}",
            row.wrote
        );
        assert_eq!(
            lines.iter().any(|line| line == "DENIED"),
            !row.wrote,
            "case {case:?}: expected the program's refused branch taken = {}, got {lines:?}",
            !row.wrote
        );
        assert_eq!(
            lines
                .iter()
                .any(|line| line.starts_with(SHELL_DIAGNOSTIC)
                    && line.contains(READ_ONLY_FILE_SYSTEM)),
            row.refusal_in_the_log,
            "case {case:?}: expected the shell's own {READ_ONLY_FILE_SYSTEM:?} diagnostic \
             delivered beside the output = {}, got {lines:?}",
            row.refusal_in_the_log
        );
        if let Some(name) = row.reported_under {
            let read_back = reported(&lines, name);
            if row.wrote {
                // The control row's redirected file holds nothing: a write the filesystem
                // took reports no diagnostic at all.
                assert_eq!(
                    read_back.as_deref().map(str::trim),
                    Some(""),
                    "case {case:?}: expected {name} to report no diagnostic, got {read_back:?} \
                     in {lines:?}"
                );
            } else {
                assert!(
                    read_back
                        .as_deref()
                        .is_some_and(|read| read.contains(READ_ONLY_FILE_SYSTEM)),
                    "case {case:?}: expected {name} to carry {READ_ONLY_FILE_SYSTEM:?} read \
                     back out of the file the diagnostic was redirected into, got \
                     {read_back:?} in {lines:?}"
                );
            }
        }
        // What the volume KEPT, read through the read-only mount point — which still answers.
        let kept = reported(&lines, "KEPT");
        assert_eq!(
            kept.as_deref(),
            Some(MARKER),
            "case {case:?}: expected the read through the read-only mount point to answer the \
             bytes the volume holds, got {kept:?} in {lines:?}"
        );
        if row.volume_unchanged {
            assert!(
                !lines
                    .iter()
                    .any(|line| line == "appended" || line == "replaced"),
                "case {case:?}: the refused write must have left no trace on the volume, got \
                 {lines:?}"
            );
        } else {
            assert!(
                lines.iter().any(|line| line == "appended"),
                "case {case:?}: expected the volume to hold what the read-write mount point \
                 took, got {lines:?}"
            );
        }

        // And the run produced a STATUS, which is what stops the task: a client polling such a
        // task converges instead of waiting for ever.
        let task = poll_until(
            || async {
                let body = ecs(
                    &router,
                    case,
                    "DescribeTasks",
                    json!({ "cluster": CLUSTER, "tasks": [task_arn.clone()] }),
                )
                .await;
                let task = body["tasks"][0].clone();
                (task["lastStatus"] == json!("STOPPED")).then_some(task)
            },
            &format!("case {case:?}: the task reaching STOPPED"),
        )
        .await;
        assert_eq!(
            task["containers"][0]["exitCode"],
            json!(0),
            "case {case:?}: expected the program's own status, got {task}"
        );
        assert_eq!(
            task["stopCode"],
            json!("EssentialContainerExited"),
            "case {case:?}: expected the stop of a task whose essential container exited, got \
             {task}"
        );
    }
}
