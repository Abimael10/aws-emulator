// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DELIVERY CHAIN of a queue event source mapping and the execution
//! environment its first delivery runs in: `CreateQueue`, `CreateFunction`,
//! `CreateEventSourceMapping` polled until the mapping reports `Enabled`, `SendMessage`, and
//! the handler's own forward read back off a sink queue.
//!
//! The rule under test is the one [`crate::pre_provisioned_execution_environments`] pins over
//! `Invoke`, read through the mapping's own data plane instead: the half of an execution
//! environment that reads no function's configuration is stood up while the function is being
//! PROVISIONED, and the first invocation of that function — whoever makes it, a caller or a
//! mapping's poller — adopts what is standing rather than building an interpreter on its own
//! path. The records the handler is handed are unchanged either way, which is what the
//! forward read back off the sink shows.
//!
//! Nothing here asserts a SPAN: what is read is which environment served the delivery
//! ([`warm_pool::adopted_blanks`]) and what the handler was handed, both of which are facts
//! rather than timings.
//!
//! The suite RUNS a handler, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::warm_pool;

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_queue, create_running_function, forwarded_records,
    router_with_queues, send_message, STATE_ENABLED,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// How long this suite gives the sink queue to hand over the record the delivery forwarded.
/// Patience rather than a weakened expectation: the assertions below are unchanged by it.
const SINK_BUDGET: Duration = Duration::from_secs(30);

/// How long a poll may look for a condition before the case gives up — generous, so a loaded
/// host cannot turn a condition that holds into a failure.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The handler the mapping's deliveries run: it forwards every record it is handed to the
/// queue its environment names, so what this suite reads back is the record a running handler
/// was given.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3

SINK_URL = os.environ["SINK_URL"]


def lambda_handler(event, context):
    client = boto3.client("sqs")
    forwarded = []
    for record in event["Records"]:
        client.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(record))
        forwarded.append(record["messageId"])
    return {"forwarded": forwarded}
"#;

/// Wait until an execution environment stands ready for the next invocation of this process:
/// one is waiting, or one has been adopted since `adopted_before` — which is what becomes of a
/// waiting one.
async fn stands_ready_for(case: &str, adopted_before: u64) {
    let give_up = Instant::now() + POLL_DEADLINE;
    loop {
        if warm_pool::standing_blanks() > 0 || warm_pool::adopted_blanks() > adopted_before {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "case {case:?}: expected the provisioning of the function to leave an execution \
             environment standing for its first delivery, got none waiting and none adopted \
             past {adopted_before}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the first delivery a mapping makes is served in the execution environment the
/// function's provisioning stood up, and the record the handler is handed is the record the
/// queue held.
#[tokio::test]
async fn a_mappings_first_delivery_is_served_in_a_standing_environment() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = router_with_queues();
    let case = "the first delivery of a queue mapping";
    let (source_url, source_arn) =
        create_queue(&router, case, "standing-environment-source", &json!({})).await;
    let (sink_url, _) = create_queue(&router, case, "standing-environment-sink", &json!({})).await;
    let adopted_before = warm_pool::adopted_blanks();
    create_running_function(
        &router,
        case,
        "standing-environment-fn",
        FORWARDING_HANDLER,
        30,
        &json!({ "SINK_URL": sink_url }),
    )
    .await;
    // The provisioning window the create opened is what the environment is stood up in, so one
    // stands ready before anything is delivered to the function.
    stands_ready_for(case, adopted_before).await;
    let created = create_mapping(
        &router,
        &json!({
            "FunctionName": "standing-environment-fn",
            "EventSourceArn": source_arn,
            "Enabled": true,
        }),
    )
    .await;
    let uuid = created.string("UUID").unwrap_or_else(|| {
        panic!(
            "case {case:?}: the create must report the mapping's UUID, got {}",
            created.text()
        )
    });
    await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

    let body = r#"{"kind":"wanted"}"#;
    send_message(&router, case, &source_url, body, None).await;
    let records = forwarded_records(&router, &sink_url, 1, SINK_BUDGET).await;
    let forwarded = records.get(body);
    assert!(
        forwarded.is_some(),
        "case {case:?}: expected the handler to be handed the message the queue held and to \
         forward it, got {records:?}"
    );
    // The delivery was served in an environment that was already standing: this process has
    // adopted one more than it had before the function was created.
    let adopted = warm_pool::adopted_blanks();
    assert!(
        adopted > adopted_before,
        "case {case:?}: expected the first delivery to adopt an execution environment that was \
         already standing, so more than {adopted_before} to have been adopted, got {adopted}"
    );
}
