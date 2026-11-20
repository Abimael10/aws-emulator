// SPDX-License-Identifier: CC0-1.0

//! WHEN a Kafka event source mapping may be given a SCHEMA REGISTRY to validate its records
//! against.
//!
//! RULE: a `SchemaRegistryConfig` — "specific configuration settings for a Kafka schema
//! registry", stated inside either Kafka event source configuration
//! (`smithy_query(query="shape", shape="SelfManagedKafkaEventSourceConfig,\
//! AmazonManagedKafkaEventSourceConfig")`) — is validated against by the mapping's own event
//! POLLERS, which "deliver to your function after schema validation", and those are the
//! pollers PROVISIONED MODE provides ("Use Provisioned Mode to customize the minimum and
//! maximum number of event pollers for your event source",
//! `smithy_query(query="shape", shape="ProvisionedPollerConfig")`), turned on by stating
//! `MinimumPollers`. So both calls that state a mapping's event source configuration admit a
//! registry configuration only for a mapping in that mode, and refuse it otherwise with the
//! modelled `InvalidParameterValueException` (HTTP 400,
//! `smithy_query(query="operation", operation="CreateEventSourceMapping,\
//! UpdateEventSourceMapping")`), creating and changing nothing. A registry those pollers DO
//! call has to say WHAT to validate: `SchemaValidationConfigs` is "an array of schema
//! validation configuration objects, which tell Lambda the message attributes you want to
//! validate and filter using your schema registry", so a configuration naming no attribute —
//! the member absent, or carrying an empty array — is refused with that same modelled error,
//! naming the member it requires.
//!
//! The rows differ along what the rule generalises over: WHICH Kafka event source
//! configuration carries the registry (the self-managed one, the Amazon MSK one, neither),
//! the PROVISIONED-POLLER CONFIGURATION in force (none at all, one stating the minimum, one
//! stating both bounds, one stating only the maximum), WHAT the registry configuration itself
//! states (a Glue registry, a Confluent registry with an access configuration) and WHAT it
//! says to validate (one attribute, both, an empty array, no member at all).
//!
//! Every row is driven through a real `CreateEventSourceMapping` and through a real
//! `UpdateEventSourceMapping` on a settled mapping of the same source, over the production
//! `MultiServiceRouter`; an accepted create is read back through `GetEventSourceMapping`, and
//! a refused one is checked to have stored nothing.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    required_registry_field_message, SCHEMA_REGISTRY_WITHOUT_PROVISIONED_MODE_MESSAGE,
    SCHEMA_VALIDATION_CONFIGS_MEMBER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, get_mapping, list_mappings,
    router_on, Answer, MAPPINGS_URI, MSK_SOURCE, STATE_ENABLED,
};

/// The modelled error a registry no poller would call is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// A bootstrap server of the self-managed Apache Kafka cluster the rows that name one poll.
const BOOTSTRAP_SERVER: &str = "broker-1.orders.example.com:9092";

/// The topic every mapping of this suite consumes.
const TOPIC: &str = "order-events";

/// A Glue schema registry: "For Glue schema registries, use the ARN of the registry"
/// (`smithy_query(query="shape", shape="KafkaSchemaRegistryConfig")`).
const GLUE_REGISTRY: &str = "arn:aws:glue:us-east-1:000000000000:registry/orders";

/// A Confluent schema registry, which is addressed by its URL instead.
const CONFLUENT_REGISTRY: &str = "https://psrc-000000.us-east-1.aws.confluent.cloud";

/// The secret a Confluent registry is authenticated with: "The URI of the secret (Secrets
/// Manager secret ARN) to authenticate with your schema registry".
const SECRET: &str = "arn:aws:secretsmanager:us-east-1:000000000000:secret:registry-auth";

/// Which Kafka event source configuration a row states its registry in, if either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carrier {
    /// `SelfManagedKafkaEventSourceConfig`, on a mapping of a self-managed cluster.
    SelfManaged,
    /// `AmazonManagedKafkaEventSourceConfig`, on a mapping of an Amazon MSK cluster.
    AmazonManaged,
    /// Neither: the row states no registry at all, which is the control.
    Neither,
}

/// The provisioned-poller configuration a row states, which is what says whether the mapping
/// is in provisioned mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pollers {
    /// No `ProvisionedPollerConfig` at all.
    None,
    /// `MinimumPollers` alone, which is what the mode is turned on by.
    Minimum,
    /// Both poller bounds.
    Bounds,
    /// `MaximumPollers` alone, which turns nothing on.
    MaximumOnly,
}

