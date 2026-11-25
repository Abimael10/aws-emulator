// SPDX-License-Identifier: CC0-1.0

//! The whole queue relay a COMMAND-LINE client wires by hand, driven over the production
//! router with IAM and Amazon SQS mounted beside Lambda on one engine: the execution role is
//! written through IAM's own `CreateRole`/`PutRolePolicy`, the relay is uploaded as a real
//! .zip deployment package, and the mapping is polled by the production poll loop rather than
//! by passes a test makes.
//!
//! RULE: a mapping that reports `Enabled` over an Amazon SQS queue receives from that queue on
//! its own and invokes the mapped function with the `Records` envelope of the batch it took, so
//! the handler's own downstream write really happens; and from the instant
//! `DeleteEventSourceMapping` is accepted that mapping polls no more — a message sent
//! afterwards is neither handed to the function nor taken out of the source queue, and the
//! mapping is no longer one a further call may act on.
//!
//! What is reached only here, and in no sibling suite of this crate: the deploy path a
//! COMMAND-LINE client drives end to end against the production poll loop. The other queue
//! suites either upload a package no engine runs (so no batch they poll is ever processed), or
//! make the poll passes themselves, or say nothing about any role because no IAM is mounted
//! beside them. Here the execution role is the least-privilege one IAM persisted, the handler
//! comes out of a real .zip archive, and the pass that hands the batch over is the one the
//! create armed.
//!
//! The first test's rows differ along what the rule generalises over: the `BatchSize` the
//! mapping reads in (one message per batch, and a ceiling well above what is waiting) and how
//! many messages are waiting when the mapping is polling. The second test takes the same relay
//! through the DELETE, which is where the rule's negative half is.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::POLL_INTERVAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, QUEUE_READS};
use crate::event_source_mappings::{
    await_mapping_state, call, call_sqs, create_mapping, create_queue, get_mapping, refusal_message,
    router_on, MAPPINGS_URI, STATE_ENABLED,
};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The managed runtime the relay runs under, and the module and attribute its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The environment variable the relay reads the sink queue's address from.
const SINK_VARIABLE: &str = "SINK_QUEUE_URL";

/// The `eventSource` every record of a queue-sourced event carries.
const EVENT_SOURCE: &str = "aws:sqs";

/// The `State` a delete moves a mapping into (`EventSourceMappingConfiguration$State`).
const STATE_DELETING: &str = "Deleting";

/// The modelled error a mapping this account no longer holds for acting on answers with, and the
/// whole of the sentence that error class carries — it interpolates nothing of the request.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
/// See [`RESOURCE_NOT_FOUND`].
const NOT_FOUND_MESSAGE: &str = "The resource you requested does not exist.";

/// The body relayed while the mapping is held, and the one sent once it is gone.
const BEFORE_THE_DELETE: &str = "before-the-delete";
/// See [`BEFORE_THE_DELETE`].
const AFTER_THE_DELETE: &str = "after-the-delete";

/// How long the absence leg keeps LOOKING once the mapping is gone: many of the poll loop's own
/// intervals, so a pass the rule forbids has every chance to happen.
const QUIET_WINDOW: Duration = POLL_INTERVAL.saturating_mul(8);

/// How long a row waits for work the CLOCK owes — the function going `Active`, the mapping
/// settling, a pass handing its batch to a running handler — before the row is a failure.
/// GENEROUS: a loaded host delays a row instead of failing it.
const BUDGET: Duration = Duration::from_secs(120);

/// How long between two looks while waiting for one of those.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How long one drain of the sink waits for the message it is looking for.
const LONG_POLL_SECONDS: i64 = 1;

/// The relay the rows deploy: it forwards the whole envelope it was handed to the queue its
/// environment names, with the SDK its managed runtime supplies. The client is built at module
/// scope, which is where a managed runtime's handler builds it.
const RELAY_SOURCE: &str = r#"import json
import os

import boto3

QUEUES = boto3.client("sqs")
SINK_URL = os.environ["SINK_QUEUE_URL"]


def handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(event))
    return {"relayed": [record["messageId"] for record in records]}
"#;

