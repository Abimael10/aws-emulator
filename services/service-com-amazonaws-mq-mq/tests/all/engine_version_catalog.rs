// SPDX-License-Identifier: CC0-1.0

//! The engine versions Amazon MQ publishes for creation, and the sentence it refuses an
//! engine type outside the modelled set with — both driven over the wire.
//!
//! RULE A: the versions an engine offers for creation are ONE list per engine type, the
//! lines the service currently offers, reported newest first; every operation that
//! publishes that list publishes the same one, and a create may name any entry of it (and
//! defaults to its newest entry when it names none).
//!
//! RULE B: a request whose `engineType` is not a member of the modelled `EngineType` set
//! is refused by every operation that takes the member with HTTP 400
//! `BadRequestException`, `errorAttribute` naming `engineType`, and Amazon MQ's own
//! sentence for that error class: "Broker engine [<the value the request sent>] is
//! invalid." — fixed text whose only variable part is the rejected value, with no list of
//! accepted values appended.
//!
//! The tables span what the rules generalise over rather than one recorded request: every
//! engine type the model enumerates crossed with every version it offers and with the
//! absent member, both catalog operations plus both creates, and several forms of an
//! engine type outside the set (another spelling of a modelled value, a modelled value
//! with a suffix, a value belonging to another member, and a value that is not a word).

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::{DEPLOYMENT_MODE_VALUES, ENGINE_TYPE_VALUES};
use service_com_amazonaws_mq_mq::state::{published_engine_versions, MAX_MAX_RESULTS};

use crate::broker_lifecycle::{string_at, Mq};

/// The version lines Amazon MQ offers for creation on each engine, newest first.
///
/// Source: the recorded catalogs this service publishes them in — the
/// `DescribeBrokerEngineTypes` `brokerEngineTypes[*].engineVersions` and the
/// `DescribeBrokerInstanceOptions` `brokerInstanceOptions[*].supportedEngineVersions`
/// answers — because the model enumerates no version string (`BrokerEngineType`'s
/// `engineVersions` targets `__listOfEngineVersion`, whose `name` is `__string`:
/// `smithy_query(query="shape", shape="BrokerEngineType,EngineVersion")`). The ActiveMQ
/// row is the engine the reported divergence did not cite.
const OFFERED_VERSIONS_NEWEST_FIRST: &[(&str, &[&str])] = &[
    ("ACTIVEMQ", &["5.19", "5.18"]),
    ("RABBITMQ", &["4.3", "4.2", "3.13"]),
];

/// `DescribeBrokerEngineTypes`' modelled path (`GET /v1/broker-engine-types`).
const ENGINE_TYPES_PATH: &str = "/v1/broker-engine-types";

/// `DescribeBrokerInstanceOptions`' modelled path (`GET /v1/broker-instance-options`).
const INSTANCE_OPTIONS_PATH: &str = "/v1/broker-instance-options";

/// The version names one `brokerEngineTypes` entry reports, in the order it reports them.
fn version_names(case: &str, entry: &Value) -> Vec<String> {
    let Some(versions) = entry.get("engineVersions").and_then(Value::as_array) else {
        panic!("case {case:?}: expected an engineVersions array, got {entry:?}");
    };
    versions
        .iter()
        .map(|version| {
            let Some(name) = version.get("name").and_then(Value::as_str) else {
                panic!("case {case:?}: expected each version to carry a name, got {version:?}");
            };
            name.to_string()
        })
        .collect()
}

/// The strings one list member carries, in order.
fn strings_at(case: &str, value: &Value, pointer: &str) -> Vec<String> {
    let Some(list) = value.pointer(pointer).and_then(Value::as_array) else {
        panic!("case {case:?}: expected a list at {pointer}, got {value:?}");
    };
    list.iter()
        .map(|item| {
            let Some(text) = item.as_str() else {
                panic!("case {case:?}: expected a string in {pointer}, got {item:?}");
            };
            text.to_string()
        })
        .collect()
}

