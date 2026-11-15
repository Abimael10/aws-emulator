// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE a configuration writer refuses a `server.properties` declaration
//! it does not configure with, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: a body declaring a broker property the service does not
//! configure is refused HTTP 400 `BadRequestException` whose message is the
//! service's per-property envelope — a JSON array with a space inside its
//! brackets, one object per offending declaration separated by `, `, each
//! object's members written one per line behind two spaces as `"<member>" :
//! <value>`, carrying the declaration's `name` and `value` as the body gave them
//! and an `errorMessages` array, on the member's own line with a space inside its
//! brackets, holding the one sentence about the name and the Apache Kafka versions
//! the service checked it against. Those versions are the versions the SERVICE
//! publishes, so the refusal names the same ones whatever the request's
//! `KafkaVersions` carried.
//!
//! The rows differ along what the rule generalises over: how many declarations
//! offend, which kind of name offends (one Apache Kafka does not have, one the
//! service manages itself, the first token of a line carrying no separator),
//! which separator the line used, what the value looks like, and which versions —
//! none, one, several, one repeated — the request named. The envelope every row is
//! asserted against is written out HERE, byte for byte, rather than rendered
//! through the implementation, so an assertion cannot agree with a renderer that
//! is wrong.

use serde_json::Value;

use service_com_amazonaws_kafka_kafka::state::{
    catalogued_kafka_versions, server_properties::SERVER_PROPERTIES_MEMBER,
};

use super::kafka_harness::{ConfigurationRequest, Emulator, Reply, REGION};

/// One row: the versions the request names, the body it submits, and the
/// declarations the service must report, in the order the body declares them.
struct Row {
    case: &'static str,
    kafka_versions: &'static [&'static str],
    body: &'static [u8],
    offending: &'static [(&'static str, &'static str)],
}

/// The table both writers walk.
const ROWS: &[Row] = &[
    Row {
        case: "an-unknown-name-with-no-version-named",
        kafka_versions: &[],
        body: b"totally.not.a.kafka.property=1\n",
        offending: &[("totally.not.a.kafka.property", "1")],
    },
    Row {
        // A real Apache Kafka property the service manages itself.
        case: "a-reserved-name-with-one-version-named",
        kafka_versions: &["3.6.0"],
        body: b"broker.id=7\n",
        offending: &[("broker.id", "7")],
    },
    Row {
        case: "an-unknown-name-with-several-versions-named",
        kafka_versions: &["3.6.0", "3.9.x"],
        body: b"nosuchkey.at.all=1\n",
        offending: &[("nosuchkey.at.all", "1")],
    },
    Row {
        // Another property the service manages itself, named with the colon
        // separator the properties grammar also admits, and a value carrying one.
        case: "a-reserved-name-after-a-colon-separator",
        kafka_versions: &["3.9.x", "3.9.x"],
        body: b"zookeeper.connect:localhost:2181\n",
        offending: &[("zookeeper.connect", "localhost:2181")],
    },
    Row {
        // A line with no separator at all: the properties grammar reads the text
        // up to the first space as the name and the rest as the value.
        case: "a-line-carrying-no-separator",
        kafka_versions: &[],
        body: b"this line has no equals sign and is not valid java properties syntax",
        offending: &[(
            "this",
            "line has no equals sign and is not valid java properties syntax",
        )],
    },
    Row {
        // Two offenders around a property the service does configure: the entry
        // separator is under test as well as the entries.
        case: "two-offending-names-around-an-admitted-one",
        kafka_versions: &["4.1.x.kraft"],
        body: b"first.not.a.property=1\nnum.partitions=2\nsecond.not.a.property=\n",
        offending: &[("first.not.a.property", "1"), ("second.not.a.property", "")],
    },
];

/// The envelope the service must answer for `offending`, written out byte for
/// byte: the layout, the sentence, and the versions the service publishes
/// (`catalogued_kafka_versions`, the one catalogue `ListKafkaVersions` serves).
fn expected_envelope(offending: &[(&str, &str)]) -> String {
    let versions = catalogued_kafka_versions().join(", ");
    let entries: Vec<String> = offending
        .iter()
        .map(|(name, value)| {
            format!(
                "{{\n  \"name\" : \"{name}\",\n  \"value\" : \"{value}\",\n  \"errorMessages\" : \
                 [ \"Key '{name}' is not supported by at least one Apache Kafka version. Key \
                 checked against versions: [{versions}]\" ]\n}}"
            )
        })
        .collect();
    format!("[ {} ]", entries.join(", "))
}

/// A request for `row` under `name`, naming the versions the row names.
fn request_for(row: &Row, name: &str) -> ConfigurationRequest {
    let mut request = ConfigurationRequest::named(name);
    request.kafka_versions = row
        .kafka_versions
        .iter()
        .map(|version| (*version).to_string())
        .collect();
    request.server_properties = row.body.to_vec();
    request
}

