// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH resources may be named as the destination of an
//! asynchronous-invocation configuration — `PutFunctionEventInvokeConfig` and
//! `UpdateFunctionEventInvokeConfig`.
//!
//! The rule under test: an asynchronous-invocation destination is the STANDARD resource of
//! each service the member enumerates — `FunctionEventInvokeConfig$DestinationConfig`
//! (`smithy_query(query="doc", shape="FunctionEventInvokeConfig",
//! member="DestinationConfig")`): "Function - The Amazon Resource Name (ARN) of a Lambda
//! function. Queue - The ARN of a standard SQS queue. Bucket - The ARN of an Amazon S3
//! bucket. Topic - The ARN of a standard SNS topic. Event Bus - The ARN of an Amazon
//! EventBridge event bus." — so a `DestinationConfig` naming, in EITHER slot, a FIFO queue
//! or a FIFO topic (a resource of those two services whose name carries the `.fifo` suffix
//! Amazon SQS and Amazon SNS require of a FIFO resource) is refused
//! `InvalidParameterValueException` (HTTP 400) with the configuration the request carried
//! quoted whole, and nothing is written: a refused put leaves the qualifier holding no
//! configuration at all, and a refused update leaves the configuration exactly as it stood.
//! A standard queue or topic in the same slot is accepted (HTTP 200), and so is a resource of
//! a service with no FIFO variant whatever its name ends with.
//!
//! Every row drives real HTTP requests through the router and reads the configuration back
//! through `GetFunctionEventInvokeConfig`, so a refusal that nevertheless wrote — and an
//! acceptance that did not — fails here. The rows differ along what the rule generalises
//! over: WHICH slot names the destination (on-success, on-failure, both), WHICH KIND of
//! resource it names (queue, topic, event bus, bucket, function), whether that resource is
//! the FIFO or the standard variant of its service, and how the suffix sits in the name
//! (at its end, in its middle), with the other slot stated as holding a destination, stated
//! with no destination at all, and not stated — and both calls are driven on the unpublished
//! version and on a published one, because the configuration is a resource of ONE qualifier.

use std::sync::Arc;

use serde_json::{json, Value};

// How this resource is addressed, how its `FunctionArn` is formed and how a version is
// published are the same here as in the suite that covers the resource as a whole, so the
// modules cannot drift on them: they are its helpers, imported rather than restated.
use crate::event_invoke_config::{config_uri, configured_arn, publish_version};
use crate::event_source_mappings::{call, create_function, router_on, Answer};

/// The wire error code of the modelled 400.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The wire error code of the modelled 404.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A FIFO queue and a FIFO topic: the resources of the two services whose destination kind
/// the model documents as the STANDARD one ("Queue - The ARN of a standard SQS queue",
/// "Topic - The ARN of a standard SNS topic"), named as those services require a FIFO
/// resource to be named.
const FIFO_QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:fifo-records.fifo";
/// See [`FIFO_QUEUE`].
const FIFO_TOPIC: &str = "arn:aws:sns:us-east-1:000000000000:fifo-records.fifo";

/// The standard variant of each of those two, in the same position.
const QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:fifo-records";
/// See [`QUEUE`].
const TOPIC: &str = "arn:aws:sns:us-east-1:000000000000:fifo-records";

/// One destination per remaining kind the same member documents, whose service has no FIFO
/// resource at all — the bus and the bucket carrying the suffix in their names, so the rule
/// is pinned to the SERVICE as well as to the name.
const BUS: &str = "arn:aws:events:us-east-1:000000000000:event-bus/fifo-records.fifo";
/// See [`BUS`].
const BUCKET: &str = "arn:aws:s3:::fifo-records.fifo";
/// See [`BUS`].
const FUNCTION: &str = "arn:aws:lambda:us-east-1:000000000000:function:fifo-records";

/// A queue whose name carries the suffix somewhere OTHER than its end, which Amazon SQS
/// does not make a FIFO queue.
const QUEUE_WITH_SUFFIX_INSIDE: &str = "arn:aws:sqs:us-east-1:000000000000:fifo-records.fifo.audit";

/// The settings the seed configuration of the update rows carries, inside the published
/// ranges (`MaximumRetryAttempts` `@range(min: 0, max: 2)`, `MaximumEventAgeInSeconds`
/// `@range(min: 60, max: 21600)`).
const SEED_RETRIES: i64 = 1;
/// See [`SEED_RETRIES`].
const SEED_AGE: i64 = 300;

/// What ONE slot of a request's `DestinationConfig` states: the slot is absent from the
/// request altogether (`None`), stated with no `Destination` member (`Some(None)`), or
/// stated with the destination given (`Some(Some(arn))`).
type Slot = Option<Option<&'static str>>;

/// One row: the destination configuration a request states, and whether the rule refuses it.
struct Row {
    case: &'static str,
    on_success: Slot,
    on_failure: Slot,
    refused: bool,
}