/// RULE A, the engine catalog: `DescribeBrokerEngineTypes` reports every offered version
/// line of every engine, newest first — unfiltered and under the engine's own filter.
#[tokio::test]
async fn the_engine_catalog_publishes_every_offered_version_newest_first() {
    let mq = Mq::new("us-east-1");
    let mut rows = 0usize;

    for (engine_type, offered) in OFFERED_VERSIONS_NEWEST_FIRST {
        // The same question asked both ways: of the whole catalog, and of the catalog
        // narrowed to this engine.
        for query in [
            format!("maxResults={MAX_MAX_RESULTS}"),
            format!("maxResults={MAX_MAX_RESULTS}&engineType={engine_type}"),
        ] {
            let case = format!("{engine_type}: {query}");
            let (status, body) = mq
                .call("GET", &format!("{ENGINE_TYPES_PATH}?{query}"), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {body:?}"
            );
            let Some(entries) = body.get("brokerEngineTypes").and_then(Value::as_array) else {
                panic!("case {case:?}: expected a brokerEngineTypes array, got {body:?}");
            };
            let Some(entry) = entries
                .iter()
                .find(|entry| entry.get("engineType").and_then(Value::as_str) == Some(engine_type))
            else {
                panic!("case {case:?}: expected an entry for {engine_type}, got {entries:?}");
            };
            let reported = version_names(&case, entry);
            assert_eq!(
                reported, *offered,
                "case {case:?}: expected the offered versions newest first {offered:?}, got \
                 {reported:?}"
            );
            rows += 1;
        }

        // The same list is the one the crate publishes to the rest of the service, so the
        // catalog and whatever quotes it cannot drift apart.
        let mut published: Vec<String> = published_engine_versions(engine_type)
            .iter()
            .map(|version| (*version).to_string())
            .collect();
        published.reverse();
        assert_eq!(
            published, *offered,
            "case {engine_type:?}: the published list must be the one the catalog serves, got \
             {published:?}"
        );
    }

    assert_eq!(
        rows,
        2 * ENGINE_TYPE_VALUES.len(),
        "the table must cover every modelled engine type both ways, covered {rows}"
    );
}

/// RULE A, the instance-option catalog: every entry of an engine reports the SAME offered
/// version list, so a client choosing a version from either catalog chooses the same set.
#[tokio::test]
async fn the_instance_option_catalog_reports_the_same_offered_versions() {
    let mq = Mq::new("eu-west-1");
    let mut rows = 0usize;

    for (engine_type, offered) in OFFERED_VERSIONS_NEWEST_FIRST {
        let case = format!("{engine_type} instance options");
        let (status, body) = mq
            .call(
                "GET",
                &format!(
                    "{INSTANCE_OPTIONS_PATH}?maxResults={MAX_MAX_RESULTS}&engineType={engine_type}"
                ),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        let Some(entries) = body.get("brokerInstanceOptions").and_then(Value::as_array) else {
            panic!("case {case:?}: expected a brokerInstanceOptions array, got {body:?}");
        };
        assert!(
            !entries.is_empty(),
            "case {case:?}: expected at least one instance option for the engine, got {body:?}"
        );
        for entry in entries {
            let case = format!(
                "{case}: {}",
                entry
                    .get("hostInstanceType")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            );
            let reported = strings_at(&case, entry, "/supportedEngineVersions");
            assert_eq!(
                reported, *offered,
                "case {case:?}: expected the same offered versions {offered:?} as the engine \
                 catalog, got {reported:?}"
            );
            rows += 1;
        }
    }

    assert!(
        rows >= ENGINE_TYPE_VALUES.len(),
        "the table must cover an entry of every engine, covered {rows}"
    );
}

/// RULE A, the create half: every version the catalogs publish is one a create takes, and
/// a create that names none gets the newest.
#[tokio::test]
async fn every_offered_version_is_one_a_create_takes_and_reads_back() {
    let mq = Mq::new("us-west-2");
    let mut rows = 0usize;

    for (engine_type, offered) in OFFERED_VERSIONS_NEWEST_FIRST {
        // Each offered line, and the absent member — which the newest line answers for.
        let mut requested: Vec<Option<&str>> = offered.iter().map(|line| Some(*line)).collect();
        requested.push(None);

        for request in requested {
            let case = format!("{engine_type}/{request:?}");
            let expected_line = request.unwrap_or(offered[0]);
            let suffix = request.unwrap_or("default").replace('.', "-");
            let host_instance_type = if *engine_type == "RABBITMQ" {
                "mq.m5.large"
            } else {
                "mq.t3.micro"
            };

            let mut broker = json!({
                "brokerName": format!("offered-{}-{suffix}", engine_type.to_lowercase()),
                "engineType": engine_type,
                "deploymentMode": "SINGLE_INSTANCE",
                "hostInstanceType": host_instance_type,
                "publiclyAccessible": false,
                "users": [{"username": "admin", "password": "SuperSecret123"}],
            });
            if let Some(version) = request {
                broker["engineVersion"] = json!(version);
            }
            let (status, created) = mq.create_broker(broker).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a version the catalog offers must be one CreateBroker takes, got \
                 {status} {created:?}"
            );
            let (status, described) = mq.describe_broker(&string_at(&created, "/brokerId")).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected to read the broker back, got {status} {described:?}"
            );
            let running = string_at(&described, "/engineVersion");
            assert!(
                running == expected_line || running.starts_with(&format!("{expected_line}.")),
                "case {case:?}: expected a release of the {expected_line:?} line, got {running:?}"
            );

            let mut configuration = json!({
                "name": format!("offered-{}-{suffix}", engine_type.to_lowercase()),
                "engineType": engine_type,
            });
            if let Some(version) = request {
                configuration["engineVersion"] = json!(version);
            }
            let (status, created) = mq
                .call("POST", "/v1/configurations", Some(configuration))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a version the catalog offers must be one CreateConfiguration \
                 takes, got {status} {created:?}"
            );
            let (status, _code, described) =
                mq.describe_configuration(&string_at(&created, "/id")).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected to read the configuration back, got {status} {described:?}"
            );
            assert_eq!(
                string_at(&described, "/engineVersion"),
                expected_line,
                "case {case:?}: expected the configuration on the {expected_line:?} line, got \
                 {described:?}"
            );
            rows += 1;
        }
    }

    assert!(
        rows >= ENGINE_TYPE_VALUES.len() + 2,
        "the table must cover every offered line of every engine plus the absent member, \
         covered {rows}"
    );
}

