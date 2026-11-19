// SPDX-License-Identifier: CC0-1.0

//! What `UpdateSubscriber` applies to a stored subscriber, what it refuses, and
//! what an unpause makes due.
//!
//! RULE: `UpdateSubscriber` writes every member the request carries onto the
//! subscriber its `SubscriberArn` addresses and leaves every member the request
//! omits exactly as it was — "Fields omitted from the request are left
//! unchanged" — answering the subscriber as it stands afterwards; a carried
//! member REPLACES the stored one wholesale rather than merging into it, an
//! empty `FilterConfiguration` clears the filter, `LastModifiedTime` moves only
//! for a call that had something to apply, and `ResumePosition` is honored only
//! on the call that transitions `State` from `STOPPED` to `RUNNING`, where
//! `LAST_PROCESSED` still owes the subscriber the backlog it accrued and
//! `LATEST` skips it. An ARN naming no stored subscriber is
//! `ResourceNotFoundException` (404), a subscriber the bus owner has revoked
//! takes no change at all (`InvalidStateException`, 409), and a request whose
//! members cannot describe a subscriber is `InvalidInputException` (400) — none
//! of which writes anything.
//!
//! The rows span that class rather than one recorded update: they differ along
//! WHICH members the request carries (one, several, all of a shape, none at
//! all), along whether a member is replaced or cleared, along the run-state
//! transition the call asks for, and along the resume position an unpause names.
//! Amazon SQS is mounted beside EventBridge V2 on ONE engine, so what an unpause
//! delivers is asserted where a client sees it: in the messages the subscriber's
//! target queue holds.
//!
//! Nothing here waits on the wall clock: an update performs the deliveries it
//! has made due before it answers, so a read that follows it is a read of a
//! settled queue. The one wait is for a bus's own provisioning transition,
//! polled for the CONDITION (`ACTIVE`) under a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use serde_json::{json, Value};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::revoke_resource::RevokeResource;
use eventsv2::ops::types::{
    BatchConfiguration, CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput,
    DescribeSubscriberInput, DescribeSubscriberOutput, Filter, FilterConfiguration,
    InvokeConfiguration, LogConfiguration, OnFailureConfiguration, PutEventsInput,
    PutEventsRequestEntry, RetryPolicy, RevokeResourceInput, SqsParameters, Transformer,
    UniversalTargetParameters, UpdateInvokeConfiguration, UpdateSubscriberInput,
    UpdateSubscriberOutput,
};
use eventsv2::ops::update_subscriber::{UpdateSubscriber, UpdateSubscriberErrorModeled};
use eventsv2::scaffolded::Scaffolded;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role the subscriber of a row is created with, of the modelled `RoleArn`
/// shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/update-delivery";

/// The role an update that carries an `InvokeConfiguration` moves the subscriber
/// to, so the write is visible as a CHANGE rather than as the created value.
const OTHER_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/update-delivery-two";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// The harness: EventBridge V2 and Amazon SQS on ONE engine
// ---------------------------------------------------------------------------