/// The table: one row per way the rule can be exercised.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a FIFO queue on failure",
            on_success: None,
            on_failure: Some(Some(FIFO_QUEUE)),
            refused: true,
        },
        Row {
            case: "a FIFO queue on success",
            on_success: Some(Some(FIFO_QUEUE)),
            on_failure: None,
            refused: true,
        },
        Row {
            case: "a FIFO topic on failure",
            on_success: None,
            on_failure: Some(Some(FIFO_TOPIC)),
            refused: true,
        },
        Row {
            case: "a FIFO topic on success beside a standard queue on failure",
            on_success: Some(Some(FIFO_TOPIC)),
            on_failure: Some(Some(QUEUE)),
            refused: true,
        },
        Row {
            case: "a FIFO queue on failure beside an on-success slot stating no destination",
            on_success: Some(None),
            on_failure: Some(Some(FIFO_QUEUE)),
            refused: true,
        },
        Row {
            case: "a FIFO queue in both slots",
            on_success: Some(Some(FIFO_QUEUE)),
            on_failure: Some(Some(FIFO_QUEUE)),
            refused: true,
        },
        Row {
            case: "a standard queue on failure",
            on_success: None,
            on_failure: Some(Some(QUEUE)),
            refused: false,
        },
        Row {
            case: "a standard topic on success beside a standard queue on failure",
            on_success: Some(Some(TOPIC)),
            on_failure: Some(Some(QUEUE)),
            refused: false,
        },
        Row {
            case: "an event bus whose name carries the suffix",
            on_success: None,
            on_failure: Some(Some(BUS)),
            refused: false,
        },
        Row {
            case: "a bucket whose name carries the suffix, on failure",
            on_success: None,
            on_failure: Some(Some(BUCKET)),
            refused: false,
        },
        Row {
            case: "a function on success",
            on_success: Some(Some(FUNCTION)),
            on_failure: None,
            refused: false,
        },
        Row {
            case: "a queue whose name carries the suffix in its middle",
            on_success: None,
            on_failure: Some(Some(QUEUE_WITH_SUFFIX_INSIDE)),
            refused: false,
        },
        Row {
            case: "a configuration stating neither slot",
            on_success: None,
            on_failure: None,
            refused: false,
        },
    ]
}

/// The `DestinationConfig` a row states, as the request writes it.
fn destination_config(on_success: Slot, on_failure: Slot) -> Value {
    let mut config = json!({});
    for (member, slot) in [("OnSuccess", on_success), ("OnFailure", on_failure)] {
        if let Some(destination) = slot {
            config[member] = match destination {
                Some(arn) => json!({ "Destination": arn }),
                None => json!({}),
            };
        }
    }
    config
}

/// The sentence the refusal carries for a row: the fixed text with the `DestinationConfig`
/// the request stated quoted whole — a slot the request did not state written as holding
/// nothing, and a stated slot as its own structure carrying the `Destination` it carried.
fn refusal_message(on_success: Slot, on_failure: Slot) -> String {
    let quoted = |member: &str, slot: Slot| match slot {
        None => "null".to_string(),
        Some(destination) => {
            format!("{member}(destination={})", destination.unwrap_or("null"))
        }
    };
    format!(
        "The provided destination config DestinationConfig(onSuccess={}, onFailure={}) is \
         invalid.",
        quoted("OnSuccess", on_success),
        quoted("OnFailure", on_failure),
    )
}

/// Assert that `answer` is the modelled 400 carrying exactly the refusal `row` earns.
fn assert_refused(case: &str, operation: &str, answer: &Answer, row: &Row) {
    assert_eq!(
        answer.status,
        400,
        "case {case:?}: expected 400 from {operation}, got {} {}",
        answer.status,
        answer.text()
    );
    let code = answer.error_code();
    assert!(
        code.ends_with(INVALID_PARAMETER),
        "case {case:?}: expected {INVALID_PARAMETER} from {operation}, got {code:?}"
    );
    let expected = refusal_message(row.on_success, row.on_failure);
    let message = answer.string("Message").or_else(|| answer.string("message"));
    assert_eq!(
        message.as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected {operation} to answer {expected:?}, got {message:?}"
    );
}

