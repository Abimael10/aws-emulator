// SPDX-License-Identifier: CC0-1.0

//! WHEN an event source mapping may hand a record of a failed batch to a KAFKA TOPIC.
//!
//! RULE: `OnFailure$Destination`'s `kafka://<topic>` form — the one value the member's own
//! pattern admits that is no ARN (`^$|kafka://([^.]([a-zA-Z0-9\-_.]{0,248}))|arn:…`,
//! `smithy_query(query="shape", shape="DestinationArn")`) — is not a resource of another
//! service but a topic of the mapping's own cluster, produced to by the mapping's own event
//! pollers, and that delivery is only made by the pollers PROVISIONED MODE provides ("Use
//! Provisioned Mode to customize the minimum and maximum number of event pollers for your
//! event source", `smithy_query(query="shape", shape="ProvisionedPollerConfig")`), which a
//! request turns on by stating `MinimumPollers`. The topic it names may not be one the mapping
//! CONSUMES either (`Topics`: "The name of the Kafka topic"), since a record produced back
//! into the topic its batch was read from is handed straight back to the mapping that gave up
//! on it. So both calls that state a mapping's destinations admit that form only for a mapping
//! in provisioned mode naming another topic, and refuse every other request carrying it with
//! the modelled `InvalidParameterValueException` (HTTP 400,
//! `smithy_query(query="operation", operation="CreateEventSourceMapping,\
//! UpdateEventSourceMapping")`), creating and changing nothing. Every other destination kind
//! the member enumerates is unaffected.
//!
//! The rows differ along what the rule generalises over: the PROVISIONED-POLLER
//! CONFIGURATION in force (none at all, one stating the minimum, one stating both bounds, one
//! stating only the maximum), the FORM of the destination (the Kafka URI, and the ARNs of the
//! three other kinds the member enumerates), the TOPIC it names (a plain name, one carrying
//! dots, and the one the mapping itself consumes), the SLOT of the configuration it is stated
//! in, and the EVENT SOURCE the mapping polls (an Amazon MSK cluster, a self-managed Apache
//! Kafka cluster named by its bootstrap servers, an Amazon Kinesis data stream, an Amazon
//! DynamoDB stream).
//!
//! Every row is driven through a real `CreateEventSourceMapping` and through a real
//! `UpdateEventSourceMapping` on a settled mapping of the same source, over the production
//! `MultiServiceRouter`; an accepted create is read back through `GetEventSourceMapping`, and
//! a refused one is checked to have stored nothing.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    kafka_destination_is_a_consumed_topic_message,
    KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, get_mapping, list_mappings,
    router_on, Answer, DYNAMODB_SOURCE, KINESIS_SOURCE, MAPPINGS_URI, MSK_SOURCE, STATE_ENABLED,
};

/// The modelled error a destination no record of a failed batch could reach is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The destination every mapping of the update rows is created holding: an Amazon SQS queue,
/// one of the kinds the member enumerates, so a refused update is visible as the destination
/// that is STILL held afterwards.
const HELD_DESTINATION: &str = "arn:aws:sqs:us-east-1:000000000000:order-dlq";

/// A bootstrap server of the self-managed Apache Kafka cluster the rows that name one poll.
const BOOTSTRAP_SERVER: &str = "broker-1.orders.example.com:9092";

/// The topic the Kafka-sourced mappings of this suite consume.
const TOPIC: &str = "order-events";

/// Which slot of a `DestinationConfig` a row states its destination in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    OnFailure,
    OnSuccess,
}

/// The provisioned-poller configuration a row states, which is what says whether the mapping
/// is in provisioned mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pollers {
    /// No `ProvisionedPollerConfig` at all: the mapping is not in provisioned mode.
    None,
    /// `MinimumPollers` alone, which is what the mode is turned on by.
    Minimum,
    /// Both poller bounds.
    Bounds,
    /// `MaximumPollers` alone, which turns nothing on.
    MaximumOnly,
}

impl Pollers {
    /// The `ProvisionedPollerConfig` this row states, or `None` when it states none.
    ///
    /// The bounds are inside the ranges the members declare (`MinimumPollers`
    /// `@range(min: 1, max: 200)`, `MaximumPollers` `@range(min: 1, max: 10000)`).
    fn config(self) -> Option<Value> {
        match self {
            Pollers::None => None,
            Pollers::Minimum => Some(json!({ "MinimumPollers": 1 })),
            Pollers::Bounds => Some(json!({ "MinimumPollers": 2, "MaximumPollers": 10 })),
            Pollers::MaximumOnly => Some(json!({ "MaximumPollers": 10 })),
        }
    }

