// SPDX-License-Identifier: CC0-1.0

//! Every status an execution moves into reaches the SQS target of a rule that matches it,
//! over the shapes a client's own scenario takes.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > Every status an execution of a STANDARD state machine MOVES INTO is published once onto
//! > the account's DEFAULT EventBridge bus as `source` `aws.states` / `detail-type`
//! > `Step Functions Execution Status Change`, carrying the execution's ARN in `resources`
//! > and the facts `DescribeExecution` reports for it in its `detail`; EventBridge then
//! > routes that envelope by its CONTENT, so every rule whose `EventPattern` matches it
//! > delivers it to that rule's targets — whatever the workflow did to reach the status,
//! > whoever started the execution, however the rule selects the envelope, and however the
//! > target queue is configured.
//!
//! The rows span what the rule generalises over: WHICH dimension the matching rule filters
//! on (the source and detail type alone, the state machine inside `detail`, a prefix of the
//! execution's name, the execution in `resources`, a wildcard over its `input`, the status
//! itself), WHAT the queue's resource policy grants (the service principal, everyone, every
//! AWS principal), HOW the queue is encrypted (the AWS-managed key, SQS-managed
//! encryption), HOW the target is configured (a FIFO queue with a message group, a target
//! taking the detail alone, one rendering through an input transformer, one naming a role),
//! WHAT the execution does to reach its status (a Choice with a Wait, a Parallel, an inline
//! Map, intrinsic functions, a JSONata definition, a Task that writes to another service
//! under a wide and a narrowly scoped role, with retries and a catch), WHO started it (a
//! client, and a rule's own state-machine target), and WHAT answers the task it waits at (a
//! worker claiming an activity, and a caller answering a callback token that travelled
//! through a queue and through the bus).
//!
//! Each row's verdict is COLLECTED and the whole table asserted at the end, so one run names
//! every shape that is not served instead of stopping at the first.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;
const REGION: &str = "us-east-1";
const SFN_SOURCE: &str = "aws.states";
const EXECUTION_STATUS_CHANGE: &str = "Step Functions Execution Status Change";
const DEFAULT_BUS: &str = "default";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
const IAM_API_VERSION: &str = "2010-05-08";
/// How long a row waits for the transitions a client's scenario owes before it gives up.
///
/// Generous: the wait is for the CONDITION — the envelope arriving — so a host running this
/// beside the rest of the workspace's tests spends the span instead of failing on it.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    router
}

fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", format!("application/x-amz-json-{json_version}"))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the probe request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "states", "AWSStepFunctions", "1.0", operation, body).await
}

async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "events", "AWSEvents", "1.1", operation, body).await
}

async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await
}

async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("iam.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("the probe request is well formed");
    let response = router.dispatch(request).await;
    String::from_utf8_lossy(response.body()).to_string()
}

