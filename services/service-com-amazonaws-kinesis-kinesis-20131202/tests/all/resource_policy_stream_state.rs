// SPDX-License-Identifier: CC0-1.0

//! The stream-state PRECONDITION of the resource-policy surface, end to end over the wire:
//! `PutResourcePolicy`, `GetResourcePolicy` and `DeleteResourcePolicy` against a stream that
//! is not available for the operation.
//!
//! RULE: a resource-policy operation addressing a Kinesis data stream — the stream itself, or
//! a consumer registered with it — requires that stream to be in a state the operation is
//! available in, which is `ACTIVE` or the `UPDATING` a change still in flight leaves it in;
//! any other state is refused with the modelled `ResourceInUseException` ("The resource is not
//! available for this operation. For successful operation, the resource must be in the ACTIVE
//! state") carrying ONE sentence that names the stream by the `StreamName` it is stored under,
//! the account that holds it and the state it is in instead. The gate stands BEFORE the policy
//! store is read or written, so a refused call reports neither the presence nor the absence of
//! a policy, and leaves the stored slot exactly as it found it.
//!
//! The recorded flows these were derived from call each of the three operations once, on ONE
//! stream in `CREATING`, addressed by its stream ARN — each one row of the tables below. The
//! rows differ along what the rule generalises over: the OPERATION called, the lifecycle STATE
//! the stream is in (every value `StreamStatus` takes: the two that refuse, and the two that
//! serve), the ARN FORM the request addresses the resource by (the stream's own ARN and a
//! consumer's), and whether the stream carries a policy when the call arrives.
//!
//! Because the gate is the stream's state AT THE TIME OF THE CALL, the refusal is transient: a
//! client that keeps looking is served as soon as the transition the create armed falls due,
//! and the very same call refuses again once a removal is accepted. Both halves are driven
//! here too, through the READ — the operation whose refusal a client uses as the readiness
//! signal to retry on.
//!
//! The wire harness, the ARN builders and the policy helpers are [`crate::stream_tags`]' and
//! [`crate::resource_policies`]', reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    STREAM_ACTIVE, STREAM_CREATING, STREAM_DELETING, STREAM_UPDATING,
};

use crate::resource_policies::{account, assert_refused, delete, get, policy_for, put, reported_policy};
use crate::resource_policy_absence::EMPTY_POLICY_DOCUMENT;
use crate::stream_tags::{poll_until, poll_until_active, stream_arn, Emulator, REGION};

/// The modelled error every one of these refusals is carried in.
const IN_USE: &str = "ResourceInUseException";

/// The three operations the precondition stands in front of, with the body each one addresses
/// `arn` with. Every body is well formed, so what refuses the call is the stream's state and
/// nothing else.
fn body_for(operation: &str, arn: &str) -> Value {
    match operation {
        "GetResourcePolicy" | "DeleteResourcePolicy" => json!({ "ResourceARN": arn }),
        "PutResourcePolicy" => json!({
            "ResourceARN": arn,
            "Policy": policy_for(arn, "Readers", "kinesis:GetRecords"),
        }),
        other => panic!("case {other}: this suite builds no body for that operation"),
    }
}

/// The operations the rule covers, in the order a client uses them.
const POLICY_OPERATIONS: &[&str] = &["PutResourcePolicy", "GetResourcePolicy", "DeleteResourcePolicy"];

/// The sentence this service states for a stream that is not available for a resource-policy
/// operation, written out here rather than borrowed from the crate so the wording itself is
/// what the cases pin.
fn not_ready_sentence(stream_name: &str, status: &str) -> String {
    format!(
        "Stream {stream_name} under account {} not {STREAM_ACTIVE} or {STREAM_UPDATING}, \
         instead in state {status}",
        account()
    )
}

/// Create a provisioned stream of one shard named `name` without waiting for the transition
/// the create armed: the stream is `CREATING` when this returns.
async fn creating_stream(emulator: &Emulator, name: &str) {
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
}