/// The destinations the configuration of `qualifier` holds, slot by slot, read back through
/// `GetFunctionEventInvokeConfig` — and the retry setting beside them, so a refused call
/// that moved a setting other than a destination fails too.
async fn read_back(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    function: &str,
    qualifier: Option<&str>,
) -> (Option<String>, Option<String>, Option<i64>) {
    let answer = call(
        router,
        "GET",
        &config_uri(function, qualifier),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected 200 from GetFunctionEventInvokeConfig, got {} {}",
        answer.status,
        answer.text()
    );
    let body = answer.json();
    let slot = |member: &str| {
        body.get("DestinationConfig")
            .and_then(|config| config.get(member))
            .and_then(|slot| slot.get("Destination"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    (
        slot("OnSuccess"),
        slot("OnFailure"),
        body.get("MaximumRetryAttempts").and_then(Value::as_i64),
    )
}

/// A put naming a FIFO destination is refused and stores no configuration at all; a put
/// naming only destinations an invocation record can reach stores the pair it named.
#[tokio::test]
async fn a_put_refuses_a_fifo_destination_and_stores_nothing() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, row) in rows().into_iter().enumerate() {
        let function = format!("fifo-destination-put-{index}");
        create_function(&router, &function).await;
        let published = publish_version(&router, &function).await;

        // The configuration is a resource of ONE qualifier, so every row is driven both on
        // the unpublished version (addressed with no qualifier at all) and on a published
        // one.
        for qualifier in [None, Some(published.as_str())] {
            let case = format!("{} (put, qualifier {qualifier:?})", row.case);
            let body = json!({
                "MaximumRetryAttempts": SEED_RETRIES,
                "MaximumEventAgeInSeconds": SEED_AGE,
                "DestinationConfig": destination_config(row.on_success, row.on_failure),
            });
            let answer = call(&router, "PUT", &config_uri(&function, qualifier), &body).await;

            if row.refused {
                assert_refused(&case, "PutFunctionEventInvokeConfig", &answer, &row);
                // A refused put creates no configuration, so the qualifier still holds
                // none: the read of one is the modelled 404.
                let read = call(
                    &router,
                    "GET",
                    &config_uri(&function, qualifier),
                    &Value::Null,
                )
                .await;
                assert_eq!(
                    read.status,
                    404,
                    "case {case:?}: expected 404 from GetFunctionEventInvokeConfig after a \
                     refused put, got {} {}",
                    read.status,
                    read.text()
                );
                let code = read.error_code();
                assert!(
                    code.ends_with(NOT_FOUND),
                    "case {case:?}: expected {NOT_FOUND} after a refused put, got {code:?}"
                );
                continue;
            }

            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected 200 from PutFunctionEventInvokeConfig, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.string("FunctionArn"),
                Some(configured_arn(&function, qualifier)),
                "case {case:?}: expected the configuration on the qualified ARN, got {}",
                answer.text()
            );
            let held = read_back(&router, &case, &function, qualifier).await;
            let expected = (
                row.on_success.flatten().map(str::to_string),
                row.on_failure.flatten().map(str::to_string),
                Some(SEED_RETRIES),
            );
            assert_eq!(
                held, expected,
                "case {case:?}: expected the configuration to hold {expected:?}, got {held:?}"
            );
        }
    }
}

/// An update naming a FIFO destination is refused and leaves the configuration the
/// qualifier holds exactly as it stood; one naming only destinations an invocation record
/// can reach replaces the slots it states.
#[tokio::test]
async fn an_update_refuses_a_fifo_destination_and_changes_nothing() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, row) in rows().into_iter().enumerate() {
        let function = format!("fifo-destination-update-{index}");
        create_function(&router, &function).await;
        let published = publish_version(&router, &function).await;

        for qualifier in [None, Some(published.as_str())] {
            let case = format!("{} (update, qualifier {qualifier:?})", row.case);

            // The seed every row updates: a configuration holding a standard destination in
            // each slot, so a refusal that nevertheless wrote is visible as a moved slot.
            let seed = json!({
                "MaximumRetryAttempts": SEED_RETRIES,
                "MaximumEventAgeInSeconds": SEED_AGE,
                "DestinationConfig": {
                    "OnSuccess": { "Destination": TOPIC },
                    "OnFailure": { "Destination": QUEUE },
                },
            });
            let seeded = call(&router, "PUT", &config_uri(&function, qualifier), &seed).await;
            assert_eq!(
                seeded.status,
                200,
                "case {case:?}: the seed put must succeed, got {} {}",
                seeded.status,
                seeded.text()
            );

            let body = json!({
                "DestinationConfig": destination_config(row.on_success, row.on_failure),
            });
            let answer = call(&router, "POST", &config_uri(&function, qualifier), &body).await;

            if row.refused {
                assert_refused(&case, "UpdateFunctionEventInvokeConfig", &answer, &row);
                let held = read_back(&router, &case, &function, qualifier).await;
                let expected = (
                    Some(TOPIC.to_string()),
                    Some(QUEUE.to_string()),
                    Some(SEED_RETRIES),
                );
                assert_eq!(
                    held, expected,
                    "case {case:?}: expected the refused update to leave {expected:?}, got \
                     {held:?}"
                );
                continue;
            }

            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected 200 from UpdateFunctionEventInvokeConfig, got {} {}",
                answer.status,
                answer.text()
            );
            // An update states each slot on its own, so a slot the row left out keeps the
            // seeded destination.
            let held = read_back(&router, &case, &function, qualifier).await;
            let expected = (
                match row.on_success {
                    None => Some(TOPIC.to_string()),
                    Some(destination) => destination.map(str::to_string),
                },
                match row.on_failure {
                    None => Some(QUEUE.to_string()),
                    Some(destination) => destination.map(str::to_string),
                },
                Some(SEED_RETRIES),
            );
            assert_eq!(
                held, expected,
                "case {case:?}: expected the configuration to hold {expected:?}, got {held:?}"
            );
        }
    }
}
