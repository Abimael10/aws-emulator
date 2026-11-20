// SPDX-License-Identifier: CC0-1.0

//! What a REFUSED data-bearing read of a durable execution SAYS, and where the key it names
//! comes from.
//!
//! The rule under test: a read that reports a durable execution's data decrypts that data
//! under the key the function's own `DurableConfig` names, so when the key service reports
//! that key unusable the read is refused with the modelled 502 for the class of failure it
//! reported, stating Lambda's own fixed sentence for the data it could not access and then
//! RELAYING the key service's answer — the exception class that service stated the refusal
//! under and the sentence it stated about the key. Every word of that message is fixed for the
//! class; the only variable part is the key ARN, which is each function's OWN configured key.
//!
//! The suite beside this one (`durable_execution_completion.rs`) walks ONE key through the key
//! states. The rows here differ along the other thing the rule generalises over: WHICH
//! function is read — two functions, each encrypted under a key of its own, each put into a
//! different unusable state at the same time — so a refusal that named a key from anywhere but
//! the function being read (a constant, the other function's key, the request) could not
//! satisfy them both. Each row's message is compared whole, for the read that asks for the
//! data by default, the read that asks for it explicitly and the history that asks for it.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_completion::{
    create_durable_function, created_key, durable_result, invoke_durable, kms_call, message_of,
    publish_version, read_execution, read_history, result_payload, router_with_key_service,
    wait_until_active, DATA_DISABLED_SENTENCE, DATA_INVALID_STATE_SENTENCE,
    DURABLE_EXECUTION_ARN_HEADER,
};
use crate::function_lifecycle::Answer;

/// One row: a durable function encrypted under a key of its own, the key-service call that
/// makes that key unusable, and the refusal every data-bearing read of its execution must then
/// state.
struct Encrypted {
    /// What makes this row its own case, and names its function and execution.
    case: &'static str,
    /// The key-service operation that puts this row's key into its unusable state, and whether
    /// it carries the pending window `ScheduleKeyDeletion` takes.
    key_call: (&'static str, bool),
    /// The key-service operations that make the key usable again, in order.
    restore: &'static [&'static str],
    /// The modelled 502 a read that asks for the data must be refused with.
    refusal: &'static str,
    /// Lambda's own fixed sentence for the data it could not access.
    sentence: &'static str,
    /// The exception class the key service stated the refusal under, which the refusal relays.
    kms_code: &'static str,
    /// What the key service said about the key, after the key's own ARN.
    key_reason: &'static str,
}

/// A durable execution whose data is encrypted under one particular key.
struct Encryption {
    /// The row this execution belongs to.
    row: &'static Encrypted,
    /// The key this function's `DurableConfig` names — this account's own key, so it really is
    /// moved between states below.
    key_arn: String,
    /// The execution `Invoke` answered a handle on.
    execution_arn: String,
}

/// The rows: two functions, each with its own key, each key made unusable in a different way,
/// so the class the refusal relays and the key it names both vary from row to row.
const ROWS: &[Encrypted] = &[
    Encrypted {
        case: "a function whose key is disabled",
        key_call: ("DisableKey", false),
        restore: &["EnableKey"],
        refusal: "KMSDisabledException",
        sentence: DATA_DISABLED_SENTENCE,
        // The exception class the key service states a refusal of a disabled key under
        // (`smithy_query(query="operation", operation="Decrypt")` of the key service's model:
        // `DisabledException`), and how it words the key's own state.
        kms_code: "DisabledException",
        key_reason: "is disabled.",
    },
    Encrypted {
        case: "a function whose key is scheduled for deletion",
        key_call: ("ScheduleKeyDeletion", true),
        // Cancelling the deletion leaves the key DISABLED, so it takes both calls to make it
        // usable again.
        restore: &["CancelKeyDeletion", "EnableKey"],
        refusal: "KMSInvalidStateException",
        sentence: DATA_INVALID_STATE_SENTENCE,
        kms_code: "KMSInvalidStateException",
        key_reason: "is pending deletion.",
    },
];

