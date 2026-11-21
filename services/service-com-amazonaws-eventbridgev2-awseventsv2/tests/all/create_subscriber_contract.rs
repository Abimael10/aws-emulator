// SPDX-License-Identifier: CC0-1.0

//! What `CreateSubscriber` answers, and that every part of it was computed.
//!
//! RULE: `CreateSubscriber` mints a subscriber on an `ACTIVE` event bus out of
//! the request's own configuration and answers 200 with that subscriber READ
//! BACK from the store — a `SubscriberArn` of the modelled form naming the
//! subscriber's own name and a minted id, the bus it was created on, and the
//! ordering type, starting position and run state the request named or the
//! documented default where it named none — the whole stored configuration is
//! what a later `DescribeSubscriber` reports, a repeat carrying the same
//! `ClientToken` replays the subscriber that token minted and mints nothing
//! else, and anything the operation refuses it refuses with one of the errors
//! its model declares for it, never with a not-implemented signal.
//!
//! The rows span that class rather than one recorded create: they differ along
//! the ordering type, the starting position (including a point-in-time
//! subscriber positioned at the retained horizon), the run state, the number of
//! filters and the scopes they read, which optional configuration blocks the
//! request carries at all, and — for the replay — along which single member of a
//! repeat differs from the create its token minted.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus's own
//! provisioning transition, polled for the CONDITION (`ACTIVE`) under a generous
//! deadline.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::types::{
    BatchConfiguration, CreateEventBusInput, CreateSubscriberInput, CreateSubscriberOutput,
    DeleteEventBusInput, DescribeEventBusInput, DescribeSubscriberInput, DescribeSubscriberOutput,
    Filter, FilterConfiguration, InvokeConfiguration, LogConfiguration, OnFailureConfiguration,
    PointInTimeConfiguration, RetryPolicy, Transformer, FILTER_LANGUAGE_VALUES,
};
use eventsv2::scaffolded::Scaffolded;

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled
/// `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The queue a subscriber delivers to. Nothing is published here — every
/// subscriber below starts at the end of an empty log — so it only has to be a
/// well-formed `TargetResourceArn`.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:contract-target";

/// A second target, so a repeat can differ in what it invokes.
const OTHER_TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:contract-other-target";

/// The dead-letter queue an `OnFailureConfiguration` names, of the modelled
/// `DeadLetterQueueArn` shape.
const DEAD_LETTER_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:contract-dlq";

/// The error shapes `CreateSubscriber` declares, per
/// `smithy_query(query="operation",operation="CreateSubscriber")` — the whole set
/// a refusal of this operation may name, and the only one a client's error
/// handling can be written against. `NotImplemented` is not among them.
const MODELLED_ERRORS: &[&str] = &[
    "AccessDeniedException",
    "ConcurrentModificationException",
    "IdempotentParameterMismatchException",
    "InternalException",
    "InvalidInputException",
    "InvalidStateException",
    "LimitExceededException",
    "ResourceAlreadyExistsException",
    "ResourceNotFoundException",
    "ThrottlingException",
];

/// Width of the id segment of a `SubscriberArn`, whose modelled pattern pins it
/// at exactly `[a-z0-9]{25}`.
const SUBSCRIBER_ID_LEN: usize = 25;

/// `OrderingType::UNORDERED`, the ordering a create that names no `Type` takes
/// ("delivers without an ordering guarantee").
const DEFAULT_ORDERING_TYPE: &str = "UNORDERED";

/// `StartingPosition::LATEST`, the position a create that names none takes.
const DEFAULT_STARTING_POSITION: &str = "LATEST";

/// `SubscriberState::RUNNING`, the run state a create that names none takes:
/// "Delivery requires State RUNNING on a subscriber that is not revoked".
const DEFAULT_RUN_STATE: &str = "RUNNING";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// A SigV4 `Authorization` header naming the region and this service's signing
/// name, which is how a request names both.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`]: the credential scope is
/// where a request names the region it addresses.
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/CreateSubscriber")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// One emulator over one shared engine, with a request context every typed call
/// reuses.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: http::request::Parts,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(router);
        Self {
            engine,
            definition,
            callback,
            parts: parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state
    /// `CreateSubscriber` requires of it. Polls the condition rather than
    /// sleeping a fixed time, because provisioning is asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let described = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                    &self.ctx(),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!("case {case:?}: DescribeEventBus must succeed, got {e:?}")
                });
            if described.state.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {arn} never became ACTIVE, last state {:?}",
                described.state
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// `CreateSubscriber` as the operation itself answers it.
    async fn create(
        &self,
        input: CreateSubscriberInput,
    ) -> Result<CreateSubscriberOutput, Scaffolded<CreateSubscriberErrorModeled>> {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    /// The subscriber at `arn`, as `DescribeSubscriber` reports it.
    async fn describe(&self, case: &str, arn: &str) -> DescribeSubscriberOutput {
        DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DescribeSubscriber({arn}) must succeed, got {e:?}")
            })
    }
}

