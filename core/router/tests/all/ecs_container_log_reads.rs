// SPDX-License-Identifier: CC0-1.0

//! WHERE a container's delivered output is found: the three reads a client uses to discover
//! and read it.
//!
//! RULE under test: a container whose `logConfiguration` names the `awslogs` driver has its
//! output delivered to the log group that configuration names, in the stream
//! `<awslogs-stream-prefix>/<container name>/<task id>` — and that stream is a real
//! CloudWatch Logs stream of that group, so all three of the reads a client reaches it by
//! answer with it: `DescribeLogStreams` LISTS it under the prefix, `GetLogEvents` READS its
//! events, and `FilterLogEvents` SEARCHES the group and finds the same events. The program
//! whose output that is comes from the launch when the launch overrides it
//! (`ContainerOverride$command`: "the command to send to the container that overrides the
//! default command from the Docker image or the task definition"), so a client that reuses
//! ONE revision for several runs gets each run's own program and each run's own stream.
//!
//! The rounds differ along what the rule generalises over: which run of the one revision it
//! is — the first, and a later one of the same revision — because each run's stream is its
//! own and a later run must not be answered with an earlier run's.
//!
//! Why this suite lives beside the router: the stream and the events are CloudWatch Logs'
//! resources, created and read through that service's OWN operations on the same engine that
//! serves Amazon ECS, while the task and its container are Amazon ECS's. Nothing reaches into
//! either service's state struct; every assertion is a wire answer.
//!
//! What it adds over `all/ecs_awslogs_driver_opens_a_stream_per_started_container.rs`, which
//! pins the stream the driver OPENS and reads it with `DescribeLogStreams`/`GetLogEvents`: the
//! third read (`FilterLogEvents`, the search a client that does not compute the stream name
//! makes), and the axis of a LATER run of the same revision — whose program is the launch's own
//! override — being answered with its own stream rather than the earlier run's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

const REGION: &str = "us-west-2";
const CLUSTER: &str = "logreads";
const CONTAINER: &str = "app";
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";
const LOG_GROUP: &str = "/emulator/ecs/log-reads";
const STREAM_PREFIX: &str = "efs";
const INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c0";
const ACCOUNT: &str = "000000000000";
const SENTINEL: &str = "A-DONE";
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

async fn json_call(
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
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = json_call(
        router,
        "ecs",
        &format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "ECS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(router, "logs", &format!("Logs_20140328.{operation}"), body).await
}

#[tokio::test]
async fn a_containers_output_is_found_by_every_read_a_client_uses() {
    let router = emulator();
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let identity = json!({
        "accountId": ACCOUNT,
        "instanceId": INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string();
    ecs(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity,
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
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

    // A revision whose container names NO program: the program comes from the launch's own
    // container overrides, the way a client that reuses one revision for several runs sends
    // it.
    ecs(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "logreads",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "essential": true,
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

    for round in 0..2 {
        let case = format!("run {round}");
        let run = ecs(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": "logreads",
                "count": 1,
                "launchType": "EC2",
                "overrides": {
                    "containerOverrides": [{
                        "name": CONTAINER,
                        "command": ["sh", "-c", format!("echo {SENTINEL}")],
                        "environment": [{ "name": "MARKER", "value": "marker" }],
                    }],
                },
            }),
        )
        .await;
        let task_arn = run["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: RunTask answers a task ARN, got {run}"))
            .to_string();
        let task_id = task_arn.rsplit('/').next().unwrap_or_default().to_string();
        let stream = format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}");

        let deadline = Instant::now() + SETTLE_DEADLINE;
        let task = loop {
            let body = ecs(
                &router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": [task_arn.clone()] }),
            )
            .await;
            let task = body["tasks"][0].clone();
            if task["lastStatus"] == json!("STOPPED") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the task never stopped, last read {task}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        };
        assert_eq!(
            task["containers"][0]["exitCode"],
            json!(0),
            "case {case:?}: expected the overridden program's status, got {task}"
        );

        // (1) the stream the driver created, as a client LISTS it.
        let (status, listed) = logs(
            &router,
            "DescribeLogStreams",
            json!({
                "logGroupName": LOG_GROUP,
                "logStreamNamePrefix": STREAM_PREFIX,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeLogStreams must answer 200, got {status} with {listed}"
        );
        let names: Vec<String> = listed["logStreams"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["logStreamName"].as_str().map(str::to_string))
            .collect();
        assert!(
            names.contains(&stream),
            "case {case:?}: expected DescribeLogStreams to list {stream}, got {names:?}"
        );

        // (2) the events, as a client READS one stream.
        let (status, read) = logs(
            &router,
            "GetLogEvents",
            json!({
                "logGroupName": LOG_GROUP,
                "logStreamName": stream.clone(),
                "startFromHead": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetLogEvents must answer 200, got {status} with {read}"
        );
        let messages: Vec<String> = read["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(
            messages.iter().any(|line| line == SENTINEL),
            "case {case:?}: expected GetLogEvents to carry {SENTINEL}, got {messages:?}"
        );

        // (3) the same events, as a client SEARCHES the group.
        let (status, filtered) = logs(
            &router,
            "FilterLogEvents",
            json!({
                "logGroupName": LOG_GROUP,
                "logStreamNames": [stream.clone()],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: FilterLogEvents must answer 200, got {status} with {filtered}"
        );
        let found: Vec<String> = filtered["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(
            found.iter().any(|line| line == SENTINEL),
            "case {case:?}: expected FilterLogEvents to carry {SENTINEL}, got {found:?}"
        );
    }
}
