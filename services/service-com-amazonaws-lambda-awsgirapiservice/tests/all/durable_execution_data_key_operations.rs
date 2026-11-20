// SPDX-License-Identifier: CC0-1.0

//! What EVERY durable-execution operation that handles an execution's DATA says when the key
//! that data is encrypted under cannot be used, and that such a refusal changes nothing.
//!
//! The rule under test: an operation that reads or writes a durable execution's data decrypts
//! or encrypts it under the key the function's own `DurableConfig` names, so when the key
//! service reports that key unusable the call is refused with the modelled 502 for the class
//! of failure that service reported — stating Lambda's own fixed sentence for the data it
//! could not access and then RELAYING the key service's answer, the exception class it stated
//! the refusal under followed by the sentence it stated about the key. Every word of that
//! message is fixed for the class; the only variable part is the key ARN inside the relayed
//! clause, which is the key the read function's own configuration carries.
//!
//! The suites beside this one assert that wording for the two READS of an execution's data.
//! The rows here differ along what else the rule generalises over: WHICH operation on the data
//! is refused — the two reads, the history, and `CheckpointDurableExecution`, the one that
//! WRITES data, carrying the execution's own current checkpoint token so nothing but the key
//! could refuse it — and the state the key is in (disabled, scheduled for deletion), which is
//! what decides the class the refusal relays. Each refusal is compared whole.
//!
//! A refusal of this class is a refusal to touch the data at all, so each row also asserts
//! what it left behind: the execution's current checkpoint token is still current, its history
//! is exactly as it was, the metadata-only reads (which decrypt nothing) are still served, and
//! the very checkpoint that was refused is applied once the key can be used again.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_STATUS_RUNNING;
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_checkpoints::{checkpoint, current_token, start_execution};
use crate::durable_execution_completion::{
    created_key, kms_call, message_of, read_execution, read_history, router_with_key_service,
    DATA_DISABLED_SENTENCE, DATA_INVALID_STATE_SENTENCE,
};
use crate::durable_execution_data_key_message::data_reads;
use crate::function_lifecycle::{create_function, package_bytes, zip_file, Answer, ROLE};

/// The `DurableConfig$ExecutionTimeout` the function here runs under, in seconds: far longer
/// than the suite takes, so nothing times out under it.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// One row of the key-state table: the state the key is moved into, and how every operation on
/// the execution's data must then be refused.
struct KeyState {
    /// What makes this row its own case, and names its execution.
    case: &'static str,
    /// The key-service operation that puts the key into this row's state, with the pending
    /// window `ScheduleKeyDeletion` takes where that operation takes one.
    unusable: (&'static str, Option<i64>),
    /// The key-service operations that make the key usable again, in order.
    restore: &'static [&'static str],
    /// The modelled 502 every data-bearing operation must be refused with.
    refusal: &'static str,
    /// Lambda's own fixed sentence for the data it could not access, for this class.
    sentence: &'static str,
    /// The exception class the key service stated the refusal under, which Lambda relays.
    kms_code: &'static str,
    /// What the key service said about the key, after the key's own ARN.
    key_reason: &'static str,
}

