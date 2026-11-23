// SPDX-License-Identifier: CC0-1.0

//! How Amazon MQ words the refusal of an `engineType` outside the modelled set, per the
//! operation that refuses it — driven over the wire.
//!
//! The rule, one level above the recorded case (a `CreateConfiguration` whose `engineType`
//! was a value no `EngineType` enumerates):
//!
//! > A value outside `EngineType`'s modelled set
//! > (`smithy_query(query="shape",shape="EngineType")`: exactly `ACTIVEMQ` and `RABBITMQ`) is
//! > refused with HTTP 400 `BadRequestException` whose `errorAttribute` names `engineType`,
//! > and Amazon MQ words the sentence beside it per operation CLASS: a request naming the
//! > engine of a resource it would CREATE states the member's own subject, the rejected
//! > value and the values it accepts — "Broker engine type [<value>] is invalid. Valid
//! > values are: [<the modelled set, comma-space separated>]" — while a CATALOG read, which
//! > takes the member as the query-string filter it narrows its published catalog by, states
//! > "Broker engine [<value>] is invalid." and appends no list of accepted values.
//!
//! The rows span what the rule generalises over: both operations of each class (the two
//! creates that take the member in the body, the two catalog reads that take it as a filter)
//! crossed with several forms of a value outside the set — a modelled value in the wrong
//! case, a modelled value with a suffix, a value belonging to another member, and a value
//! that is no word at all. The recorded case is ONE cell of that table.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::{DEPLOYMENT_MODE_VALUES, ENGINE_TYPE_VALUES};

use crate::broker_lifecycle::Mq;

/// Which class an operation's refusal is worded in: what decides the sentence is whether the
/// member FILTERS a published catalog or names the engine of a resource being created.
#[derive(Debug, Clone, Copy)]
enum Class {
    CatalogFilter,
    CreatedResource,
}

impl Class {
    /// The sentence this class states for `engine_type`.
    fn sentence(self, engine_type: &str) -> String {
        match self {
            Self::CatalogFilter => format!("Broker engine [{engine_type}] is invalid."),
            Self::CreatedResource => format!(
                "Broker engine type [{engine_type}] is invalid. Valid values are: [{}]",
                ENGINE_TYPE_VALUES.join(", ")
            ),
        }
    }
}

/// One request that carries an `engineType`: the operation, how it carries the member, and
/// the class its refusal is worded in.
struct Request {
    operation: &'static str,
    method: &'static str,
    /// The path, with `{engine}` standing for the value under test where the member travels
    /// in the query string.
    path: &'static str,
    /// The body, with the member filled in, for an operation that takes it in the body.
    body: Option<fn(&str) -> Value>,
    class: Class,
}

fn requests() -> Vec<Request> {
    vec![
        Request {
            operation: "DescribeBrokerEngineTypes",
            method: "GET",
            path: "/v1/broker-engine-types?engineType={engine}",
            body: None,
            class: Class::CatalogFilter,
        },
        Request {
            operation: "DescribeBrokerInstanceOptions",
            method: "GET",
            path: "/v1/broker-instance-options?engineType={engine}",
            body: None,
            class: Class::CatalogFilter,
        },
        Request {
            operation: "CreateBroker",
            method: "POST",
            path: "/v1/brokers",
            body: Some(|engine| {
                json!({
                    "brokerName": "engine-type-refusal",
                    "engineType": engine,
                    "deploymentMode": DEPLOYMENT_MODE_VALUES[0],
                    "hostInstanceType": "mq.t3.micro",
                    "publiclyAccessible": false,
                    "users": [{"username": "admin", "password": "SuperSecret123"}],
                })
            }),
            class: Class::CreatedResource,
        },
        Request {
            operation: "CreateConfiguration",
            method: "POST",
            path: "/v1/configurations",
            body: Some(|engine| json!({"name": "engine-type-refusal", "engineType": engine})),
            class: Class::CreatedResource,
        },
    ]
}

/// Values outside the modelled set, built FROM that set rather than copied from one request.
fn engine_types_outside_the_set() -> Vec<String> {
    vec![
        ENGINE_TYPE_VALUES[0].to_lowercase(),
        format!("{}_EDGE", ENGINE_TYPE_VALUES[1]),
        DEPLOYMENT_MODE_VALUES[0].to_string(),
        "NOTAREALENGINE".to_string(),
        "0".to_string(),
    ]
}

/// RULE: every operation that takes the member refuses a value outside the set with 400
/// `BadRequestException` naming `engineType`, worded as its own class words it.
#[tokio::test]
async fn an_engine_type_outside_the_set_is_worded_as_its_operation_class_words_it() {
    let mq = Mq::new("us-east-1");
    let mut cells = 0usize;

    for engine_type in engine_types_outside_the_set() {
        for request in requests() {
            let case = format!("{} with engineType {engine_type:?}", request.operation);
            let path = request.path.replace("{engine}", &engine_type);
            let body = request.body.map(|build| build(&engine_type));
            let (status, code, answer) = mq.call_with_code(request.method, &path, body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {answer:?}"
            );
            assert_eq!(
                code, "BadRequestException",
                "case {case:?}: expected the modelled error code, got {code:?} {answer:?}"
            );
            assert_eq!(
                answer.get("errorAttribute").and_then(Value::as_str),
                Some("engineType"),
                "case {case:?}: the refusal must name the member at fault, got {answer:?}"
            );
            let expected = request.class.sentence(&engine_type);
            assert_eq!(
                answer.get("message").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
            cells += 1;
        }
    }

    assert_eq!(
        cells,
        requests().len() * engine_types_outside_the_set().len(),
        "the table must cover every form against every operation, covered {cells}"
    );
}