impl Pollers {
    /// The `ProvisionedPollerConfig` this row states, or `None` when it states none. The
    /// bounds are inside the ranges the members declare (`MinimumPollers`
    /// `@range(min: 1, max: 200)`, `MaximumPollers` `@range(min: 1, max: 10000)`).
    fn config(self) -> Option<Value> {
        match self {
            Pollers::None => None,
            Pollers::Minimum => Some(json!({ "MinimumPollers": 1 })),
            Pollers::Bounds => Some(json!({ "MinimumPollers": 2, "MaximumPollers": 20 })),
            Pollers::MaximumOnly => Some(json!({ "MaximumPollers": 20 })),
        }
    }

    /// Whether a mapping with this configuration is in provisioned mode.
    fn provisioned(self) -> bool {
        matches!(self, Pollers::Minimum | Pollers::Bounds)
    }
}

/// What a row's registry configuration says to VALIDATE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Validation {
    /// One attribute of the message.
    One,
    /// Both attributes the enum declares (`smithy_query(query="shape",
    /// shape="KafkaSchemaValidationAttribute")`).
    Both,
    /// The member as an empty array, which names no attribute.
    Empty,
    /// No member at all.
    None,
}

impl Validation {
    /// The `SchemaValidationConfigs` this row states, or `None` when it states none.
    fn configs(self) -> Option<Value> {
        match self {
            Validation::One => Some(json!([{ "Attribute": "VALUE" }])),
            Validation::Both => {
                Some(json!([{ "Attribute": "KEY" }, { "Attribute": "VALUE" }]))
            }
            Validation::Empty => Some(json!([])),
            Validation::None => None,
        }
    }

    /// Whether this row's configuration says what to validate.
    fn states_an_attribute(self) -> bool {
        matches!(self, Validation::One | Validation::Both)
    }
}

/// One row: where the registry configuration is stated, what it states, and the mode in force.
struct Row {
    case: &'static str,
    carrier: Carrier,
    /// The registry the configuration names, and the access configurations beside it.
    registry: &'static str,
    authenticated: bool,
    validation: Validation,
    pollers: Pollers,
}

/// The rows: see the module docs.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a self-managed cluster's Glue registry in provisioned mode",
            carrier: Carrier::SelfManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::Minimum,
        },
        Row {
            case: "an Amazon MSK cluster's Confluent registry with a secret, both bounds \
                   stated",
            carrier: Carrier::AmazonManaged,
            registry: CONFLUENT_REGISTRY,
            authenticated: true,
            validation: Validation::Both,
            pollers: Pollers::Bounds,
        },
        Row {
            case: "a self-managed cluster's Confluent registry with a secret, minimum stated",
            carrier: Carrier::SelfManaged,
            registry: CONFLUENT_REGISTRY,
            authenticated: true,
            validation: Validation::One,
            pollers: Pollers::Minimum,
        },
        Row {
            case: "a self-managed cluster's registry with no poller configuration",
            carrier: Carrier::SelfManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::None,
        },
        Row {
            case: "an Amazon MSK cluster's registry with no poller configuration",
            carrier: Carrier::AmazonManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::None,
        },
        Row {
            case: "a Confluent registry with a secret and no poller configuration",
            carrier: Carrier::SelfManaged,
            registry: CONFLUENT_REGISTRY,
            authenticated: true,
            validation: Validation::One,
            pollers: Pollers::None,
        },
        Row {
            case: "a registry for a configuration that states only the maximum",
            carrier: Carrier::AmazonManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::MaximumOnly,
        },
        Row {
            case: "a provisioned registry that says nothing to validate",
            carrier: Carrier::SelfManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::None,
            pollers: Pollers::Minimum,
        },
        Row {
            case: "a provisioned Confluent registry whose validation array is empty",
            carrier: Carrier::AmazonManaged,
            registry: CONFLUENT_REGISTRY,
            authenticated: true,
            validation: Validation::Empty,
            pollers: Pollers::Bounds,
        },
        Row {
            case: "a registry outside provisioned mode that says nothing to validate, the \
                   mode first",
            carrier: Carrier::SelfManaged,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::None,
            pollers: Pollers::None,
        },
        Row {
            case: "no registry at all, outside provisioned mode",
            carrier: Carrier::Neither,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::None,
        },
        Row {
            case: "no registry at all, with only the maximum stated",
            carrier: Carrier::Neither,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::MaximumOnly,
        },
        Row {
            case: "no registry at all, in provisioned mode",
            carrier: Carrier::Neither,
            registry: GLUE_REGISTRY,
            authenticated: false,
            validation: Validation::One,
            pollers: Pollers::Minimum,
        },
    ]
}