/// Call `operation` against `arn` and assert it was refused with the one sentence for `status`.
async fn assert_refused_with_the_sentence(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    arn: &str,
    stream_name: &str,
    status: &str,
) {
    let (http_status, response) = emulator.call(operation, &body_for(operation, arn)).await;
    assert_refused(
        case,
        http_status,
        &response,
        IN_USE,
        &not_ready_sentence(stream_name, status),
    );
}

/// The `StreamStatus` the stream named `name` reports, so a case asserts the state it claims to
/// have placed the stream in before it calls the operation under test.
async fn reported_status(emulator: &Emulator, case: &str, name: &str) -> String {
    let summary = emulator
        .ok(case, "DescribeStreamSummary", &json!({ "StreamName": name }))
        .await;
    summary
        .get("StreamDescriptionSummary")
        .and_then(|description| description.get("StreamStatus"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// What the store holds in the policy slot of `arn`: `Some(document)` for a resource carrying
/// one, `None` for a resource carrying none. Read from the very store the handlers write, so a
/// refused call's effect on it — there must be none — is what a case can assert.
fn stored_policy(emulator: &Emulator, case: &str, arn: &str) -> Option<String> {
    emulator
        .store()
        .lock()
        .resource_policy_by_arn(std::time::SystemTime::now(), REGION, arn)
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: this account and region must still hold {arn}"))
}

/// A stream whose create has not fallen due yet refuses every one of the three operations,
/// with the one sentence naming the stored name, this account and `CREATING` — and the refused
/// attach writes nothing, so the stream carries no policy once it is ready.
#[tokio::test]
async fn a_creating_stream_refuses_every_resource_policy_operation() {
    let emulator = Emulator::new();

    for operation in POLICY_OPERATIONS {
        let name = format!("creating-{}", operation.to_lowercase());
        // Created and called straight away, so the stream is still inside the transition its
        // create armed when the call arrives.
        creating_stream(&emulator, &name).await;
        let arn = stream_arn(REGION, &name);
        assert_refused_with_the_sentence(
            &emulator,
            &name,
            operation,
            &arn,
            &name,
            STREAM_CREATING,
        )
        .await;
        assert_eq!(
            stored_policy(&emulator, &name, &arn),
            None,
            "case {name}: a refused {operation} must leave the stream carrying no policy"
        );
    }

    // ... and once every one of those streams is ready, the read is served and reports the
    // empty document: nothing was attached behind the refusals.
    for operation in POLICY_OPERATIONS {
        let name = format!("creating-{}", operation.to_lowercase());
        poll_until_active(&emulator, &name, &name).await;
        let arn = stream_arn(REGION, &name);
        let (status, read) = get(&emulator, REGION, &arn).await;
        assert_eq!(
            reported_policy(&name, status, &read),
            EMPTY_POLICY_DOCUMENT,
            "case {name}: expected the empty document for a stream nothing attached a policy \
             to, got {read}"
        );
    }
}

/// A stream whose removal has been accepted refuses every one of the three operations with the
/// same sentence, naming the state it reports — even though it DOES carry a policy, which is
/// what shows the gate precedes the policy lookup: the refusal is the state's, not an absence,
/// and the document the stream carries is still there afterwards.
#[tokio::test]
async fn a_deleting_stream_refuses_every_resource_policy_operation() {
    let emulator = Emulator::new();

    let names: Vec<String> = POLICY_OPERATIONS
        .iter()
        .map(|operation| format!("deleting-{}", operation.to_lowercase()))
        .collect();
    for name in &names {
        creating_stream(&emulator, name).await;
    }
    for name in &names {
        poll_until_active(&emulator, name, name).await;
    }

    for (operation, name) in POLICY_OPERATIONS.iter().zip(&names) {
        let arn = stream_arn(REGION, name);
        let attached = policy_for(&arn, "Attached", "kinesis:DescribeStreamSummary");
        let (status, response) = put(&emulator, REGION, &arn, &attached).await;
        assert_eq!(
            status, 200,
            "case {name}: the attach on the ACTIVE stream must be served, got {status} with \
             body {response}"
        );

        // The removal is armed and the call arrives before it falls due, so the stream is
        // `DELETING` rather than gone.
        emulator
            .ok(name, "DeleteStream", &json!({ "StreamName": name }))
            .await;
        assert_refused_with_the_sentence(
            &emulator,
            name,
            operation,
            &arn,
            name,
            STREAM_DELETING,
        )
        .await;
        assert_eq!(
            stored_policy(&emulator, name, &arn).as_deref(),
            Some(attached.as_str()),
            "case {name}: a refused {operation} must leave the attached document alone"
        );
    }
}

/// A CONSUMER's policy is addressed through the stream that holds it, so the same gate stands
/// in front of a `ConsumerARN`: the refusal names the STREAM it is registered with, by the
/// stored stream name rather than by the ARN the request carried.
#[tokio::test]
async fn a_consumer_arn_is_gated_by_the_stream_that_holds_it() {
    let emulator = Emulator::new();
    let name = "consumer-arn-gate";
    creating_stream(&emulator, name).await;
    poll_until_active(&emulator, name, name).await;
    let stream = stream_arn(REGION, name);

    let registered = emulator
        .ok(
            name,
            "RegisterStreamConsumer",
            &json!({ "StreamARN": stream, "ConsumerName": "gated-consumer" }),
        )
        .await;
    let consumer = registered
        .get("Consumer")
        .and_then(|consumer| consumer.get("ConsumerARN"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the registration must carry a ConsumerARN, got {registered}"))
        .to_string();

    // While the stream serves, the consumer's own policy round-trips.
    let attached = policy_for(&consumer, "ConsumerReaders", "kinesis:SubscribeToShard");
    let (status, response) = put(&emulator, REGION, &consumer, &attached).await;
    assert_eq!(
        status, 200,
        "the attach on the consumer of an ACTIVE stream must be served, got {status} with body \
         {response}"
    );

    // Its stream's removal is accepted, and from then on every one of the three operations is
    // refused for the consumer too, with the sentence the STREAM's state states. The removal
    // carries `EnforceConsumerDeletion`, since the stream has a consumer registered with it.
    emulator
        .ok(
            name,
            "DeleteStream",
            &json!({ "StreamName": name, "EnforceConsumerDeletion": true }),
        )
        .await;
    for operation in POLICY_OPERATIONS {
        let case = format!("{operation} on a consumer of a deleting stream");
        let (http_status, response) = emulator
            .call(operation, &body_for(operation, &consumer))
            .await;
        assert_refused(
            &case,
            http_status,
            &response,
            IN_USE,
            &not_ready_sentence(name, STREAM_DELETING),
        );
        let message = response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !message.contains(&consumer),
            "case {case}: the sentence names the stored stream, not the ARN the request used, \
             got {message}"
        );
    }
}

/// The states that SERVE: `ACTIVE`, and the `UPDATING` a change still in flight leaves the
/// stream in ("You can continue to read and write data to your stream while its status is
/// UPDATING"). All three operations are served in both, and the document round-trips byte for
/// byte through each.
#[tokio::test]
async fn an_active_or_updating_stream_serves_every_resource_policy_operation() {
    let emulator = Emulator::new();
    let name = "serving-states";
    // Two shards, so the shard-count change below has somewhere to go.
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 2 }),
        )
        .await;
    poll_until_active(&emulator, name, name).await;
    let arn = stream_arn(REGION, name);

    for round in [STREAM_ACTIVE, STREAM_UPDATING] {
        if round == STREAM_UPDATING {
            // A shard-count change leaves the stream `UPDATING` until the re-tiling it armed
            // falls due.
            emulator
                .ok(
                    name,
                    "UpdateShardCount",
                    &json!({
                        "StreamName": name,
                        "TargetShardCount": 4,
                        "ScalingType": "UNIFORM_SCALING",
                    }),
                )
                .await;
        }

        // The state the case claims to be in is the state the stream reports.
        let reported = reported_status(&emulator, round, name).await;
        assert_eq!(
            reported, round,
            "case {round}: expected the stream to be in that state when the calls arrive, got \
             {reported}"
        );

        // Attached, reported byte for byte, and detached — every one served.
        let attached = policy_for(&arn, round, "kinesis:GetRecords");
        let (status, response) = put(&emulator, REGION, &arn, &attached).await;
        assert_eq!(
            status, 200,
            "case {round}: expected the attach to be served, got {status} with body {response}"
        );
        let (status, read) = get(&emulator, REGION, &arn).await;
        assert_eq!(
            reported_policy(round, status, &read),
            attached,
            "case {round}: expected the document the attach carried, got {read}"
        );
        let (status, response) = delete(&emulator, REGION, &arn).await;
        assert_eq!(
            status, 200,
            "case {round}: expected the detach to be served, got {status} with body {response}"
        );
    }
}

/// Every value `StreamStatus` takes, beside whether a resource-policy call is available for a
/// stream in it. The catalog is the model's own
/// (`smithy_query(query="shape",shape="StreamStatus")` -> `CREATING`, `DELETING`, `ACTIVE`,
/// `UPDATING`), so a state this service grows a stream into later has no row here and fails
/// the table rather than passing unnoticed.
const STATUS_CATALOG: &[(&str, bool)] = &[
    (STREAM_CREATING, false),
    (STREAM_DELETING, false),
    (STREAM_ACTIVE, true),
    (STREAM_UPDATING, true),
];

/// Bring the stream named `name` into `status`, attaching a policy to it wherever the attach is
/// available in that state, and answer its ARN beside the document it ends up carrying.
///
/// A transitional state is reached by ARMING the transition and returning straight away, so the
/// stream is inside it when the caller's request arrives. The rows that end up refusing are set
/// up with their policy slot OCCUPIED wherever an attach can reach it, which is what makes the
/// table say the state decides the call rather than the slot.
async fn stream_in_status(
    emulator: &Emulator,
    name: &str,
    status: &str,
) -> (String, Option<String>) {
    // Two shards, so the shard-count change the `UPDATING` row arms has somewhere to go.
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 2 }),
        )
        .await;
    let arn = stream_arn(REGION, name);
    if status == STREAM_CREATING {
        return (arn, None);
    }
    poll_until_active(emulator, name, name).await;

    let attached = policy_for(&arn, status, "kinesis:GetRecords");
    let (http_status, response) = put(emulator, REGION, &arn, &attached).await;
    assert_eq!(
        http_status, 200,
        "case {name}: the attach on the ready stream must be served, got {http_status} with \
         body {response}"
    );

    match status {
        STREAM_ACTIVE => {}
        // A shard-count change leaves the stream `UPDATING` until the re-tiling falls due.
        STREAM_UPDATING => {
            emulator
                .ok(
                    name,
                    "UpdateShardCount",
                    &json!({
                        "StreamName": name,
                        "TargetShardCount": 4,
                        "ScalingType": "UNIFORM_SCALING",
                    }),
                )
                .await;
        }
        // An accepted removal leaves it `DELETING` until the release falls due.
        STREAM_DELETING => {
            emulator
                .ok(name, "DeleteStream", &json!({ "StreamName": name }))
                .await;
        }
        other => panic!("case {other}: this suite places no stream in that state"),
    }
    (arn, Some(attached))
}

