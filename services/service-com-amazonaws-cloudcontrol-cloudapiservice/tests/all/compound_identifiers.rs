// SPDX-License-Identifier: CC0-1.0

//! The primary identifier of a resource type whose registry schema declares a COMPOUND one.
//!
//! The rule every case here holds the service to: **`ProgressEvent.Identifier` and
//! `ResourceDescription.Identifier` are "the primary identifier for the resource", taken
//! from the type's registry schema `primaryIdentifier` — so for a type whose schema points
//! it at several properties, the identifier is those properties' values, in the order the
//! schema lists the pointers, joined by the `|` delimiter, and that one value is what
//! `GetResource`, `UpdateResource` and `DeleteResource` take back.**
//!
//! The rows differ along what the rule generalises over: how many properties the identifier
//! is made of (two, three), whether the resources live in another service's store
//! (`AWS::Logs::LogStream`) or in this one (`AWS::ApplicationAutoScaling::ScalableTarget`,
//! `AWS::EKS::AccessEntry`), what the segments are (names, an ARN, a service-scoped resource
//! id), and which segment two resources share.

use serde_json::{json, Value};

use super::emulator::{code, descriptions, message, text, Emulator, STATUS_FAILED};

/// A type whose schema points `primaryIdentifier` at the group and the stream
/// (`["/properties/LogGroupName", "/properties/LogStreamName"]`), and whose resources live
/// in the CloudWatch Logs service's own store.
const LOG_STREAM: &str = "AWS::Logs::LogStream";

/// The type whose resources hold those streams.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// A type whose schema points `primaryIdentifier` at three properties
/// (`["/properties/ResourceId", "/properties/ScalableDimension",
/// "/properties/ServiceNamespace"]`), so its identifier EMBEDS the id of the resource it
/// scales.
const SCALABLE_TARGET: &str = "AWS::ApplicationAutoScaling::ScalableTarget";

/// A type whose schema points `primaryIdentifier` at a cluster name and a principal ARN
/// (`["/properties/ClusterName", "/properties/PrincipalArn"]`).
const ACCESS_ENTRY: &str = "AWS::EKS::AccessEntry";

/// The delimiter the registry joins the segments of a compound primary identifier with.
const DELIMITER: char = '|';

/// Create the log groups the stream rows are created in, through this service.
async fn provision_groups(emulator: &Emulator, names: &[&str]) {
    for name in names {
        emulator
            .provision(LOG_GROUP, &json!({ "LogGroupName": name }))
            .await;
    }
}