impl Row {
    /// Whether the call stating this row's configuration must be accepted: a row that states
    /// no registry always is, and one that states a registry only when it earns no refusal.
    fn accepted(&self) -> bool {
        self.refusal().is_none()
    }

    /// The Kafka event source configuration this row states, if any, and the member it
    /// arrives under.
    fn event_source_config(&self) -> Option<(&'static str, Value)> {
        let mut registry = Map::new();
        registry.insert("SchemaRegistryURI".to_string(), json!(self.registry));
        registry.insert("EventRecordFormat".to_string(), json!("JSON"));
        if self.authenticated {
            registry.insert(
                "AccessConfigs".to_string(),
                json!([{ "Type": "BASIC_AUTH", "URI": SECRET }]),
            );
        }
        if let Some(configs) = self.validation.configs() {
            registry.insert("SchemaValidationConfigs".to_string(), configs);
        }
        let config = json!({ "SchemaRegistryConfig": Value::Object(registry) });
        match self.carrier {
            Carrier::SelfManaged => Some(("SelfManagedKafkaEventSourceConfig", config)),
            Carrier::AmazonManaged => Some(("AmazonManagedKafkaEventSourceConfig", config)),
            Carrier::Neither => None,
        }
    }

    /// The whole refusal this row's registry configuration earns, and `None` when the call
    /// stating it must be accepted: the MODE is decided first, and what to validate after it.
    fn refusal(&self) -> Option<String> {
        if self.carrier == Carrier::Neither {
            return None;
        }
        if !self.pollers.provisioned() {
            return Some(SCHEMA_REGISTRY_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string());
        }
        (!self.validation.states_an_attribute())
            .then(|| required_registry_field_message(SCHEMA_VALIDATION_CONFIGS_MEMBER))
    }

    /// Whether this row's mapping polls the self-managed cluster rather than the Amazon MSK
    /// one: the carrier says which cluster the configuration belongs to, and a row that names
    /// neither polls the Amazon MSK cluster.
    fn self_managed_source(&self) -> bool {
        self.carrier == Carrier::SelfManaged
    }

    /// The `CreateEventSourceMapping` body for this row.
    fn create_body(&self, function: &str, with_registry: bool) -> Value {
        let mut body = Map::new();
        body.insert("FunctionName".to_string(), json!(function));
        if self.self_managed_source() {
            body.insert(
                "SelfManagedEventSource".to_string(),
                json!({ "Endpoints": { "KAFKA_BOOTSTRAP_SERVERS": [BOOTSTRAP_SERVER] } }),
            );
        } else {
            body.insert("EventSourceArn".to_string(), json!(MSK_SOURCE));
        }
        body.insert("Topics".to_string(), json!([TOPIC]));
        if let Some(config) = self.pollers.config() {
            body.insert("ProvisionedPollerConfig".to_string(), config);
        }
        if with_registry {
            if let Some((member, config)) = self.event_source_config() {
                body.insert(member.to_string(), config);
            }
        }
        Value::Object(body)
    }