/// The READ over the whole `StreamStatus` catalog: exactly the two states the operation is
/// available in report the stored document, and every other state the stream can be in is
/// refused with the one sentence — whether the slot holds a document (the removal rows) or
/// holds none (the create row), which is what shows the gate stands ahead of the policy lookup.
/// A refused read reports no `Policy` at all and leaves the slot exactly as it found it.
#[tokio::test]
async fn the_read_is_decided_by_the_streams_status_over_the_whole_catalog() {
    let emulator = Emulator::new();

    for (status, serves) in STATUS_CATALOG {
        let name = format!("read-in-{}", status.to_lowercase());
        let (arn, carried) = stream_in_status(&emulator, &name, status).await;

        // The state the row claims to have placed the stream in is the state it reports.
        let reported = reported_status(&emulator, &name, &name).await;
        assert_eq!(
            &reported, status,
            "case {name}: expected the stream to be in that state when the read arrives, got \
             {reported}"
        );

        let (http_status, response) = get(&emulator, REGION, &arn).await;
        if *serves {
            // A row the read is available for had its policy attached while the stream served,
            // so there IS a document to report: a row that forgot to attach one fails here
            // rather than silently comparing the empty document against itself.
            let expected = carried.clone().unwrap_or_else(|| {
                panic!("case {name}: a serving row must have attached a document to report")
            });
            assert_eq!(
                reported_policy(&name, http_status, &response),
                expected,
                "case {name}: expected the document the stream carries, got {response}"
            );
        } else {
            assert_refused(
                &name,
                http_status,
                &response,
                IN_USE,
                &not_ready_sentence(&name, status),
            );
            assert!(
                response.get("Policy").is_none(),
                "case {name}: a refused read must report no policy at all, got {response}"
            );
            assert_eq!(
                stored_policy(&emulator, &name, &arn),
                carried,
                "case {name}: a refused read must leave the stored slot as it found it"
            );
        }
    }
}