// ---------------------------------------------------------------------------
// The pieces a row is built from
// ---------------------------------------------------------------------------

/// The target every subscriber below invokes, unless a row names another.
fn invoke(target_arn: &str) -> InvokeConfiguration {
    InvokeConfiguration {
        role_arn: ROLE_ARN.to_string(),
        target_arn: target_arn.to_string(),
        ..Default::default()
    }
}

/// A `FilterConfiguration` holding one `(Pattern, Scope)` pair per entry, which
/// are implicitly ANDed.
fn filters(pairs: &[(&str, &str)]) -> FilterConfiguration {
    FilterConfiguration {
        language: None,
        filters: Some(
            pairs
                .iter()
                .map(|(pattern, scope)| Filter {
                    pattern: (*pattern).to_string(),
                    scope: (*scope).to_string(),
                })
                .collect(),
        ),
    }
}

/// A `Tags` map from `pairs`.
fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// An event pattern matched against the event's payload (`FilterScope::DATA`).
const DATA_PATTERN: &str = r#"{"status":["shipped"]}"#;

/// An event pattern matched against the publisher's metadata
/// (`FilterScope::METADATA`).
const METADATA_PATTERN: &str = r#"{"source":["orders.api"]}"#;

/// An event pattern matched against the event's system metadata
/// (`FilterScope::SYSTEM_METADATA`).
const SYSTEM_METADATA_PATTERN: &str = r#"{"ContentType":["application/json"]}"#;

/// One create, and what the answer must carry for the members the request may
/// leave to the service.
struct Row {
    case: &'static str,
    name: &'static str,
    input: fn(&str) -> CreateSubscriberInput,
    ordering_type: &'static str,
    starting_position: &'static str,
    run_state: &'static str,
}

