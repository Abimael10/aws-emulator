// SPDX-License-Identifier: CC0-1.0

//! A create this service REFUSES leaves no child behind, so the bus it named
//! stays deletable.
//!
//! RULE: `CreateSubscriber` refuses a request whose configuration no subscriber
//! could deliver under — the parameters member its target does not take, a
//! target or a role of another account, a filter configuration that selects
//! nothing, a starting position without the point it needs, a transformation
//! the service cannot perform, a universal call it does not make — with the
//! modelled exception of that class and AWS's wording for it, and such a
//! request mints NOTHING: no subscriber is stored, so the bus's own in-use gate
//! ("an event bus with subscribers or event sources cannot be deleted until they
//! are deleted") is not held by one, `DeleteEventBus` is served and a read of
//! the bus ends in `ResourceNotFoundException`.
//!
//! The rows of the table below span what that rule generalises over: WHICH part
//! of the request is wrong (the target, the role, the filters, the starting
//! position, the transformer, the universal call, the batching and a configured
//! value), and across the modelled refusals both error classes the two kinds of
//! fault travel in.
//!
//! The second rule is the other half of the gate: a subscriber that IS stored
//! holds its bus, so the delete is refused until that subscriber is deleted —
//! which is what makes the gate a question about live children rather than about
//! children that ever existed.
//!
//! Everything is read off the `rpcv2Cbor` wire through the harness
//! `modelled_input_refusal.rs` owns. Nothing here waits on the wall clock: the
//! two waits are the bus's own provisioning and deletion transitions, each
//! polled for the CONDITION under a generous deadline.

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_subscriber::DeleteSubscriber;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::types::{
    BatchConfiguration, CreateSubscriberInput, DeleteEventBusInput, DeleteSubscriberInput,
    EventBusV2Parameters, Filter, FilterConfiguration, InvokeConfiguration, JsonataConfiguration,
    LambdaParameters, ListSubscribersInput, PointInTimeConfiguration, SqsParameters, Transformer,
    UniversalTargetParameters,
};

use crate::modelled_input_refusal::{refusal, rendered, Wire, INVALID_INPUT_EXCEPTION, REGION};

/// The account the emulator mints ARNs under, which every target and role below
/// belongs to unless the row is about another account.
const ACCOUNT: &str = "000000000000";

/// An account that is not the calling one, for the rows about a resource of
/// another account.
const OTHER_ACCOUNT: &str = "111111111111";

/// The error shape a permission refusal travels in, and the whole `Message` of
/// that class: fixed text, written out here so this suite pins the wording.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED: &str = "Access denied.";

/// The error shape `DeleteEventBus` refuses a bus its children still hold with.
const RESOURCE_IN_USE_EXCEPTION: &str = "ResourceInUseException";

/// `MaxResults`' modelled maximum on `ListSubscribers`, which the enumeration
/// below asks for: one page holds every subscriber this bus could have.
/// Source: `smithy_query(query="operation", operation="ListSubscribers")`.
const MAX_RESULTS: i32 = 100;

/// The whole `Message` of that refusal for the SUBSCRIBER relation, written out
/// here rather than read from the service.
const SUBSCRIBERS_IN_USE: &str = "The event bus cannot be deleted because it has one or more \
     subscribers that are not revoked. Delete or revoke them and retry.";

/// A queue of the calling account, the target most rows deliver to.
fn queue() -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:refused-create-target")
}

/// A role of the calling account, which every row names unless it is about a
/// role of another account.
fn role() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/refused-create-delivery")
}

/// An event bus of the PREVIOUS generation of EventBridge (`event-bus`, not this
/// service's `event-busv2`), which takes no parameters member and only the raw
/// transformation.
fn classic_bus() -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/refused-create-classic")
}

/// A universal (aws-sdk) target naming `service`'s `action`, the form
/// `UniversalTargetParameters` documents.
fn universal(service: &str, action: &str) -> String {
    format!("arn:aws:events:::aws-sdk:{service}:{action}")
}

/// The invoke configuration of a row: the calling account's role and queue.
fn invoke() -> InvokeConfiguration {
    InvokeConfiguration {
        role_arn: role(),
        target_arn: queue(),
        ..Default::default()
    }
}

/// A filter of `scope` carrying `pattern`.
fn filter(scope: &str, pattern: &str) -> Filter {
    Filter {
        pattern: pattern.to_string(),
        scope: scope.to_string(),
    }
}

