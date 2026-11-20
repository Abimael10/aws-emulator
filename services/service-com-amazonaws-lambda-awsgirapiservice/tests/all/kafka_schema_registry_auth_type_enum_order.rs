// SPDX-License-Identifier: CC0-1.0

//! How this service states the VALUE SET of `KafkaSchemaRegistryAuthType` when a request
//! names an access configuration `Type` the type does not admit.
//!
//! RULE: a value outside an enumerated type's set is refused with the modelled
//! `ValidationException` (HTTP 400) naming the rejected value, the member path — including
//! the 1-based position of the list element the value sits in — and the whole set that type
//! admits, listed in the order AWS states it in, which is neither the order the model
//! declares the enum in nor ascending order. For `KafkaSchemaRegistryAuthType` (`BASIC_AUTH |
//! CLIENT_CERTIFICATE_TLS_AUTH | SERVER_ROOT_CA_CERTIFICATE | OAUTHBEARER_AUTH` as the model
//! declares it, `smithy_query(query="shape", shape="KafkaSchemaRegistryAuthType")`) that
//! order is `[CLIENT_CERTIFICATE_TLS_AUTH, OAUTHBEARER_AUTH, SERVER_ROOT_CA_CERTIFICATE,
//! BASIC_AUTH]`.
//!
//! The rows differ along what the rule generalises over: WHICH value is rejected (a word of
//! no enum, a declared value in the wrong case, a value of the sibling `SourceAccessType`
//! enum that this one does not declare, a prefix of a declared value), WHICH POSITION of the
//! `accessConfigs` list carries it, and WHICH member of the input the registry configuration
//! arrived in — the order belongs to the enum SHAPE, so a `Type` inside
//! `selfManagedKafkaEventSourceConfig` and one inside `amazonManagedKafkaEventSourceConfig`
//! are stated alike. The rows beside them are the control: every value the type DOES declare
//! is accepted, so the sentence is the answer to a value outside the set and not to the
//! operation.
//!
//! Every row drives a real `CreateEventSourceMapping` through the production
//! `MultiServiceRouter` against a function this suite created, and the refused rows are
//! checked to have left nothing behind.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::event_source_mappings::{
    create_function, create_mapping, list_mappings, router_on, MSK_SOURCE,
};

/// The modelled error a value outside an enumerated type's set is refused with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The order AWS lists `KafkaSchemaRegistryAuthType`'s value set in.
const STATED_SET: &str =
    "[CLIENT_CERTIFICATE_TLS_AUTH, OAUTHBEARER_AUTH, SERVER_ROOT_CA_CERTIFICATE, BASIC_AUTH]";

/// A bootstrap server of the self-managed Apache Kafka cluster the rows that name one poll.
const BOOTSTRAP_SERVER: &str = "broker-1.orders.example.com:9092";

/// The topic every mapping of this suite consumes.
const TOPIC: &str = "order-events";

/// The schema registry every row points at: "For Glue schema registries, use the ARN of the
/// registry" (`smithy_query(query="shape", shape="KafkaSchemaRegistryConfig")`).
const REGISTRY_URI: &str = "arn:aws:glue:us-east-1:000000000000:registry/orders";

/// The secret an access configuration authenticates with: "The URI of the secret (Secrets
/// Manager secret ARN) to authenticate with your schema registry".
const SECRET_URI: &str = "arn:aws:secretsmanager:us-east-1:000000000000:secret:registry-auth";

/// Which member of the input the registry configuration arrives in. Both structures declare
/// the same `SchemaRegistryConfig` member (`smithy_query(query="shape",
/// shape="SelfManagedKafkaEventSourceConfig,AmazonManagedKafkaEventSourceConfig")`), so a
/// value outside the enum's set is the same violation in either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carrier {
    SelfManaged,
    AmazonManaged,
}

impl Carrier {
    /// The input member this carrier is, in the lowerCamel spelling a refusal states it in.
    fn member_path(self) -> &'static str {
        match self {
            Carrier::SelfManaged => "selfManagedKafkaEventSourceConfig",
            Carrier::AmazonManaged => "amazonManagedKafkaEventSourceConfig",
        }
    }

    /// The input member this carrier is, as the wire spells it in a request.
    fn request_member(self) -> &'static str {
        match self {
            Carrier::SelfManaged => "SelfManagedKafkaEventSourceConfig",
            Carrier::AmazonManaged => "AmazonManagedKafkaEventSourceConfig",
        }
    }
}

/// One row: where the registry configuration arrives, the `Type` of each access configuration
/// it carries, and which of them — by its 1-based position — the type does not admit.
struct Row {
    case: &'static str,
    carrier: Carrier,
    types: &'static [&'static str],
    /// The 1-based position of the element the clause must name, or `None` when every value
    /// the row carries is one the type declares.
    refused_at: Option<usize>,
}