/// Assert `reply` is the per-property refusal for `offending`: 400
/// `BadRequestException`, the envelope as the WHOLE message, and the submitted
/// member named as the parameter at fault.
fn assert_envelope(reply: &Reply, case: &str, operation: &str, offending: &[(&str, &str)]) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (400, "BadRequestException"),
        "case {case} ({operation}): expected 400 BadRequestException, got {} {}",
        reply.status,
        reply.body
    );
    let expected = expected_envelope(offending);
    assert_eq!(
        reply.error_message(),
        expected,
        "case {case} ({operation}): expected the envelope {expected:?}, got {:?}",
        reply.error_message()
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(SERVER_PROPERTIES_MEMBER),
        "case {case} ({operation}): expected the refusal to name {SERVER_PROPERTIES_MEMBER}, got {}",
        reply.body
    );

    // The envelope is also a JSON document, so a caller that parses it reads one
    // object per offending declaration with those three members.
    let parsed: Value = serde_json::from_str(reply.error_message()).unwrap_or_else(|error| {
        panic!(
            "case {case} ({operation}): the message is not JSON ({error}): {:?}",
            reply.error_message()
        )
    });
    let entries = parsed.as_array().unwrap_or_else(|| {
        panic!(
            "case {case} ({operation}): expected a JSON array, got {parsed}"
        )
    });
    assert_eq!(
        entries.len(),
        offending.len(),
        "case {case} ({operation}): expected {} entries, got {parsed}",
        offending.len()
    );
    for (entry, (name, value)) in entries.iter().zip(offending) {
        assert_eq!(
            (
                entry.get("name").and_then(Value::as_str),
                entry.get("value").and_then(Value::as_str),
                entry
                    .get("errorMessages")
                    .and_then(Value::as_array)
                    .map(Vec::len),
            ),
            (Some(*name), Some(*value), Some(1)),
            "case {case} ({operation}): expected the declaration reported as submitted, got {entry}"
        );
    }
}

/// RULE: `CreateConfiguration` refuses a body declaring a property the service
/// does not configure with the per-property envelope, whatever the request's
/// `KafkaVersions` carried — and registers nothing.
#[tokio::test]
async fn a_create_answers_the_per_property_envelope() {
    let emulator = Emulator::new();

    for row in ROWS {
        let name = format!("envelope-create-{}", row.case);
        let reply = emulator
            .call(
                "POST",
                "/v1/configurations",
                Some(request_for(row, &name).to_json()),
            )
            .await;
        assert_envelope(&reply, row.case, "CreateConfiguration", row.offending);
        assert!(
            !emulator.listed_configuration_names().await.contains(&name),
            "case {}: a refused create must register nothing, but {name:?} is listed",
            row.case
        );
    }
}

/// RULE: `UpdateConfiguration` answers the SAME envelope for the same body — the
/// versions it names are the service's, not the ones the configuration itself was
/// created with — and appends nothing.
#[tokio::test]
async fn an_update_answers_the_same_envelope() {
    let emulator = Emulator::new();
    // A configuration pinned to ONE version, so a version-scoped refusal would
    // name that one rather than the service's catalogue.
    let mut created = ConfigurationRequest::named("envelope-updated");
    created.kafka_versions = vec!["3.6.0".to_string()];
    created.server_properties = b"num.partitions=1\n".to_vec();
    let arn = emulator
        .create_configuration_row_ok_in(REGION, &created)
        .await;

    for row in ROWS {
        let reply = emulator.update_configuration(&arn, None, row.body).await;
        assert_envelope(&reply, row.case, "UpdateConfiguration", row.offending);
        let described = emulator.describe_configuration(&arn).await;
        assert_eq!(
            described.at("/latestRevision/revision").as_i64(),
            Some(1),
            "case {}: a refused update must append nothing, got {}",
            row.case,
            described.body
        );
    }
}

/// RULE: the versions a refusal names come from the service, so the SAME
/// offending body answers a byte-identical message however many versions the
/// request named — none, one, several, or one repeated — and that list is the
/// catalogue `ListKafkaVersions` publishes.
#[tokio::test]
async fn the_named_versions_do_not_depend_on_the_request() {
    let emulator = Emulator::new();
    let named: [&[&str]; 5] = [
        &[],
        &["3.6.0"],
        &["3.9.x"],
        &["3.6.0", "3.9.x", "1.1.1"],
        &["4.1.x.kraft", "4.1.x.kraft"],
    ];

    let mut answered: Vec<String> = Vec::new();
    for (index, versions) in named.iter().enumerate() {
        let name = format!("envelope-versions-{index}");
        let mut request = ConfigurationRequest::named(&name);
        request.kafka_versions = versions.iter().map(|v| (*v).to_string()).collect();
        request.server_properties = b"broker.id=7\n".to_vec();
        let reply = emulator
            .call("POST", "/v1/configurations", Some(request.to_json()))
            .await;
        assert_envelope(
            &reply,
            "a request's own versions",
            "CreateConfiguration",
            &[("broker.id", "7")],
        );
        answered.push(reply.error_message().to_string());
    }

    let first = answered
        .first()
        .expect("the table above names at least one request");
    for (index, message) in answered.iter().enumerate() {
        assert_eq!(
            message, first,
            "case versions={:?}: expected the message of a request naming none, got {message:?}",
            named[index]
        );
    }
    // The catalogue is what the list is read from, so the sentence names every
    // version the service publishes — asserted on a version this suite's rows
    // never name.
    assert!(
        first.contains("2.8.2.tiered")
            && first.contains(&catalogued_kafka_versions().join(", ")),
        "expected the published catalogue in the sentence, got {first:?}"
    );
}