/// A filter configuration carrying `filters`.
fn filters(filters: Vec<Filter>) -> FilterConfiguration {
    FilterConfiguration {
        language: None,
        filters: Some(filters),
    }
}

/// The batching a universal target has to state, with the values inside the
/// members' own modelled ranges.
fn batching() -> BatchConfiguration {
    BatchConfiguration {
        max_batch_size: Some(5),
        max_batch_window_in_seconds: Some(1),
    }
}

/// One row: the request, the error shape it is refused with, its HTTP status and
/// the whole `Message`.
struct Row {
    case: &'static str,
    input: CreateSubscriberInput,
    shape: &'static str,
    status: u16,
    message: String,
}

/// The rows, each a create no subscriber could ever deliver under.
fn rows(bus: &str) -> Vec<Row> {
    let refused = |case: &'static str,
                   carry: &dyn Fn(&mut CreateSubscriberInput),
                   shape: &'static str,
                   status: u16,
                   message: String| {
        let mut input = CreateSubscriberInput {
            name: "refused-create".to_string(),
            event_bus_arn: bus.to_string(),
            invoke_configuration: invoke(),
            ..Default::default()
        };
        carry(&mut input);
        Row {
            case,
            input,
            shape,
            status,
            message,
        }
    };

    vec![
        refused(
            "a parameters member the target does not take",
            &|input| {
                input.invoke_configuration.lambda_parameters = Some(LambdaParameters::default());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            format!(
                "LambdaParameters is not supported for target {}. Expected SqsParameters",
                queue()
            ),
        ),
        refused(
            "a parameters member on a target that takes none",
            &|input| {
                input.invoke_configuration.target_arn = classic_bus();
                input.invoke_configuration.event_bus_v2_parameters =
                    Some(EventBusV2Parameters::default());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            format!(
                "EventBusV2Parameters is not supported for target {}. Expected no target \
                 parameters",
                classic_bus()
            ),
        ),
        refused(
            "a transformation a previous-generation target cannot apply",
            &|input| {
                input.invoke_configuration.target_arn = classic_bus();
                input.transformer = Some(Transformer {
                    r#type: Some("WITH_METADATA".to_string()),
                    jsonata_configuration: None,
                });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Classic EventBridge targets only support Transformer.Type RAW".to_string(),
        ),
        refused(
            "a target of another account",
            &|input| {
                input.invoke_configuration.target_arn =
                    format!("arn:aws:sqs:{REGION}:{OTHER_ACCOUNT}:somebody-elses-queue");
            },
            INVALID_INPUT_EXCEPTION,
            400,
            format!(
                "Target resource account ({OTHER_ACCOUNT}) must match the subscriber account \
                 ({ACCOUNT})"
            ),
        ),
        refused(
            "a role of another account",
            &|input| {
                input.invoke_configuration.role_arn =
                    format!("arn:aws:iam::{OTHER_ACCOUNT}:role/somebody-elses-role");
            },
            ACCESS_DENIED_EXCEPTION,
            403,
            ACCESS_DENIED.to_string(),
        ),
        refused(
            "two filters of one scope",
            &|input| {
                input.filter_configuration = Some(filters(vec![
                    filter("DATA", r#"{"detail":{"kind":["order"]}}"#),
                    filter("DATA", r#"{"detail":{"kind":["refund"]}}"#),
                ]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Duplicate filter scope: DATA".to_string(),
        ),
        refused(
            "a filter whose pattern is blank",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter("METADATA", "")]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Filter pattern must not be empty".to_string(),
        ),
        refused(
            "a filter whose pattern is not JSON",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter("DATA", "{not json")]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Invalid filter pattern: Filter pattern for scope DATA is not valid JSON at line 1, \
             column 2"
                .to_string(),
        ),
        refused(
            "a filter whose pattern is a JSON value but no object",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter("METADATA", "[\"team\"]")]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "METADATA filter pattern must be a JSON object".to_string(),
        ),
        refused(
            "a metadata field compared by an operator",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter(
                    "METADATA",
                    r#"{"tenant":[{"prefix":"acme-"}]}"#,
                )]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "METADATA filter field 'tenant' supports exact-match values only; operators are not \
             allowed"
                .to_string(),
        ),
        refused(
            "a metadata field compared by a nested pattern",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter(
                    "METADATA",
                    r#"{"tenant":{"name":["acme"]}}"#,
                )]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "METADATA filter field 'tenant' supports exact-match values only; operators are not \
             allowed"
                .to_string(),
        ),
        refused(
            "a wildcard matcher",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter(
                    "DATA",
                    r#"{"detail":{"sku":[{"wildcard":"a*z"}]}}"#,
                )]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Filter pattern must not contain wildcard matchers (wildcard, anything-but wildcard)"
                .to_string(),
        ),
        refused(
            "a wildcard matcher under anything-but",
            &|input| {
                input.filter_configuration = Some(filters(vec![filter(
                    "DATA",
                    r#"{"detail":{"sku":[{"anything-but":{"wildcard":"a*z"}}]}}"#,
                )]));
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "Filter pattern must not contain wildcard matchers (wildcard, anything-but wildcard)"
                .to_string(),
        ),
        refused(
            "the point-in-time position with no point",
            &|input| {
                input.starting_position = Some("POINT_IN_TIME".to_string());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "PointInTimeConfiguration is required when StartingPosition is POINT_IN_TIME"
                .to_string(),
        ),
        refused(
            "a point beside the latest position",
            &|input| {
                input.starting_position = Some("LATEST".to_string());
                input.point_in_time_configuration = Some(PointInTimeConfiguration {
                    point_type: "HORIZON".to_string(),
                    starting_point: None,
                    end_point: None,
                });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "PointInTimeConfiguration must not be provided when StartingPosition is LATEST"
                .to_string(),
        ),
        refused(
            "a transformation by an expression that carries none",
            &|input| {
                input.transformer = Some(Transformer {
                    r#type: Some("JSONATA".to_string()),
                    jsonata_configuration: None,
                });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "JsonataConfiguration is required when Transformer.Type is JSONATA".to_string(),
        ),
        refused(
            "an expression the language does not admit",
            &|input| {
                input.transformer = Some(Transformer {
                    r#type: Some("JSONATA".to_string()),
                    jsonata_configuration: Some(JsonataConfiguration {
                        expression: "{% $events.Data. %}".to_string(),
                    }),
                });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "JsonataConfiguration.Expression is not a valid JSONata expression.".to_string(),
        ),
        refused(
            "a configured value that reaches for the expression delimiters",
            &|input| {
                input.invoke_configuration.sqs_parameters = Some(SqsParameters {
                    delay_seconds: Some(" {% $events.Data.detail.delay %}".to_string()),
                    ..Default::default()
                });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "SqsParameters.DelaySeconds is not a valid JSONata expression. It must be wrapped in \
             {% %} delimiters with no surrounding whitespace and parse as JSONata."
                .to_string(),
        ),
        refused(
            "a universal call that only reads",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "getItem");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"TableName":"orders","Key":{}}"#.to_string(),
                        invocation_timeout_seconds: None,
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "getItem API is not supported".to_string(),
        ),
        refused(
            "a universal call of another service that only reads",
            &|input| {
                input.invoke_configuration.target_arn = universal("s3", "listBuckets");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: "{}".to_string(),
                        invocation_timeout_seconds: None,
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "listBuckets API is not supported".to_string(),
        ),
        refused(
            "a universal call addressed at EventBridge itself",
            &|input| {
                input.invoke_configuration.target_arn = universal("eventbridge", "putEvents");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"Entries":[]}"#.to_string(),
                        invocation_timeout_seconds: None,
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "aws-sdk:eventbridge:putEvents API is not supported".to_string(),
        ),
        refused(
            "a universal call naming an api the service does not have",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "frobnicate");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: "{}".to_string(),
                        invocation_timeout_seconds: None,
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "The api frobnicate is not valid for the service dynamodb".to_string(),
        ),
        refused(
            "a universal request missing the members its api requires",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "putItem");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"ReturnValues":"NONE"}"#.to_string(),
                        invocation_timeout_seconds: None,
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "UniversalTargetParameters.Input is missing required field(s): Item, TableName"
                .to_string(),
        ),
        refused(
            "a universal target that states no parameters at all",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "putItem");
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "UniversalTargetParameters.Input is missing required field(s): Item, TableName"
                .to_string(),
        ),
        refused(
            "a universal target that states no batching",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "putItem");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"TableName":"orders","Item":{}}"#.to_string(),
                        invocation_timeout_seconds: None,
                    });
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "BatchConfiguration must be provided with valid MaxBatchSize and MaxBatchWindow \
             values for USI targets"
                .to_string(),
        ),
        refused(
            "an invocation timeout past the top of its range",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "putItem");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"TableName":"orders","Item":{}}"#.to_string(),
                        invocation_timeout_seconds: Some("31".to_string()),
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "InvocationTimeoutSeconds must be between 1 and 30".to_string(),
        ),
        refused(
            "an invocation timeout under the bottom of its range",
            &|input| {
                input.invoke_configuration.target_arn = universal("dynamodb", "putItem");
                input.invoke_configuration.universal_target_parameters =
                    Some(UniversalTargetParameters {
                        input: r#"{"TableName":"orders","Item":{}}"#.to_string(),
                        invocation_timeout_seconds: Some("0".to_string()),
                    });
                input.batch_configuration = Some(batching());
            },
            INVALID_INPUT_EXCEPTION,
            400,
            "InvocationTimeoutSeconds must be between 1 and 30".to_string(),
        ),
    ]
}

