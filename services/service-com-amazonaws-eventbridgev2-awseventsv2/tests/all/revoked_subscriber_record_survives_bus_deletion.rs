// SPDX-License-Identifier: CC0-1.0

//! What a revoked subscriber's RECORD is worth once the bus it was created on has
//! finished disappearing.
//!
//! RULE: a subscriber's record belongs to the subscriber, not to its bus, once the
//! subscriber has been revoked — revoking is what released the bus for deletion,
//! and `DescribeSubscriberResponse$Revoked` states the rest: "Revocation is
//! terminal: it never returns to false. Mutating operations on a revoked
//! subscriber fail with InvalidStateException, except DeleteSubscriber, which
//! stays available so a revoked subscriber can still be cleaned up". So the
//! completion of the bus's asynchronous deletion changes nothing about such a
//! subscriber: the read of it still answers 200 with the `SubscriberArn` it was
//! asked for and every member it reported before the bus went, a mutating call on
//! it is still refused `InvalidStateException`, and only its OWN delete removes
//! it, after which its ARN resolves to nothing.
//!
//! `tests/all/revoked_resource_outlives_its_bus.rs` pins the survival itself
//! across both revocable resource types; this suite pins what the surviving
//! subscriber record CONTAINS and what it still refuses, read over the
//! `rpcv2Cbor` wire so the HTTP status is the emulator's own.
//!
//! The rows span what the rule generalises over: how much configuration the
//! subscriber carries (only the required members, through to every optional shape
//! at once), its run state, its delivery ordering mode, whether it filters, and
//! how many revoked subscribers the vanishing bus carried (several, and exactly
//! one).
//!
//! Nothing here waits on the wall clock: the two waits are the bus's own
//! provisioning and deletion transitions, each polled for the CONDITION under a
//! generous deadline.

use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_subscriber::DeleteSubscriber;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    BatchConfiguration, CreateSubscriberInput, DeleteEventBusInput, DeleteSubscriberInput,
    DescribeSubscriberInput, DescribeSubscriberOutput, Filter, FilterConfiguration,
    LogConfiguration, OnFailureConfiguration, RetryPolicy, RevokeResourceInput, Transformer,
    UpdateSubscriberInput,
};
use eventsv2::ops::update_subscriber::UpdateSubscriber;

use crate::modelled_input_refusal::{
    create_subscriber, refusal, rendered, with_client_token, Wire, RESOURCE_NOT_FOUND_EXCEPTION,
};

/// The status `DescribeSubscriber` models its only refusal at.
/// Source: `smithy_query(query="operation", operation="DescribeSubscriber")`.
const RESOURCE_NOT_FOUND_STATUS: u16 = 404;

/// The `__type` a refusal of a mutating call on a revoked subscriber carries, and
/// the status `UpdateSubscriber` models it at.
/// Source: `smithy_query(query="operation", operation="UpdateSubscriber")`.
const INVALID_STATE_EXCEPTION: &str = "InvalidStateException";
const INVALID_STATE_STATUS: u16 = 409;

/// The whole `Message` a mutating call on a revoked subscriber is refused with —
/// spelled out here rather than read from the crate, so a reworded refusal fails
/// this suite.
const REVOKED_SUBSCRIBER_MESSAGE: &str = "Subscriber is revoked and cannot be updated";

/// A dead-letter target an `OnFailureConfiguration` can name. Nothing is published
/// below, so it only has to be a well-formed ARN.
const DEAD_LETTER_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:survives-dead-letter";

/// An event pattern a filter can carry, of the one language this service reads.
const PATTERN: &str = r#"{"source":["survives.probe"]}"#;

/// The `FilterScope` the filter below is read in, one of DATA | METADATA |
/// SYSTEM_METADATA.
const FILTER_SCOPE: &str = "METADATA";

/// `SubscriberState`, one of RUNNING | STOPPED.
const STOPPED: &str = "STOPPED";

/// `OrderingType`, one of FIFO | UNORDERED.
const FIFO: &str = "FIFO";

/// `StartingPosition`, one of LATEST | POINT_IN_TIME.
const LATEST: &str = "LATEST";

/// `TransformerType`, one of RAW | WITH_METADATA.
const WITH_METADATA: &str = "WITH_METADATA";