/// The refusal is the stream's state AT THE TIME OF THE CALL, which makes it the readiness
/// signal a client retries on: the read refuses while the create is still in flight, is served
/// once that transition falls due — reporting the policy the stream really carries, with none of
/// the error members the refusal carried — and refuses again the moment a removal is accepted,
/// with the attached document still stored behind the refusal.
#[tokio::test]
async fn a_refused_read_clears_when_the_stream_becomes_ready_and_returns_when_it_stops_being() {
    let emulator = Emulator::new();
    let name = "read-readiness-signal";
    creating_stream(&emulator, name).await;
    let arn = stream_arn(REGION, name);

    // Inside the transition the create armed: refused, with the modelled error rather than a
    // policy-shaped success.
    let (http_status, response) = get(&emulator, REGION, &arn).await;
    assert_refused(
        name,
        http_status,
        &response,
        IN_USE,
        &not_ready_sentence(name, STREAM_CREATING),
    );
    assert!(
        response.get("Policy").is_none(),
        "case {name}: a refused read must report no policy at all, got {response}"
    );

    // The retry loop a client runs against AWS: look again until the read stops refusing.
    poll_until(name, "the policy read stops refusing the stream", || async {
        get(&emulator, REGION, &arn).await.0 == 200
    })
    .await;

    // What it is served reports the policy the stream really carries — none, since every attach
    // inside that window was refused — and carries no error members.
    let (http_status, served) = get(&emulator, REGION, &arn).await;
    assert_eq!(
        reported_policy(name, http_status, &served),
        EMPTY_POLICY_DOCUMENT,
        "case {name}: expected the empty document for a stream carrying no policy, got {served}"
    );
    assert!(
        served.get("__type").is_none() && served.get("message").is_none(),
        "case {name}: a served read carries no error members, got {served}"
    );

    // An attach from here on round-trips byte for byte ...
    let attached = policy_for(&arn, "AfterReadiness", "kinesis:PutRecord");
    let (http_status, response) = put(&emulator, REGION, &arn, &attached).await;
    assert_eq!(
        http_status, 200,
        "case {name}: expected the attach to be served, got {http_status} with body {response}"
    );
    let (http_status, read) = get(&emulator, REGION, &arn).await;
    assert_eq!(
        reported_policy(name, http_status, &read),
        attached,
        "case {name}: expected the document the attach carried, got {read}"
    );

    // ... and the moment the stream leaves a serving state the very same read refuses again.
    emulator
        .ok(name, "DeleteStream", &json!({ "StreamName": name }))
        .await;
    let (http_status, response) = get(&emulator, REGION, &arn).await;
    assert_refused(
        name,
        http_status,
        &response,
        IN_USE,
        &not_ready_sentence(name, STREAM_DELETING),
    );
    assert_eq!(
        stored_policy(&emulator, name, &arn).as_deref(),
        Some(attached.as_str()),
        "case {name}: a refused read must leave the attached document alone"
    );
}