/// RULE: each of those creates is refused with the modelled exception of its
/// class and AWS's wording, and mints nothing — so the bus ends the run with no
/// subscriber at all, `DeleteEventBus` is served, and the bus read ends in
/// `ResourceNotFoundException`.
#[tokio::test]
async fn a_refused_create_leaves_the_bus_with_no_child_and_deletable() {
    let wire = Wire::new();
    let bus = wire.active_bus("the refused creates' bus", "refused-create-bus").await;

    for (index, mut row) in rows(&bus).into_iter().enumerate() {
        let case = row.case;
        // A token of its own per row, so no row is read as a retry of another;
        // `ClientToken`'s own `@pattern` admits no space, which is why it is not
        // the case text.
        row.input.client_token = Some(format!("refused-create-token-{index}"));
        let (status, body) = wire.call::<CreateSubscriber<InMemoryEngine>>(&row.input).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with {:?}",
            row.status,
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            (code.as_str(), message.as_str()),
            (row.shape, row.message.as_str()),
            "case {case:?}: expected {} with {:?}, got {code} with {message:?}",
            row.shape,
            row.message
        );

        // The whole of what a refused create must leave behind: nothing.
        let listed = wire
            .ok::<ListSubscribers<InMemoryEngine>>(
                case,
                &ListSubscribersInput {
                    event_bus_arn: Some(bus.clone()),
                    // `MaxResults` is a plain `i32` here, so the request always
                    // carries one: the top of its own modelled range, which is
                    // every subscriber this bus could hold.
                    max_results: MAX_RESULTS,
                    ..Default::default()
                },
            )
            .await
            .subscribers
            .unwrap_or_default();
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused create must leave no subscriber, got {:?}",
            listed
                .iter()
                .map(|summary| summary.subscriber_arn.clone())
                .collect::<Vec<_>>()
        );
    }

    // And with no child holding it, the bus is deletable — the delete is served
    // and the read of it ends in ResourceNotFoundException.
    let case = "the delete of the bus every create was refused on";
    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        })
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: DeleteEventBus must be served, got {status} with {:?}",
        rendered(&body)
    );
    wire.await_bus_gone(case, &bus).await;
}