/// Lambda, IAM and Amazon SQS over ONE engine, the shape `core/router` assembles: the role is
/// written where IAM holds it, the queues where Amazon SQS holds them, and the mapping is
/// polled by the production loop.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A .zip deployment package holding [`RELAY_SOURCE`] as `index.py`, written in memory the way
/// a command-line client uploads one.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.py", options)
            .expect("the entry starts");
        writer
            .write_all(RELAY_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the relay as `role_arn`, told through its environment which queue to write to, and
/// wait until it reports `Active`: a mapping polls nothing while its function cannot be
/// invoked.
async fn given_relay_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    sink_url: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Environment": { "Variables": { SINK_VARIABLE: sink_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read.string("State").as_deref() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `SendMessage` of `body` to the queue `url` addresses.
async fn send_message(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, sent) = call_sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SendMessage to answer 200, got {status} {sent}"
    );
}

/// Every body the queue `url` addresses hands over to ONE long poll, left on the queue for the
/// next look (`VisibilityTimeout` 0), which is how a client watches for a relay.
async fn handed_over(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, received) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": LONG_POLL_SECONDS,
            "VisibilityTimeout": 0,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage to answer 200, got {status} {received}"
    );
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `Records` of a relayed envelope, or `None` for a body that is not one.
fn relayed_records(body: &str) -> Option<Vec<Value>> {
    let envelope: Value = serde_json::from_str(body).ok()?;
    envelope["Records"].as_array().cloned()
}

/// Look at the sink until it holds an envelope carrying `marker` as a record's body, and answer
/// that envelope's records.
async fn relayed_envelope(
    router: &MultiServiceRouter,
    case: &str,
    sink_url: &str,
    uuid: &str,
    marker: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let bodies = handed_over(router, case, sink_url).await;
        let relayed = bodies.iter().filter_map(|body| relayed_records(body)).find(
            |records| {
                records
                    .iter()
                    .any(|record| record["body"].as_str() == Some(marker))
            },
        );
        if let Some(records) = relayed {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must hand its function the message it polled and the \
             handler relays what it was handed, so the sink must come to hold an envelope \
             carrying {marker:?}; it handed over {bodies:?}, and the mapping reads {}",
            get_mapping(router, uuid).await.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The whole wiring one case drives: two queues with their DEFAULT attributes, the
/// least-privilege execution role IAM persisted, the relay out of a real .zip running as it, and
/// a mapping reading the source in batches of `batch_size` — answered once that mapping reports
/// the state it polls in.
///
/// Answers the source's url and ARN, the sink's url, and the mapping's UUID.
async fn given_relay(
    router: &MultiServiceRouter,
    case: &str,
    batch_size: i64,
) -> (String, String, String, String) {
    let (source_url, source_arn) =
        create_queue(router, case, &format!("{case}-source"), &json!({})).await;
    let (sink_url, sink_arn) = create_queue(router, case, &format!("{case}-sink"), &json!({})).await;
    // Exactly the grant AWS's own managed execution-role policy for a queue source carries, on
    // this source alone, plus the one send the handler itself makes and the log writes every
    // execution role carries. Nothing else: a relay that needed a further grant would not be
    // made here.
    let role_arn = given_role(
        router,
        case,
        &format!("{case}-relay"),
        Some(json!([
            allow("sqs", &QUEUE_READS, &source_arn),
            allow("sqs", &["SendMessage"], &sink_arn),
            logs_only(),
        ])),
    )
    .await;
    let function = format!("{case}-relay");
    given_relay_function(router, case, &function, &role_arn, &sink_url).await;

    let created = create_mapping(
        router,
        &json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": batch_size,
        }),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
    await_mapping_state(router, case, &uuid, STATE_ENABLED).await;
    (source_url, source_arn, sink_url, uuid)
}

/// One row: the `BatchSize` the mapping reads in, and the bodies waiting on the source when it
/// is polling.
struct Row {
    case: &'static str,
    batch_size: i64,
    bodies: &'static [&'static str],
}

