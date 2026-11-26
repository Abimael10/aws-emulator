// SPDX-License-Identifier: CC0-1.0

//! The sentence `CreateDeliveryStream` and `UpdateDestination` refuse a request that does
//! not name exactly ONE destination with, and the one they refuse a destination named in
//! both of its spellings with.
//!
//! The rules under test:
//!
//! 1. "A Firehose stream is configured with a single destination" and "You must specify
//!    only one of the following destination configuration parameters", so a request that
//!    carries no destination configuration at all, and one that carries destinations of
//!    several types, are the same arity fault — refused with HTTP 400
//!    `InvalidArgumentException` and the one fixed sentence stating that rule. The sentence
//!    interpolates nothing — it names no member, because no one member is at fault, and it
//!    is the same whatever else the request carried.
//! 2. The model gives one Amazon S3 destination TWO members, the one it marks
//!    `@deprecated` and the one that replaced it, so a request supplying both says one
//!    destination twice; that is refused with the same code and a fixed sentence naming
//!    those two members, and it is judged before the count of destinations because it is
//!    the more specific exclusion. The member names come from the model and nothing else
//!    in the sentence varies.
//!
//! Either way the refused request changes nothing: a refused create stores no stream and a
//! refused update leaves the destination and the version the stream had.
//!
//! The rows span what the rules must NOT depend on: the stream name, the declared
//! `DeliveryStreamType`, the source configuration that type is fed by, the tags, a
//! server-side-encryption input, which destination members are named and how many of them,
//! and whether two spellings of one destination describe the same bucket. A create carrying
//! exactly one destination is accepted beside them, so what the rows isolate is the
//! destination's arity and not the other members.
//!
//! The harness (`Emulator`, `error_code`, `error_message`, `s3_configuration`) is the
//! lifecycle suite's.

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::{
    error_code, error_message, s3_configuration, Emulator, BUCKET_ARN, ROLE_ARN,
};

/// Firehose's sentence for the destination-arity rule, as the recorded refusal words it:
/// fixed text with nothing taken from the request or from state.
///
/// `pub(crate)` so the suite that owns this service's refusal wording asserts the same
/// sentence rather than carrying a second copy of it.
pub(crate) const ARITY_REFUSAL: &str =
    "Exactly one destination configuration is supported for a Firehose";