#[tokio::test]
async fn a_compound_primary_identifier_is_every_segment_joined_by_the_delimiter() {
    let emulator = Emulator::with_cloudwatch_logs();
    provision_groups(&emulator, &["app-a", "app-b"]).await;

    // (case, type name, desired state, the identifier the type's schema says the resource
    // answers to)
    let rows: [(&str, &str, Value, String); 5] = [
        (
            "two segments: a stream of one group",
            LOG_STREAM,
            json!({ "LogGroupName": "app-a", "LogStreamName": "host-1" }),
            format!("app-a{DELIMITER}host-1"),
        ),
        (
            "two segments: the same stream name under another group",
            LOG_STREAM,
            json!({ "LogGroupName": "app-b", "LogStreamName": "host-1" }),
            format!("app-b{DELIMITER}host-1"),
        ),
        (
            "two segments: another stream of the first group",
            LOG_STREAM,
            json!({ "LogGroupName": "app-a", "LogStreamName": "host-2" }),
            format!("app-a{DELIMITER}host-2"),
        ),
        (
            "three segments, the first of them a service-scoped resource id",
            SCALABLE_TARGET,
            json!({
                "ResourceId": "table/orders",
                "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
                "ServiceNamespace": "dynamodb",
                "MinCapacity": 1,
                "MaxCapacity": 10,
            }),
            format!(
                "table/orders{DELIMITER}dynamodb:table:ReadCapacityUnits{DELIMITER}dynamodb"
            ),
        ),
        (
            "two segments, the second of them an ARN",
            ACCESS_ENTRY,
            json!({
                "ClusterName": "fleet",
                "PrincipalArn": "arn:aws:iam::000000000000:role/operator",
            }),
            format!("fleet{DELIMITER}arn:aws:iam::000000000000:role/operator"),
        ),
    ];

    // Started together, then waited for: the creates are independent, so one span covers
    // them all.
    let mut accepted = Vec::new();
    for (_, type_name, desired, _) in &rows {
        let event = emulator.create_ok(type_name, desired, None).await;
        accepted.push(text(&event, "RequestToken"));
    }
    for ((case, type_name, desired, expected), request_token) in rows.iter().zip(&accepted) {
        let settled = emulator.settled(request_token).await;
        let reported = text(&settled, "Identifier");
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected the identifier {expected:?}, got {reported:?}"
        );
        // The identifier a caller is handed is the one every other answer takes back: the
        // read resolves it, and the model it reports carries each segment the identifier is
        // made of.
        let properties = emulator.properties(type_name, expected).await;
        for (property, value) in desired.as_object().expect("a resource model") {
            let Some(segment) = value.as_str() else {
                continue;
            };
            if !expected.split(DELIMITER).any(|part| part == segment) {
                continue;
            }
            assert_eq!(
                properties.get(property).and_then(Value::as_str),
                Some(segment),
                "case {case:?}: expected the model to carry {property:?} = {segment:?}, \
                 got {properties:?}"
            );
        }
        let listed = emulator.walk(type_name, None).await;
        assert!(
            listed.contains(expected),
            "case {case:?}: expected the listing to report {expected:?}, got {listed:?}"
        );
    }

    // A stream Cloud Control API created EXISTS in the owning service, under the group the
    // first segment of its identifier names.
    let described = emulator
        .logs_ok("DescribeLogStreams", json!({ "logGroupName": "app-a" }))
        .await;
    let held: Vec<String> = described["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|stream| text(stream, "logStreamName"))
        .collect();
    assert_eq!(
        held,
        vec!["host-1".to_string(), "host-2".to_string()],
        "CloudWatch Logs must hold the streams Cloud Control API created, got {held:?}"
    );
}

