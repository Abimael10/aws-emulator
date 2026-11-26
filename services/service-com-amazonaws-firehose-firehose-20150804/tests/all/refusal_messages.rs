// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a refusal, over the wire.
//!
//! Three rules are asserted here, each over a set of inputs rather than one recorded
//! request:
//!
//! 1. a constraint violation states the member path and the constraint and NEVER the
//!    value that violated it;
//! 2. a `@pattern` is reported as the expression a value must satisfy — the model's
//!    anchoring normalised away, and restated once around the group for a top-level
//!    alternation;
//! 3. a refusal the handlers raise themselves is scoped to the ACCOUNT: a duplicate
//!    name states name and account, a lookup that found nothing states the account and
//!    not the name it was given, a delete the state blocks states name, account and
//!    state, and a create that does not name exactly one destination configuration is
//!    refused with the one fixed sentence stating that arity rule;
//! 4. a put to a stream that is not accepting data states the stream the request named
//!    and the account the put was scoped to, and not the state the stream is in;
//! 5. a request this endpoint names no operation for is refused by its own JSON-RPC
//!    surface rather than by an operation: a target that names THIS API and an operation it
//!    does not publish is `400 UnknownOperationException` with no prose, and a request that
//!    names no API of this endpoint at all — an unresolvable target, or a media type this
//!    endpoint reads no document in — identifies nothing to answer for and is answered the
//!    request framework's `200 application/json` unknown-operation envelope;
//! 6. a request body the JSON wire could not read is a serialization fault and not a
//!    constraint violation: `400 SerializationException`, carrying only the sentence that
//!    framework states for the token the body opened with, under the capitalized `Message`.
//!
//! Every request goes through the real router, so what is asserted is the body a client
//! reads. The harness (`Emulator`, `s3_configuration`, …) is the lifecycle suite's.

use serde_json::{json, Value};

use super::delivery_stream_lifecycle::{
    error_code, s3_configuration, Emulator, BUCKET_ARN, PAST_THE_TRANSITION, ROLE_ARN,
};
// `error_message` lives beside `error_code` in the lifecycle suite, so every suite in this
// test crate reads a refusal the same way rather than carrying its own copy. It is
// re-exported here because a sibling suite imports it from this module.
pub(crate) use super::delivery_stream_lifecycle::error_message;
// The arity sentence is the `destination_arity_refusal` suite's: one wording for one rule,
// asserted there on the rule itself and here on the several-destination fault.
use super::destination_arity_refusal::{exclusive_members_refusal, ARITY_REFUSAL};

/// The envelope one constraint violation is reported in.
fn one_error(clause: String) -> String {
    format!("1 validation error detected: {clause}")
}

/// The clause for a value that was supplied and rejected: the member and the
/// constraint, and nothing of the value.
///
/// `pub(crate)` so a sibling suite asserting a constraint refusal words it the same way
/// rather than carrying a second copy of the sentence.
pub(crate) fn value_at(member: &str, requirement: &str) -> String {
    one_error(format!(
        "Value at '{member}' failed to satisfy constraint: {requirement}"
    ))
}

/// The clause for a required member that was absent: there was no value at all, so there is
/// no value token in the clause either — the member and the requirement are all of it.
///
/// `pub(crate)` beside [`value_at`], for the same reason.
pub(crate) fn absent_at(member: &str) -> String {
    one_error(format!(
        "Value at '{member}' failed to satisfy constraint: Member must not be null"
    ))
}

/// The requirement clause for a `@pattern`, with the pattern as it is REPORTED.
///
/// `pub(crate)` so a sibling suite asserting a `@pattern` refusal states the clause the
/// same way rather than re-spelling it.
pub(crate) fn must_match(reported_pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {reported_pattern}")
}

/// An `ExtendedS3DestinationConfiguration` with its two required members
/// (`RoleARN`, `BucketARN`) and whatever else a case overrides.
fn extended_s3_configuration() -> Value {
    json!({ "RoleARN": ROLE_ARN, "BucketARN": BUCKET_ARN })
}

/// A `SplunkDestinationConfiguration` with its required members: `HECEndpoint`,
/// `HECEndpointType` ("This type can be either \"Raw\" or \"Event.\"") and
/// `S3Configuration`.
fn splunk_configuration() -> Value {
    json!({
        "HECEndpoint": "https://splunk.example.com:8088",
        "HECEndpointType": "Raw",
        "S3Configuration": s3_configuration(),
    })
}

/// An `HttpEndpointDestinationConfiguration` whose `EndpointConfiguration` is `endpoint`,
/// with the `S3Configuration` the model requires beside it.
fn http_endpoint_configuration(endpoint: Value) -> Value {
    json!({
        "EndpointConfiguration": endpoint,
        "S3Configuration": s3_configuration(),
    })
}