#[tokio::test]
async fn a_create_naming_no_destination_is_refused_with_the_arity_rule() {
    let emulator = Emulator::new();

    let rows: Vec<(&str, Value)> = vec![
        // The row the report cites: nothing but the name.
        (
            "nothing but the stream name",
            json!({ "DeliveryStreamName": "arity-name-only" }),
        ),
        (
            "a name at the shortest the model allows",
            json!({ "DeliveryStreamName": "a" }),
        ),
        (
            "a name at the longest the model allows",
            json!({ "DeliveryStreamName": "z".repeat(64) }),
        ),
        (
            "the default stream type stated explicitly",
            json!({
                "DeliveryStreamName": "arity-direct-put",
                "DeliveryStreamType": "DirectPut",
            }),
        ),
        (
            "a stream type fed by a source configuration, with that configuration supplied",
            json!({
                "DeliveryStreamName": "arity-kinesis-source",
                "DeliveryStreamType": "KinesisStreamAsSource",
                "KinesisStreamSourceConfiguration": {
                    "KinesisStreamARN":
                        "arn:aws:kinesis:us-west-2:000000000000:stream/arity-source",
                    "RoleARN": ROLE_ARN,
                },
            }),
        ),
        (
            "tags but no destination",
            json!({
                "DeliveryStreamName": "arity-tagged",
                "Tags": [{ "Key": "team", "Value": "data" }],
            }),
        ),
        (
            "server-side encryption but no destination",
            json!({
                "DeliveryStreamName": "arity-encrypted",
                "DeliveryStreamEncryptionConfigurationInput": { "KeyType": "AWS_OWNED_CMK" },
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, refused) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "InvalidArgumentException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, ARITY_REFUSAL,
            "case {case:?}: expected {ARITY_REFUSAL:?}, got {message:?}"
        );
    }

    // Nothing was stored for any refused create.
    let (status, listed) = emulator.call("ListDeliveryStreams", json!({})).await;
    assert_eq!(status, 200, "list: got {listed:?}");
    assert_eq!(
        listed["DeliveryStreamNames"],
        json!([]),
        "a refused create stores nothing, got {listed:?}"
    );

    // The same requests with ONE destination added are accepted: what the sentence above
    // is about is the destination's arity and not the members beside it.
    for (case, member) in [
        ("the deprecated spelling", "S3DestinationConfiguration"),
        ("the extended spelling", "ExtendedS3DestinationConfiguration"),
    ] {
        let mut request = json!({
            "DeliveryStreamName": format!("arity-accepted-{member}"),
            "Tags": [{ "Key": "team", "Value": "data" }],
        });
        request[member] = s3_configuration();
        let (status, created) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(status, 200, "case {case:?}: got {created:?}");
    }
}

/// An `ExtendedS3DestinationConfiguration` with its two required members (`RoleARN`,
/// `BucketARN`).
fn extended_s3_configuration() -> Value {
    json!({ "RoleARN": ROLE_ARN, "BucketARN": BUCKET_ARN })
}

/// An `HttpEndpointDestinationConfiguration` with its required `EndpointConfiguration`
/// (whose `Url` the model requires) and `S3Configuration`.
fn http_endpoint_configuration() -> Value {
    json!({
        "EndpointConfiguration": {
            "Url": "https://example.com/firehose",
            "Name": "an-endpoint",
        },
        "S3Configuration": s3_configuration(),
        "RoleARN": ROLE_ARN,
    })
}

/// A `SplunkDestinationConfiguration` with its three required members (`HECEndpoint`,
/// `HECEndpointType`, `S3Configuration`).
fn splunk_configuration() -> Value {
    json!({
        "HECEndpoint": "https://splunk.example.com:8088",
        "HECEndpointType": "Raw",
        "S3Configuration": s3_configuration(),
    })
}

#[tokio::test]
async fn a_create_naming_several_destinations_is_refused_with_the_same_arity_rule() {
    let emulator = Emulator::new();

    // The rows differ in WHICH destination members are named and HOW MANY: the sentence is
    // the one arity rule either way, so none of that reaches the message.
    let rows: Vec<(&str, Value)> = vec![
        (
            "an extended S3 destination beside an HTTP endpoint one",
            json!({
                "DeliveryStreamName": "arity-extended-and-http",
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(),
            }),
        ),
        (
            "the deprecated S3 member beside an HTTP endpoint one",
            json!({
                "DeliveryStreamName": "arity-s3-and-http",
                "S3DestinationConfiguration": s3_configuration(),
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(),
            }),
        ),
        (
            "three destinations at once, with tags beside them",
            json!({
                "DeliveryStreamName": "arity-three",
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(),
                "SplunkDestinationConfiguration": splunk_configuration(),
                "S3DestinationConfiguration": s3_configuration(),
                "Tags": [{ "Key": "team", "Value": "data" }],
            }),
        ),
        (
            "two destinations on a stream fed by a source",
            json!({
                "DeliveryStreamName": "arity-source-fed-two",
                "DeliveryStreamType": "KinesisStreamAsSource",
                "KinesisStreamSourceConfiguration": {
                    "KinesisStreamARN":
                        "arn:aws:kinesis:us-west-2:000000000000:stream/arity-source",
                    "RoleARN": ROLE_ARN,
                },
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(),
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, refused) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "InvalidArgumentException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, ARITY_REFUSAL,
            "case {case:?}: expected {ARITY_REFUSAL:?}, got {message:?}"
        );
    }

    // Nothing was stored for any refused create.
    let (status, listed) = emulator.call("ListDeliveryStreams", json!({})).await;
    assert_eq!(status, 200, "list: got {listed:?}");
    assert_eq!(
        listed["DeliveryStreamNames"],
        json!([]),
        "a refused create stores nothing, got {listed:?}"
    );
}

#[tokio::test]
async fn an_update_that_does_not_name_exactly_one_destination_is_refused_with_the_arity_rule() {
    // The arity rule is the STREAM's, not one operation's: an update that names no
    // destination update, or several, breaks it exactly as a create that names none or
    // several destinations does, and is refused with the same sentence.
    let emulator = Emulator::new();
    emulator.create_stream("arity-update-stream", None).await;
    emulator.await_active("arity-update-stream").await;

    let described = describe(&emulator, "arity-update-stream").await;
    let version = described["VersionId"].as_str().unwrap_or_default().to_string();
    let destination_id = described["Destinations"][0]["DestinationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !version.is_empty() && !destination_id.is_empty(),
        "expected a version and a destination id, got {described:?}"
    );

    // (case, the update members the request carries)
    let rows: Vec<(&str, Value)> = vec![
        (
            "no destination update at all",
            json!({}),
        ),
        (
            "the stored type's update beside another type's",
            json!({
                "S3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "HttpEndpointDestinationUpdate": {
                    "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                },
            }),
        ),
        (
            "two updates of types the stream does not have",
            json!({
                "ExtendedS3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "SplunkDestinationUpdate": { "HECEndpointType": "Raw" },
            }),
        ),
        (
            "three updates at once",
            json!({
                "S3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "SplunkDestinationUpdate": { "HECEndpointType": "Raw" },
                "HttpEndpointDestinationUpdate": {
                    "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                },
            }),
        ),
    ];

    for (case, updates) in rows {
        let mut request = json!({
            "DeliveryStreamName": "arity-update-stream",
            "CurrentDeliveryStreamVersionId": version,
            "DestinationId": destination_id,
        });
        for (member, update) in updates.as_object().expect("a table of update members") {
            request[member] = update.clone();
        }
        let (status, refused) = emulator.call("UpdateDestination", request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "InvalidArgumentException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, ARITY_REFUSAL,
            "case {case:?}: expected {ARITY_REFUSAL:?}, got {message:?}"
        );
    }

    // The stream kept the destination and the version it had: a refused update applies
    // none of what it carried.
    let after = describe(&emulator, "arity-update-stream").await;
    assert_eq!(
        after["VersionId"], described["VersionId"],
        "a refused update leaves the version alone, got {after:?}"
    );
    assert_eq!(
        after["Destinations"], described["Destinations"],
        "a refused update leaves the destination alone, got {after:?}"
    );
}

/// The `DeliveryStreamDescription` of `stream`.
async fn describe(emulator: &Emulator, stream: &str) -> Value {
    let (status, described) = emulator
        .call(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    assert_eq!(status, 200, "describe {stream}: got {described:?}");
    described["DeliveryStreamDescription"].clone()
}

/// Firehose's sentence for a destination supplied in BOTH of its spellings: fixed text
/// naming the two members that exclude each other — the one the model marks `@deprecated`
/// and the one that replaced it — with nothing taken from the request.
///
/// `pub(crate)` so the suite that owns this service's refusal wording asserts the same
/// sentence rather than carrying a second copy of it.
pub(crate) fn exclusive_members_refusal(deprecated: &str, replacement: &str) -> String {
    format!("Exactly one of {deprecated} or {replacement} must be specified.")
}

#[tokio::test]
async fn a_create_supplying_both_spellings_of_one_s3_destination_names_the_two_members() {
    // The deprecated `S3DestinationConfiguration` and the `ExtendedS3DestinationConfiguration`
    // that replaced it configure the SAME destination
    // (`smithy_query(query="operation",operation="CreateDeliveryStream")` marks the first
    // `@deprecated`), so a request carrying both is refused as that either/or rather than
    // as the general arity rule: the refusal names the two members a caller has to drop
    // one of.
    let emulator = Emulator::new();
    let expected = exclusive_members_refusal(
        "S3DestinationConfiguration",
        "ExtendedS3DestinationConfiguration",
    );

    // The rows differ in what stands BESIDE the conflicting pair — nothing, tags, a
    // declared type with its source configuration, a third destination of another type —
    // and in whether the two spellings describe the same bucket, because none of that
    // reaches the sentence.
    let rows: Vec<(&str, Value)> = vec![
        (
            "the pair and nothing else",
            json!({
                "DeliveryStreamName": "pair-only",
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
            }),
        ),
        (
            "the pair with tags beside it",
            json!({
                "DeliveryStreamName": "pair-tagged",
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
                "Tags": [{ "Key": "team", "Value": "data" }],
            }),
        ),
        (
            "the pair naming two different buckets",
            json!({
                "DeliveryStreamName": "pair-two-buckets",
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": json!({
                    "RoleARN": ROLE_ARN,
                    "BucketARN": "arn:aws:s3:::another-bucket",
                }),
            }),
        ),
        (
            "the pair on a stream fed by a source",
            json!({
                "DeliveryStreamName": "pair-source-fed",
                "DeliveryStreamType": "KinesisStreamAsSource",
                "KinesisStreamSourceConfiguration": {
                    "KinesisStreamARN":
                        "arn:aws:kinesis:us-west-2:000000000000:stream/arity-source",
                    "RoleARN": ROLE_ARN,
                },
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
            }),
        ),
        (
            "the pair beside a destination of another type",
            json!({
                "DeliveryStreamName": "pair-and-http",
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(),
            }),
        ),
    ];

    let mut refused_names: Vec<String> = Vec::new();
    for (case, request) in rows {
        refused_names.push(
            request["DeliveryStreamName"]
                .as_str()
                .expect("every row names a stream")
                .to_string(),
        );
        let (status, refused) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "InvalidArgumentException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // No refused create left a stream behind: each name resolves to nothing and none of
    // them is listed.
    for name in &refused_names {
        let (status, described) = emulator
            .call(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": name }),
            )
            .await;
        assert_eq!(status, 400, "describe {name}: got {described:?}");
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "describe {name}: got {described:?}"
        );
    }
    let (status, listed) = emulator.call("ListDeliveryStreams", json!({})).await;
    assert_eq!(status, 200, "list: got {listed:?}");
    assert_eq!(
        listed["DeliveryStreamNames"],
        json!([]),
        "a refused create stores nothing, got {listed:?}"
    );

    // Either spelling ON ITS OWN is accepted and the stream it makes settles: what is
    // refused above is naming both of them, not either member.
    for (case, member, configuration) in [
        (
            "the deprecated spelling alone",
            "S3DestinationConfiguration",
            s3_configuration(),
        ),
        (
            "the extended spelling alone",
            "ExtendedS3DestinationConfiguration",
            extended_s3_configuration(),
        ),
    ] {
        let name = format!("pair-resolved-{member}");
        let mut request = json!({ "DeliveryStreamName": name });
        request[member] = configuration;
        let (status, created) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(status, 200, "case {case:?}: got {created:?}");
        emulator.await_active(&name).await;
    }
}

#[tokio::test]
async fn an_update_supplying_both_spellings_of_one_s3_destination_names_the_two_members() {
    // The same pair of spellings on the update path
    // (`smithy_query(query="operation",operation="UpdateDestination")` marks
    // `S3DestinationUpdate` `@deprecated`), refused the same way and naming the update
    // members: the rule is the pair, not one operation.
    let emulator = Emulator::new();
    emulator.create_stream("pair-update-stream", None).await;
    emulator.await_active("pair-update-stream").await;

    let described = describe(&emulator, "pair-update-stream").await;
    let version = described["VersionId"].as_str().unwrap_or_default().to_string();
    let destination_id = described["Destinations"][0]["DestinationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !version.is_empty() && !destination_id.is_empty(),
        "expected a version and a destination id, got {described:?}"
    );

    let expected = exclusive_members_refusal("S3DestinationUpdate", "ExtendedS3DestinationUpdate");
    let rows: Vec<(&str, Value)> = vec![
        (
            "the pair and nothing else",
            json!({
                "S3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "ExtendedS3DestinationUpdate": { "BucketARN": BUCKET_ARN },
            }),
        ),
        (
            "the pair naming two different buckets",
            json!({
                "S3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "ExtendedS3DestinationUpdate": { "BucketARN": "arn:aws:s3:::another-bucket" },
            }),
        ),
        (
            "the pair beside an update of another type",
            json!({
                "S3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "ExtendedS3DestinationUpdate": { "BucketARN": BUCKET_ARN },
                "SplunkDestinationUpdate": { "HECEndpointType": "Raw" },
            }),
        ),
    ];

    for (case, updates) in rows {
        let mut request = json!({
            "DeliveryStreamName": "pair-update-stream",
            "CurrentDeliveryStreamVersionId": version,
            "DestinationId": destination_id,
        });
        for (member, update) in updates.as_object().expect("a table of update members") {
            request[member] = update.clone();
        }
        let (status, refused) = emulator.call("UpdateDestination", request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "InvalidArgumentException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The stream kept the destination and the version it had.
    let after = describe(&emulator, "pair-update-stream").await;
    assert_eq!(
        after["VersionId"], described["VersionId"],
        "a refused update leaves the version alone, got {after:?}"
    );
    assert_eq!(
        after["Destinations"], described["Destinations"],
        "a refused update leaves the destination alone, got {after:?}"
    );
}