/// RULE: the other half of the gate — a subscriber the service DID store holds
/// its bus, so the delete is refused with `ResourceInUseException` (409) and
/// AWS's wording for that relation until the subscriber is deleted; the bus is
/// deletable straight afterwards.
#[tokio::test]
async fn a_stored_subscriber_holds_its_bus_until_it_is_deleted() {
    let wire = Wire::new();
    let case = "a bus with one subscriber";
    let bus = wire.active_bus(case, "held-bus").await;

    let created = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            case,
            &CreateSubscriberInput {
                name: "held-subscriber".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(),
                client_token: Some("token-for-held".to_string()),
                ..Default::default()
            },
        )
        .await;
    let subscriber = created
        .subscriber_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must report a SubscriberArn"));

    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        })
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a bus a subscriber holds must be refused 409, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        (code.as_str(), message.as_str()),
        (RESOURCE_IN_USE_EXCEPTION, SUBSCRIBERS_IN_USE),
        "case {case:?}: expected {RESOURCE_IN_USE_EXCEPTION} with {SUBSCRIBERS_IN_USE:?}, got \
         {code} with {message:?}"
    );

    let case = "the same bus once the subscriber is deleted";
    wire.ok::<DeleteSubscriber<InMemoryEngine>>(
        case,
        &DeleteSubscriberInput {
            subscriber_arn: subscriber,
        },
    )
    .await;

    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: bus.clone(),
        })
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: DeleteEventBus must be served, got {status} with {:?}",
        rendered(&body)
    );
    wire.await_bus_gone(case, &bus).await;
}