/// `RetryStrategy`, whose one declared entry is ALL.
const RETRY_STRATEGY: &str = "ALL";

/// `LogConfiguration$Level` and `$IncludePayload`, from the model's enums.
const LOG_LEVEL: &str = "INFO";
const INCLUDE_PAYLOAD: &str = "FULL";

/// The configuration the fully-configured row submits, inside each member's
/// published range.
/// Source: `smithy_query(query="shape", shape="BatchConfiguration,RetryPolicy")`.
const MAX_BATCH_SIZE: i32 = 25;
const MAX_BATCH_WINDOW: i32 = 7;
const MAX_RETRY_ATTEMPTS: i32 = 3;
const MAX_EVENT_AGE: i32 = 120;

/// The descriptions the two describing rows submit.
const FIFO_DESCRIPTION: &str = "the subscriber the bus owner withdrew";
const FULL_DESCRIPTION: &str = "fully configured, then withdrawn";

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// What a row writes onto the create it configures.
type ConfigureCreate = fn(&mut CreateSubscriberInput);

/// What an update row writes onto the update it configures.
type ConfigureUpdate = fn(&mut UpdateSubscriberInput);

/// What a row asserts a read of its subscriber reports: the members its create
/// NAMED, so a projection that silently dropped them cannot be mistaken for a
/// surviving record.
type AssertReported = fn(&str, &Record);

/// One row of the table: the case it stands for, the name segment its subscriber
/// takes, what its create carries beyond the required members, and what a read of
/// the result must therefore report.
struct Row {
    case: &'static str,
    name: &'static str,
    configure: ConfigureCreate,
    assert_reported: AssertReported,
}

/// One filter a `FilterConfiguration` holds, as `(Pattern, Scope)`.
type StoredFilter = (String, String);

/// A `FilterConfiguration`, as `(Language, every filter it holds)`.
type StoredFilters = (Option<String>, Vec<StoredFilter>);