    /// Whether a mapping with this configuration is in provisioned mode.
    fn provisioned(self) -> bool {
        matches!(self, Pollers::Minimum | Pollers::Bounds)
    }
}

/// One row: the source the mapping polls, the poller configuration in force, the destination a
/// call states and the slot it states it in.
struct Row {
    case: &'static str,
    /// The `EventSourceArn`, or `None` for a self-managed Apache Kafka cluster, which is
    /// named by `SelfManagedEventSource` instead.
    event_source: Option<&'static str>,
    pollers: Pollers,
    destination: &'static str,
    slot: Slot,
}

/// The rows: see the module docs.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Kafka topic for a cluster mapping in provisioned mode",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::Minimum,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a dotted topic name for a cluster mapping stating both poller bounds",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::Bounds,
            destination: "kafka://order-events.failed.v2",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a Kafka topic for a self-managed cluster mapping in provisioned mode",
            event_source: None,
            pollers: Pollers::Bounds,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a Kafka topic in the on-success slot of a provisioned mapping",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::Minimum,
            destination: "kafka://failed-batches",
            slot: Slot::OnSuccess,
        },
        Row {
            case: "a Kafka topic for a cluster mapping with no poller configuration",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::None,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a Kafka topic for a self-managed cluster mapping with no poller \
                   configuration",
            event_source: None,
            pollers: Pollers::None,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a Kafka topic for a mapping whose configuration states only the maximum",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::MaximumOnly,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a dotted topic name for a mapping with no poller configuration",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::None,
            destination: "kafka://order-events.failed.v2",
            slot: Slot::OnFailure,
        },
        Row {
            case: "a Kafka topic in the on-success slot of a mapping not in provisioned mode",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::None,
            destination: "kafka://failed-batches",
            slot: Slot::OnSuccess,
        },
        Row {
            case: "a Kafka topic for a data-stream mapping with no poller configuration",
            event_source: Some(KINESIS_SOURCE),
            pollers: Pollers::None,
            destination: "kafka://failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "the very topic the cluster mapping consumes, in provisioned mode",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::Minimum,
            destination: "kafka://order-events",
            slot: Slot::OnFailure,
        },
        Row {
            case: "the topic a self-managed cluster mapping consumes, in provisioned mode",
            event_source: None,
            pollers: Pollers::Bounds,
            destination: "kafka://order-events",
            slot: Slot::OnFailure,
        },
        Row {
            case: "the consumed topic in the on-success slot of a provisioned mapping",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::Minimum,
            destination: "kafka://order-events",
            slot: Slot::OnSuccess,
        },
        Row {
            case: "the consumed topic for a mapping not in provisioned mode, the mode first",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::None,
            destination: "kafka://order-events",
            slot: Slot::OnFailure,
        },
        Row {
            case: "an Amazon SQS queue, which takes no provisioned mode",
            event_source: Some(MSK_SOURCE),
            pollers: Pollers::None,
            destination: "arn:aws:sqs:us-east-1:000000000000:failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "an Amazon SNS topic for a data-stream mapping, which takes no mode either",
            event_source: Some(KINESIS_SOURCE),
            pollers: Pollers::None,
            destination: "arn:aws:sns:us-east-1:000000000000:failed-batches",
            slot: Slot::OnFailure,
        },
        Row {
            case: "an Amazon S3 bucket for a stream mapping with no poller configuration",
            event_source: Some(DYNAMODB_SOURCE),
            pollers: Pollers::None,
            destination: "arn:aws:s3:::failed-batches",
            slot: Slot::OnFailure,
        },
    ]
}