#[tokio::test]
async fn a_refused_read_names_the_key_the_read_functions_durable_config_carries() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_key_service(&engine);

    let mut encryptions: Vec<Encryption> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let key_arn = created_key(&router).await;
        let name = format!("durable-key-identity-{index}");
        create_durable_function(&router, &name, &key_arn).await;
        wait_until_active(&engine, &router, row.case, &name, "$LATEST").await;
        let version = publish_version(&router, &name).await;
        wait_until_active(&engine, &router, row.case, &name, &version).await;

        let invoked = invoke_durable(
            &router,
            &format!("{name}:{version}"),
            "",
            &format!("exec-key-identity-{index}"),
        )
        .await;
        assert_eq!(
            invoked.status,
            200,
            "case {:?}: expected 200 from Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let execution_arn = invoked
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the invocation must answer an execution ARN",
                    row.case
                )
            });
        encryptions.push(Encryption {
            row,
            key_arn,
            execution_arn,
        });
    }

    // Each key is made unusable in its own way, and all of them before any read, so every
    // refusal below has both keys to choose its ARN from.
    for encryption in &encryptions {
        let (operation, with_window) = encryption.row.key_call;
        let mut body = json!({ "KeyId": encryption.key_arn });
        if with_window {
            // The floor of `ScheduleKeyDeletionRequest$PendingWindowInDays`
            // (`@range(min: 7, max: 30)`).
            body["PendingWindowInDays"] = json!(7);
        }
        let answered = kms_call(&router, operation, &body).await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected 200 from {operation}, got {} {}",
            encryption.row.case,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
    }

    for encryption in &encryptions {
        let row = encryption.row;
        // Lambda's own fixed sentence for this class, then the key service's answer: the class
        // it stated and the sentence it stated about THIS function's key.
        let expected = format!(
            "{} KMS Exception: {} KMS Message: {} {}",
            row.sentence, row.kms_code, encryption.key_arn, row.key_reason
        );
        for (read, answer) in data_reads(&router, &encryption.execution_arn).await {
            let case = format!("{} read by {read}", row.case);
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
                "case {case:?}: expected the refusal to state the data it could not access \
                 and relay the key service's answer about this function's own key, got {:?}",
                message_of(&answer)
            );
        }
    }

    // Nothing about the refusal is recorded: the key is asked at each read, so a key that can
    // be used again serves every read again — with the execution's own result.
    for encryption in &encryptions {
        let row = encryption.row;
        for operation in row.restore {
            let answered =
                kms_call(&router, operation, &json!({ "KeyId": encryption.key_arn })).await;
            assert_eq!(
                answered.status,
                200,
                "case {:?}: expected 200 from {operation}, got {} {}",
                row.case,
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
        }
        for (read, answer) in data_reads(&router, &encryption.execution_arn).await {
            let case = format!("{} read by {read} once its key is usable again", row.case);
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected the read to be served, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                result_payload(&answer.json()),
                Some(durable_result()),
                "case {case:?}: expected the execution's own result, got {}",
                String::from_utf8_lossy(&answer.body)
            );
        }
    }
}

/// Every read of one execution that asks for its DATA — including the one that asks for it by
/// leaving `IncludeExecutionData` at its documented default of true — each labelled by how it
/// asked.
async fn data_reads(router: &MultiServiceRouter, arn: &str) -> Vec<(&'static str, Answer)> {
    vec![
        (
            "GetDurableExecution, asking for the data by default",
            read_execution(router, arn, "").await,
        ),
        (
            "GetDurableExecution, asking for the data",
            read_execution(router, arn, "?IncludeExecutionData=true").await,
        ),
        (
            "GetDurableExecutionHistory, asking for the data",
            read_history(router, arn, "?IncludeExecutionData=true").await,
        ),
    ]
}