fn text(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

async fn queue(router: &MultiServiceRouter, name: &str, principal: &Value) -> (String, String) {
    let (_, created) = sqs(router, "CreateQueue", &json!({"QueueName": name})).await;
    let url = text(&created, "/QueueUrl");
    let (_, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = text(&attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": principal,
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

async fn rule(
    router: &MultiServiceRouter,
    name: &str,
    pattern: &Value,
    target: &Value,
) -> (u16, Value) {
    events(
        router,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    events(
        router,
        "PutTargets",
        &json!({"Rule": name, "EventBusName": DEFAULT_BUS, "Targets": [target]}),
    )
    .await
}

async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let (_, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::new();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        bodies.push(
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null),
        );
    }
    bodies
}

/// Everything that arrived on `url` until an envelope reports a TERMINAL status, or the
/// deadline passes.
async fn until_terminal(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let started = Instant::now();
    let mut arrived: Vec<Value> = Vec::new();
    while started.elapsed() < DELIVERY_DEADLINE {
        arrived.extend(drain(router, url).await);
        if arrived.iter().any(|envelope| {
            !matches!(
                envelope["detail"]["status"].as_str(),
                None | Some("RUNNING")
            )
        }) {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    arrived
}

/// A role trusted by both service principals, allowed everything the probe's tasks and
/// targets do.
async fn probe_role(router: &MultiServiceRouter, name: &str, resource: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": [STATES_PRINCIPAL, EVENTS_PRINCIPAL]},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = element(&created, "Arn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": "sqs:SendMessage", "Resource": resource},
            {"Effect": "Allow", "Action": "states:StartExecution", "Resource": "*"},
        ],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// A STANDARD state machine carrying `definition`; answers its ARN and the create's answer.
async fn machine(
    router: &MultiServiceRouter,
    name: &str,
    definition: &Value,
    role_arn: &str,
) -> (String, Value) {
    let (_, created) = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": role_arn,
            "type": "STANDARD",
        }),
    )
    .await;
    (text(&created, "/stateMachineArn"), created)
}

const PASSES: fn() -> Value = || {
    json!({"StartAt": "Ok", "States": {"Ok": {"Type": "Pass", "Result": {"a": 1}, "End": true}}})
};

/// An ACTIVITY task the probe itself works: the execution waits at it, the probe claims it
/// with `GetActivityTask` and answers it with `SendTaskSuccess`, and the terminal status
/// change must then reach the rule's queue.
async fn activity_worker_row(router: &MultiServiceRouter, role: &str) -> Verdict {
    let name = "probe-activity".to_string();
    let (url, queue_arn) = queue(router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
    let (_, activity) = sfn(router, "CreateActivity", &json!({"name": &name})).await;
    let activity_arn = text(&activity, "/activityArn");
    let definition = json!({
        "StartAt": "Work",
        "States": {"Work": {"Type": "Task", "Resource": &activity_arn, "End": true}},
    });
    let (machine_arn, _) = machine(router, &name, &definition, role).await;
    rule(
        router,
        &name,
        &json!({"source": [SFN_SOURCE]}),
        &json!({"Id": "t1", "Arn": &queue_arn}),
    )
    .await;
    sfn(
        router,
        "StartExecution",
        &json!({"stateMachineArn": &machine_arn, "name": &name}),
    )
    .await;
    let (claimed_status, claimed) = sfn(
        router,
        "GetActivityTask",
        &json!({"activityArn": &activity_arn, "workerName": "probe"}),
    )
    .await;
    let token = text(&claimed, "/taskToken");
    let (answered_status, answered) = sfn(
        router,
        "SendTaskSuccess",
        &json!({"taskToken": &token, "output": r#"{"done":true}"#}),
    )
    .await;
    let arrived = until_terminal(router, &url).await;
    let terminal = arrived
        .iter()
        .find(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")))
        .cloned()
        .unwrap_or(Value::Null);
    Verdict {
        case: "an activity task the caller claims and answers",
        ok: terminal["detail"]["status"].as_str() == Some("SUCCEEDED"),
        detail: format!(
            "GetActivityTask {claimed_status} {claimed}, SendTaskSuccess {answered_status} \
             {answered}, terminal {terminal}"
        ),
    }
}

/// A CALLBACK (`.waitForTaskToken`) task whose token travels through `through` — straight
/// into a queue (`sqs`), or onto the default bus and through a rule into a queue
/// (`events`) — which the probe reads it out of and answers, as a worker does.
async fn callback_row(
    router: &MultiServiceRouter,
    role: &str,
    through: &'static str,
) -> Verdict {
    let name = format!("probe-callback-{through}");
    let (watched_url, watched_arn) =
        queue(router, &format!("{name}-watch"), &json!({"Service": EVENTS_PRINCIPAL})).await;
    let (token_url, token_arn) =
        queue(router, &format!("{name}-token"), &json!({"Service": EVENTS_PRINCIPAL})).await;
    let source = format!("probe.{through}");
    let state = match through {
        "sqs" => json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
            "Parameters": {"QueueUrl": &token_url, "MessageBody": {"token.$": "$$.Task.Token"}},
            "End": true,
        }),
        _ => json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::events:putEvents.waitForTaskToken",
            "Parameters": {
                "Entries": [{
                    "EventBusName": DEFAULT_BUS,
                    "Source": &source,
                    "DetailType": "probe.token",
                    "Detail": {"token.$": "$$.Task.Token"},
                }],
            },
            "End": true,
        }),
    };
    if through == "events" {
        rule(
            router,
            &format!("{name}-token"),
            &json!({"source": [&source]}),
            &json!({"Id": "t1", "Arn": &token_arn}),
        )
        .await;
    }
    let definition = json!({"StartAt": "Hand over", "States": {"Hand over": state}});
    let (machine_arn, created) = machine(router, &name, &definition, role).await;
    rule(
        router,
        &name,
        &json!({"source": [SFN_SOURCE]}),
        &json!({"Id": "t1", "Arn": &watched_arn}),
    )
    .await;
    let (started_status, started) = sfn(
        router,
        "StartExecution",
        &json!({"stateMachineArn": &machine_arn, "name": &name}),
    )
    .await;
    // The worker: read the token out of the queue it travelled to, and answer the task.
    let started_at = Instant::now();
    let mut token = String::new();
    while token.is_empty() && started_at.elapsed() < DELIVERY_DEADLINE {
        for body in drain(router, &token_url).await {
            let carried = match through {
                "sqs" => text(&body, "/token"),
                _ => text(&body, "/detail/token"),
            };
            if !carried.is_empty() {
                token = carried;
            }
        }
        if token.is_empty() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    let (answered_status, answered) = sfn(
        router,
        "SendTaskSuccess",
        &json!({"taskToken": &token, "output": r#"{"done":true}"#}),
    )
    .await;
    let arrived = until_terminal(router, &watched_url).await;
    let terminal = arrived
        .iter()
        .find(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")))
        .cloned()
        .unwrap_or(Value::Null);
    Verdict {
        case: match through {
            "sqs" => "a callback task whose token travels through a queue",
            _ => "a callback task whose token travels through the bus",
        },
        ok: terminal["detail"]["status"].as_str() == Some("SUCCEEDED"),
        detail: format!(
            "CreateStateMachine {created}, StartExecution {started_status} {started}, token \
             {token:?}, SendTaskSuccess {answered_status} {answered}, terminal {terminal}"
        ),
    }
}

/// One row's verdict: what it drove, whether the envelope arrived, and what it saw.
struct Verdict {
    case: &'static str,
    ok: bool,
    detail: String,
}

/// RULE: every status an execution moves into reaches the target of every rule whose
/// pattern matches its envelope, over every shape of the scenario below.
#[test]
fn every_status_change_reaches_its_rules_target_in_every_shape_of_the_scenario() {
    std::thread::Builder::new()
        .name("sfn-status-change-scenarios".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

async fn drive() {
    let router = router();
    let mut verdicts: Vec<Verdict> = Vec::new();

    // The target queue a Task state sends to, and the role that admits both.
    let (task_queue_url, task_queue_arn) =
        queue(&router, "probe-task-target", &json!({"Service": EVENTS_PRINCIPAL})).await;
    let wide_role = probe_role(&router, "probe-wide", "*").await;
    let scoped_role = probe_role(&router, "probe-scoped", &task_queue_arn).await;

    // --- rows whose variation is the RULE PATTERN -------------------------------------
    /// How one row writes its rule's pattern: over the state machine's ARN, the names the
    /// row used, and the execution's ARN.
    type Pattern = fn(&str, &str, &str) -> Value;
    let patterns: Vec<(&'static str, Pattern)> = vec![
        (
            "the source and detail-type alone",
            |_sm, _name, _exec| json!({"source": [SFN_SOURCE], "detail-type": [EXECUTION_STATUS_CHANGE]}),
        ),
        (
            "scoped by detail.stateMachineArn",
            |sm, _name, _exec| {
                json!({
                    "source": [SFN_SOURCE],
                    "detail-type": [EXECUTION_STATUS_CHANGE],
                    "detail": {"stateMachineArn": [sm]},
                })
            },
        ),
        (
            "scoped by a detail.name prefix",
            |_sm, name, _exec| {
                json!({
                    "source": [SFN_SOURCE],
                    "detail": {"name": [{"prefix": name}]},
                })
            },
        ),
        (
            "scoped by the execution in resources",
            |_sm, _name, exec| json!({"source": [SFN_SOURCE], "resources": [exec]}),
        ),
        (
            "scoped by a detail.input wildcard",
            |_sm, _name, _exec| {
                json!({
                    "source": [SFN_SOURCE],
                    "detail": {"input": [{"wildcard": "*probe-nonce*"}]},
                })
            },
        ),
        (
            "scoped by detail.status on the terminal status",
            |_sm, _name, _exec| {
                json!({
                    "source": [SFN_SOURCE],
                    "detail": {"status": ["SUCCEEDED"]},
                })
            },
        ),
    ];
    for (index, (case, pattern)) in patterns.into_iter().enumerate() {
        let name = format!("probe-pattern-{index}");
        let (url, queue_arn) = queue(&router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        let execution_arn = format!(
            "arn:aws:states:{REGION}:{}:execution:{name}:{name}",
            engine::EMULATOR_ACCOUNT_ID
        );
        let (status, targeted) = rule(
            &router,
            &name,
            &pattern(&machine_arn, &name, &execution_arn),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        let (started_status, started) = sfn(
            &router,
            "StartExecution",
            &json!({
                "stateMachineArn": &machine_arn,
                "name": &name,
                "input": r#"{"nonce":"probe-nonce"}"#,
            }),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let ok = arrived
            .iter()
            .any(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")));
        verdicts.push(Verdict {
            case,
            ok,
            detail: format!(
                "PutTargets {status} {targeted}, StartExecution {started_status} {started}, \
                 arrived {arrived:?}"
            ),
        });
    }

    // --- a row whose variation is the QUEUE POLICY's principal ------------------------
    for (index, (case, principal)) in [
        ("a queue policy granting everyone", json!("*")),
        (
            "a queue policy granting every AWS principal",
            json!({"AWS": "*"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("probe-principal-{index}");
        let (url, queue_arn) = queue(&router, &name, &principal).await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        sfn(
            &router,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": &name}),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let ok = arrived
            .iter()
            .any(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")));
        verdicts.push(Verdict {
            case,
            ok,
            detail: format!("arrived {arrived:?}"),
        });
    }

    // --- rows whose variation is WHAT THE EXECUTION DOES ------------------------------
    let send = |url: &str| {
        json!({
            "StartAt": "Send",
            "States": {
                "Send": {
                    "Type": "Task",
                    "Resource": "arn:aws:states:::sqs:sendMessage",
                    "Parameters": {"QueueUrl": url, "MessageBody": "probe"},
                    "End": true,
                },
            },
        })
    };
    for (index, (case, role)) in [
        ("a Task sending to SQS under a role allowed everything", &wide_role),
        (
            "a Task sending to SQS under a role scoped to that queue",
            &scoped_role,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("probe-task-{index}");
        let (url, queue_arn) = queue(&router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
        let (machine_arn, _) = machine(&router, &name, &send(&task_queue_url), role).await;
        rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        sfn(
            &router,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": &name}),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let terminal = arrived
            .iter()
            .find(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")))
            .cloned()
            .unwrap_or(Value::Null);
        let sent = drain(&router, &task_queue_url).await;
        verdicts.push(Verdict {
            case,
            ok: terminal["detail"]["status"].as_str() == Some("SUCCEEDED"),
            detail: format!("terminal {terminal}, the task's own queue holds {sent:?}"),
        });
    }

    // --- a row whose variation is WHO STARTED THE EXECUTION ---------------------------
    {
        let name = "probe-events-started".to_string();
        let (url, queue_arn) = queue(&router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        let (status, targeted) = rule(
            &router,
            "probe-starter",
            &json!({"source": ["probe.client"]}),
            &json!({"Id": "t1", "Arn": &machine_arn, "RoleArn": &wide_role}),
        )
        .await;
        let (published_status, published) = events(
            &router,
            "PutEvents",
            &json!({
                "Entries": [{
                    "EventBusName": DEFAULT_BUS,
                    "Source": "probe.client",
                    "DetailType": "probe",
                    "Detail": json!({"nonce": "probe-nonce"}).to_string(),
                }],
            }),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let ok = arrived
            .iter()
            .any(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")));
        verdicts.push(Verdict {
            case: "an execution a rule's state-machine target started",
            ok,
            detail: format!(
                "PutTargets {status} {targeted}, PutEvents {published_status} {published}, \
                 arrived {arrived:?}"
            ),
        });
    }

    // --- rows whose variation is the DEFINITION the execution runs --------------------
    let definitions: Vec<(&'static str, Value)> = vec![
        (
            "a Choice over the input, a Wait and a Succeed",
            json!({
                "Comment": "a probe",
                "Version": "1.0",
                "StartAt": "Pick",
                "States": {
                    "Pick": {
                        "Type": "Choice",
                        "Choices": [{"Variable": "$.nonce", "StringEquals": "probe-nonce", "Next": "Hold"}],
                        "Default": "Hold",
                    },
                    "Hold": {"Type": "Wait", "Seconds": 1, "Next": "Done"},
                    "Done": {"Type": "Succeed"},
                },
            }),
        ),
        (
            "a Parallel of two branches",
            json!({
                "StartAt": "Both",
                "States": {
                    "Both": {
                        "Type": "Parallel",
                        "Branches": [
                            {"StartAt": "L", "States": {"L": {"Type": "Pass", "Result": 1, "End": true}}},
                            {"StartAt": "R", "States": {"R": {"Type": "Pass", "Result": 2, "End": true}}},
                        ],
                        "End": true,
                    },
                },
            }),
        ),
        (
            "an inline Map over three items",
            json!({
                "StartAt": "Each",
                "States": {
                    "Each": {
                        "Type": "Map",
                        "ItemsPath": "$.items",
                        "ItemProcessor": {
                            "ProcessorConfig": {"Mode": "INLINE"},
                            "StartAt": "One",
                            "States": {"One": {"Type": "Pass", "End": true}},
                        },
                        "End": true,
                    },
                },
            }),
        ),
        (
            "intrinsic functions in a Pass",
            json!({
                "StartAt": "Render",
                "States": {
                    "Render": {
                        "Type": "Pass",
                        "Parameters": {
                            "said.$": "States.Format('nonce is {}', $.nonce)",
                            "text.$": "States.JsonToString($)",
                        },
                        "End": true,
                    },
                },
            }),
        ),
        (
            "a JSONata definition",
            json!({
                "QueryLanguage": "JSONata",
                "StartAt": "Render",
                "States": {
                    "Render": {
                        "Type": "Pass",
                        "Output": {"said": "{% 'nonce is ' & $states.input.nonce %}"},
                        "End": true,
                    },
                },
            }),
        ),
        (
            "a Task with Retry and Catch around a send",
            json!({
                "StartAt": "Send",
                "States": {
                    "Send": {
                        "Type": "Task",
                        "Resource": "arn:aws:states:::sqs:sendMessage",
                        "Parameters": {"QueueUrl": &task_queue_url, "MessageBody": "probe"},
                        "Retry": [{"ErrorEquals": ["States.ALL"], "MaxAttempts": 2}],
                        "Catch": [{"ErrorEquals": ["States.ALL"], "Next": "Fallback"}],
                        "End": true,
                    },
                    "Fallback": {"Type": "Pass", "End": true},
                },
            }),
        ),
    ];
    for (index, (case, definition)) in definitions.into_iter().enumerate() {
        let name = format!("probe-definition-{index}");
        let (url, queue_arn) = queue(&router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
        let (machine_arn, created) = machine(&router, &name, &definition, &wide_role).await;
        rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        let (started_status, started) = sfn(
            &router,
            "StartExecution",
            &json!({
                "stateMachineArn": &machine_arn,
                "name": &name,
                "input": r#"{"nonce":"probe-nonce","items":[1,2,3]}"#,
            }),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let terminal = arrived
            .iter()
            .find(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")))
            .cloned()
            .unwrap_or(Value::Null);
        verdicts.push(Verdict {
            case,
            ok: terminal["detail"]["status"].as_str() == Some("SUCCEEDED"),
            detail: format!(
                "CreateStateMachine {created}, StartExecution {started_status} {started}, \
                 terminal {terminal}"
            ),
        });
    }

    // --- rows whose variation is the TARGET's own configuration -----------------------
    {
        // A FIFO queue admits a rule's delivery only with a message group, which is the
        // target's own `SqsParameters`.
        let name = "probe-fifo".to_string();
        let (_, created) = sqs(
            &router,
            "CreateQueue",
            &json!({
                "QueueName": format!("{name}.fifo"),
                "Attributes": {"FifoQueue": "true", "ContentBasedDeduplication": "true"},
            }),
        )
        .await;
        let url = text(&created, "/QueueUrl");
        let (_, attributes) = sqs(
            &router,
            "GetQueueAttributes",
            &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        let queue_arn = text(&attributes, "/Attributes/QueueArn");
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": EVENTS_PRINCIPAL},
                "Action": "sqs:SendMessage",
                "Resource": &queue_arn,
            }],
        })
        .to_string();
        sqs(
            &router,
            "SetQueueAttributes",
            &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
        )
        .await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        let (status, targeted) = rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({
                "Id": "t1",
                "Arn": &queue_arn,
                "SqsParameters": {"MessageGroupId": "probe"},
            }),
        )
        .await;
        sfn(
            &router,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": &name}),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let ok = arrived
            .iter()
            .any(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")));
        verdicts.push(Verdict {
            case: "a FIFO queue target with a message group",
            ok,
            detail: format!("PutTargets {status} {targeted}, arrived {arrived:?}"),
        });
    }
    for (index, (case, extra)) in [
        (
            "a target taking the detail alone through InputPath",
            json!({"InputPath": "$.detail"}),
        ),
        (
            "a target rendering through an InputTransformer",
            json!({
                "InputTransformer": {
                    "InputPathsMap": {"status": "$.detail.status", "run": "$.detail.executionArn"},
                    "InputTemplate": "{\"status\": <status>, \"run\": <run>}",
                },
            }),
        ),
        (
            "a target carrying a dead-letter queue and a role",
            json!({"RoleArn": "arn:aws:iam::000000000000:role/probe-wide"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("probe-target-{index}");
        let (url, queue_arn) = queue(&router, &name, &json!({"Service": EVENTS_PRINCIPAL})).await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        let mut target = json!({"Id": "t1", "Arn": &queue_arn});
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            target[member] = value;
        }
        let (status, targeted) = rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &target,
        )
        .await;
        let (_, started) = sfn(
            &router,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": &name}),
        )
        .await;
        let execution_arn = text(&started, "/executionArn");
        // Two of these rows receive a RENDERING of the envelope rather than the envelope,
        // so what is asserted of each is what every shape has to carry: this execution and
        // the terminal status it reached.
        let started_at = Instant::now();
        let mut arrived: Vec<Value> = Vec::new();
        let mut carried = false;
        while !carried && started_at.elapsed() < DELIVERY_DEADLINE {
            arrived.extend(drain(&router, &url).await);
            carried = arrived.iter().any(|body| {
                let rendered = body.to_string();
                rendered.contains(&execution_arn) && rendered.contains("SUCCEEDED")
            });
            if !carried {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        verdicts.push(Verdict {
            case,
            ok: carried,
            detail: format!("PutTargets {status} {targeted}, arrived {arrived:?}"),
        });
    }

    // --- rows whose variation is HOW THE QUEUE IS ENCRYPTED ---------------------------
    for (index, (case, attributes)) in [
        (
            "a queue under the AWS-managed SQS key",
            json!({"KmsMasterKeyId": "alias/aws/sqs"}),
        ),
        (
            "a queue under SQS-managed encryption",
            json!({"SqsManagedSseEnabled": "true"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("probe-sse-{index}");
        let mut request = json!({"QueueName": &name});
        request["Attributes"] = attributes;
        let (create_status, created) = sqs(&router, "CreateQueue", &request).await;
        let url = text(&created, "/QueueUrl");
        let (_, got) = sqs(
            &router,
            "GetQueueAttributes",
            &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        let queue_arn = text(&got, "/Attributes/QueueArn");
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": EVENTS_PRINCIPAL},
                "Action": "sqs:SendMessage",
                "Resource": &queue_arn,
            }],
        })
        .to_string();
        sqs(
            &router,
            "SetQueueAttributes",
            &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
        )
        .await;
        let (machine_arn, _) = machine(&router, &name, &PASSES(), &wide_role).await;
        rule(
            &router,
            &name,
            &json!({"source": [SFN_SOURCE]}),
            &json!({"Id": "t1", "Arn": &queue_arn}),
        )
        .await;
        sfn(
            &router,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": &name}),
        )
        .await;
        let arrived = until_terminal(&router, &url).await;
        let ok = arrived
            .iter()
            .any(|envelope| !matches!(envelope["detail"]["status"].as_str(), None | Some("RUNNING")));
        verdicts.push(Verdict {
            case,
            ok,
            detail: format!("CreateQueue {create_status} {created}, arrived {arrived:?}"),
        });
    }

    // --- rows whose variation is WHAT ANSWERS THE EXECUTION'S OUTSTANDING TASK --------
    verdicts.push(activity_worker_row(&router, &wide_role).await);
    verdicts.push(callback_row(&router, &wide_role, "sqs").await);
    verdicts.push(callback_row(&router, &wide_role, "events").await);

    let failed: Vec<&Verdict> = verdicts.iter().filter(|verdict| !verdict.ok).collect();
    let report: Vec<String> = verdicts
        .iter()
        .map(|verdict| {
            format!(
                "[{}] {}: {}",
                if verdict.ok { "ok" } else { "FAILED" },
                verdict.case,
                verdict.detail
            )
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} of {} shapes of the scenario did not deliver the execution's status change:\n{}",
        failed.len(),
        verdicts.len(),
        report.join("\n\n")
    );
}