/// A SigV4 `Authorization` header naming `service` in [`REGION`], which is how a
/// request names both.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`].
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/UpdateSubscriber")
        .header("authorization", authorization("events"))
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// One emulator over one shared engine, with Amazon SQS mounted beside
/// EventBridge V2 — the shape `core/router/src/lib.rs` mounts them in, which is
/// what makes a handler's in-process callback reach the queue a subscriber
/// targets.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self {
            engine,
            definition,
            callback,
            router,
            parts: parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// One awsJson1_0 call against Amazon SQS, required to answer 200.
    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("sqs.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("AmazonSQS.{operation}"))
            .header("authorization", authorization("sqs"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the harness request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed: Value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or(Value::Null)
        };
        assert_eq!(
            status, 200,
            "case {case:?}: SQS {operation} must answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// A queue called `name`: its url, and the ARN a subscriber targets it by.
    async fn queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateQueue reported no QueueUrl in {created}")
            })
            .to_string();
        (url, format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"))
    }

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state a
    /// create of a subscriber and a publish both require of it. Polls the
    /// condition rather than sleeping a fixed time, because provisioning is
    /// asynchronous.
    async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("token-for-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

        let deadline = Instant::now() + PATIENCE;
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

    /// Create the subscriber `input` describes and hand back its ARN.
    async fn create_subscriber(&self, case: &str, input: CreateSubscriberInput) -> String {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"))
            .subscriber_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber reported no SubscriberArn"))
    }

    /// `UpdateSubscriber` as the operation itself answers it.
    async fn update(
        &self,
        input: UpdateSubscriberInput,
    ) -> Result<UpdateSubscriberOutput, Scaffolded<UpdateSubscriberErrorModeled>> {
        UpdateSubscriber::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    /// The subscriber at `arn` as `DescribeSubscriber` reports it.
    async fn describe(&self, case: &str, arn: &str) -> DescribeSubscriberOutput {
        DescribeSubscriber::new(Arc::clone(&self.engine))
            .handle(
                DescribeSubscriberInput {
                    subscriber_arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DescribeSubscriber must succeed, got {e:?}"))
    }

    /// Withdraw the resource at `arn`.
    async fn revoke(&self, case: &str, arn: &str) {
        RevokeResource::new(Arc::clone(&self.engine))
            .handle(
                RevokeResourceInput {
                    arn: arn.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: RevokeResource must succeed, got {e:?}"));
    }

    /// Publish `count` events onto `event_bus_arn`, each distinguishable by its
    /// detail, requiring every one of them to be accepted.
    async fn publish(&self, case: &str, event_bus_arn: &str, count: usize) {
        let entries: Vec<PutEventsRequestEntry> = (0..count)
            .map(|position| PutEventsRequestEntry {
                source: "update.suite".to_string(),
                detail_type: "BacklogEvent".to_string(),
                detail: Some(format!("{{\"position\":{position}}}")),
                ..Default::default()
            })
            .collect();
        let answer = PutEvents::new(Arc::clone(&self.engine))
            .handle(
                PutEventsInput {
                    event_bus_arn: event_bus_arn.to_string(),
                    entries,
                    deduplication_configuration: None,
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutEvents must succeed, got {e:?}"));
        assert_eq!(
            answer.failed_entry_count, 0,
            "case {case:?}: every published entry must be accepted, got {answer:?}"
        );
    }

    /// How many messages the queue at `queue_url` reports available right now.
    async fn queue_depth(&self, case: &str, queue_url: &str) -> u64 {
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": queue_url, "AttributeNames": ["ApproximateNumberOfMessages"] }),
            )
            .await;
        let reported = attributes["Attributes"]["ApproximateNumberOfMessages"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: GetQueueAttributes reported no depth in {attributes}")
            });
        reported.parse::<u64>().unwrap_or_else(|error| {
            panic!("case {case:?}: ApproximateNumberOfMessages {reported:?} is no decimal: {error}")
        })
    }

    /// How many messages the queue at `queue_url` holds, polled until `expected`
    /// of them have been read or a generous deadline passes, each deleted as it
    /// is read so a later poll reports what is left.
    async fn received(&self, case: &str, queue_url: &str, expected: usize) -> usize {
        let deadline = Instant::now() + PATIENCE;
        let mut read = 0;
        loop {
            let answer = self
                .sqs(
                    case,
                    "ReceiveMessage",
                    &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
                )
                .await;
            for message in answer["Messages"].as_array().unwrap_or(&Vec::new()) {
                read += 1;
                self.sqs(
                    case,
                    "DeleteMessage",
                    &json!({
                        "QueueUrl": queue_url,
                        "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                    }),
                )
                .await;
            }
            if read >= expected || Instant::now() >= deadline {
                return read;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The subscriber a row starts from, and the facts a read reports about it
// ---------------------------------------------------------------------------

/// One `Filter`, in the scope it reads. `FilterScope`'s declared values are
/// `DATA | METADATA | SYSTEM_METADATA`.
fn filter(pattern: &str, scope: &str) -> Filter {
    Filter {
        pattern: pattern.to_string(),
        scope: scope.to_string(),
    }
}

/// A `FilterConfiguration` holding `filters`, which are implicitly ANDed.
fn filters(filters: Vec<Filter>) -> FilterConfiguration {
    FilterConfiguration {
        language: None,
        filters: Some(filters),
    }
}

/// The create every row of the merge table starts from: a subscriber carrying a
/// value for every member an update can change, so each row's write is visible
/// as a CHANGE and every member it omits is visible as untouched.
fn created(name: &str, event_bus_arn: &str, target_arn: &str) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        client_token: Some(format!("token-for-{name}")),
        description: Some("the subscriber as created".to_string()),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: target_arn.to_string(),
            sqs_parameters: Some(SqsParameters {
                message_group_id: Some("created-group".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
        filter_configuration: Some(filters(vec![filter(
            "{\"source\":[\"created.source\"]}",
            "METADATA",
        )])),
        batch_configuration: Some(BatchConfiguration {
            // `MaxBatchSize` is @range 1..500 and `MaxBatchWindowInSeconds`
            // @range 0..300.
            max_batch_size: Some(10),
            max_batch_window_in_seconds: Some(5),
        }),
        transformer: Some(Transformer {
            // `TransformerType`: RAW | WITH_METADATA | JSONATA.
            r#type: Some("RAW".to_string()),
            jsonata_configuration: None,
        }),
        retry_policy: Some(RetryPolicy {
            // `MaxRetryAttempts` is @range 0..185, `MaxEventAgeInSeconds`
            // @range 60..86400, and `RetryStrategy` declares ALL.
            max_retry_attempts: Some(5),
            max_event_age_in_seconds: Some(300),
            retry_strategy: Some("ALL".to_string()),
        }),
        on_failure_configuration: Some(OnFailureConfiguration {
            arn: Some(format!("arn:aws:sqs:{REGION}:{ACCOUNT}:created-failures")),
        }),
        log_configuration: Some(LogConfiguration {
            // `LogLevel`: OFF | ERROR | INFO.
            level: Some("ERROR".to_string()),
            include_payload: None,
        }),
        ..Default::default()
    }
}

/// Everything a read reports about a subscriber that an update can change, in
/// one comparable value — so a row states the WHOLE subscriber it expects rather
/// than only the member it wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    description: Option<String>,
    role_arn: String,
    target_arn: String,
    sqs_message_group: Option<String>,
    filters: Option<Vec<(String, String)>>,
    batch: Option<(Option<i32>, Option<i32>)>,
    transformer: Option<String>,
    retry: Option<(Option<i32>, Option<i32>, Option<String>)>,
    on_failure: Option<String>,
    log_level: Option<String>,
    state: Option<String>,
    ordering_type: Option<String>,
    starting_position: Option<String>,
}

/// The facts of the subscriber `described` reports.
fn facts_of(described: &DescribeSubscriberOutput) -> Facts {
    let invoke = described
        .invoke_configuration
        .clone()
        .expect("a read reports the subscriber's InvokeConfiguration");
    Facts {
        description: described.description.clone(),
        role_arn: invoke.role_arn,
        target_arn: invoke.target_arn,
        sqs_message_group: invoke
            .sqs_parameters
            .and_then(|parameters| parameters.message_group_id),
        filters: described.filter_configuration.as_ref().map(|configuration| {
            configuration
                .filters
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|filter| (filter.pattern, filter.scope))
                .collect()
        }),
        batch: described
            .batch_configuration
            .as_ref()
            .map(|batch| (batch.max_batch_size, batch.max_batch_window_in_seconds)),
        transformer: described
            .transformer
            .as_ref()
            .and_then(|transformer| transformer.r#type.clone()),
        retry: described.retry_policy.as_ref().map(|retry| {
            (
                retry.max_retry_attempts,
                retry.max_event_age_in_seconds,
                retry.retry_strategy.clone(),
            )
        }),
        on_failure: described
            .on_failure_configuration
            .as_ref()
            .and_then(|configuration| configuration.arn.clone()),
        log_level: described
            .log_configuration
            .as_ref()
            .and_then(|configuration| configuration.level.clone()),
        state: described.state.clone(),
        ordering_type: described.r#type.clone(),
        starting_position: described.starting_position.clone(),
    }
}

// ---------------------------------------------------------------------------
// The merge rule, over a table
// ---------------------------------------------------------------------------

/// An update writes the members it carries and leaves the ones it omits exactly
/// as they were, and a carried member replaces the stored one wholesale.
///
/// The rows differ along which members the request carries — one, several, a
/// whole shape replaced, a shape cleared, and none at all.
#[tokio::test]
async fn an_update_writes_the_members_it_carries_and_leaves_the_rest_alone() {
    let harness = Harness::new();
    let bus = harness.active_bus("merge", "update-merge-bus").await;
    let (_, target) = harness.queue("merge", "update-merge-target").await;

    // (case, the update of that row, the facts it must leave behind, whether it
    // had anything to apply)
    type Row = (
        &'static str,
        fn(String) -> UpdateSubscriberInput,
        fn(Facts) -> Facts,
        bool,
    );
    let rows: [Row; 8] = [
        (
            "only a description",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                description: Some("an updated description".to_string()),
                ..Default::default()
            },
            |facts| Facts {
                description: Some("an updated description".to_string()),
                ..facts
            },
            true,
        ),
        (
            "only the run state",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                state: Some("STOPPED".to_string()),
                ..Default::default()
            },
            |facts| Facts {
                state: Some("STOPPED".to_string()),
                ..facts
            },
            true,
        ),
        (
            "an invoke configuration, which keeps the target and drops what it omits",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                invoke_configuration: Some(UpdateInvokeConfiguration {
                    role_arn: OTHER_ROLE_ARN.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |facts| Facts {
                role_arn: OTHER_ROLE_ARN.to_string(),
                sqs_message_group: None,
                ..facts
            },
            true,
        ),
        (
            "a filter configuration naming two filters",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                filter_configuration: Some(filters(vec![
                    filter("{\"source\":[\"updated.source\"]}", "METADATA"),
                    filter("{\"detail\":{\"kind\":[\"order\"]}}", "DATA"),
                ])),
                ..Default::default()
            },
            |facts| Facts {
                filters: Some(vec![
                    (
                        "{\"source\":[\"updated.source\"]}".to_string(),
                        "METADATA".to_string(),
                    ),
                    (
                        "{\"detail\":{\"kind\":[\"order\"]}}".to_string(),
                        "DATA".to_string(),
                    ),
                ]),
                ..facts
            },
            true,
        ),
        (
            "an empty filter configuration, which clears the filter",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                filter_configuration: Some(FilterConfiguration::default()),
                ..Default::default()
            },
            |facts| Facts {
                filters: None,
                ..facts
            },
            true,
        ),
        (
            "the batch, retry, on-failure and log configurations at once",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                batch_configuration: Some(BatchConfiguration {
                    max_batch_size: Some(1),
                    max_batch_window_in_seconds: Some(300),
                }),
                retry_policy: Some(RetryPolicy {
                    max_retry_attempts: Some(0),
                    max_event_age_in_seconds: Some(86_400),
                    retry_strategy: None,
                }),
                on_failure_configuration: Some(OnFailureConfiguration {
                    arn: Some(format!("arn:aws:sqs:{REGION}:{ACCOUNT}:updated-failures")),
                }),
                log_configuration: Some(LogConfiguration {
                    level: Some("INFO".to_string()),
                    include_payload: None,
                }),
                ..Default::default()
            },
            |facts| Facts {
                batch: Some((Some(1), Some(300))),
                retry: Some((Some(0), Some(86_400), None)),
                on_failure: Some(format!("arn:aws:sqs:{REGION}:{ACCOUNT}:updated-failures")),
                log_level: Some("INFO".to_string()),
                ..facts
            },
            true,
        ),
        (
            "a transformer",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                transformer: Some(Transformer {
                    r#type: Some("WITH_METADATA".to_string()),
                    jsonata_configuration: None,
                }),
                ..Default::default()
            },
            |facts| Facts {
                transformer: Some("WITH_METADATA".to_string()),
                ..facts
            },
            true,
        ),
        (
            "nothing but the subscriber ARN",
            |arn| UpdateSubscriberInput {
                subscriber_arn: arn,
                ..Default::default()
            },
            |facts| facts,
            false,
        ),
    ];

    for (row, (case, update, expected, modifies)) in rows.into_iter().enumerate() {
        let arn = harness
            .create_subscriber(
                case,
                created(&format!("update-merge-{row}"), &bus, &target),
            )
            .await;
        let before = harness.describe(case, &arn).await;
        let baseline = facts_of(&before);

        let answer = harness
            .update(update(arn.clone()))
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: UpdateSubscriber must succeed, got {e:?}"));
        let after = harness.describe(case, &arn).await;

        // What the update answered IS the subscriber a later read reports: every
        // member of the response comes back out of the store.
        assert_eq!(
            (
                answer.subscriber_arn.as_deref(),
                answer.name.as_deref(),
                answer.event_bus_arn.as_deref(),
                answer.r#type.as_deref(),
                answer.starting_position.as_deref(),
                answer.state.as_deref(),
                answer.last_modified_time.map(|time| time.secs()),
            ),
            (
                after.subscriber_arn.as_deref(),
                after.name.as_deref(),
                after.event_bus_arn.as_deref(),
                after.r#type.as_deref(),
                after.starting_position.as_deref(),
                after.state.as_deref(),
                after.last_modified_time.map(|time| time.secs()),
            ),
            "case {case:?}: the update must answer the subscriber a read reports, got \
             {answer:?} against {after:?}"
        );

        let reported = facts_of(&after);
        let wanted = expected(baseline.clone());
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected {wanted:?}, got {reported:?}"
        );

        // `LastModifiedTime` reports a modification: it moves for a call that
        // had something to apply and stands still for one that carried nothing.
        let (was, is) = (
            before.last_modified_time.map(|time| time.secs()),
            after.last_modified_time.map(|time| time.secs()),
        );
        if modifies {
            assert!(
                is >= was,
                "case {case:?}: LastModifiedTime must not go backwards, was {was:?}, is {is:?}"
            );
        } else {
            assert_eq!(
                is, was,
                "case {case:?}: an update that carried nothing must leave LastModifiedTime at \
                 {was:?}, got {is:?}"
            );
        }
        // The subscriber's identity is never a thing an update changes.
        assert_eq!(
            (
                after.subscriber_arn.as_deref(),
                after.name.as_deref(),
                after.event_bus_arn.as_deref(),
                after.revoked,
            ),
            (
                before.subscriber_arn.as_deref(),
                before.name.as_deref(),
                before.event_bus_arn.as_deref(),
                before.revoked,
            ),
            "case {case:?}: an update must not change the subscriber's identity, got {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The refusals, over a table
// ---------------------------------------------------------------------------

/// An update the service cannot apply is refused in the shape the model declares
/// for it, and writes nothing.
///
/// The rows differ along WHY the request cannot be applied: an ARN naming no
/// stored subscriber, and three shapes of request no subscriber could be
/// described by.
#[tokio::test]
async fn an_update_that_is_refused_leaves_the_subscriber_as_it_was() {
    let harness = Harness::new();
    let bus = harness.active_bus("refused", "update-refused-bus").await;
    let (_, target) = harness.queue("refused", "update-refused-target").await;
    let arn = harness
        .create_subscriber("refused", created("update-refused", &bus, &target))
        .await;
    let before = facts_of(&harness.describe("refused", &arn).await);

    // A well-formed `SubscriberArn` (its id segment is the modelled
    // `[a-z0-9]{25}`) that this store never minted.
    let unknown =
        format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/absent/aaaaaaaaaaaaaaaaaaaaaaaaa");

    // (case, the refused update, the error shape, the status it is served on)
    let rows: [(&str, UpdateSubscriberInput, &str, u16); 5] = [
        (
            "an ARN naming no stored subscriber",
            UpdateSubscriberInput {
                subscriber_arn: unknown,
                description: Some("nowhere to apply this".to_string()),
                ..Default::default()
            },
            "ResourceNotFoundException",
            404,
        ),
        (
            "a filter configuration naming a language but no filters",
            UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                filter_configuration: Some(FilterConfiguration {
                    language: Some("EVENT_BRIDGE_PATTERN".to_string()),
                    filters: None,
                }),
                ..Default::default()
            },
            "InvalidInputException",
            400,
        ),
        (
            "a filter configuration naming an empty filter list",
            UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                filter_configuration: Some(filters(Vec::new())),
                ..Default::default()
            },
            "InvalidInputException",
            400,
        ),
        (
            "a filter whose pattern is no event pattern",
            UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                filter_configuration: Some(filters(vec![filter("not a pattern", "DATA")])),
                ..Default::default()
            },
            "InvalidInputException",
            400,
        ),
        (
            "a transformer on a universal target",
            UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                invoke_configuration: Some(UpdateInvokeConfiguration {
                    role_arn: ROLE_ARN.to_string(),
                    universal_target_parameters: Some(UniversalTargetParameters {
                        input: "{}".to_string(),
                        invocation_timeout_seconds: None,
                    }),
                    ..Default::default()
                }),
                transformer: Some(Transformer {
                    r#type: Some("WITH_METADATA".to_string()),
                    jsonata_configuration: None,
                }),
                ..Default::default()
            },
            "InvalidInputException",
            400,
        ),
    ];

    for (case, input, shape, status) in rows {
        let refused = harness
            .update(input)
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: UpdateSubscriber must be refused"));
        assert_eq!(
            (refused.shape_name(), refused.http_status()),
            (shape, status),
            "case {case:?}: expected {shape} on HTTP {status}, got {:?} on {}",
            refused.shape_name(),
            refused.http_status()
        );
        let reported = facts_of(&harness.describe(case, &arn).await);
        assert_eq!(
            reported, before,
            "case {case:?}: a refused update must write nothing, expected {before:?}, got \
             {reported:?}"
        );
    }
}

/// RULE: revocation is terminal, so a revoked subscriber takes no further change
/// — the update is refused with `InvalidStateException` (409) naming that the
/// subscriber is revoked, and nothing it asked for is written.
#[tokio::test]
async fn a_revoked_subscriber_cannot_be_updated() {
    let harness = Harness::new();
    let bus = harness.active_bus("revoked", "update-revoked-bus").await;
    let (_, target) = harness.queue("revoked", "update-revoked-target").await;

    // The rows differ along the run state the subscriber was left in when the
    // bus owner withdrew it, and along what the refused update asks for.
    let rows: [(&str, &str, UpdateSubscriberInput); 2] = [
        (
            "a running subscriber, a description",
            "RUNNING",
            UpdateSubscriberInput {
                subscriber_arn: String::new(),
                description: Some("after the revocation".to_string()),
                ..Default::default()
            },
        ),
        (
            "a stopped subscriber, an unpause",
            "STOPPED",
            UpdateSubscriberInput {
                subscriber_arn: String::new(),
                state: Some("RUNNING".to_string()),
                resume_position: Some("LATEST".to_string()),
                ..Default::default()
            },
        ),
    ];

    for (row, (case, state, asked)) in rows.into_iter().enumerate() {
        let arn = harness
            .create_subscriber(
                case,
                CreateSubscriberInput {
                    state: Some(state.to_string()),
                    ..created(&format!("update-revoked-{row}"), &bus, &target)
                },
            )
            .await;
        let before = facts_of(&harness.describe(case, &arn).await);
        harness.revoke(case, &arn).await;

        let refused = harness
            .update(UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                ..asked
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: UpdateSubscriber must be refused"));
        assert_eq!(
            (refused.shape_name(), refused.http_status()),
            ("InvalidStateException", 409),
            "case {case:?}: expected InvalidStateException on HTTP 409, got {:?} on {}",
            refused.shape_name(),
            refused.http_status()
        );
        let message = match refused {
            Scaffolded::Modeled(UpdateSubscriberErrorModeled::InvalidStateException(invalid)) => {
                invalid.message.clone().unwrap_or_default()
            }
            other => panic!("case {case:?}: expected InvalidStateException, got {other:?}"),
        };
        assert_eq!(
            message, "Subscriber is revoked and cannot be updated",
            "case {case:?}: unexpected refusal wording {message:?}"
        );

        // The withdrawal is all that changed: the refused update wrote none of
        // what it asked for.
        let after = harness.describe(case, &arn).await;
        assert_eq!(
            after.revoked,
            Some(true),
            "case {case:?}: the subscriber must report Revoked true, got {:?}",
            after.revoked
        );
        assert_eq!(
            facts_of(&after),
            before,
            "case {case:?}: the refused update must write nothing, expected {before:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The unpause, over a table
// ---------------------------------------------------------------------------

/// RULE: an update that transitions `State` from `STOPPED` to `RUNNING` resumes
/// delivery from the position `ResumePosition` names — `LAST_PROCESSED` (the
/// default) still owes the subscriber the backlog that accrued while it was
/// stopped, `LATEST` skips that backlog — and whichever position it resumes
/// from, every event published afterwards is delivered.
///
/// The rows differ along the resume position the unpause names, including the
/// call that names none at all.
#[tokio::test]
async fn an_unpause_resumes_delivery_from_the_position_it_names() {
    let harness = Harness::new();
    let bus = harness.active_bus("unpause", "update-unpause-bus").await;

    /// How many events each row publishes while its subscriber is stopped.
    const BACKLOG: usize = 3;

    // (case, the ResumePosition the unpause names, how much of the backlog it is
    // owed)
    let rows: [(&str, Option<&str>, usize); 3] = [
        ("resuming from the last processed position", Some("LAST_PROCESSED"), BACKLOG),
        ("resuming with no position named", None, BACKLOG),
        ("resuming at the tip", Some("LATEST"), 0),
    ];

    for (row, (case, resume_position, owed)) in rows.into_iter().enumerate() {
        let (queue_url, target) = harness
            .queue(case, &format!("update-unpause-target-{row}"))
            .await;
        let arn = harness
            .create_subscriber(
                case,
                CreateSubscriberInput {
                    // A stopped subscriber delivers nothing, so what its bus
                    // accepts next is the backlog this row resumes over.
                    state: Some("STOPPED".to_string()),
                    // No filter configuration at all: every event of the bus is
                    // this subscriber's, so the count is about the POSITION
                    // alone.
                    filter_configuration: None,
                    ..created(&format!("update-unpause-{row}"), &bus, &target)
                },
            )
            .await;

        harness.publish(case, &bus, BACKLOG).await;
        assert_eq!(
            harness.queue_depth(case, &queue_url).await,
            0,
            "case {case:?}: a stopped subscriber must be delivered nothing"
        );

        let answer = harness
            .update(UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                state: Some("RUNNING".to_string()),
                resume_position: resume_position.map(str::to_string),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the unpause must succeed, got {e:?}"));
        assert_eq!(
            answer.state.as_deref(),
            Some("RUNNING"),
            "case {case:?}: the unpause must report the subscriber RUNNING, got {:?}",
            answer.state
        );

        // The update performs the deliveries it made due before it answers, so
        // the queue is settled: what it holds now is what the resume position
        // owed this subscriber, no more.
        assert_eq!(
            harness.queue_depth(case, &queue_url).await,
            owed as u64,
            "case {case:?}: the unpause must deliver {owed} of the {BACKLOG} backlogged events"
        );
        assert_eq!(
            harness.received(case, &queue_url, owed).await,
            owed,
            "case {case:?}: expected {owed} delivered events"
        );

        // Delivery has resumed either way: what the bus accepts from here on is
        // delivered whichever position the subscriber resumed from.
        harness.publish(case, &bus, 1).await;
        assert_eq!(
            harness.received(case, &queue_url, 1).await,
            1,
            "case {case:?}: an event published after the unpause must be delivered"
        );
    }
}

/// RULE: `ResumePosition` is honored ONLY on the call that transitions `State`
/// from `STOPPED` to `RUNNING`, so a call that is no unpause leaves the
/// subscriber's position alone and the backlog it accrued is still owed to it.
#[tokio::test]
async fn a_resume_position_is_ignored_on_a_call_that_is_no_unpause() {
    let harness = Harness::new();
    let bus = harness.active_bus("ignored", "update-ignored-bus").await;
    let (queue_url, target) = harness.queue("ignored", "update-ignored-target").await;
    let case = "a resume position carried by a call that keeps the subscriber stopped";
    let arn = harness
        .create_subscriber(
            case,
            CreateSubscriberInput {
                state: Some("STOPPED".to_string()),
                filter_configuration: None,
                ..created("update-ignored", &bus, &target)
            },
        )
        .await;

    harness.publish(case, &bus, 2).await;

    // `LATEST` would skip the backlog — but this call transitions no state, so
    // it is ignored and nothing moves.
    harness
        .update(UpdateSubscriberInput {
            subscriber_arn: arn.clone(),
            description: Some("still stopped".to_string()),
            resume_position: Some("LATEST".to_string()),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: the update must succeed, got {e:?}"));
    assert_eq!(
        harness.queue_depth(case, &queue_url).await,
        0,
        "case {case:?}: a subscriber that is still stopped must be delivered nothing"
    );

    // The unpause that follows, naming no position, resumes from where delivery
    // stopped: the whole backlog is still owed.
    harness
        .update(UpdateSubscriberInput {
            subscriber_arn: arn.clone(),
            state: Some("RUNNING".to_string()),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: the unpause must succeed, got {e:?}"));
    assert_eq!(
        harness.received(case, &queue_url, 2).await,
        2,
        "case {case:?}: the backlog the ignored resume position would have skipped must still \
         be delivered"
    );
}