/// RULE: every operation that handles a durable execution's data is refused, while the key
/// that data is encrypted under cannot be used, with the modelled 502 for the class of failure
/// the key service reported — Lambda's own fixed sentence for the data it could not access,
/// then the key service's own exception class and sentence about the key — and such a refusal
/// touches none of the execution's state.
#[tokio::test]
async fn every_operation_on_execution_data_is_refused_with_the_key_services_relayed_answer() {
    let rows = [
        KeyState {
            case: "a disabled key",
            unusable: ("DisableKey", None),
            restore: &["EnableKey"],
            refusal: "KMSDisabledException",
            sentence: DATA_DISABLED_SENTENCE,
            // The exception class the key service states a refusal of a disabled key under
            // (`smithy_query(query="operation", operation="Decrypt")` of the key service's
            // model: `DisabledException`), and how it words the key's own state.
            kms_code: "DisabledException",
            key_reason: "is disabled.",
        },
        KeyState {
            case: "a key scheduled for deletion",
            // The floor of `ScheduleKeyDeletionRequest$PendingWindowInDays`
            // (`@range(min: 7, max: 30)`).
            unusable: ("ScheduleKeyDeletion", Some(7)),
            // Cancelling the deletion leaves the key DISABLED, so it takes both calls to make
            // it usable again.
            restore: &["CancelKeyDeletion", "EnableKey"],
            refusal: "KMSInvalidStateException",
            sentence: DATA_INVALID_STATE_SENTENCE,
            kms_code: "KMSInvalidStateException",
            key_reason: "is pending deletion.",
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);
    let key_arn = created_key(&router).await;
    let function = "durable-data-key-operations";
    create_encrypted_durable_function(&router, function, &key_arn).await;

    for (index, row) in rows.iter().enumerate() {
        let name = format!("exec-data-key-operations-{index}");
        let arn = start_execution(&engine, function, &name, br#"{"amount":7}"#);
        // The token the Lambda runtime driving this execution holds: the only token a
        // checkpoint of it is applied against, so a checkpoint carrying it is refused below
        // for its key and for nothing else.
        let token = current_token(&engine, function, &name).unwrap_or_else(|| {
            panic!(
                "case {:?}: a running execution must hold a current checkpoint token",
                row.case
            )
        });
        let events_before = history_events(&router, &arn).await;

        let (operation, window) = row.unusable;
        let mut body = json!({ "KeyId": key_arn });
        if let Some(days) = window {
            body["PendingWindowInDays"] = json!(days);
        }
        let answered = kms_call(&router, operation, &body).await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected 200 from {operation}, got {} {}",
            row.case,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );

        // Lambda's own fixed sentence for this class, then the key service's answer: the class
        // it stated and the sentence it stated about this function's key.
        let expected = format!(
            "{} KMS Exception: {} KMS Message: {key_arn} {}",
            row.sentence, row.kms_code, row.key_reason
        );
        for (label, answer) in data_operations(&router, &arn, &token).await {
            let case = format!("{} through {label}", row.case);
            assert_eq!(
                answer.status,
                502,
                "case {case:?}: expected the modelled 502, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                row.refusal,
                "case {case:?}: expected {}, got {}",
                row.refusal,
                answer.error_code()
            );
            assert_eq!(
                message_of(&answer),
                expected,
                "case {case:?}: expected the refusal to state the data it could not access and \
                 relay the key service's answer about the function's own key, got {:?}",
                message_of(&answer)
            );
        }

        // A refusal of this class touched no data, so it consumed no token and recorded no
        // event.
        assert_eq!(
            current_token(&engine, function, &name).as_deref(),
            Some(token.as_str()),
            "case {:?}: a refused checkpoint must leave the token it carried current, got {:?}",
            row.case,
            current_token(&engine, function, &name)
        );
        assert_eq!(
            history_events(&router, &arn).await,
            events_before,
            "case {:?}: a refused operation must leave the history exactly as it was, which \
             was {events_before:?}",
            row.case
        );

        // The metadata-only read decrypts nothing, so it is served whatever state that key is
        // in — with the status the execution is in, no data, and the configured key.
        let compact = read_execution(&router, &arn, "?IncludeExecutionData=false").await;
        assert_eq!(
            compact.status,
            200,
            "case {:?}: expected the metadata-only read to be served, got {} {}",
            row.case,
            compact.status,
            String::from_utf8_lossy(&compact.body)
        );
        let metadata = compact.json();
        assert_eq!(
            metadata.get("Status").and_then(Value::as_str),
            Some(EXECUTION_STATUS_RUNNING),
            "case {:?}: expected the metadata-only read to report {EXECUTION_STATUS_RUNNING}, \
             got {metadata}",
            row.case
        );
        assert_eq!(
            metadata
                .get("ExecutionDataIncluded")
                .and_then(Value::as_bool),
            Some(false),
            "case {:?}: expected ExecutionDataIncluded false, got {metadata}",
            row.case
        );
        assert_eq!(
            metadata
                .pointer("/DurableConfig/KMSKeyArn")
                .and_then(Value::as_str),
            Some(key_arn.as_str()),
            "case {:?}: expected the configured key to stay reported, got {metadata}",
            row.case
        );

        // Nothing about the refusal is recorded: the key is asked at each call, so the very
        // checkpoint that was refused is applied once the key can be used again, against the
        // token it carried then.
        for operation in row.restore {
            let answered = kms_call(&router, operation, &json!({ "KeyId": key_arn })).await;
            assert_eq!(
                answered.status,
                200,
                "case {:?}: expected 200 from {operation}, got {} {}",
                row.case,
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
        }
        let applied = checkpoint(&router, &arn, &checkpoint_body(&token)).await;
        assert_eq!(
            applied.status,
            200,
            "case {:?}: expected the refused checkpoint to be applied once the key is usable \
             again, got {} {}",
            row.case,
            applied.status,
            String::from_utf8_lossy(&applied.body)
        );
        let rotated = applied.string("CheckpointToken").unwrap_or_else(|| {
            panic!(
                "case {:?}: an applied checkpoint must answer the next token, got {}",
                row.case,
                String::from_utf8_lossy(&applied.body)
            )
        });
        assert_ne!(
            rotated, token,
            "case {:?}: an applied checkpoint must consume the token it carried",
            row.case
        );
        let served = read_execution(&router, &arn, "?IncludeExecutionData=true").await;
        assert_eq!(
            served.status,
            200,
            "case {:?}: expected the data read to be served once the key is usable again, got \
             {} {}",
            row.case,
            served.status,
            String::from_utf8_lossy(&served.body)
        );
        assert_eq!(
            history_events(&router, &arn).await.len(),
            events_before.len() + 1,
            "case {:?}: expected the applied checkpoint to record its one update, got {:?}",
            row.case,
            history_events(&router, &arn).await
        );
    }
}

/// Every operation that handles ONE execution's data: the three reads that ask for it (the
/// axis the suite beside this one defines), and the checkpoint that WRITES data — each
/// labelled by how it asked.
async fn data_operations(
    router: &MultiServiceRouter,
    arn: &str,
    token: &str,
) -> Vec<(&'static str, Answer)> {
    let mut operations = data_reads(router, arn).await;
    operations.push((
        "CheckpointDurableExecution, carrying the execution's current token",
        checkpoint(router, arn, &checkpoint_body(token)).await,
    ));
    operations
}

/// One checkpoint carrying `token` and the single update whose data it would store.
fn checkpoint_body(token: &str) -> Value {
    json!({
        "CheckpointToken": token,
        "Updates": [{ "Id": "step-1", "Type": "STEP", "Action": "START" }],
    })
}

/// The `EventType` of every event the metadata-only history of `arn` reports, in order — the
/// read that decrypts nothing, so it is served whatever state the key is in.
async fn history_events(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let answered = read_history(router, arn, "?IncludeExecutionData=false").await;
    assert_eq!(
        answered.status,
        200,
        "the metadata-only history must be served, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    answered
        .json()
        .get("Events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    event
                        .get("EventType")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A durable function whose execution data is encrypted under `key_arn`. Nothing here invokes
/// it, so its package needs no runnable handler: its executions are started through the state
/// the `Invoke` handler registers one in.
async fn create_encrypted_durable_function(
    router: &MultiServiceRouter,
    name: &str,
    key_arn: &str,
) {
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "DurableConfig": { "KMSKeyArn": key_arn, "ExecutionTimeout": EXECUTION_TIMEOUT },
            "Code": { "ZipFile": zip_file(&package_bytes(64)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup {name:?}: expected 201 from CreateFunction, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}