/// The creates this suite spans the class with: each row varies a different part
/// of what a subscriber IS.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the least a create can name",
            name: "contract-minimal",
            input: |bus| CreateSubscriberInput {
                name: "contract-minimal".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(TARGET_ARN),
                ..Default::default()
            },
            ordering_type: DEFAULT_ORDERING_TYPE,
            starting_position: DEFAULT_STARTING_POSITION,
            run_state: DEFAULT_RUN_STATE,
        },
        Row {
            case: "an unordered subscriber stopped on arrival, filtering on the payload",
            name: "contract-stopped",
            input: |bus| CreateSubscriberInput {
                name: "contract-stopped".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(TARGET_ARN),
                r#type: Some("UNORDERED".to_string()),
                state: Some("STOPPED".to_string()),
                description: Some("a subscriber nobody has started".to_string()),
                filter_configuration: Some(filters(&[(DATA_PATTERN, "DATA")])),
                ..Default::default()
            },
            ordering_type: "UNORDERED",
            starting_position: DEFAULT_STARTING_POSITION,
            run_state: "STOPPED",
        },
        Row {
            case: "a FIFO subscriber ANDing two filters in two scopes",
            name: "contract-fifo",
            input: |bus| CreateSubscriberInput {
                name: "contract-fifo".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(TARGET_ARN),
                r#type: Some("FIFO".to_string()),
                filter_configuration: Some(filters(&[
                    (DATA_PATTERN, "DATA"),
                    (METADATA_PATTERN, "METADATA"),
                ])),
                tags: Some(tags(&[("team", "orders"), ("tier", "gold")])),
                ..Default::default()
            },
            ordering_type: "FIFO",
            starting_position: DEFAULT_STARTING_POSITION,
            run_state: DEFAULT_RUN_STATE,
        },
        Row {
            case: "a subscriber positioned at the retained horizon",
            name: "contract-horizon",
            input: |bus| CreateSubscriberInput {
                name: "contract-horizon".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(TARGET_ARN),
                starting_position: Some("POINT_IN_TIME".to_string()),
                point_in_time_configuration: Some(PointInTimeConfiguration {
                    point_type: "HORIZON".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ordering_type: DEFAULT_ORDERING_TYPE,
            starting_position: "POINT_IN_TIME",
            run_state: DEFAULT_RUN_STATE,
        },
        Row {
            case: "a subscriber that names the end of the log and its run state itself",
            name: "contract-latest",
            input: |bus| CreateSubscriberInput {
                name: "contract-latest".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(OTHER_TARGET_ARN),
                starting_position: Some("LATEST".to_string()),
                state: Some("RUNNING".to_string()),
                filter_configuration: Some(filters(&[(
                    SYSTEM_METADATA_PATTERN,
                    "SYSTEM_METADATA",
                )])),
                tags: Some(tags(&[("team", "orders")])),
                ..Default::default()
            },
            ordering_type: DEFAULT_ORDERING_TYPE,
            starting_position: "LATEST",
            run_state: DEFAULT_RUN_STATE,
        },
        Row {
            case: "a subscriber carrying every delivery block the model offers",
            name: "contract-configured",
            input: |bus| CreateSubscriberInput {
                name: "contract-configured".to_string(),
                event_bus_arn: bus.to_string(),
                invoke_configuration: invoke(TARGET_ARN),
                // A `Description` of zero length, which `Description`'s own
                // `@length(0, 512)` admits.
                description: Some(String::new()),
                filter_configuration: Some(filters(&[
                    (DATA_PATTERN, "DATA"),
                    (METADATA_PATTERN, "METADATA"),
                    (SYSTEM_METADATA_PATTERN, "SYSTEM_METADATA"),
                ])),
                batch_configuration: Some(BatchConfiguration {
                    max_batch_size: Some(10),
                    max_batch_window_in_seconds: Some(5),
                }),
                transformer: Some(Transformer {
                    r#type: Some("RAW".to_string()),
                    jsonata_configuration: None,
                }),
                retry_policy: Some(RetryPolicy {
                    max_retry_attempts: Some(0),
                    max_event_age_in_seconds: Some(60),
                    retry_strategy: Some("ALL".to_string()),
                }),
                on_failure_configuration: Some(OnFailureConfiguration {
                    arn: Some(DEAD_LETTER_ARN.to_string()),
                }),
                log_configuration: Some(LogConfiguration {
                    level: Some("INFO".to_string()),
                    include_payload: Some("FULL".to_string()),
                }),
                ..Default::default()
            },
            ordering_type: DEFAULT_ORDERING_TYPE,
            starting_position: DEFAULT_STARTING_POSITION,
            run_state: DEFAULT_RUN_STATE,
        },
    ]
}

// ---------------------------------------------------------------------------
// RULE: a create mints the subscriber, and the store is what the answer carries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_created_subscriber_is_minted_from_the_request_and_read_back() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the bus every row is created on", "contract-bus")
        .await;

    let mut minted: Vec<String> = Vec::new();
    let rows = rows();
    for row in &rows {
        let case = row.case;
        let requested = (row.input)(&bus);
        let created = harness
            .create(requested.clone())
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"));

        let arn = created
            .subscriber_arn
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no SubscriberArn"));
        assert_subscriber_arn(case, &arn, row.name);
        assert_eq!(
            created.name.as_deref(),
            Some(row.name),
            "case {case:?}: expected Name {:?}, got {:?}",
            row.name,
            created.name
        );
        assert_eq!(
            created.event_bus_arn.as_deref(),
            Some(bus.as_str()),
            "case {case:?}: expected EventBusArn {bus:?}, got {:?}",
            created.event_bus_arn
        );
        assert_eq!(
            created.r#type.as_deref(),
            Some(row.ordering_type),
            "case {case:?}: expected Type {:?}, got {:?}",
            row.ordering_type,
            created.r#type
        );
        assert_eq!(
            created.starting_position.as_deref(),
            Some(row.starting_position),
            "case {case:?}: expected StartingPosition {:?}, got {:?}",
            row.starting_position,
            created.starting_position
        );
        assert_eq!(
            created.state.as_deref(),
            Some(row.run_state),
            "case {case:?}: expected State {:?}, got {:?}",
            row.run_state,
            created.state
        );
        assert_eq!(
            created
                .point_in_time_configuration
                .as_ref()
                .map(|point| point.point_type.clone()),
            requested
                .point_in_time_configuration
                .as_ref()
                .map(|point| point.point_type.clone()),
            "case {case:?}: expected the PointInTimeConfiguration the request named, got {:?}",
            created.point_in_time_configuration
        );
        assert!(
            created.creation_time.is_some(),
            "case {case:?}: the create must report a CreationTime"
        );
        assert!(
            !minted.contains(&arn),
            "case {case:?}: every subscriber must carry its own SubscriberArn, but {arn:?} \
             was minted twice"
        );
        minted.push(arn.clone());

        // The answer again, from the read a client uses: every member of it
        // came out of the store rather than out of the create's own response.
        let described = harness.describe(case, &arn).await;
        assert_stored(case, &requested, &created, &described);
    }
    assert_eq!(
        minted.len(),
        rows.len(),
        "every row must have minted a subscriber, got {minted:?}"
    );
}