    /// The registry URI the answer `answer` reports for this row's carrier.
    fn reported_registry(&self, answer: &Answer) -> Option<String> {
        let (member, _) = self.event_source_config()?;
        answer
            .json()
            .get(member)?
            .get("SchemaRegistryConfig")?
            .get("SchemaRegistryURI")?
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

/// RULE: a `CreateEventSourceMapping` stating a schema registry configuration is accepted only
/// for a mapping it puts in provisioned mode whose registry says what to validate, and is
/// otherwise refused with the modelled 400 stating which of the two the request broke, leaving
/// no mapping behind.
#[tokio::test]
async fn a_create_states_a_schema_registry_only_in_provisioned_mode() {
    let router = emulator();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let function = format!("kafka-registry-mode-create-{index}");
        create_function(&router, &function).await;

        let answer = create_mapping(&router, &row.create_body(&function, true)).await;
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let held = listed.mappings().len();

        if row.accepted() {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: the mapping is in provisioned mode or states no registry, so \
                 the create must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                held, 1,
                "case {case:?}: an accepted create must store one mapping, got {held} \
                 mapping(s) in {}",
                listed.text()
            );
            // Read back: the registry the create accepted is the one the mapping holds.
            let uuid = answer.string("UUID").unwrap_or_default();
            let reading = get_mapping(&router, &uuid).await;
            if row.carrier != Carrier::Neither {
                let reported = row.reported_registry(&reading);
                assert_eq!(
                    reported.as_deref(),
                    Some(row.registry),
                    "case {case:?}: the stored mapping must report {} as its registry, got {}",
                    row.registry,
                    reading.text()
                );
            }
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: a schema registry takes provisioned mode and an attribute to \
             validate, so the create must answer 400, got {} {}",
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
/// the mapping's own, since these updates state no poller configuration — so a registry stated
/// for a mapping that is not in provisioned mode is refused with that same modelled 400 and the
/// mapping keeps the configuration it held.
#[tokio::test]
async fn an_update_states_a_schema_registry_only_in_provisioned_mode() {
    let router = emulator();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        if row.carrier == Carrier::Neither {
            // There is nothing for an update to state for a row that names no registry; the
            // create rows above cover the control.
            continue;
        }
        let function = format!("kafka-registry-mode-update-{index}");
        create_function(&router, &function).await;

        // The mapping is created WITHOUT a registry, in whatever mode the row states, so the
        // update is the only call that states one.
        let created = create_mapping(&router, &row.create_body(&function, false)).await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: the seed create must be accepted, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created.string("UUID").unwrap_or_default();

        // A mapping that is still settling is not available to take another change, and that
        // settle is owed by the CLOCK: this waits for the mapping to REPORT the state its
        // create asked for rather than for a span of its own.
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

        let (member, config) = row
            .event_source_config()
            .unwrap_or_else(|| panic!("case {case:?}: the row states a registry"));
        let answer = update_mapping(&router, &uuid, &json!({ member: config })).await;

        if row.accepted() {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: the mapping is in provisioned mode, so the update must be \
                 accepted, got {} {}",
                answer.status,
                answer.text()
            );
            let reported = row.reported_registry(&answer);
            assert_eq!(
                reported.as_deref(),
                Some(row.registry),
                "case {case:?}: the update must report {} as the registry, got {}",
                row.registry,
                answer.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: a schema registry takes provisioned mode and an attribute to \
             validate, so the update must answer 400, got {} {}",
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

        // Nothing changed: the mapping still holds no registry.
        let reading = get_mapping(&router, &uuid).await;
        let held = row.reported_registry(&reading);
        assert_eq!(
            held, None,
            "case {case:?}: a refused update must leave the mapping without a registry, got {}",
            reading.text()
        );
    }
}

/// RULE: the mode a registry is judged against is the one in force AFTER the update, so an
/// update that itself turns provisioned mode on may state a registry in the same call.
#[tokio::test]
async fn an_update_that_turns_provisioned_mode_on_may_state_a_schema_registry() {
    let router = emulator();
    let function = "kafka-registry-mode-turned-on";
    create_function(&router, function).await;

    let row = Row {
        case: "an update that turns provisioned mode on",
        carrier: Carrier::AmazonManaged,
        registry: GLUE_REGISTRY,
        authenticated: false,
        validation: Validation::One,
        pollers: Pollers::None,
    };
    let created = create_mapping(&router, &row.create_body(function, false)).await;
    assert_eq!(
        created.status,
        202,
        "the seed create must be accepted, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created.string("UUID").unwrap_or_default();
    await_mapping_state(&router, row.case, &uuid, STATE_ENABLED).await;

    let (member, config) = row
        .event_source_config()
        .expect("the row states a registry");
    let answer = update_mapping(
        &router,
        &uuid,
        &json!({
            member: config,
            "ProvisionedPollerConfig": { "MinimumPollers": 1 },
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "an update that turns provisioned mode on may state a registry, got {} {}",
        answer.status,
        answer.text()
    );
    let reported = row.reported_registry(&answer);
    assert_eq!(
        reported.as_deref(),
        Some(GLUE_REGISTRY),
        "the update must report the registry it stated, got {}",
        answer.text()
    );
}
