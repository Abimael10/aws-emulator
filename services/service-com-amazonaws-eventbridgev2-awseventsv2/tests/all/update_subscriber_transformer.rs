// SPDX-License-Identifier: CC0-1.0

//! The `Transformer` an `UpdateSubscriber` installs, as the target sees it.
//!
//! RULE: the transformation a subscriber hands its target is the one the store holds for
//! it at the moment the event is delivered, so the `Transformer` an update writes decides
//! every delivery made afterwards — `RAW` (and no transformer at all) hands the target the
//! event envelope, `WITH_METADATA` hands it the event inside this service's metadata
//! envelope, and `JSONATA` hands it what its expression produces over the event, a string
//! unquoted and every other value its JSON serialization. And because the delivery path
//! hands a target what the transformer produces and NOTHING else, a transformation this
//! service could not perform is refused at the call that asks for it — the same reading
//! for the create that mints a subscriber and the update that rewrites one — so a
//! subscriber is never left installed with one and silently stops delivering.
//!
//! The rows span that class rather than one recorded swap: they differ along the
//! transformation the subscriber starts from (none, `RAW`, `WITH_METADATA`, a `JSONATA`
//! expression), along the one the update installs, and along the TYPE the installed
//! expression produces (a computed string, a number, an object). The refusal rows differ
//! along WHY the transformation cannot be performed (a type this service declares none, a
//! `JSONATA` type with no configuration, an expression the language will not parse) and
//! each asserts the subscriber keeps delivering in the form it already had.
//!
//! Amazon SQS is mounted beside EventBridge V2 on ONE engine, so every assertion is made
//! where a client observes it: in the message the subscriber's target queue holds.
//!
//! Nothing here waits on the wall clock: a publish performs the deliveries it has made due
//! before it answers, so a read that follows it is a read of a settled queue. The one wait
//! is for the bus's own provisioning transition, polled for the CONDITION (`ACTIVE`) under
//! a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::InMemoryEngine;
use serde_json::{json, Value};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::{CreateSubscriber, CreateSubscriberErrorModeled};
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput, DescribeSubscriberInput,
    DescribeSubscriberOutput, InvokeConfiguration, JsonataConfiguration, PutEventsInput,
    PutEventsRequestEntry, SqsParameters, Transformer, UpdateSubscriberInput,
    UpdateSubscriberOutput,
};
use eventsv2::ops::update_subscriber::{UpdateSubscriber, UpdateSubscriberErrorModeled};
use eventsv2::scaffolded::Scaffolded;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn` shape
/// and of the calling account.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/transformer-delivery";

/// The `Source` every publish below carries.
const SOURCE: &str = "update.transformer.suite";

/// The `DetailType` every publish below carries.
const DETAIL_TYPE: &str = "TransformerSwap";

/// The number the published payload carries beside its marker, so a row can assert an
/// expression that produces a NUMBER rather than a string.
const PAYLOAD_SIZE: i64 = 2;

/// How long a poll waits for a condition the emulator owes: far above the provisioning
/// span a bus takes, so a loaded host has ample room. The loop stops as soon as the
/// condition holds, so the deadline costs nothing on a passing run.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

// ---------------------------------------------------------------------------
// The harness: EventBridge V2 and Amazon SQS on ONE engine
// ---------------------------------------------------------------------------

/// A SigV4 `Authorization` header naming `service` in [`REGION`], which is how a request
/// names both.
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

/// One emulator over one shared engine, with Amazon SQS mounted beside EventBridge V2 —
/// the shape `core/router/src/lib.rs` mounts them in, which is what makes a handler's
/// in-process callback reach the queue a subscriber targets.
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

    /// A bus called `name`, polled until it REPORTS `ACTIVE` — the state a create of a
    /// subscriber and a publish both require of it. Polls the CONDITION rather than
    /// sleeping a fixed time, because provisioning is asynchronous.
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

    /// `CreateSubscriber` as the operation itself answers it.
    async fn create(
        &self,
        input: CreateSubscriberInput,
    ) -> Result<String, Scaffolded<CreateSubscriberErrorModeled>> {
        CreateSubscriber::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
            .map(|created| created.subscriber_arn.unwrap_or_default())
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

    /// Publish ONE event carrying `marker` onto `event_bus_arn`, requiring it to be
    /// accepted.
    async fn publish(&self, case: &str, event_bus_arn: &str, marker: &str) {
        let answer = PutEvents::new(Arc::clone(&self.engine))
            .handle(
                PutEventsInput {
                    event_bus_arn: event_bus_arn.to_string(),
                    entries: vec![PutEventsRequestEntry {
                        source: SOURCE.to_string(),
                        detail_type: DETAIL_TYPE.to_string(),
                        detail: Some(format!(
                            "{{\"marker\":\"{marker}\",\"size\":{PAYLOAD_SIZE}}}"
                        )),
                        ..Default::default()
                    }],
                    deduplication_configuration: None,
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutEvents must succeed, got {e:?}"));
        assert_eq!(
            answer.failed_entry_count, 0,
            "case {case:?}: the published entry must be accepted, got {answer:?}"
        );
    }

    /// EXACTLY one body off the queue at `queue_url`: polled until it arrives under a
    /// generous deadline, deleted as it is read, and then one more pass, which must find
    /// nothing — a queue holding more than the rule allows is a failure too.
    async fn one_body(&self, case: &str, queue_url: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        let mut held: Vec<String> = Vec::new();
        loop {
            held.extend(self.drain(case, queue_url).await);
            if !held.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: no delivery ever arrived on {queue_url}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let extra = self.drain(case, queue_url).await;
        assert!(
            extra.is_empty() && held.len() == 1,
            "case {case:?}: expected exactly one delivery on {queue_url}, got {held:?} and \
             {extra:?}"
        );
        held.remove(0)
    }

    /// One pass over the queue at `queue_url`: the body of every message it holds right
    /// now, each deleted as it is read so a later pass reports what is left.
    async fn drain(&self, case: &str, queue_url: &str) -> Vec<String> {
        let answer = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let mut read = Vec::new();
        for message in answer["Messages"].as_array().unwrap_or(&Vec::new()) {
            self.sqs(
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": queue_url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            read.push(
                message["Body"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a received message carries no Body: {message}")
                    })
                    .to_string(),
            );
        }
        read
    }
}

// ---------------------------------------------------------------------------
// The transformer a row configures, and the body it must produce
// ---------------------------------------------------------------------------

/// A `Transformer` of `r#type`, carrying `expression` when the type is the one computed by
/// an expression.
fn transformer(r#type: &str, expression: Option<&str>) -> Transformer {
    Transformer {
        r#type: Some(r#type.to_string()),
        jsonata_configuration: expression.map(|expression| JsonataConfiguration {
            expression: expression.to_string(),
        }),
    }
}