/// A record whose `Data` holds `bytes` bytes.
fn record_of(bytes: usize) -> Value {
    json!({ "Data": aws_smithy_types::base64::encode(vec![b'x'; bytes]) })
}

/// The modelled maximum of `Data` (`@length max 1024000`).
const MAX_RECORD_BYTES: usize = 1_024_000;

#[tokio::test]
async fn a_constraint_violation_names_the_member_and_the_constraint_but_not_the_value() {
    let emulator = Emulator::new();

    // One row per constrained shape kind the model uses — string length, list length,
    // blob length, integer range on two different members, a nested member two and
    // three levels down, a list element, and an absent required member. Every bound
    // comes from the model
    // (`smithy_query(query="shape", shape="DeliveryStreamName,Data,SizeInMBs,...")`).
    //
    // (case, operation, request, expected message)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a name one character over its 64-character maximum",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "a".repeat(65),
                "S3DestinationConfiguration": s3_configuration(),
            }),
            value_at(
                "deliveryStreamName",
                "Member must have length less than or equal to 64",
            ),
        ),
        (
            "an empty record list",
            "PutRecordBatch",
            json!({ "DeliveryStreamName": "any-stream", "Records": [] }),
            value_at(
                "records",
                "Member must have length greater than or equal to 1",
            ),
        ),
        (
            "one record over the 500-record maximum",
            "PutRecordBatch",
            json!({
                "DeliveryStreamName": "any-stream",
                "Records": vec![record_of(1); 501],
            }),
            value_at(
                "records",
                "Member must have length less than or equal to 500",
            ),
        ),
        (
            "a record one byte over its maximum",
            "PutRecord",
            json!({
                "DeliveryStreamName": "any-stream",
                "Record": record_of(MAX_RECORD_BYTES + 1),
            }),
            value_at(
                "record.data",
                "Member must have length less than or equal to 1024000",
            ),
        ),
        (
            "the second record of a batch one byte over its maximum",
            "PutRecordBatch",
            json!({
                "DeliveryStreamName": "any-stream",
                "Records": [record_of(1), record_of(MAX_RECORD_BYTES + 1)],
            }),
            value_at(
                "records.2.member.data",
                "Member must have length less than or equal to 1024000",
            ),
        ),
        (
            "a tag value one character over its 256-character maximum",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "tagged-stream",
                "S3DestinationConfiguration": s3_configuration(),
                "Tags": [{ "Key": "owner", "Value": "v".repeat(257) }],
            }),
            value_at(
                "tags.1.member.value",
                "Member must have length less than or equal to 256",
            ),
        ),
        (
            "a buffering size above its maximum",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "buffered-stream",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": BUCKET_ARN,
                    "BufferingHints": { "SizeInMBs": 129, "IntervalInSeconds": 60 },
                },
            }),
            value_at(
                "extendedS3DestinationConfiguration.bufferingHints.sizeInMBs",
                "Member must have value less than or equal to 128",
            ),
        ),
        (
            "a splunk retry duration below its minimum",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "splunk-stream",
                "SplunkDestinationConfiguration": {
                    "HECEndpoint": "https://splunk.example.com:8088",
                    "HECEndpointType": "Raw",
                    "S3Configuration": s3_configuration(),
                    "RetryOptions": { "DurationInSeconds": -1 },
                },
            }),
            value_at(
                "splunkDestinationConfiguration.retryOptions.durationInSeconds",
                "Member must have value greater than or equal to 0",
            ),
        ),
        (
            "an absent required member",
            "CreateDeliveryStream",
            json!({ "S3DestinationConfiguration": s3_configuration() }),
            absent_at("deliveryStreamName"),
        ),
        (
            "an absent required member two levels down",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "iceberg-stream",
                "IcebergDestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "S3Configuration": s3_configuration(),
                },
            }),
            absent_at("icebergDestinationConfiguration.catalogConfiguration"),
        ),
        (
            "an absent required member three levels down",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "splunk-stream",
                "SplunkDestinationConfiguration": {
                    "HECEndpoint": "https://splunk.example.com:8088",
                    "HECEndpointType": "Raw",
                    "S3Configuration": { "RoleARN": ROLE_ARN },
                },
            }),
            absent_at("splunkDestinationConfiguration.s3Configuration.bucketARN"),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, refused) = emulator.call(operation, request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // The value-free form has no `Value '…'` subject and no `Value null` either, so
        // nothing the caller sent — a record payload, a tag value — can reach the body
        // through it, and an absent member is reported with no value token at all.
        assert!(
            !message.contains("Value '") && !message.contains("Value null"),
            "case {case:?}: expected no rejected value in the message, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_pattern_is_reported_without_the_models_anchoring() {
    let emulator = Emulator::new();

    // (case, operation, request, member, model pattern, reported pattern)
    //
    // The model patterns are `smithy_query(query="shape",
    // shape="DeliveryStreamName,FileExtension,CustomTimeZone,HttpEndpointUrl,
    // AmazonOpenSearchServerlessCollectionEndpoint,HttpEndpointName,
    // HttpEndpointAttributeName")`. A single expression anchored at both ends is reported
    // bare; a top-level alternation keeps one pair of anchors around the group, because
    // dropping them branch by branch would widen the language the clause describes; an
    // expression anchored at ONE end has the run it leaves free spelled out, because the
    // clause describes a whole value; and one carrying a look-ahead keeps the anchoring
    // that positions it, less an end anchor a trailing run of any character already
    // reaches.
    let rows: Vec<(&str, &str, Value, &str, &str, &str)> = vec![
        (
            "a stream name with an illegal character, on the control plane",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "bad name!",
                "S3DestinationConfiguration": s3_configuration(),
            }),
            "deliveryStreamName",
            "^[a-zA-Z0-9_.-]+$",
            "[a-zA-Z0-9_.-]+",
        ),
        (
            "the same name rule on the data plane",
            "PutRecord",
            json!({
                "DeliveryStreamName": "bad/name",
                "Record": record_of(1),
            }),
            "deliveryStreamName",
            "^[a-zA-Z0-9_.-]+$",
            "[a-zA-Z0-9_.-]+",
        ),
        (
            "a file extension with no leading dot (an alternation)",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "extension-stream",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": BUCKET_ARN,
                    "FileExtension": "nodot",
                },
            }),
            "extendedS3DestinationConfiguration.fileExtension",
            "^$|\\.[0-9a-z!\\-_.*'()]+$",
            "^(|\\.[0-9a-z!\\-_.*'()]+)$",
        ),
        (
            // Not a member the issue cited: the same alternation shape, so the same
            // rendering rule has to hold for it.
            "a custom time zone with an illegal character (an alternation)",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "timezone-stream",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": BUCKET_ARN,
                    "CustomTimeZone": "Berlin+1",
                },
            }),
            "extendedS3DestinationConfiguration.customTimeZone",
            "^$|[a-zA-Z/_]+$",
            "^(|[a-zA-Z/_]+)$",
        ),
        (
            // Anchored at the START only: the constraint admits any continuation, so the
            // clause spells that run out rather than describing the prefix alone.
            "an HTTP endpoint URL that is not https",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "http-endpoint-stream",
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(json!({
                    "Url": "http://example.com/firehose",
                    "Name": "an-endpoint",
                })),
            }),
            "httpEndpointDestinationConfiguration.endpointConfiguration.url",
            "^https://",
            "https://.*",
        ),
        (
            // Not a member the issue cited: the same start-anchored shape on another
            // destination's endpoint, so the same rendering rule has to hold for it.
            "an OpenSearch Serverless collection endpoint that is not https",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "collection-stream",
                "AmazonOpenSearchServerlessDestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "CollectionEndpoint": "http://collection.example.com",
                    "IndexName": "logs",
                    "S3Configuration": s3_configuration(),
                },
            }),
            "amazonOpenSearchServerlessDestinationConfiguration.collectionEndpoint",
            "^https:",
            "https:.*",
        ),
        (
            // A look-ahead: its anchoring stays, less the end anchor the trailing run of
            // any character already reaches.
            "a whitespace-only HTTP endpoint name",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "blank-name-stream",
                "HttpEndpointDestinationConfiguration": http_endpoint_configuration(json!({
                    "Url": "https://example.com/firehose",
                    "Name": "   ",
                })),
            }),
            "httpEndpointDestinationConfiguration.endpointConfiguration.name",
            "^(?!\\s*$).+$",
            "^(?!\\s*$).+",
        ),
        (
            // Another member of the same non-blank shape class, which the issue did not
            // cite: one rule for both.
            "a whitespace-only common attribute name",
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "blank-attribute-stream",
                "HttpEndpointDestinationConfiguration": {
                    "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                    "S3Configuration": s3_configuration(),
                    "RequestConfiguration": {
                        "CommonAttributes": [
                            { "AttributeName": "\t", "AttributeValue": "v" },
                        ],
                    },
                },
            }),
            "httpEndpointDestinationConfiguration.requestConfiguration.commonAttributes.1.member.attributeName",
            "^(?!\\s*$).+$",
            "^(?!\\s*$).+",
        ),
    ];

    for (case, operation, request, member, model_pattern, reported) in rows {
        let (status, refused) = emulator.call(operation, request).await;
        assert_eq!(status, 400, "case {case:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        let expected = value_at(member, &must_match(reported));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(model_pattern),
            "case {case:?}: expected the reported pattern, not the model's \
             {model_pattern:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_range_is_enforced_at_its_published_bounds_and_reported_without_the_value() {
    let emulator = Emulator::new();

    // Each published bound, and one step past each — the ranges are
    // `smithy_query(query="shape", shape="SizeInMBs,IntervalInSeconds,\
    // SplunkRetryDurationInSeconds")`: SizeInMBs 1..128, IntervalInSeconds 0..900,
    // SplunkRetryDurationInSeconds 0..7200.
    //
    // (case, destination member, value, member path, expected requirement or accepted)
    struct Row {
        case: &'static str,
        request: Value,
        /// `None` when the value is inside the range and the create is accepted.
        refused: Option<(&'static str, String)>,
    }

    fn buffering_row(
        case: &'static str,
        stream: &str,
        size: i64,
        interval: i64,
        refused: Option<(&'static str, String)>,
    ) -> Row {
        Row {
            case,
            request: json!({
                "DeliveryStreamName": stream,
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": BUCKET_ARN,
                    "BufferingHints": { "SizeInMBs": size, "IntervalInSeconds": interval },
                },
            }),
            refused,
        }
    }

    fn splunk_row(
        case: &'static str,
        stream: &str,
        duration: i64,
        refused: Option<(&'static str, String)>,
    ) -> Row {
        let mut splunk = splunk_configuration();
        splunk["RetryOptions"] = json!({ "DurationInSeconds": duration });
        Row {
            case,
            request: json!({
                "DeliveryStreamName": stream,
                "SplunkDestinationConfiguration": splunk,
            }),
            refused,
        }
    }

    const SIZE: &str = "extendedS3DestinationConfiguration.bufferingHints.sizeInMBs";
    const INTERVAL: &str = "extendedS3DestinationConfiguration.bufferingHints.intervalInSeconds";
    const DURATION: &str = "splunkDestinationConfiguration.retryOptions.durationInSeconds";

    let rows = vec![
        buffering_row(
            "a buffering size below its minimum",
            "size-under",
            0,
            60,
            Some((
                SIZE,
                "Member must have value greater than or equal to 1".to_string(),
            )),
        ),
        buffering_row("a buffering size at its minimum", "size-min", 1, 60, None),
        buffering_row("a buffering size at its maximum", "size-max", 128, 60, None),
        buffering_row(
            "a buffering size above its maximum",
            "size-over",
            129,
            60,
            Some((
                SIZE,
                "Member must have value less than or equal to 128".to_string(),
            )),
        ),
        buffering_row(
            "a buffering interval below its minimum",
            "interval-under",
            5,
            -1,
            Some((
                INTERVAL,
                "Member must have value greater than or equal to 0".to_string(),
            )),
        ),
        buffering_row(
            "a buffering interval at its minimum",
            "interval-min",
            5,
            0,
            None,
        ),
        buffering_row(
            "a buffering interval at its maximum",
            "interval-max",
            5,
            900,
            None,
        ),
        buffering_row(
            "a buffering interval above its maximum",
            "interval-over",
            5,
            901,
            Some((
                INTERVAL,
                "Member must have value less than or equal to 900".to_string(),
            )),
        ),
        splunk_row(
            "a splunk retry duration below its minimum",
            "duration-under",
            -1,
            Some((
                DURATION,
                "Member must have value greater than or equal to 0".to_string(),
            )),
        ),
        splunk_row(
            "a splunk retry duration at its minimum",
            "duration-min",
            0,
            None,
        ),
        splunk_row(
            "a splunk retry duration at its maximum",
            "duration-max",
            7200,
            None,
        ),
        splunk_row(
            "a splunk retry duration above its maximum",
            "duration-over",
            7201,
            Some((
                DURATION,
                "Member must have value less than or equal to 7200".to_string(),
            )),
        ),
    ];

    for row in rows {
        let (status, answer) = emulator.call("CreateDeliveryStream", row.request).await;
        match row.refused {
            None => assert_eq!(
                status, 200,
                "case {:?}: a value inside the range is accepted, got {answer:?}",
                row.case
            ),
            Some((member, requirement)) => {
                assert_eq!(status, 400, "case {:?}: got {answer:?}", row.case);
                let message = error_message(&answer);
                let expected = value_at(member, &requirement);
                assert_eq!(
                    message, expected,
                    "case {:?}: expected {expected:?}, got {message:?}",
                    row.case
                );
            }
        }
    }
}

#[tokio::test]
async fn a_duplicate_name_is_refused_with_the_name_and_the_account_that_holds_it() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();

    // The name is the caller's, so it varies; the account is the one the emulator
    // presents, whatever the name.
    for name in ["duplicate-one", "duplicate.two", "DUPLICATE_3"] {
        emulator.create_stream(name, None).await;
        let (status, refused) = emulator
            .call(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": name,
                    "S3DestinationConfiguration": s3_configuration(),
                }),
            )
            .await;
        assert_eq!(status, 400, "case {name:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "ResourceInUseException",
            "case {name:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        let expected = format!("Firehose {name} under accountId {account} already exists");
        assert_eq!(
            message, expected,
            "case {name:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_lookup_that_resolved_nothing_names_the_account_and_never_the_probed_name() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();
    // The control plane states the account the lookup was scoped to; the data plane
    // states the same fact in its own wording. Neither echoes the name it was given.
    let control_plane = format!("Firehose under account {account} not found.");
    let data_plane = format!("Firehose not found under account {account}.");

    // One stream exists, so the refusals below are a failed lookup and not an empty
    // register.
    emulator.create_stream("present-stream", None).await;

    /// One operation that resolves a stream by name, and the sentence it states when
    /// the lookup finds none.
    struct Lookup {
        case: &'static str,
        operation: &'static str,
        /// Builds the request for a probed name.
        request: fn(&str) -> Value,
        expected: String,
    }

    let operations = vec![
        Lookup {
            case: "describe",
            operation: "DescribeDeliveryStream",
            request: |name| json!({ "DeliveryStreamName": name }),
            expected: control_plane.clone(),
        },
        Lookup {
            case: "delete",
            operation: "DeleteDeliveryStream",
            request: |name| json!({ "DeliveryStreamName": name }),
            expected: control_plane.clone(),
        },
        Lookup {
            case: "list tags",
            operation: "ListTagsForDeliveryStream",
            request: |name| json!({ "DeliveryStreamName": name }),
            expected: control_plane.clone(),
        },
        Lookup {
            case: "put a record",
            operation: "PutRecord",
            request: |name| json!({ "DeliveryStreamName": name, "Record": { "Data": "eA==" } }),
            expected: data_plane.clone(),
        },
        Lookup {
            case: "put a batch",
            operation: "PutRecordBatch",
            request: |name| json!({ "DeliveryStreamName": name, "Records": [{ "Data": "eA==" }] }),
            expected: data_plane.clone(),
        },
    ];

    // The probed names differ in every way a name can: the message must not. (Each is
    // chosen not to occur incidentally inside the expected sentence, so the
    // "never echoed" assertion below means what it says.)
    for probed in ["ghost", "q", "present-stream-2", &"z".repeat(64)] {
        for lookup in &operations {
            let case = lookup.case;
            let expected = &lookup.expected;
            let (status, refused) = emulator
                .call(lookup.operation, (lookup.request)(probed))
                .await;
            assert_eq!(
                status, 400,
                "case {case:?} of {probed:?}: got {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?} of {probed:?}: got {refused:?}"
            );
            let message = error_message(&refused);
            assert_eq!(
                &message, expected,
                "case {case:?} of {probed:?}: expected {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains(probed),
                "case {case:?} of {probed:?}: the probed name must not be echoed, \
                 got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_put_to_a_stream_that_is_not_active_names_the_stream_and_the_account() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();

    // A create answers with the stream in `CREATING`, and "attempts to send data to a
    // delivery stream that is not in the ACTIVE state cause an exception". The sentence
    // is built from the request's own stream name and the account the put was scoped to,
    // whichever put operation raised it and whichever non-`ACTIVE` state the stream is in.
    //
    // (case, operation, request for a name)
    struct Put {
        case: &'static str,
        operation: &'static str,
        /// Builds the request that puts to a stream of that name.
        request: fn(&str) -> Value,
    }

    let puts = vec![
        Put {
            case: "put a record",
            operation: "PutRecord",
            request: |name| json!({ "DeliveryStreamName": name, "Record": { "Data": "eA==" } }),
        },
        Put {
            case: "put a batch",
            operation: "PutRecordBatch",
            request: |name| json!({ "DeliveryStreamName": name, "Records": [{ "Data": "eA==" }] }),
        },
    ];

    // The names differ in every way a stream name can — including one that, with the
    // per-operation suffix, is exactly the modelled 64-character maximum; the template
    // does not.
    for name in ["creating-put", "creating.put.2", &"c".repeat(49)] {
        for put in &puts {
            let (case, operation) = (put.case, put.operation);
            let stream = format!("{name}-{operation}");
            emulator.create_stream(&stream, None).await;
            let (status, refused) = emulator.call(operation, (put.request)(&stream)).await;
            assert_eq!(status, 400, "case {case:?} of {stream:?}: got {refused:?}");
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?} of {stream:?}: got {refused:?}"
            );
            let message = error_message(&refused);
            let expected = format!("Firehose {stream} is not active in {account} account.");
            assert_eq!(
                message, expected,
                "case {case:?} of {stream:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    // The same stream takes data once the creation span has passed, so the refusal was
    // the state and not the name.
    tokio::time::sleep(PAST_THE_TRANSITION).await;
    for put in &puts {
        let (case, operation) = (put.case, put.operation);
        let stream = format!("creating-put-{operation}");
        let (status, accepted) = emulator.call(operation, (put.request)(&stream)).await;
        assert_eq!(
            status, 200,
            "case {case:?} of {stream:?}: an ACTIVE stream takes data, got {accepted:?}"
        );
    }
}

#[tokio::test]
async fn a_delete_the_state_blocks_names_the_stream_the_account_and_that_state() {
    let emulator = Emulator::new();
    let account = engine::emulator_account_id();

    // A create answers with the stream in `CREATING`, and "you can't delete a Firehose
    // stream that is in the CREATING state" — the state that blocks the delete is what
    // the message states, whatever it is.
    for name in ["creating-one", "creating.two"] {
        emulator.create_stream(name, None).await;
        let (status, refused) = emulator
            .call(
                "DeleteDeliveryStream",
                json!({ "DeliveryStreamName": name }),
            )
            .await;
        assert_eq!(status, 400, "case {name:?}: got {refused:?}");
        assert_eq!(
            error_code(&refused),
            "ResourceInUseException",
            "case {name:?}: got {refused:?}"
        );
        let message = error_message(&refused);
        let expected =
            format!("Firehose {name} under account {account} cannot be deleted in CREATING state");
        assert_eq!(
            message, expected,
            "case {name:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn destinations_that_exclude_each_other_are_refused_with_the_arity_rule() {
    let emulator = Emulator::new();

    // The model declares each destination as its own optional member with no union over
    // them (`smithy_query(query="operation", operation="CreateDeliveryStream")`), and "A
    // Firehose stream is configured with a single destination": naming destinations of
    // several types breaks the same arity rule as naming none, so the refusal is the one
    // fixed sentence that states it — it names no member, however many the request
    // supplied and in whatever order it listed them. Naming ONE destination in both of its
    // spellings is the separate exclusion asserted at the end of this test.
    //
    // (case, request)
    let rows: Vec<(&str, Value)> = vec![
        (
            "two unrelated destinations",
            json!({
                "DeliveryStreamName": "s3-and-splunk",
                "S3DestinationConfiguration": s3_configuration(),
                "SplunkDestinationConfiguration": splunk_configuration(),
            }),
        ),
        (
            "a later member beside an earlier one",
            json!({
                "DeliveryStreamName": "extended-and-splunk",
                "SplunkDestinationConfiguration": splunk_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
            }),
        ),
        (
            "three destinations at once",
            json!({
                "DeliveryStreamName": "three-destinations",
                "SplunkDestinationConfiguration": splunk_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
                "HttpEndpointDestinationConfiguration": {
                    "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                    "S3Configuration": s3_configuration(),
                    "RoleARN": ROLE_ARN,
                },
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

    // One destination named in BOTH of its spellings — the member the model marks
    // `@deprecated` and the member that replaced it — is a different fault and a different
    // sentence: the two members exclude each other, and the refusal names them.
    let (status, refused) = emulator
        .call(
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": "two-s3",
                "S3DestinationConfiguration": s3_configuration(),
                "ExtendedS3DestinationConfiguration": extended_s3_configuration(),
            }),
        )
        .await;
    assert_eq!(status, 400, "both S3 spellings: got {refused:?}");
    assert_eq!(
        error_code(&refused),
        "InvalidArgumentException",
        "both S3 spellings: got {refused:?}"
    );
    let expected = exclusive_members_refusal(
        "S3DestinationConfiguration",
        "ExtendedS3DestinationConfiguration",
    );
    let message = error_message(&refused);
    assert_eq!(
        message, expected,
        "both S3 spellings: expected {expected:?}, got {message:?}"
    );

    // Nothing was stored for any refused create.
    let (_, listed) = emulator.call("ListDeliveryStreams", json!({})).await;
    assert_eq!(
        listed["DeliveryStreamNames"],
        json!([]),
        "a refused create stores nothing, got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// The endpoint's own JSON-RPC surface: rules 5 and 6 of this suite's header.
// ---------------------------------------------------------------------------

/// The class the endpoint answers a target it read and resolved no operation from.
const UNKNOWN_OPERATION: &str = "UnknownOperationException";

/// The class the endpoint answers a body its JSON wire could not read.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The sentence the request framework states for a list opening where the input structure
/// must open. Fixed text: it names the token CLASS and no value of the request.
const START_OF_LIST: &str = "Start of list found where not expected";

/// The whole document the endpoint answers a request that named no operation of this API —
/// the request framework's JSON-RPC envelope, every byte of it fixed text.
fn unrouted_envelope() -> Value {
    json!({
        "Output": { "__type": "com.amazon.coral.service#UnknownOperationException" },
        "Version": "1.0",
    })
}

/// The media type that framework's own envelope is announced in, which is not the
/// `awsJson1_1` one this service's own documents carry.
const FRAMEWORK_MEDIA_TYPE: &str = "application/json";

/// The media type this endpoint serves its API under, which every request below announces
/// unless the case under test is the announcement itself.
const PROTOCOL_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// RULE 5a: a target that names THIS endpoint's API and an operation it does not publish
/// was read and resolved nothing, so it is refused 400 `UnknownOperationException` with no
/// prose at all — there is no operation, member or value for a sentence to state.
#[tokio::test]
async fn a_target_naming_no_operation_of_this_api_is_refused_without_prose() {
    let emulator = Emulator::new();

    // Every row carries this endpoint's own target prefix; what differs is the NAME after
    // it, none of which the service binds (`smithy_query(query="list_operations")`).
    let rows: &[(&str, &str)] = &[
        // The plural of an operation the API does publish.
        ("a near miss of a published name", "DescribeDeliveryStreams"),
        ("a name of another API", "CreateStream"),
        ("a published name in another case", "listdeliverystreams"),
        ("an empty name", ""),
    ];

    for (case, operation) in rows {
        let target = format!("Firehose_20150804.{operation}");
        let answer = emulator
            .call_raw(Some(PROTOCOL_MEDIA_TYPE), Some(&target), b"{}")
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} ({target}): expected 400, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer.body),
            UNKNOWN_OPERATION,
            "case {case:?} ({target}): expected {UNKNOWN_OPERATION}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body.get("message"),
            None,
            "case {case:?} ({target}): expected no lowercase message, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body.get("Message"),
            None,
            "case {case:?} ({target}): expected no Message, got {:?}",
            answer.body
        );
    }
}

/// RULE 5b: a target that does not name this endpoint's API at all — no target, no
/// separator, or another endpoint's prefix — identifies no operation to answer for, so the
/// endpoint answers the request framework's JSON-RPC envelope: 200 `application/json`
/// carrying the unknown-operation report INSIDE it. An operation name this API DOES publish
/// after a foreign prefix is no exception: the prefix is what names the API.
#[tokio::test]
async fn a_target_that_names_no_api_of_this_endpoint_is_answered_in_the_framework_envelope() {
    let emulator = Emulator::new();

    // (case, the whole x-amz-target header the request carries)
    let rows: &[(&str, Option<&str>)] = &[
        (
            "another endpoint's prefix over a name this API publishes",
            Some("Kinesis_20131202.ListDeliveryStreams"),
        ),
        (
            "this prefix at another API version",
            Some("Firehose_20150805.ListDeliveryStreams"),
        ),
        (
            "this prefix in another case, which is another prefix",
            Some("firehose_20150804.ListDeliveryStreams"),
        ),
        (
            "a name with no prefix and no separator",
            Some("ListDeliveryStreams"),
        ),
        ("an empty prefix", Some(".ListDeliveryStreams")),
        ("no target at all", None),
    ];

    for (case, target) in rows {
        let answer = emulator
            .call_raw(Some(PROTOCOL_MEDIA_TYPE), *target, b"{}")
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} ({target:?}): expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.content_type.as_deref(),
            Some(FRAMEWORK_MEDIA_TYPE),
            "case {case:?} ({target:?}): expected {FRAMEWORK_MEDIA_TYPE}, got {:?}",
            answer.content_type
        );
        assert_eq!(
            answer.body,
            unrouted_envelope(),
            "case {case:?} ({target:?}): expected {:?}, got {:?}",
            unrouted_envelope(),
            answer.body
        );
    }

    // The refusal is the endpoint's answer and not a call: nothing was served for any of
    // those requests, so no stream was created by the create-shaped one below either.
    let created = emulator
        .call_raw(
            Some(PROTOCOL_MEDIA_TYPE),
            Some("Kinesis_20131202.CreateDeliveryStream"),
            br#"{"DeliveryStreamName":"unrouted-create","S3DestinationConfiguration":{"RoleARN":"arn:aws:iam::000000000000:role/firehose-delivery","BucketARN":"arn:aws:s3:::demo-bucket"}}"#,
        )
        .await;
    assert_eq!(
        created.body,
        unrouted_envelope(),
        "a create under a foreign prefix is not served, got {:?}",
        created.body
    );
    let (_, listed) = emulator.call("ListDeliveryStreams", json!({})).await;
    assert_eq!(
        listed["DeliveryStreamNames"],
        json!([]),
        "a request the endpoint does not route stores nothing, got {listed:?}"
    );
}

/// RULE 5c: the endpoint reads a document only in the media type its own protocol publishes,
/// so a request announcing another one — with a target this API does publish — announces a
/// protocol no operation of this endpoint is read in and is answered the same framework
/// envelope. A request announcing NONE has announced no other protocol and is served.
#[tokio::test]
async fn a_request_announcing_another_media_type_is_answered_in_the_framework_envelope() {
    let emulator = Emulator::new();

    // (case, the media type the request announces, whether the API serves it)
    let rows: &[(&str, Option<&str>, bool)] = &[
        ("this protocol's own media type", Some(PROTOCOL_MEDIA_TYPE), true),
        (
            "it with a parameter, which is the same media type",
            Some("application/x-amz-json-1.1; charset=utf-8"),
            true,
        ),
        ("none announced at all", None, true),
        (
            "plain JSON, which is another protocol of this family",
            Some("application/json"),
            false,
        ),
        (
            "another version of the awsJson family",
            Some("application/x-amz-json-1.0"),
            false,
        ),
        ("XML, which is no JSON at all", Some("application/xml"), false),
        ("plain text", Some("text/plain"), false),
    ];

    for (case, media_type, served) in rows {
        let answer = emulator
            .call_raw(
                *media_type,
                Some("Firehose_20150804.ListDeliveryStreams"),
                b"{}",
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        if *served {
            assert_eq!(
                answer.body["DeliveryStreamNames"],
                json!([]),
                "case {case:?}: expected the operation's own answer, got {:?}",
                answer.body
            );
        } else {
            assert_eq!(
                answer.body,
                unrouted_envelope(),
                "case {case:?}: expected {:?}, got {:?}",
                unrouted_envelope(),
                answer.body
            );
            assert_eq!(
                answer.content_type.as_deref(),
                Some(FRAMEWORK_MEDIA_TYPE),
                "case {case:?}: expected {FRAMEWORK_MEDIA_TYPE}, got {:?}",
                answer.content_type
            );
        }
    }
}

/// RULE 6: a body the JSON wire could not read into the operation's input is a fault in the
/// caller's SERIALIZER, not in a value — nothing became a value, so no constraint on one
/// was reached. It is refused 400 `SerializationException`, never the constraint class, and
/// the only prose it carries is the sentence this request framework states for the token
/// the body opened with, under the capitalized `Message`.
#[tokio::test]
async fn a_body_the_wire_cannot_read_is_a_serialization_fault() {
    let emulator = Emulator::new();

    // (case, the body's bytes, the sentence the answer carries under `Message`)
    let rows: &[(&str, &'static [u8], Option<&str>)] = &[
        (
            "a truncated document, which is no document",
            br#"{"DeliveryStreamName""#,
            None,
        ),
        ("bytes that are no JSON at all", b"not json at all", None),
        ("a document opening with a list", b"[]", Some(START_OF_LIST)),
        (
            "a list carrying what the input structure would have been",
            br#"[{"DeliveryStreamName":"listed"}]"#,
            Some(START_OF_LIST),
        ),
        (
            "a string where the input structure must open",
            br#""DeliveryStreamName""#,
            None,
        ),
        ("a number where it must open", b"42", None),
        ("null where it must open", b"null", None),
    ];

    // The fault is in the body and not in the operation, so every operation of this API
    // answers it the same way: one that takes a required member, and one whose input shape
    // requires none at all.
    for operation in ["DescribeDeliveryStream", "ListDeliveryStreams"] {
        let target = format!("Firehose_20150804.{operation}");
        for (case, body, message) in rows {
            let answer = emulator
                .call_raw(Some(PROTOCOL_MEDIA_TYPE), Some(&target), body)
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?} on {operation}: expected 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                error_code(&answer.body),
                SERIALIZATION_EXCEPTION,
                "case {case:?} on {operation}: expected {SERIALIZATION_EXCEPTION}, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.body.get("message"),
                None,
                "case {case:?} on {operation}: expected no lowercase message, got {:?}",
                answer.body
            );
            let stated = answer.body.get("Message").and_then(Value::as_str);
            assert_eq!(
                stated, *message,
                "case {case:?} on {operation}: expected {message:?} under Message, got \
                 {stated:?}"
            );
        }
    }

    // A body the wire CAN read is untouched by that layer: the empty document every
    // all-optional input is read from is served, and a value the body did carry that breaks
    // a modelled rule is still the constraint refusal.
    let served = emulator
        .call_raw(
            Some(PROTOCOL_MEDIA_TYPE),
            Some("Firehose_20150804.ListDeliveryStreams"),
            b"{}",
        )
        .await;
    assert_eq!(
        served.status, 200,
        "the empty document is read, got {} {:?}",
        served.status, served.body
    );
    let (status, refused) = emulator
        .call("DescribeDeliveryStream", json!({ "DeliveryStreamName": "" }))
        .await;
    assert_eq!(status, 400, "an empty name: got {refused:?}");
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "a value that WAS read and broke a rule stays the constraint class, got {refused:?}"
    );
}