/// The forms an `engineType` outside the modelled set arrives in. Each is DERIVED from the
/// model's own value set or from another member's, never copied from a request: another
/// spelling of a modelled value, a modelled value with a suffix, a value belonging to
/// another enumerated member, and a value that is not a word at all.
fn engine_types_outside_the_set() -> Vec<String> {
    vec![
        ENGINE_TYPE_VALUES[0].to_lowercase(),
        format!("{}_EDGE", ENGINE_TYPE_VALUES[1]),
        DEPLOYMENT_MODE_VALUES[0].to_string(),
        "0".to_string(),
    ]
}

/// RULE B: every operation that takes `engineType` refuses a value outside the modelled
/// set with Amazon MQ's own sentence, naming the member and no accepted values.
#[tokio::test]
async fn an_engine_type_outside_the_modelled_set_is_refused_the_same_way_everywhere() {
    let mq = Mq::new("us-east-1");
    let mut rows = 0usize;

    for engine_type in engine_types_outside_the_set() {
        // Each operation that takes the member: both catalog reads, which take it as a
        // query-string filter, and both creates, which take it in the body.
        let requests: Vec<(String, &str, &str, Option<Value>)> = vec![
            (
                format!("{ENGINE_TYPES_PATH}?engineType={engine_type}"),
                "GET",
                "DescribeBrokerEngineTypes",
                None,
            ),
            (
                format!("{INSTANCE_OPTIONS_PATH}?engineType={engine_type}"),
                "GET",
                "DescribeBrokerInstanceOptions",
                None,
            ),
            (
                "/v1/brokers".to_string(),
                "POST",
                "CreateBroker",
                Some(json!({
                    "brokerName": "outside-the-set",
                    "engineType": engine_type,
                    "deploymentMode": "SINGLE_INSTANCE",
                    "hostInstanceType": "mq.t3.micro",
                    "publiclyAccessible": false,
                    "users": [{"username": "admin", "password": "SuperSecret123"}],
                })),
            ),
            (
                "/v1/configurations".to_string(),
                "POST",
                "CreateConfiguration",
                Some(json!({"name": "outside-the-set", "engineType": engine_type})),
            ),
        ];

        for (path, method, operation, body) in requests {
            let case = format!("{operation} with engineType {engine_type:?}");
            let (status, code, answer) = mq.call_with_code(method, &path, body).await;
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
            let expected = format!("Broker engine [{engine_type}] is invalid.");
            assert_eq!(
                answer.get("message").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
            rows += 1;
        }
    }

    assert_eq!(
        rows,
        4 * engine_types_outside_the_set().len(),
        "the table must cover every form against every operation, covered {rows}"
    );
}