/// The rows the rule spans, each a differently configured subscriber.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a subscriber carrying only the required members",
            name: "bare",
            configure: |_| {},
            // The create named none of the optional members, so this row states
            // only what every subscriber carries: the invoke configuration the
            // create required of it.
            assert_reported: |case, record| {
                assert!(
                    record.invoke.is_some(),
                    "case {case:?}: every subscriber reports the InvokeConfiguration it was \
                     created with, got {:?}",
                    record.invoke
                );
            },
        },
        Row {
            case: "a stopped subscriber",
            name: "stopped",
            configure: |input| {
                input.state = Some(STOPPED.to_string());
            },
            assert_reported: |case, record| {
                assert_eq!(
                    record.run_state.as_deref(),
                    Some(STOPPED),
                    "case {case:?}: expected the run state the create named, got {:?}",
                    record.run_state
                );
            },
        },
        Row {
            case: "a FIFO subscriber that describes itself and filters",
            name: "fifo-filtered",
            configure: |input| {
                input.r#type = Some(FIFO.to_string());
                input.description = Some(FIFO_DESCRIPTION.to_string());
                input.filter_configuration = Some(FilterConfiguration {
                    language: None,
                    filters: Some(vec![Filter {
                        pattern: PATTERN.to_string(),
                        scope: FILTER_SCOPE.to_string(),
                    }]),
                });
            },
            assert_reported: |case, record| {
                assert_eq!(
                    (
                        record.ordering_type.as_deref(),
                        record.description.as_deref()
                    ),
                    (Some(FIFO), Some(FIFO_DESCRIPTION)),
                    "case {case:?}: expected the ordering mode and description the create named, \
                     got {:?} / {:?}",
                    record.ordering_type,
                    record.description
                );
                let filters = record
                    .filters
                    .as_ref()
                    .map(|(_, filters)| filters.clone())
                    .unwrap_or_default();
                assert_eq!(
                    filters,
                    vec![(PATTERN.to_string(), FILTER_SCOPE.to_string())],
                    "case {case:?}: expected the one filter the create named, got {filters:?}"
                );
            },
        },
        Row {
            case: "a subscriber naming its starting position explicitly",
            name: "from-latest",
            configure: |input| {
                input.starting_position = Some(LATEST.to_string());
            },
            assert_reported: |case, record| {
                assert_eq!(
                    record.starting_position.as_deref(),
                    Some(LATEST),
                    "case {case:?}: expected the starting position the create named, got {:?}",
                    record.starting_position
                );
            },
        },
        Row {
            case: "a subscriber carrying every optional configuration shape at once",
            name: "fully-configured",
            configure: |input| {
                input.description = Some(FULL_DESCRIPTION.to_string());
                input.batch_configuration = Some(BatchConfiguration {
                    max_batch_size: Some(MAX_BATCH_SIZE),
                    max_batch_window_in_seconds: Some(MAX_BATCH_WINDOW),
                });
                input.retry_policy = Some(RetryPolicy {
                    max_retry_attempts: Some(MAX_RETRY_ATTEMPTS),
                    max_event_age_in_seconds: Some(MAX_EVENT_AGE),
                    retry_strategy: Some(RETRY_STRATEGY.to_string()),
                });
                input.transformer = Some(Transformer {
                    r#type: Some(WITH_METADATA.to_string()),
                    jsonata_configuration: None,
                });
                input.on_failure_configuration = Some(OnFailureConfiguration {
                    arn: Some(DEAD_LETTER_ARN.to_string()),
                });
                input.log_configuration = Some(LogConfiguration {
                    level: Some(LOG_LEVEL.to_string()),
                    include_payload: Some(INCLUDE_PAYLOAD.to_string()),
                });
            },
            // Every member this create named, as the read must report it: the
            // surviving record carries the CONFIGURATION, not just an ARN.
            assert_reported: |case, record| {
                assert_eq!(
                    record.description.as_deref(),
                    Some(FULL_DESCRIPTION),
                    "case {case:?}: expected the description the create named, got {:?}",
                    record.description
                );
                assert_eq!(
                    record.batch,
                    Some((Some(MAX_BATCH_SIZE), Some(MAX_BATCH_WINDOW))),
                    "case {case:?}: expected the batching configuration the create named, got {:?}",
                    record.batch
                );
                assert_eq!(
                    record.retry,
                    Some((
                        Some(MAX_RETRY_ATTEMPTS),
                        Some(MAX_EVENT_AGE),
                        Some(RETRY_STRATEGY.to_string())
                    )),
                    "case {case:?}: expected the retry policy the create named, got {:?}",
                    record.retry
                );
                assert_eq!(
                    record.transformer,
                    Some(Some(WITH_METADATA.to_string())),
                    "case {case:?}: expected the transformer the create named, got {:?}",
                    record.transformer
                );
                assert_eq!(
                    record.on_failure,
                    Some(Some(DEAD_LETTER_ARN.to_string())),
                    "case {case:?}: expected the on-failure target the create named, got {:?}",
                    record.on_failure
                );
                assert_eq!(
                    record.log,
                    Some((
                        Some(LOG_LEVEL.to_string()),
                        Some(INCLUDE_PAYLOAD.to_string())
                    )),
                    "case {case:?}: expected the log configuration the create named, got {:?}",
                    record.log
                );
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// The record a read of a subscriber reports
// ---------------------------------------------------------------------------

/// Every member `DescribeSubscriber` projects, in one comparable value — so an
/// assertion states the WHOLE record rather than the two members a survival check
/// would otherwise look at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    subscriber_arn: Option<String>,
    name: Option<String>,
    event_bus_arn: Option<String>,
    description: Option<String>,
    run_state: Option<String>,
    ordering_type: Option<String>,
    starting_position: Option<String>,
    /// `PointInTimeConfiguration`, as `(PointType, StartingPoint, EndPoint)`.
    point_in_time: Option<(String, Option<i64>, Option<i64>)>,
    revoked: Option<bool>,
    creation_time: Option<i64>,
    last_modified_time: Option<i64>,
    /// `InvokeConfiguration`, as `(RoleArn, TargetArn)`.
    invoke: Option<(String, String)>,
    /// `FilterConfiguration`, as [`StoredFilters`].
    filters: Option<StoredFilters>,
    /// `BatchConfiguration`, as `(MaxBatchSize, MaxBatchWindowInSeconds)`.
    batch: Option<(Option<i32>, Option<i32>)>,
    /// `RetryPolicy`, as `(MaxRetryAttempts, MaxEventAgeInSeconds, RetryStrategy)`.
    retry: Option<(Option<i32>, Option<i32>, Option<String>)>,
    /// `Transformer$Type`; the outer `None` means the member is absent.
    transformer: Option<Option<String>>,
    /// `OnFailureConfiguration$Arn`.
    on_failure: Option<Option<String>>,
    /// `LogConfiguration`, as `(Level, IncludePayload)`.
    log: Option<(Option<String>, Option<String>)>,
}

/// What `described` reports, as one [`Record`].
fn record_of(described: &DescribeSubscriberOutput) -> Record {
    Record {
        subscriber_arn: described.subscriber_arn.clone(),
        name: described.name.clone(),
        event_bus_arn: described.event_bus_arn.clone(),
        description: described.description.clone(),
        run_state: described.state.clone(),
        ordering_type: described.r#type.clone(),
        starting_position: described.starting_position.clone(),
        point_in_time: described
            .point_in_time_configuration
            .as_ref()
            .map(|configuration| {
                (
                    configuration.point_type.clone(),
                    configuration.starting_point.as_ref().map(|at| at.secs()),
                    configuration.end_point.as_ref().map(|at| at.secs()),
                )
            }),
        revoked: described.revoked,
        creation_time: described.creation_time.as_ref().map(|at| at.secs()),
        last_modified_time: described.last_modified_time.as_ref().map(|at| at.secs()),
        invoke: described
            .invoke_configuration
            .as_ref()
            .map(|configuration| {
                (
                    configuration.role_arn.clone(),
                    configuration.target_arn.clone(),
                )
            }),
        filters: described
            .filter_configuration
            .as_ref()
            .map(|configuration| {
                (
                    configuration.language.clone(),
                    configuration
                        .filters
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|filter| (filter.pattern, filter.scope))
                        .collect(),
                )
            }),
        batch: described
            .batch_configuration
            .as_ref()
            .map(|batch| (batch.max_batch_size, batch.max_batch_window_in_seconds)),
        retry: described.retry_policy.as_ref().map(|policy| {
            (
                policy.max_retry_attempts,
                policy.max_event_age_in_seconds,
                policy.retry_strategy.clone(),
            )
        }),
        transformer: described
            .transformer
            .as_ref()
            .map(|transformer| transformer.r#type.clone()),
        on_failure: described
            .on_failure_configuration
            .as_ref()
            .map(|configuration| configuration.arn.clone()),
        log: described.log_configuration.as_ref().map(|configuration| {
            (
                configuration.level.clone(),
                configuration.include_payload.clone(),
            )
        }),
    }
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// One revoked subscriber, with the record it reported while its bus still stood
/// and the assertion its row makes about what a read of it reports.
struct Withdrawn {
    case: &'static str,
    arn: String,
    before: Record,
    assert_reported: AssertReported,
}

/// Read the subscriber at `arn`, requiring the 200 the model's only success
/// status.
async fn read(wire: &Wire, case: &str, arn: &str) -> DescribeSubscriberOutput {
    wire.ok::<DescribeSubscriber<InMemoryEngine>>(
        case,
        &DescribeSubscriberInput {
            subscriber_arn: arn.to_string(),
        },
    )
    .await
}

/// Create one subscriber per row on `bus`, revoke each, and hand back what each
/// read reported while the bus still stood.
async fn withdrawn_subscribers(
    wire: &Wire,
    bus: &str,
    prefix: &str,
    rows: &[Row],
) -> Vec<Withdrawn> {
    let mut withdrawn = Vec::new();
    for row in rows {
        let case = row.case;
        let mut input = create_subscriber(&format!("{prefix}-{}", row.name), bus, None);
        (row.configure)(&mut input);
        let created = wire
            .ok::<CreateSubscriber<InMemoryEngine>>(case, &with_client_token(input))
            .await;
        let arn = created.subscriber_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: CreateSubscriber must report a SubscriberArn, got {created:?}")
        });

        let fresh = read(wire, case, &arn).await;
        assert_eq!(
            fresh.revoked, None,
            "case {case:?}: a subscriber nobody has revoked must report no Revoked member, got \
             {:?}",
            fresh.revoked
        );
        // What the create named is reported before anything is revoked or
        // deleted, so the comparison below cannot be satisfied by two equally
        // empty projections.
        (row.assert_reported)(case, &record_of(&fresh));

        let answered = wire
            .ok::<RevokeResource<InMemoryEngine>>(case, &RevokeResourceInput { arn: arn.clone() })
            .await
            .arn;
        assert_eq!(
            answered.as_deref(),
            Some(arn.as_str()),
            "case {case:?}: RevokeResource must answer the ARN it withdrew, got {answered:?}"
        );

        let described = read(wire, case, &arn).await;
        assert_eq!(
            described.revoked,
            Some(true),
            "case {case:?}: the read after the withdrawal must report Revoked true, got {:?}",
            described.revoked
        );
        withdrawn.push(Withdrawn {
            case,
            before: record_of(&described),
            arn,
            assert_reported: row.assert_reported,
        });
    }
    withdrawn
}