/// The create every row starts from: a subscriber of `event_bus_arn` delivering to the
/// queue `target_arn` under `asked`, with NO filter configuration, so what reaches the
/// queue is decided by the transformation alone.
fn created(
    name: &str,
    event_bus_arn: &str,
    target_arn: &str,
    asked: Option<Transformer>,
) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        client_token: Some(format!("token-for-{name}")),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: target_arn.to_string(),
            // The parameters a queue target takes, named and left to their defaults,
            // exactly as a client that wants a plain delivery writes them.
            sqs_parameters: Some(SqsParameters::default()),
            ..Default::default()
        },
        transformer: asked,
        ..Default::default()
    }
}

/// The shape a delivered body must have for the transformation in force.
#[derive(Debug, Clone)]
enum Expected {
    /// The event ENVELOPE, untransformed: what `RAW` — and a subscriber that configures
    /// no transformer — hands its target.
    Envelope,
    /// The event inside this service's own metadata envelope: what `WITH_METADATA` hands
    /// it.
    WithMetadata,
    /// The exact text a `JSONATA` expression produces.
    Text(String),
}

/// Assert `body` is the shape `expected` names for the event carrying `marker`.
fn assert_body(case: &str, marker: &str, expected: &Expected, body: &str) {
    match expected {
        Expected::Envelope => {
            let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: the envelope {body:?} is no JSON document: {error}")
            });
            assert_eq!(
                document["detail"]["marker"].as_str(),
                Some(marker),
                "case {case:?}: expected the envelope of {marker:?}, got {body:?}"
            );
            assert_eq!(
                document["detail-type"].as_str(),
                Some(DETAIL_TYPE),
                "case {case:?}: expected the envelope's detail-type, got {body:?}"
            );
        }
        Expected::WithMetadata => {
            let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: the metadata envelope {body:?} is no JSON document: {error}")
            });
            assert_eq!(
                document["Data"]["detail"]["marker"].as_str(),
                Some(marker),
                "case {case:?}: expected the metadata envelope of {marker:?}, got {body:?}"
            );
            assert_eq!(
                document["Metadata"]["Source"].as_str(),
                Some(SOURCE),
                "case {case:?}: expected the publisher's metadata, got {body:?}"
            );
            assert!(
                document["SystemMetadata"].is_object(),
                "case {case:?}: expected the event's system metadata, got {body:?}"
            );
        }
        Expected::Text(text) => assert_eq!(
            body, text,
            "case {case:?}: expected the transformed body {text:?}, got {body:?}"
        ),
    }
}