impl Row {
    /// The whole refusal this row's destination earns, and `None` when the call stating it
    /// must be accepted: a destination that is no Kafka topic is not judged by this rule, and
    /// a Kafka topic is judged on the MODE first and on the consumed topic after it.
    fn refusal(&self) -> Option<String> {
        let topic = self.destination.strip_prefix("kafka://")?;
        if !self.pollers.provisioned() {
            return Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string());
        }
        (self.polls_a_kafka_cluster() && topic == TOPIC)
            .then(|| kafka_destination_is_a_consumed_topic_message(topic))
    }

    /// Whether the call stating this row's destination must be accepted.
    fn accepted(&self) -> bool {
        self.refusal().is_none()
    }

    /// The `DestinationConfig` this row states.
    fn destination_config(&self) -> Value {
        self.config_with(self.destination)
    }

    /// A `DestinationConfig` stating `destination` in this row's slot.
    fn config_with(&self, destination: &str) -> Value {
        match self.slot {
            Slot::OnFailure => json!({ "OnFailure": { "Destination": destination } }),
            Slot::OnSuccess => json!({ "OnSuccess": { "Destination": destination } }),
        }
    }

    /// The member the destination of this row's slot is reported under.
    fn slot_member(&self) -> &'static str {
        match self.slot {
            Slot::OnFailure => "OnFailure",
            Slot::OnSuccess => "OnSuccess",
        }
    }

    /// The `CreateEventSourceMapping` body for this row's source and poller configuration,
    /// with `destination_config` as its `DestinationConfig`.
    ///
    /// `StartingPosition` is "Required for Amazon Kinesis and Amazon DynamoDB Stream event
    /// sources", so a mapping on one of those names a position; a self-managed Apache Kafka
    /// cluster is named by `SelfManagedEventSource` rather than by an ARN, and a mapping on a
    /// Kafka cluster consumes a topic.
    fn create_body(&self, function: &str, destination_config: Option<Value>) -> Value {
        let mut body = Map::new();
        body.insert("FunctionName".to_string(), json!(function));
        match self.event_source {
            Some(event_source) => {
                body.insert("EventSourceArn".to_string(), json!(event_source));
            }
            None => {
                body.insert(
                    "SelfManagedEventSource".to_string(),
                    json!({ "Endpoints": { "KAFKA_BOOTSTRAP_SERVERS": [BOOTSTRAP_SERVER] } }),
                );
            }
        }
        if self.polls_a_kafka_cluster() {
            body.insert("Topics".to_string(), json!([TOPIC]));
        } else {
            body.insert("StartingPosition".to_string(), json!("LATEST"));
        }
        if let Some(config) = self.pollers.config() {
            body.insert("ProvisionedPollerConfig".to_string(), config);
        }
        if let Some(config) = destination_config {
            body.insert("DestinationConfig".to_string(), config);
        }
        Value::Object(body)
    }

    /// Whether this row's mapping polls a Kafka cluster — an Amazon MSK one, or the
    /// self-managed one its bootstrap servers name.
    fn polls_a_kafka_cluster(&self) -> bool {
        self.event_source.is_none_or(|source| source == MSK_SOURCE)
    }

    /// The destination the answer `answer` reports in this row's slot.
    fn reported_destination(&self, answer: &Answer) -> Option<String> {
        answer
            .json()
            .get("DestinationConfig")?
            .get(self.slot_member())?
            .get("Destination")?
            .as_str()
            .map(str::to_string)
    }
}

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// Lambda alone: the rule is about what the control plane refuses, so nothing the values name
/// has to exist.
fn emulator() -> MultiServiceRouter {
    router_on(&Arc::new(engine::InMemoryEngine::new()))
}

/// RULE: a `CreateEventSourceMapping` stating a `kafka://<topic>` destination is accepted only
/// for a mapping it puts in provisioned mode naming a topic that mapping does not consume, and
/// is otherwise refused with the modelled 400 stating which half of the rule the request
/// broke, leaving no mapping behind.
#[tokio::test]
async fn a_create_states_a_kafka_topic_only_in_provisioned_mode() {
    let router = emulator();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let function = format!("kafka-destination-create-{index}");
        create_function(&router, &function).await;

        let answer = create_mapping(
            &router,
            &row.create_body(&function, Some(row.destination_config())),
        )
        .await;
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let held = listed.mappings().len();

        if row.accepted() {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: {} is a destination this mapping can deliver to, so the create \
                 must be accepted, got {} {}",
                row.destination,
                answer.status,
                answer.text()
            );
            assert_eq!(
                held, 1,
                "case {case:?}: an accepted create must store one mapping, got {held} \
                 mapping(s) in {}",
                listed.text()
            );
            // Read back: the destination the create accepted is the one the mapping holds.
            let uuid = answer.string("UUID").unwrap_or_default();
            let reading = get_mapping(&router, &uuid).await;
            let reported = row.reported_destination(&reading);
            assert_eq!(
                reported.as_deref(),
                Some(row.destination),
                "case {case:?}: the stored mapping must report {} in its {:?} slot, got {}",
                row.destination,
                row.slot,
                reading.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: a Kafka topic takes provisioned mode and a topic the mapping does \
             not consume, so the create must answer 400, got {} {}",
            answer.status,
            answer.text()
        );
        let code = answer.error_code();
        assert!(
            code.contains(INVALID_PARAMETER),
            "case {case:?}: the refusal must be {INVALID_PARAMETER}, got {code:?}"
        );
        let message = answer.string("message").unwrap_or_default();
        let expected = row.refusal().unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert_eq!(
            held, 0,
            "case {case:?}: a refused create must store no mapping, got {held} mapping(s) in {}",
            listed.text()
        );
    }
}