/// The `SubscriberArn` of `name`: the modelled form
/// `arn:aws:events:<region>:<account>:subscriber/<name>/[a-z0-9]{25}`.
fn assert_subscriber_arn(case: &str, arn: &str, name: &str) {
    let account = engine::emulator_account_id();
    let prefix = format!("arn:aws:events:{REGION}:{account}:subscriber/{name}/");
    let Some(id) = arn.strip_prefix(&prefix) else {
        panic!("case {case:?}: expected a SubscriberArn starting {prefix:?}, got {arn:?}");
    };
    assert_eq!(
        id.len(),
        SUBSCRIBER_ID_LEN,
        "case {case:?}: the ARN's id segment must be {SUBSCRIBER_ID_LEN} characters, got {id:?}"
    );
    assert!(
        id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "case {case:?}: the ARN's id segment must be lowercase alphanumerics, got {id:?}"
    );
}

/// What the store holds for the subscriber `requested` minted: the identity the
/// create answered, and every configuration member the request named.
fn assert_stored(
    case: &str,
    requested: &CreateSubscriberInput,
    created: &CreateSubscriberOutput,
    described: &DescribeSubscriberOutput,
) {
    assert_eq!(
        described.subscriber_arn, created.subscriber_arn,
        "case {case:?}: the read must report the SubscriberArn the create minted, got {:?}",
        described.subscriber_arn
    );
    assert_eq!(
        described.name.as_deref(),
        Some(requested.name.as_str()),
        "case {case:?}: expected Name {:?}, got {:?}",
        requested.name,
        described.name
    );
    assert_eq!(
        described.event_bus_arn.as_deref(),
        Some(requested.event_bus_arn.as_str()),
        "case {case:?}: expected EventBusArn {:?}, got {:?}",
        requested.event_bus_arn,
        described.event_bus_arn
    );
    assert_eq!(
        described.r#type, created.r#type,
        "case {case:?}: the read and the create must report one Type, got {:?} and {:?}",
        described.r#type, created.r#type
    );
    assert_eq!(
        described.starting_position, created.starting_position,
        "case {case:?}: the read and the create must report one StartingPosition, got {:?} \
         and {:?}",
        described.starting_position, created.starting_position
    );
    assert_eq!(
        described.state, created.state,
        "case {case:?}: the read and the create must report one State, got {:?} and {:?}",
        described.state, created.state
    );
    assert_eq!(
        described.creation_time, created.creation_time,
        "case {case:?}: the read and the create must report one CreationTime, got {:?} and {:?}",
        described.creation_time, created.creation_time
    );
    assert_eq!(
        described.last_modified_time, created.creation_time,
        "case {case:?}: a subscriber nobody has updated was last modified when it was \
         created, got {:?}",
        described.last_modified_time
    );
    assert_eq!(
        described.revoked, None,
        "case {case:?}: a subscriber nobody has revoked must report no Revoked, got {:?}",
        described.revoked
    );
    assert_eq!(
        described.description, requested.description,
        "case {case:?}: expected Description {:?}, got {:?}",
        requested.description, described.description
    );
    let invoked = described
        .invoke_configuration
        .as_ref()
        .unwrap_or_else(|| panic!("case {case:?}: the read must report an InvokeConfiguration"));
    assert_eq!(
        invoked.target_arn, requested.invoke_configuration.target_arn,
        "case {case:?}: expected TargetArn {:?}, got {:?}",
        requested.invoke_configuration.target_arn, invoked.target_arn
    );
    assert_eq!(
        invoked.role_arn, requested.invoke_configuration.role_arn,
        "case {case:?}: expected RoleArn {:?}, got {:?}",
        requested.invoke_configuration.role_arn, invoked.role_arn
    );
    assert_eq!(
        selected(described.filter_configuration.as_ref()),
        selected(requested.filter_configuration.as_ref()),
        "case {case:?}: expected the filters the request named, got {:?}",
        described.filter_configuration
    );
    assert_eq!(
        described
            .on_failure_configuration
            .as_ref()
            .and_then(|configuration| configuration.arn.clone()),
        requested
            .on_failure_configuration
            .as_ref()
            .and_then(|configuration| configuration.arn.clone()),
        "case {case:?}: expected the OnFailureConfiguration the request named, got {:?}",
        described.on_failure_configuration
    );
    assert_eq!(
        described
            .retry_policy
            .as_ref()
            .map(|policy| (policy.max_retry_attempts, policy.max_event_age_in_seconds)),
        requested
            .retry_policy
            .as_ref()
            .map(|policy| (policy.max_retry_attempts, policy.max_event_age_in_seconds)),
        "case {case:?}: expected the RetryPolicy the request named, got {:?}",
        described.retry_policy
    );
    assert_eq!(
        described
            .batch_configuration
            .as_ref()
            .map(|batch| (batch.max_batch_size, batch.max_batch_window_in_seconds)),
        requested
            .batch_configuration
            .as_ref()
            .map(|batch| (batch.max_batch_size, batch.max_batch_window_in_seconds)),
        "case {case:?}: expected the BatchConfiguration the request named, got {:?}",
        described.batch_configuration
    );
    assert_eq!(
        described
            .log_configuration
            .as_ref()
            .map(|log| (log.level.clone(), log.include_payload.clone())),
        requested
            .log_configuration
            .as_ref()
            .map(|log| (log.level.clone(), log.include_payload.clone())),
        "case {case:?}: expected the LogConfiguration the request named, got {:?}",
        described.log_configuration
    );
    assert_eq!(
        described
            .transformer
            .as_ref()
            .map(|transformer| transformer.r#type.clone()),
        requested
            .transformer
            .as_ref()
            .map(|transformer| transformer.r#type.clone()),
        "case {case:?}: expected the Transformer the request named, got {:?}",
        described.transformer
    );
}

/// What a filter configuration selects, as comparable values: the patterns and
/// the scopes they read, in order. `FilterConfiguration` carries no equality of
/// its own, which is why this is written out.
fn selected(configuration: Option<&FilterConfiguration>) -> Vec<(String, String)> {
    configuration
        .and_then(|configuration| configuration.filters.as_deref())
        .unwrap_or_default()
        .iter()
        .map(|filter| (filter.pattern.clone(), filter.scope.clone()))
        .collect()
}

/// What a repeat changes about the request its `ClientToken` minted: one member,
/// per row.
type Mutation = fn(&mut CreateSubscriberInput);

// ---------------------------------------------------------------------------
// RULE: a repeat carrying the token replays, and any other repeat is a mismatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repeated_client_token_replays_the_subscriber_it_minted() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the bus the replayed subscriber lives on", "replay-bus")
        .await;

    let token = "a-client-token";
    let first_description = "the subscriber the token minted";
    let base = |bus: &str| CreateSubscriberInput {
        name: "replayed-subscriber".to_string(),
        event_bus_arn: bus.to_string(),
        invoke_configuration: invoke(TARGET_ARN),
        description: Some(first_description.to_string()),
        filter_configuration: Some(filters(&[(DATA_PATTERN, "DATA")])),
        tags: Some(tags(&[("team", "orders")])),
        client_token: Some(token.to_string()),
        ..Default::default()
    };

    let first = harness
        .create(base(&bus))
        .await
        .expect("case \"the first create\": CreateSubscriber must succeed");
    let arn = first
        .subscriber_arn
        .clone()
        .expect("case \"the first create\": the create reported no SubscriberArn");

    let replay = harness
        .create(base(&bus))
        .await
        .expect("case \"a byte-identical repeat\": the repeat must replay, not fail");
    assert_eq!(
        replay.subscriber_arn, first.subscriber_arn,
        "case \"a byte-identical repeat\": expected the subscriber the token minted ({arn}), \
         got {:?}",
        replay.subscriber_arn
    );
    assert_eq!(
        replay.creation_time, first.creation_time,
        "case \"a byte-identical repeat\": a replay reports the first create's CreationTime, \
         got {:?}",
        replay.creation_time
    );

    // Each row changes ONE member of the request the token minted, which is
    // what makes the repeat a different request rather than a retry.
    let mutations: Vec<(&str, Mutation)> = vec![
        ("another description", |input| {
            input.description = Some("something else entirely".to_string());
        }),
        ("no description at all", |input| {
            input.description = None;
        }),
        ("another name", |input| {
            input.name = "replayed-subscriber-elsewhere".to_string();
        }),
        ("another filter pattern", |input| {
            input.filter_configuration = Some(filters(&[(METADATA_PATTERN, "DATA")]));
        }),
        ("another filter scope", |input| {
            input.filter_configuration = Some(filters(&[(DATA_PATTERN, "METADATA")]));
        }),
        ("one filter more", |input| {
            input.filter_configuration = Some(filters(&[
                (DATA_PATTERN, "DATA"),
                (METADATA_PATTERN, "METADATA"),
            ]));
        }),
        ("another ordering type", |input| {
            input.r#type = Some("FIFO".to_string());
        }),
        ("another run state", |input| {
            input.state = Some("STOPPED".to_string());
        }),
        ("another target", |input| {
            input.invoke_configuration = invoke(OTHER_TARGET_ARN);
        }),
        ("another tag set", |input| {
            input.tags = Some(tags(&[("team", "payments")]));
        }),
    ];
    for (case, mutate) in mutations {
        let mut input = base(&bus);
        mutate(&mut input);
        let error = harness
            .create(input)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the repeat must be refused"));
        assert_modelled(case, &error);
        assert_eq!(
            error.shape_name(),
            "IdempotentParameterMismatchException",
            "case {case:?}: expected IdempotentParameterMismatchException, got {error:?}"
        );
        assert_eq!(
            error.http_status(),
            409,
            "case {case:?}: expected HTTP 409, got {}",
            error.http_status()
        );
    }

    // Every refused repeat left the subscriber the token minted exactly as it
    // was: the mismatch mints nothing and changes nothing.
    let described = harness.describe("after the refused repeats", &arn).await;
    assert_eq!(
        described.description.as_deref(),
        Some(first_description),
        "case \"after the refused repeats\": expected Description {first_description:?}, \
         got {:?}",
        described.description
    );
    assert_eq!(
        described.creation_time, first.creation_time,
        "case \"after the refused repeats\": the subscriber must be the one the token minted, \
         got CreationTime {:?}",
        described.creation_time
    );
    assert_eq!(
        selected(described.filter_configuration.as_ref()),
        vec![(DATA_PATTERN.to_string(), "DATA".to_string())],
        "case \"after the refused repeats\": the stored filters must be the first create's, \
         got {:?}",
        described.filter_configuration
    );
}

// ---------------------------------------------------------------------------
// RULE: a filter configuration that selects nothing is a refused request
// ---------------------------------------------------------------------------

/// `FilterConfiguration`: "On CreateSubscriber, Filters is required and must
/// contain at least one Filter with a non-empty Pattern … Any non-empty shape
/// (including `{Language:X}` without Filters) must contain a valid Filters
/// list". So a create whose filter configuration names no readable filter is
/// refused with the modelled `InvalidInputException` (400) and mints nothing —
/// rather than minting a subscriber that silently takes every event of its bus.
#[tokio::test]
async fn a_filter_configuration_that_names_no_readable_filter_is_refused() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the bus the refused creates name", "filter-gate-bus")
        .await;

    // The one language `FilterConfiguration$Language` declares, from
    // `FILTER_LANGUAGE_VALUES`.
    let language = FILTER_LANGUAGE_VALUES
        .first()
        .copied()
        .expect("FilterLanguage declares at least one value");

    // (case, the configuration the create carries, the whole Message)
    let rows: Vec<(&str, FilterConfiguration, String)> = vec![
        (
            "a filter configuration naming nothing at all",
            FilterConfiguration::default(),
            "Filters is required".to_string(),
        ),
        (
            "a filter configuration naming only its language",
            FilterConfiguration {
                language: Some(language.to_string()),
                filters: None,
            },
            "Filters is required".to_string(),
        ),
        (
            "a filter list with no filter in it",
            FilterConfiguration {
                language: None,
                filters: Some(Vec::new()),
            },
            "Filters is required".to_string(),
        ),
        (
            "a pattern that is not a document",
            filters(&[("every shipped order", "DATA")]),
            String::new(),
        ),
        (
            "a pattern whose member holds a bare value rather than a clause",
            filters(&[(r#"{"status":"shipped"}"#, "DATA")]),
            String::new(),
        ),
        (
            "a readable filter beside an unreadable one",
            filters(&[(DATA_PATTERN, "DATA"), ("{", "METADATA")]),
            String::new(),
        ),
    ];

    for (case, configuration, message) in rows {
        let error = harness
            .create(CreateSubscriberInput {
                name: "filter-gate-subscriber".to_string(),
                event_bus_arn: bus.clone(),
                invoke_configuration: invoke(TARGET_ARN),
                filter_configuration: Some(configuration),
                ..Default::default()
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the create must be refused"));
        assert_modelled(case, &error);
        assert_eq!(
            error.shape_name(),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {error:?}"
        );
        assert_eq!(
            error.http_status(),
            400,
            "case {case:?}: expected HTTP 400, got {}",
            error.http_status()
        );
        if !message.is_empty() {
            let reported = match &error {
                Scaffolded::Modeled(CreateSubscriberErrorModeled::InvalidInputException(
                    invalid,
                )) => invalid.message.clone().unwrap_or_default(),
                other => panic!("case {case:?}: expected InvalidInputException, got {other:?}"),
            };
            assert_eq!(
                reported, message,
                "case {case:?}: expected the Message {message:?}, got {reported:?}"
            );
        }
    }

    // None of those refusals minted anything: the name every one of them asked
    // for is still free, so a create that does name a readable filter takes it.
    let created = harness
        .create(CreateSubscriberInput {
            name: "filter-gate-subscriber".to_string(),
            event_bus_arn: bus.clone(),
            invoke_configuration: invoke(TARGET_ARN),
            filter_configuration: Some(filters(&[(DATA_PATTERN, "DATA")])),
            ..Default::default()
        })
        .await
        .expect("case \"after the refused creates\": a readable filter must be accepted");
    assert_subscriber_arn(
        "after the refused creates",
        created
            .subscriber_arn
            .as_deref()
            .expect("case \"after the refused creates\": the create reported no SubscriberArn"),
        "filter-gate-subscriber",
    );
}

// ---------------------------------------------------------------------------
// RULE: every refusal is one the model declares for this operation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_subscriber_refuses_only_with_the_errors_its_model_declares() {
    let harness = Harness::new();
    let bus = harness
        .active_bus("the bus the refusals are measured against", "refusal-bus")
        .await;

    // --- a bus that does not exist -----------------------------------------
    let absent = format!(
        "arn:aws:events:{REGION}:{}:event-busv2/absent-bus/{}",
        engine::emulator_account_id(),
        "a".repeat(SUBSCRIBER_ID_LEN)
    );
    let error = harness
        .create(CreateSubscriberInput {
            name: "refusal-no-bus".to_string(),
            event_bus_arn: absent,
            invoke_configuration: invoke(TARGET_ARN),
            ..Default::default()
        })
        .await
        .expect_err("case \"a bus that does not exist\": the create must be refused");
    assert_modelled("a bus that does not exist", &error);
    assert_eq!(
        error.shape_name(),
        "ResourceNotFoundException",
        "case \"a bus that does not exist\": expected ResourceNotFoundException, got {error:?}"
    );
    assert_eq!(
        error.http_status(),
        404,
        "case \"a bus that does not exist\": expected HTTP 404, got {}",
        error.http_status()
    );

    // --- a name already held on the bus ------------------------------------
    harness
        .create(CreateSubscriberInput {
            name: "refusal-taken".to_string(),
            event_bus_arn: bus.clone(),
            invoke_configuration: invoke(TARGET_ARN),
            client_token: Some("the-first-token".to_string()),
            ..Default::default()
        })
        .await
        .expect("case \"a name already held\": the first create must succeed");
    let error = harness
        .create(CreateSubscriberInput {
            name: "refusal-taken".to_string(),
            event_bus_arn: bus.clone(),
            invoke_configuration: invoke(TARGET_ARN),
            client_token: Some("a-second-token".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("case \"a name already held\": the second create must be refused");
    assert_modelled("a name already held", &error);
    assert_eq!(
        error.shape_name(),
        "ResourceAlreadyExistsException",
        "case \"a name already held\": expected ResourceAlreadyExistsException, got {error:?}"
    );
    assert_eq!(
        error.http_status(),
        409,
        "case \"a name already held\": expected HTTP 409, got {}",
        error.http_status()
    );

    // --- a bus that is no longer ACTIVE ------------------------------------
    // "The bus must be ACTIVE" (`CreateSubscriber`): a bus whose deletion has
    // begun takes on no new subscriber. Which refusal it earns depends on how
    // far the clock has carried that deletion — the bus is still there and not
    // ACTIVE, or it is gone — and both are refusals of this operation's
    // modelled set, so the row asserts the pair rather than a race.
    //
    // The row retires a bus of its OWN, because a bus a subscriber still holds
    // cannot be retired at all (`DeleteEventBus` refuses it as in use) and what
    // is measured here is the create's refusal, not the delete's.
    let retiring = harness
        .active_bus("a bus that is no longer ACTIVE", "refusal-retiring-bus")
        .await;
    DeleteEventBus::new(Arc::clone(&harness.engine))
        .handle(
            DeleteEventBusInput {
                event_bus_arn: retiring.clone(),
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"a bus that is no longer ACTIVE\": DeleteEventBus must succeed");
    let error = harness
        .create(CreateSubscriberInput {
            name: "refusal-deleting".to_string(),
            event_bus_arn: retiring.clone(),
            invoke_configuration: invoke(TARGET_ARN),
            ..Default::default()
        })
        .await
        .expect_err("case \"a bus that is no longer ACTIVE\": the create must be refused");
    assert_modelled("a bus that is no longer ACTIVE", &error);
    assert!(
        matches!(
            (error.shape_name(), error.http_status()),
            ("InvalidStateException", 409) | ("ResourceNotFoundException", 404)
        ),
        "case \"a bus that is no longer ACTIVE\": expected InvalidStateException (409) or \
         ResourceNotFoundException (404), got {error:?} at {}",
        error.http_status()
    );
}

/// Every refusal of this operation is one of the errors its model declares — and
/// so never the scaffold's not-implemented signal, which is in no model.
fn assert_modelled(case: &str, error: &Scaffolded<CreateSubscriberErrorModeled>) {
    assert!(
        matches!(error, Scaffolded::Modeled(_)),
        "case {case:?}: expected one of this operation's modelled errors, got {error:?}"
    );
    assert!(
        MODELLED_ERRORS.contains(&error.shape_name()),
        "case {case:?}: expected one of {MODELLED_ERRORS:?}, got {:?}",
        error.shape_name()
    );
    assert_ne!(
        error.http_status(),
        501,
        "case {case:?}: CreateSubscriber must never answer a not-implemented refusal, \
         got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_wire_answer_carries_the_created_subscriber() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let created_bus = cbor_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "wire-contract-bus".to_string(),
            // The create's `@idempotencyToken`, which the model leaves optional.
            client_token: Some("a-wire-bus-token".to_string()),
            ..Default::default()
        },
        "the wire create of the bus",
    )
    .await
    .event_bus_arn
    .expect("case \"the wire create of the bus\": CreateEventBus reported no EventBusArn");

    // The bus has to be ACTIVE before it takes a subscriber, so poll for that
    // CONDITION over the wire.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = cbor_ok::<DescribeEventBus<InMemoryEngine>>(
            &router,
            &DescribeEventBusInput {
                event_bus_arn: created_bus.clone(),
            },
            "the wire create",
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the wire create\": the bus never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let (status, body) = cbor_call::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &CreateSubscriberInput {
            name: "wire-contract-subscriber".to_string(),
            event_bus_arn: created_bus.clone(),
            invoke_configuration: invoke(TARGET_ARN),
            filter_configuration: Some(filters(&[(DATA_PATTERN, "DATA")])),
            // The create's `@idempotencyToken`, which the model leaves optional.
            client_token: Some("a-wire-subscriber-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the wire create\": CreateSubscriber expected 200, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert!(
        rendered.contains("wire-contract-subscriber"),
        "case \"the wire create\": the body must carry the subscriber's name, got {rendered:?}"
    );
    assert!(
        !rendered.contains("NotImplemented"),
        "case \"the wire create\": the body must not name a not-implemented refusal, \
         got {rendered:?}"
    );

    // A refusal on the wire is the modelled shape at the modelled status, not a
    // 501 either.
    let (status, body) = cbor_call::<CreateSubscriber<InMemoryEngine>>(
        &router,
        &CreateSubscriberInput {
            name: "wire-contract-subscriber".to_string(),
            event_bus_arn: created_bus,
            invoke_configuration: invoke(TARGET_ARN),
            filter_configuration: Some(FilterConfiguration::default()),
            client_token: Some("a-wire-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 400,
        "case \"the wire refusal\": expected 400, got {status} with {rendered:?}"
    );
    assert!(
        rendered.contains("InvalidInputException"),
        "case \"the wire refusal\": the body must name the modelled shape, got {rendered:?}"
    );
    assert!(
        rendered.contains("Filters is required"),
        "case \"the wire refusal\": the body must carry the Message, got {rendered:?}"
    );
}

/// The same call, requiring a 200 and decoding the typed output.
async fn cbor_ok<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
    case: &str,
) -> O::Output
where
    O::Output: DeserializableStruct,
{
    let (status, body) = cbor_call::<O>(router, input).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} must answer 200, got {status} with {:?}",
        O::NAME,
        String::from_utf8_lossy(&body)
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&body);
    O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
        panic!(
            "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
            O::NAME
        )
    })
}

/// Encode `input` as this service's wire protocol, dispatch it through the
/// router, and hand back the HTTP status and the raw response body.
async fn cbor_call<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
) -> (u16, bytes::Bytes) {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
        .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
    let body = serializer.finish();

    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        .body(bytes::Bytes::from(body))
        .unwrap_or_else(|e| panic!("{}: the request must be well formed, got {e:?}", O::NAME));

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body())
}