#[tokio::test]
async fn a_listing_scoped_by_one_segment_reports_only_that_segments_resources() {
    let emulator = Emulator::with_cloudwatch_logs();
    provision_groups(&emulator, &["scoped-a", "scoped-b"]).await;

    for (group, stream) in [
        ("scoped-a", "one"),
        ("scoped-a", "two"),
        ("scoped-b", "three"),
    ] {
        emulator
            .provision(
                LOG_STREAM,
                &json!({ "LogGroupName": group, "LogStreamName": stream }),
            )
            .await;
    }

    // "The resource model to use to select the resources to return": a partial model, so a
    // listing scoped by ONE segment of the compound identifier reports the resources whose
    // own model carries it — each under the whole delimited identifier.
    for (case, group, expected) in [
        (
            "the group holding two streams",
            "scoped-a",
            vec![
                format!("scoped-a{DELIMITER}one"),
                format!("scoped-a{DELIMITER}two"),
            ],
        ),
        (
            "the group holding one",
            "scoped-b",
            vec![format!("scoped-b{DELIMITER}three")],
        ),
    ] {
        let listed = emulator
            .cc_ok(
                "ListResources",
                Emulator::list_request(
                    LOG_STREAM,
                    None,
                    None,
                    Some(&json!({ "LogGroupName": group })),
                ),
            )
            .await;
        let mut reported: Vec<String> = descriptions(&listed)
            .iter()
            .map(|description| text(description, "Identifier"))
            .collect();
        reported.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the listing to report {expected:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_compound_identifier_cannot_be_half_named_or_patched() {
    let emulator = Emulator::with_cloudwatch_logs();
    provision_groups(&emulator, &["patched"]).await;
    let identifier = emulator
        .provision(
            LOG_STREAM,
            &json!({ "LogGroupName": "patched", "LogStreamName": "host" }),
        )
        .await;

    // A desired state that names only part of the combination identifies no resource, so it
    // is refused by the front door naming the part that is missing — as the model validation
    // failure it is where the schema declares that part `required`, and as the identifier it
    // cannot form where the schema leaves it to the generated-name rule.
    for (case, desired, expected_code, offending) in [
        (
            "the first segment missing",
            json!({ "LogStreamName": "orphan" }),
            "ValidationException",
            "LogGroupName",
        ),
        (
            "the second segment missing",
            json!({ "LogGroupName": "patched" }),
            "InvalidRequestException",
            "LogStreamName",
        ),
    ] {
        let (status, body) = emulator.create(LOG_STREAM, &desired, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert_eq!(
            code(&body),
            expected_code,
            "case {case:?}: unexpected error code in {body:?}"
        );
        assert!(
            message(&body).contains(offending),
            "case {case:?}: expected the refusal to name {offending:?}, got {:?}",
            message(&body)
        );
    }

    // Every property the identifier is made of addresses the resource, so a patch of ANY of
    // them is refused as the create-only property it is, naming the one it touched.
    for (case, path, pointer) in [
        ("the first segment", "/LogGroupName", "/properties/LogGroupName"),
        (
            "the second segment",
            "/LogStreamName",
            "/properties/LogStreamName",
        ),
    ] {
        let patch = json!([{ "op": "replace", "path": path, "value": "moved" }]);
        let (status, body) = emulator
            .update(LOG_STREAM, &identifier, &patch, None)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert_eq!(
            code(&body),
            "NotUpdatableException",
            "case {case:?}: unexpected error code in {body:?}"
        );
        assert_eq!(
            message(&body),
            format!("Invalid patch update: createOnlyProperties [{pointer}] cannot be updated"),
            "case {case:?}: unexpected wording"
        );
    }

    // And the delete takes the compound identifier back: the stream leaves the owning
    // service's store with it.
    emulator.delete_and_settle(LOG_STREAM, &identifier).await;
    let described = emulator
        .logs_ok("DescribeLogStreams", json!({ "logGroupName": "patched" }))
        .await;
    let held = described["logStreams"].as_array().cloned().unwrap_or_default();
    assert!(
        held.is_empty(),
        "CloudWatch Logs must no longer hold a stream Cloud Control API deleted, got \
         {held:?}"
    );
    let (status, body) = emulator.get(LOG_STREAM, &identifier).await;
    assert_eq!(
        status, 400,
        "the deleted stream must resolve to nothing, got {status} with body {body:?}"
    );
}

#[tokio::test]
async fn a_resource_is_addressable_by_a_json_identifier_of_the_schemas_properties() {
    let emulator = Emulator::with_cloudwatch_logs();
    provision_groups(&emulator, &["json-group"]).await;
    let stream = emulator
        .provision(
            LOG_STREAM,
            &json!({ "LogGroupName": "json-group", "LogStreamName": "host" }),
        )
        .await;

    // "Primary identifiers can be specified as a string or JSON": a client that read the
    // type's schema names the identifier's properties, keyed by the property name or by the
    // pointer the schema spells it with, and every form resolves the same resource.
    for (case, type_name, reference, expected) in [
        (
            "one property, by name",
            LOG_GROUP,
            json!({ "LogGroupName": "json-group" }).to_string(),
            "json-group".to_string(),
        ),
        (
            "one property, by the schema's pointer",
            LOG_GROUP,
            json!({ "/properties/LogGroupName": "json-group" }).to_string(),
            "json-group".to_string(),
        ),
        (
            "both properties of a compound identifier",
            LOG_STREAM,
            json!({ "LogGroupName": "json-group", "LogStreamName": "host" }).to_string(),
            stream.clone(),
        ),
        (
            "the same two in the other order",
            LOG_STREAM,
            json!({ "LogStreamName": "host", "LogGroupName": "json-group" }).to_string(),
            stream.clone(),
        ),
    ] {
        let fetched = emulator
            .cc_ok(
                "GetResource",
                json!({ "TypeName": type_name, "Identifier": reference }),
            )
            .await;
        assert_eq!(
            text(&fetched["ResourceDescription"], "Identifier"),
            expected,
            "case {case:?}: expected the read to resolve {expected:?}, got {fetched:?}"
        );
    }

    // A reference that is neither form identifies no resource of the type, so it is refused
    // by this service's own request validation rather than looked up and reported missing.
    for (case, reference) in [
        ("one segment where the schema declares two", "json-group"),
        ("more segments than the schema declares", "json-group|host|extra"),
    ] {
        let (status, body) = emulator.get(LOG_STREAM, reference).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert_eq!(
            code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body:?}"
        );
        assert_eq!(
            message(&body),
            format!(
                "Identifier {reference} is not valid for identifier \
                 [/properties/LogGroupName, /properties/LogStreamName]"
            ),
            "case {case:?}: unexpected wording"
        );
    }

    // A JSON object that does not name the type's identifier — a part of it missing, a
    // property that identifies nothing, or one property too many — is refused as the
    // identifier it is not.
    for (case, type_name, reference, declared) in [
        (
            "a part of a compound identifier missing",
            LOG_STREAM,
            json!({ "LogGroupName": "json-group" }).to_string(),
            "/properties/LogGroupName, /properties/LogStreamName",
        ),
        (
            "a property that identifies no resource of the type",
            LOG_GROUP,
            json!({ "RetentionInDays": 7 }).to_string(),
            "/properties/LogGroupName",
        ),
        (
            "the identifier and one property beside it",
            LOG_GROUP,
            json!({ "LogGroupName": "json-group", "RetentionInDays": 7 }).to_string(),
            "/properties/LogGroupName",
        ),
    ] {
        let (status, body) = emulator.get(type_name, &reference).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert_eq!(
            code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body:?}"
        );
        assert_eq!(
            message(&body),
            format!(
                "When specifying the identifier as JSON, you must specify exactly one \
                 identifier from [[{declared}]], and no additional properties can be \
                 specified"
            ),
            "case {case:?}: unexpected wording"
        );
    }

    // The other operations take the JSON form too: a patch applied through it reaches the
    // resource, and a delete through it removes the one it names.
    let patch = json!([{ "op": "add", "path": "/RetentionInDays", "value": 14 }]);
    emulator
        .update_and_settle(
            LOG_GROUP,
            &json!({ "LogGroupName": "json-group" }).to_string(),
            &patch,
        )
        .await;
    let properties = emulator.properties(LOG_GROUP, "json-group").await;
    assert_eq!(
        properties["RetentionInDays"].as_i64(),
        Some(14),
        "expected the patch addressed by a JSON identifier to apply, got {properties:?}"
    );
    emulator
        .delete_and_settle(
            LOG_STREAM,
            &json!({ "LogGroupName": "json-group", "LogStreamName": "host" }).to_string(),
        )
        .await;
    let (status, body) = emulator.get(LOG_STREAM, &stream).await;
    assert_eq!(
        status, 400,
        "the deleted stream must resolve to nothing, got {status} with body {body:?}"
    );
}

#[tokio::test]
async fn a_stream_under_a_group_that_is_not_there_fails_the_request() {
    let emulator = Emulator::with_cloudwatch_logs();

    // The owning service holds no such group, so its handler finds no resource to create
    // the stream under: the request reaches a terminal FAILED carrying that verdict rather
    // than a success nothing performed.
    let accepted = emulator
        .create_ok(
            LOG_STREAM,
            &json!({ "LogGroupName": "absent", "LogStreamName": "host" }),
            None,
        )
        .await;
    let failed = emulator
        .terminal(&text(&accepted, "RequestToken"))
        .await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "expected the request to fail, got {failed:?}"
    );
    assert_eq!(
        text(&failed, "ErrorCode"),
        "NotFound",
        "unexpected handler error code in {failed:?}"
    );
    let (status, body) = emulator
        .get(LOG_STREAM, &format!("absent{DELIMITER}host"))
        .await;
    assert_eq!(
        status, 400,
        "nothing must resolve under the identifier of a stream that was not created, got \
         {status} with body {body:?}"
    );
}