/// RULE: a mapping reporting `Enabled` over a queue receives from it as the function's
/// execution role and invokes that function with the batch it took, so the handler's own write
/// to a second queue really happens.
#[tokio::test]
async fn a_queue_mapping_relays_what_it_polled_through_its_running_handler() {
    let rows = [
        Row {
            case: "one-message-per-batch",
            batch_size: 1,
            bodies: &["before-one"],
        },
        Row {
            case: "a-batch-ceiling-above-what-is-waiting",
            batch_size: 10,
            bodies: &["before-a", "before-b"],
        },
    ];
    let router = emulator();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn, sink_url, uuid) =
            given_relay(&router, case, row.batch_size).await;

        for body in row.bodies {
            send_message(&router, case, &source_url, body).await;
        }
        for body in row.bodies {
            let records = relayed_envelope(&router, case, &sink_url, &uuid, body).await;
            let record = records
                .iter()
                .find(|record| record["body"].as_str() == Some(body))
                .unwrap_or_else(|| panic!("case {case:?}: no record of {body:?} in {records:?}"));
            assert_eq!(
                (
                    record["eventSource"].as_str(),
                    record["eventSourceARN"].as_str(),
                ),
                (Some(EVENT_SOURCE), Some(source_arn.as_str())),
                "case {case:?}: the record must be about the message that was sent, on this \
                 source, got {record}"
            );
            assert!(
                records.len() <= usize::try_from(row.batch_size).unwrap_or(1),
                "case {case:?}: a batch holds at most BatchSize records, got {}",
                records.len()
            );
        }
    }
}

/// RULE's negative half: from the instant the delete is accepted the mapping polls no more, and
/// it is no mapping a further call may act on.
#[tokio::test]
async fn a_deleted_queue_mapping_relays_nothing_further() {
    let case = "a-relay-whose-mapping-is-deleted";
    let router = emulator();
    let (source_url, _source_arn, sink_url, uuid) = given_relay(&router, case, 1).await;

    // The positive control: while the mapping is held it consumes, so the message reaches the
    // running handler and the envelope it was handed is relayed.
    send_message(&router, case, &source_url, BEFORE_THE_DELETE).await;
    relayed_envelope(&router, case, &sink_url, &uuid, BEFORE_THE_DELETE).await;

    let deleted = call(
        &router,
        "DELETE",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        (deleted.status, deleted.string("State").as_deref()),
        (202, Some(STATE_DELETING)),
        "case {case:?}: the delete must answer the mapping as being deleted, got {} {}",
        deleted.status,
        deleted.text()
    );

    // A mapping the service no longer holds is no mapping a further call may act on.
    let refused = call(
        &router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "BatchSize": 2 }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code().contains(RESOURCE_NOT_FOUND)),
        (404, true),
        "case {case:?}: UpdateEventSourceMapping on the deleted mapping must answer 404 \
         {RESOURCE_NOT_FOUND}, got {} {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refusal_message(&refused).as_deref(),
        Some(NOT_FOUND_MESSAGE),
        "case {case:?}: that refusal carries the whole fixed sentence of its class, got {}",
        refused.text()
    );

    // And nothing is consumed from here on: the message sent now is neither relayed nor taken
    // out of the source, over a window of many poll intervals.
    send_message(&router, case, &source_url, AFTER_THE_DELETE).await;
    let quiet_until = Instant::now() + QUIET_WINDOW;
    loop {
        let relayed: Vec<String> = handed_over(&router, case, &sink_url)
            .await
            .iter()
            .filter_map(|body| relayed_records(body))
            .flat_map(|records| {
                records
                    .iter()
                    .map(|record| record["body"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            relayed,
            vec![BEFORE_THE_DELETE.to_string()],
            "case {case:?}: a deleted mapping polls nothing, so the sink can hold no record \
             beyond the one relayed while the mapping was held, got {relayed:?}"
        );
        if Instant::now() >= quiet_until {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // The message nothing polled is still the source queue's to hand over, which is what makes
    // the absence a withholding rather than a loss.
    let waiting = handed_over(&router, case, &source_url).await;
    assert_eq!(
        waiting,
        vec![AFTER_THE_DELETE.to_string()],
        "case {case:?}: the message sent after the delete must still be receivable on the \
         source, got {waiting:?}"
    );
}