/// The catalog. Each rejected value is a different way of being outside the set; each
/// admitted row carries only values the model declares.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a word of no enum, in the only access configuration",
            carrier: Carrier::SelfManaged,
            types: &["NOT_A_TYPE"],
            refused_at: Some(1),
        },
        Row {
            case: "a declared value in the wrong case",
            carrier: Carrier::SelfManaged,
            types: &["basic_auth"],
            refused_at: Some(1),
        },
        Row {
            case: "a value of the sibling source-access enum this one does not declare",
            carrier: Carrier::SelfManaged,
            types: &["SASL_SCRAM_512_AUTH"],
            refused_at: Some(1),
        },
        Row {
            case: "the second of two access configurations",
            carrier: Carrier::SelfManaged,
            types: &["BASIC_AUTH", "OAUTHBEARER_SCOPE"],
            refused_at: Some(2),
        },
        Row {
            case: "the third of three access configurations, carrying a prefix of a value",
            carrier: Carrier::AmazonManaged,
            types: &["BASIC_AUTH", "SERVER_ROOT_CA_CERTIFICATE", "OAUTHBEARER"],
            refused_at: Some(3),
        },
        Row {
            case: "a word of no enum, in the Amazon-managed carrier",
            carrier: Carrier::AmazonManaged,
            types: &["NOT_A_TYPE"],
            refused_at: Some(1),
        },
        Row {
            case: "every value the type declares, in the self-managed carrier",
            carrier: Carrier::SelfManaged,
            types: &[
                "BASIC_AUTH",
                "CLIENT_CERTIFICATE_TLS_AUTH",
                "SERVER_ROOT_CA_CERTIFICATE",
                "OAUTHBEARER_AUTH",
            ],
            refused_at: None,
        },
        Row {
            case: "one value the type declares, in the Amazon-managed carrier",
            carrier: Carrier::AmazonManaged,
            types: &["CLIENT_CERTIFICATE_TLS_AUTH"],
            refused_at: None,
        },
    ]
}

impl Row {
    /// The whole refusal this row must answer: the envelope, the rejected value as the request
    /// sent it, the member path down to the element's own `type`, and the set in the order AWS
    /// states it.
    fn refusal(&self) -> Option<String> {
        let position = self.refused_at?;
        let value = self.types.get(position - 1)?;
        Some(format!(
            "1 validation error detected: Value '{value}' at \
             '{member}.schemaRegistryConfig.accessConfigs.{position}.member.type' failed to \
             satisfy constraint: Member must satisfy enum value set: {STATED_SET}",
            member = self.carrier.member_path(),
        ))
    }

    /// The `CreateEventSourceMapping` body this row sends: a mapping on the source its
    /// carrier belongs to, consuming one topic, with the registry configuration this row
    /// states.
    ///
    /// A registry configuration is only available to a mapping in PROVISIONED MODE, so every
    /// row puts the mapping in that mode (the rule is
    /// [`crate::kafka_schema_registry_provisioned_mode`]'s); what these rows are about is the
    /// value set the refusal states, which is reached whatever the mode.
    fn create_body(&self, function: &str) -> Value {
        let access_configs: Vec<Value> = self
            .types
            .iter()
            .map(|r#type| json!({ "Type": r#type, "URI": SECRET_URI }))
            .collect();
        let config = json!({
            "SchemaRegistryConfig": {
                "SchemaRegistryURI": REGISTRY_URI,
                "EventRecordFormat": "JSON",
                "AccessConfigs": access_configs,
                "SchemaValidationConfigs": [{ "Attribute": "VALUE" }],
            }
        });
        let mut body = Map::new();
        body.insert("FunctionName".to_string(), json!(function));
        body.insert("Topics".to_string(), json!([TOPIC]));
        body.insert(
            "ProvisionedPollerConfig".to_string(),
            json!({ "MinimumPollers": 1 }),
        );
        match self.carrier {
            Carrier::SelfManaged => {
                body.insert(
                    "SelfManagedEventSource".to_string(),
                    json!({ "Endpoints": { "KAFKA_BOOTSTRAP_SERVERS": [BOOTSTRAP_SERVER] } }),
                );
            }
            Carrier::AmazonManaged => {
                body.insert("EventSourceArn".to_string(), json!(MSK_SOURCE));
            }
        }
        body.insert(self.carrier.request_member().to_string(), config);
        Value::Object(body)
    }
}

#[tokio::test]
async fn a_schema_registry_auth_type_outside_the_set_is_refused_with_awss_own_ordering() {
    let router = router_on(&Arc::new(engine::InMemoryEngine::new()));

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let function = format!("kafka-registry-auth-{index}");
        create_function(&router, &function).await;
        let answer = create_mapping(&router, &row.create_body(&function)).await;
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let held = listed.mappings().len();

        let Some(expected) = row.refusal() else {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: every value the type declares must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                held, 1,
                "case {case:?}: an accepted create must store one mapping, got {held} \
                 mapping(s) in {}",
                listed.text()
            );
            continue;
        };

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status,
            answer.text()
        );
        let code = answer.error_code();
        assert_eq!(
            code, VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {code:?}"
        );
        let message = answer
            .string("message")
            .or_else(|| answer.string("Message"))
            .unwrap_or_default();
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