/// One swap: what the subscriber is created with, what the update installs, and the body
/// its target must be handed before and after that update.
struct Row {
    case: &'static str,
    /// The `Transformer` the create names, or `None` for a create that names none.
    created: Option<(&'static str, Option<&'static str>)>,
    /// The body the target is handed before the update, for a marker.
    before: fn(&str) -> Expected,
    /// The `Transformer` the update installs.
    updated: (&'static str, Option<&'static str>),
    /// The body the target is handed afterwards, for a marker.
    after: fn(&str) -> Expected,
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// RULE: a delivery is shaped by the `Transformer` the store holds for the subscriber when
/// the event is delivered, so the one an update installs decides every later delivery and
/// the subscriber keeps delivering, in the new form.
#[tokio::test]
async fn the_transformer_an_update_installs_shapes_every_later_delivery() {
    let rows: [Row; 6] = [
        Row {
            case: "no transformer, then an expression computing a string",
            created: None,
            before: |_| Expected::Envelope,
            updated: ("JSONATA", Some("{% 'v2:' & $events.Data.detail.marker %}")),
            after: |marker| Expected::Text(format!("v2:{marker}")),
        },
        Row {
            case: "the raw transformation, then an expression producing a number",
            created: Some(("RAW", None)),
            before: |_| Expected::Envelope,
            updated: ("JSONATA", Some("{% $events.Data.detail.size + 1 %}")),
            after: |_| Expected::Text((PAYLOAD_SIZE + 1).to_string()),
        },
        Row {
            case: "the raw transformation, then the metadata envelope",
            created: Some(("RAW", None)),
            before: |_| Expected::Envelope,
            updated: ("WITH_METADATA", None),
            after: |_| Expected::WithMetadata,
        },
        Row {
            case: "an expression, then another expression",
            created: Some((
                "JSONATA",
                Some("{% 'v1:' & $events.Data.detail.marker %}"),
            )),
            before: |marker| Expected::Text(format!("v1:{marker}")),
            updated: ("JSONATA", Some("{% 'v2:' & $events.Data.detail.marker %}")),
            after: |marker| Expected::Text(format!("v2:{marker}")),
        },
        Row {
            case: "an expression, then back to the raw transformation",
            created: Some((
                "JSONATA",
                Some("{% 'v1:' & $events.Data.detail.marker %}"),
            )),
            before: |marker| Expected::Text(format!("v1:{marker}")),
            updated: ("RAW", None),
            after: |_| Expected::Envelope,
        },
        Row {
            case: "the metadata envelope, then an expression producing an object",
            created: Some(("WITH_METADATA", None)),
            before: |_| Expected::WithMetadata,
            updated: (
                "JSONATA",
                Some(r#"{% { "marker": $events.Data.detail.marker } %}"#),
            ),
            after: |marker| Expected::Text(format!("{{\"marker\":\"{marker}\"}}")),
        },
    ];

    let harness = Harness::new();
    let bus = harness.active_bus("the swap suite's bus", "transformer-swap-bus").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (queue_url, target) = harness
            .queue(case, &format!("transformer-swap-target-{index}"))
            .await;
        let arn = harness
            .create(created(
                &format!("transformer-swap-{index}"),
                &bus,
                &target,
                row.created
                    .map(|(r#type, expression)| transformer(r#type, expression)),
            ))
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"));

        // The form the subscriber delivers in BEFORE the update.
        let before_marker = format!("m-{index}-before");
        harness.publish(case, &bus, &before_marker).await;
        let delivered = harness.one_body(case, &queue_url).await;
        assert_body(case, &before_marker, &(row.before)(&before_marker), &delivered);

        // The swap, and what a later read reports about it.
        let (r#type, expression) = row.updated;
        harness
            .update(UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                transformer: Some(transformer(r#type, expression)),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: UpdateSubscriber must succeed, got {e:?}"));
        let reported = harness.describe(case, &arn).await.transformer;
        assert_eq!(
            (
                reported
                    .as_ref()
                    .and_then(|held| held.r#type.as_deref())
                    .unwrap_or_default(),
                reported
                    .as_ref()
                    .and_then(|held| held.jsonata_configuration.as_ref())
                    .map(|configuration| configuration.expression.as_str()),
            ),
            (r#type, expression),
            "case {case:?}: the read must report the installed transformer, got {reported:?}"
        );

        // And the form it delivers in AFTERWARDS, which is the one just installed.
        let after_marker = format!("m-{index}-after");
        harness.publish(case, &bus, &after_marker).await;
        let delivered = harness.one_body(case, &queue_url).await;
        assert_body(case, &after_marker, &(row.after)(&after_marker), &delivered);
    }
}

// ---------------------------------------------------------------------------
// The transformations neither call admits
// ---------------------------------------------------------------------------

/// RULE: a transformation this service could not perform is refused — `InvalidInputException`
/// (400) — at the call that asks for it, by the same reading for the create that mints a
/// subscriber and the update that rewrites one, and the refused update writes nothing: the
/// subscriber keeps delivering in the form it already had.
///
/// The rows differ along WHY the transformation cannot be performed: a `Type` this service
/// declares no `TransformerType` for, the `JSONATA` type with no configuration at all, and
/// an expression the language will not parse.
#[tokio::test]
async fn a_transformation_this_service_cannot_perform_is_refused_by_both_calls() {
    // (case, the transformer asked for, the whole Message of the refusal)
    let rows: [(&str, Transformer, String); 4] = [
        (
            "a type this service declares no TransformerType for",
            transformer("TRANSFORM", None),
            eventsv2::state::transformer_type_undeclared_message("TRANSFORM"),
        ),
        (
            // The declared values are case-sensitive: `TransformerType` declares
            // `JSONATA`, not its lowercase spelling.
            "a declared value in the wrong case",
            transformer("jsonata", None),
            eventsv2::state::transformer_type_undeclared_message("jsonata"),
        ),
        (
            "the expression type with no configuration at all",
            transformer("JSONATA", None),
            eventsv2::state::JSONATA_CONFIGURATION_REQUIRED_MESSAGE.to_string(),
        ),
        (
            "an expression the language will not parse",
            transformer("JSONATA", Some("{% 'v2:' & %}")),
            eventsv2::state::JSONATA_EXPRESSION_INVALID_MESSAGE.to_string(),
        ),
    ];

    let harness = Harness::new();
    let bus = harness
        .active_bus("the refusal suite's bus", "transformer-refused-bus")
        .await;

    for (index, (case, asked, message)) in rows.into_iter().enumerate() {
        let (queue_url, target) = harness
            .queue(case, &format!("transformer-refused-target-{index}"))
            .await;

        // The CREATE that asks for it is refused, and mints nothing.
        let refused = harness
            .create(created(
                &format!("transformer-refused-{index}"),
                &bus,
                &target,
                Some(asked.clone()),
            ))
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber must be refused"));
        assert_eq!(
            (refused.shape_name(), refused.http_status()),
            ("InvalidInputException", 400),
            "case {case:?}: expected InvalidInputException on HTTP 400 from the create, got \
             {:?} on {}",
            refused.shape_name(),
            refused.http_status()
        );
        let reported = match refused {
            Scaffolded::Modeled(CreateSubscriberErrorModeled::InvalidInputException(invalid)) => {
                invalid.message.unwrap_or_default()
            }
            other => panic!("case {case:?}: expected InvalidInputException, got {other:?}"),
        };
        assert_eq!(
            reported, message,
            "case {case:?}: unexpected wording from the create, {reported:?}"
        );

        // A subscriber that delivers the plain envelope, so the refused UPDATE below is
        // visible as a subscriber that kept its own transformation.
        let arn = harness
            .create(created(
                &format!("transformer-kept-{index}"),
                &bus,
                &target,
                Some(transformer("RAW", None)),
            ))
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateSubscriber must succeed, got {e:?}"));

        let refused = harness
            .update(UpdateSubscriberInput {
                subscriber_arn: arn.clone(),
                transformer: Some(asked.clone()),
                ..Default::default()
            })
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: UpdateSubscriber must be refused"));
        assert_eq!(
            (refused.shape_name(), refused.http_status()),
            ("InvalidInputException", 400),
            "case {case:?}: expected InvalidInputException on HTTP 400 from the update, got \
             {:?} on {}",
            refused.shape_name(),
            refused.http_status()
        );
        let reported = match refused {
            Scaffolded::Modeled(UpdateSubscriberErrorModeled::InvalidInputException(invalid)) => {
                invalid.message.unwrap_or_default()
            }
            other => panic!("case {case:?}: expected InvalidInputException, got {other:?}"),
        };
        assert_eq!(
            reported, message,
            "case {case:?}: unexpected wording from the update, {reported:?}"
        );

        // The refused update wrote nothing: the subscriber still reports the
        // transformation it was created with...
        let held = harness.describe(case, &arn).await.transformer;
        assert_eq!(
            held.as_ref().and_then(|held| held.r#type.as_deref()),
            Some("RAW"),
            "case {case:?}: the refused update must leave the transformer alone, got {held:?}"
        );
        // ...and goes on delivering in it, rather than silently delivering nothing.
        let marker = format!("m-refused-{index}");
        harness.publish(case, &bus, &marker).await;
        let delivered = harness.one_body(case, &queue_url).await;
        assert_body(case, &marker, &Expected::Envelope, &delivered);
    }
}