/// RULE: the same holds for `UpdateEventSourceMapping`, judged on the mode in force after it —
/// the mapping's own, since these updates state no poller configuration — so a Kafka topic
/// stated for a mapping that is not in provisioned mode is refused with that same modelled 400
/// and the mapping keeps the destination it held.
#[tokio::test]
async fn an_update_states_a_kafka_topic_only_in_provisioned_mode() {
    let router = emulator();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let function = format!("kafka-destination-update-{index}");
        create_function(&router, &function).await;

        // The mapping starts out holding a queue in the row's own slot, so a refused update is
        // visible as the destination that is still held afterwards.
        let created = create_mapping(
            &router,
            &row.create_body(&function, Some(row.config_with(HELD_DESTINATION))),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: the seed create must be accepted, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created.string("UUID").unwrap_or_default();

        // A mapping that is still settling is not available to take another change, and the
        // settle is owed by the CLOCK: this waits for the mapping to REPORT the state its
        // create asked for rather than for a span of its own.
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

        let answer = update_mapping(
            &router,
            &uuid,
            &json!({ "DestinationConfig": row.destination_config() }),
        )
        .await;

        if row.accepted() {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: {} is a destination this mapping can deliver to, so the update \
                 must be accepted, got {} {}",
                row.destination,
                answer.status,
                answer.text()
            );
            let reported = row.reported_destination(&answer);
            assert_eq!(
                reported.as_deref(),
                Some(row.destination),
                "case {case:?}: the update must report {} in its {:?} slot, got {}",
                row.destination,
                row.slot,
                answer.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: a Kafka topic takes provisioned mode and a topic the mapping does \
             not consume, so the update must answer 400, got {} {}",
            answer.status,
            answer.text()
        );
        let code = answer.error_code();
        assert!(
            code.contains(INVALID_PARAMETER),
            "case {case:?}: the refusal must be {INVALID_PARAMETER}, got {code:?}"
        );
        let message = answer.string("message").unwrap_or_default();
        let expected = row.refusal().unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // Nothing changed: the mapping still holds the destination its create stored.
        let reading = get_mapping(&router, &uuid).await;
        let held = row.reported_destination(&reading);
        assert_eq!(
            held.as_deref(),
            Some(HELD_DESTINATION),
            "case {case:?}: a refused update must leave the held destination alone, got {}",
            reading.text()
        );
    }
}

/// RULE: the mode a Kafka topic is judged against is the one in force AFTER the update, so an
/// update that itself turns provisioned mode ON may state a Kafka topic in the same call,
/// while one that leaves a provisioned mapping's mode alone keeps it.
#[tokio::test]
async fn an_update_that_turns_provisioned_mode_on_may_state_a_kafka_topic() {
    let router = emulator();
    let function = "kafka-destination-mode-turned-on";
    create_function(&router, function).await;

    let row = Row {
        case: "an update that turns provisioned mode on",
        event_source: Some(MSK_SOURCE),
        pollers: Pollers::None,
        destination: "kafka://failed-batches",
        slot: Slot::OnFailure,
    };
    let created = create_mapping(
        &router,
        &row.create_body(function, Some(row.config_with(HELD_DESTINATION))),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "the seed create must be accepted, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created.string("UUID").unwrap_or_default();
    await_mapping_state(&router, row.case, &uuid, STATE_ENABLED).await;

    // The same call states the mode and the destination: the mode in force afterwards is the
    // one it states, so the Kafka topic stands.
    let answer = update_mapping(
        &router,
        &uuid,
        &json!({
            "DestinationConfig": row.destination_config(),
            "ProvisionedPollerConfig": { "MinimumPollers": 1 },
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "an update that turns provisioned mode on may state a Kafka topic, got {} {}",
        answer.status,
        answer.text()
    );
    let reported = row.reported_destination(&answer);
    assert_eq!(
        reported.as_deref(),
        Some(row.destination),
        "the update must report {} in its on-failure slot, got {}",
        row.destination,
        answer.text()
    );

    // And a later update that states nothing about the mode is judged against the mode the
    // mapping now holds, so a Kafka topic still stands. The update above left the mapping
    // settling again, which is waited out by looking for the state it settles into.
    await_mapping_state(&router, row.case, &uuid, STATE_ENABLED).await;
    let again = update_mapping(
        &router,
        &uuid,
        &json!({ "DestinationConfig": row.config_with("kafka://failed-batches.v2") }),
    )
    .await;
    assert_eq!(
        again.status,
        202,
        "a mapping already in provisioned mode keeps it, got {} {}",
        again.status,
        again.text()
    );
    let reported = row.reported_destination(&again);
    assert_eq!(
        reported.as_deref(),
        Some("kafka://failed-batches.v2"),
        "the second update must report the topic it stated, got {}",
        again.text()
    );
}