/// Start the bus's deletion and poll until the bus itself no longer resolves,
/// which is the condition every assertion afterwards is made under.
async fn delete_bus_and_wait_gone(wire: &Wire, case: &str, bus: &str) {
    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: bus.to_string(),
        })
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: DeleteEventBus must succeed once every subscriber of the bus is revoked, \
         got {status} with {:?}",
        rendered(&body)
    );
    // The shared harness owns this poll, next to the one that waits for a bus to
    // report ACTIVE.
    wire.await_bus_gone(case, bus).await;
}

/// Every row's revoked subscriber, read back after `bus` has gone: the read must
/// answer 200, carry the `SubscriberArn` it was asked for, report the
/// configuration its create named, and report exactly what it reported while the
/// bus stood.
async fn assert_each_record_survived(wire: &Wire, withdrawn: &[Withdrawn], bus: &str) {
    for subscriber in withdrawn {
        let case = subscriber.case;
        // `Wire::ok` is what requires the 200 the model's only success status, so
        // one round trip settles both the status and the body.
        let described = read(
            wire,
            &format!("{case} read back after its bus is gone"),
            &subscriber.arn,
        )
        .await;
        assert_eq!(
            described.subscriber_arn.as_deref(),
            Some(subscriber.arn.as_str()),
            "case {case:?}: the read must answer the SubscriberArn it was asked for, got {:?}",
            described.subscriber_arn
        );
        assert_eq!(
            described.event_bus_arn.as_deref(),
            Some(bus),
            "case {case:?}: the surviving record must still report the bus it was created on, got \
             {:?}",
            described.event_bus_arn
        );
        assert_eq!(
            described.revoked,
            Some(true),
            "case {case:?}: the surviving record must still report Revoked true, got {:?}",
            described.revoked
        );
        let after = record_of(&described);
        // The configuration the create named, asserted against the submitted
        // values rather than against the earlier projection, so an empty one
        // cannot satisfy the comparison below.
        (subscriber.assert_reported)(case, &after);
        assert_eq!(
            after, subscriber.before,
            "case {case:?}: the record of a revoked subscriber must be unchanged by its bus's \
             deletion, expected {:?}, got {after:?}",
            subscriber.before
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: the whole record survives the bus's deletion
// ---------------------------------------------------------------------------

/// RULE, per row and with several revoked subscribers on the vanishing bus: each
/// one reads back whole once the bus no longer resolves.
#[tokio::test]
async fn every_revoked_subscriber_reads_back_whole_once_its_bus_has_disappeared() {
    let case = "the surviving table";
    let wire = Wire::new();
    let bus = wire.active_bus(case, "survives-table-bus").await;
    let withdrawn = withdrawn_subscribers(&wire, &bus, "survives", &rows()).await;

    delete_bus_and_wait_gone(&wire, case, &bus).await;

    assert_each_record_survived(&wire, &withdrawn, &bus).await;
}

/// RULE where the vanishing bus carried exactly ONE subscriber, so the surviving
/// record is the only one the store ever held for that bus.
#[tokio::test]
async fn the_only_revoked_subscriber_of_a_bus_reads_back_whole_once_the_bus_has_disappeared() {
    let case = "the only subscriber of its bus";
    let wire = Wire::new();
    let bus = wire.active_bus(case, "survives-single-bus").await;
    let only: Vec<Row> = vec![Row {
        case,
        name: "only",
        configure: |input| {
            input.description = Some(FULL_DESCRIPTION.to_string());
        },
        assert_reported: |case, record| {
            assert_eq!(
                record.description.as_deref(),
                Some(FULL_DESCRIPTION),
                "case {case:?}: expected the description the create named, got {:?}",
                record.description
            );
        },
    }];
    let withdrawn = withdrawn_subscribers(&wire, &bus, "single", &only).await;

    delete_bus_and_wait_gone(&wire, case, &bus).await;

    assert_each_record_survived(&wire, &withdrawn, &bus).await;
}

// ---------------------------------------------------------------------------
// RULE: the terminal revoked state, and the delete left for cleanup, outlive
// the bus too
// ---------------------------------------------------------------------------

/// RULE, per row: a surviving revoked subscriber is still terminal — whichever
/// member a mutating call carries, it is refused `InvalidStateException` (409)
/// with the sentence the model states — while its own delete is still available,
/// and after that delete its ARN resolves to nothing (404).
#[tokio::test]
async fn a_revoked_subscriber_whose_bus_is_gone_is_still_terminal_and_still_deletable() {
    let case = "the terminal table";
    let wire = Wire::new();
    let bus = wire.active_bus(case, "survives-terminal-bus").await;
    let withdrawn = withdrawn_subscribers(&wire, &bus, "terminal", &rows()).await;

    delete_bus_and_wait_gone(&wire, case, &bus).await;

    // The mutating requests the refusal must hold for, differing in which member
    // the update carries: `UpdateSubscriber` leaves an omitted member unchanged,
    // so each of these is a different change asked of the same record.
    let updates: [(&str, ConfigureUpdate); 4] = [
        ("an update carrying nothing but the ARN", |_| {}),
        ("an update of the description", |input| {
            input.description = Some("a change the withdrawal must refuse".to_string());
        }),
        ("an update of the run state", |input| {
            input.state = Some(STOPPED.to_string());
        }),
        ("an update of the batching configuration", |input| {
            input.batch_configuration = Some(BatchConfiguration {
                max_batch_size: Some(1),
                max_batch_window_in_seconds: Some(0),
            });
        }),
    ];

    for subscriber in &withdrawn {
        for (change, configure) in &updates {
            let case = format!("{} + {change}", subscriber.case);
            let mut input = UpdateSubscriberInput {
                subscriber_arn: subscriber.arn.clone(),
                ..Default::default()
            };
            configure(&mut input);
            let (status, body) = wire.call::<UpdateSubscriber<InMemoryEngine>>(&input).await;
            let (code, message) = refusal(&case, &body);
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (
                    INVALID_STATE_STATUS,
                    INVALID_STATE_EXCEPTION,
                    REVOKED_SUBSCRIBER_MESSAGE
                ),
                "case {case:?}: a mutating call on a revoked subscriber must stay refused after \
                 its bus is gone, got {status} with {code:?} / {message:?}"
            );
        }
    }

    // "Delete stays available for cleanup": the record goes only when its own
    // delete removes it.
    for subscriber in &withdrawn {
        let case = subscriber.case;
        let (status, body) = wire
            .call::<DeleteSubscriber<InMemoryEngine>>(&DeleteSubscriberInput {
                subscriber_arn: subscriber.arn.clone(),
            })
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: DeleteSubscriber on a revoked subscriber must succeed, got {status} \
             with {:?}",
            rendered(&body)
        );

        let (status, body) = wire
            .call::<DescribeSubscriber<InMemoryEngine>>(&DescribeSubscriberInput {
                subscriber_arn: subscriber.arn.clone(),
            })
            .await;
        let (code, _) = refusal(case, &body);
        assert_eq!(
            (status, code.as_str()),
            (RESOURCE_NOT_FOUND_STATUS, RESOURCE_NOT_FOUND_EXCEPTION),
            "case {case:?}: once its own delete removed it, the ARN must read as \
             ResourceNotFoundException (404), got {status} with {code:?}"
        );
    }
}
