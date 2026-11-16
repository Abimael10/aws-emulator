// SPDX-License-Identifier: CC0-1.0

//! The two preconditions `CreateChannel` takes on a general purpose bucket destination and on
//! the capacity mode of the stream it reads, end to end over the wire.
//!
//! RULE 1: the `OutputKeyTemplate` of a general purpose bucket destination "is used to
//! construct the Amazon S3 object key for delivered objects"
//! (`S3StorageConfiguration$OutputKeyTemplate`), so a value that renders no such key is
//! refused with the error this service answers for an input it validates before it provisions
//! anything (`ValidationException`, 400) and NO channel is created, each fault stating its own
//! sentence: the template "must not contain consecutive periods, period-only path segments, or
//! start with /", it "must not contain consecutive slashes (//)", it may carry no unclosed
//! variable placeholder, and a variable it names has to be one the service expands. A template
//! that renders a key is accepted and reported back as written.
//!
//! RULE 2: a channel is "only supported for data streams with the on-demand capacity mode"
//! (`CreateChannel`), so a create whose source stream is in any other mode is refused with
//! that same error, naming the stream by NAME under the account that holds it and the mode it
//! is in, and asking for the mode the feature needs.
//!
//! The rows differ along what each rule generalises over: for rule 1 the position and the form
//! of the offending segment or expansion, and the forms a template may take inside the modelled
//! `@pattern`; for rule 2 the stream's name and the region the request is signed for.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    capacity_mode_prose, STREAM_MODE_ON_DEMAND, STREAM_MODE_PROVISIONED,
};

/// The sentences `CreateChannel` refuses a template that renders no object key with, written
/// out here rather than borrowed from the crate, so the wording is what this suite pins.
const INVALID_TEMPLATE_MESSAGE: &str =
    "OutputKeyTemplate must not contain consecutive periods, period-only path segments, or \
     start with /";
const CONSECUTIVE_SLASHES_MESSAGE: &str =
    "OutputKeyTemplate must not contain consecutive slashes (//)";
const UNCLOSED_VARIABLE_MESSAGE: &str =
    "OutputKeyTemplate contains an unclosed variable placeholder";

use crate::channel_lifecycle::{
    bucket_arn, on_demand_stream, role_arn, storage_configuration, stream_configuration,
};
use crate::stream_tags::{error_code, error_message, poll_until_active_in, stream_arn, Emulator, REGION};

/// The error this service answers for an input it validates before it provisions anything.
const VALIDATION: &str = "ValidationException";

/// A `CreateChannel` body delivering to a general purpose bucket, with `template` as the
/// destination's `OutputKeyTemplate` when a case names one.
fn create(name: &str, source: &str, template: Option<&str>) -> Value {
    let mut storage = storage_configuration("key-template-bucket");
    if let Some(template) = template {
        storage["OutputKeyTemplate"] = json!(template);
    }
    json!({
        "ChannelName": name,
        "ServiceExecutionRoleARN": role_arn("channel-delivery"),
        "StreamConfigurationList": [stream_configuration(source, "STRING", None)],
        "S3DestinationConfiguration": { "StorageConfiguration": storage },
    })
}

/// A template that renders an object key is taken as written; one that cannot is refused and
/// leaves no channel behind.
#[tokio::test]
async fn an_output_key_template_that_renders_no_object_key_is_refused() {
    let emulator = Emulator::new();
    let source = on_demand_stream(&emulator, "key-template-source").await;

    // (case, the template, the sentence it is refused with, or `None` when it renders a key).
    // Every accepted row is inside the modelled `@pattern` `^[0-9A-Za-z!\-_'.*()\/=:{}]+$`
    // (`smithy_query(query="shape", shape="com.amazonaws.kinesis#S3StorageConfiguration")`),
    // which is what the request walk already enforces.
    let rows: Vec<(&str, &str, Option<String>)> = vec![
        ("a bare name", "records", None),
        ("a variable this service expands", "records/!{yyyy}", None),
        (
            "the calendar variables and the record's own",
            "r/!{yyyy}/!{MM}/!{dd}/!{HH}/!{mm}/!{ss}/!{sequence-number}!{extension}",
            None,
        ),
        (
            "the channel's own name and id",
            "r/!{channel-name}/!{channel-id}",
            None,
        ),
        ("every other character the pattern admits", "a.b-c_d'e!f(g)=h:i", None),
        // A brace that opens no variable is text: a variable opens with `!{`.
        ("a brace that opens no variable", "records/{yyyy}", None),
        ("a trailing separator", "a/", None),
        (
            "consecutive periods in the middle",
            "a/../b",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "consecutive periods at the start",
            "../b",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "consecutive periods inside a segment",
            "a..b",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "a period-only segment",
            "a/./b",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "nothing but a period",
            ".",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "a leading separator",
            "/a",
            Some(INVALID_TEMPLATE_MESSAGE.to_string()),
        ),
        (
            "two separators in a row",
            "a//b",
            Some(CONSECUTIVE_SLASHES_MESSAGE.to_string()),
        ),
        (
            "two separators after a variable",
            "a/!{yyyy}//b",
            Some(CONSECUTIVE_SLASHES_MESSAGE.to_string()),
        ),
        (
            "a variable that is never closed",
            "a/!{yyyy",
            Some(UNCLOSED_VARIABLE_MESSAGE.to_string()),
        ),
        (
            "a variable this service does not expand",
            "records/!{partition-id}",
            Some("OutputKeyTemplate contains invalid variable: !{partition-id}".to_string()),
        ),
    ];

    for (index, (case, template, refusal)) in rows.iter().enumerate() {
        let name = format!("key-template-{index}");
        let (status, response) = emulator
            .call("CreateChannel", &create(&name, &source, Some(template)))
            .await;
        let Some(refusal) = refusal else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected {template:?} accepted, got {status} {response}"
            );
            let reported = response["ChannelDescription"]["S3DestinationConfiguration"]
                ["StorageConfiguration"]["OutputKeyTemplate"]
                .as_str();
            assert_eq!(
                reported,
                Some(*template),
                "case {case:?}: expected the template reported back as written, got {reported:?}"
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {case:?}: expected {template:?} refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {response}"
        );
        assert_eq!(
            &error_message(&response),
            refusal,
            "case {case:?}: expected the refusal to state the fault at hand"
        );
        // A refused create provisions nothing, so the name it asked for is still free.
        let (status, read) = emulator
            .call("DescribeChannel", &json!({ "ChannelName": name }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must leave no channel, got {status} {read}"
        );
    }
}

/// A create whose source stream is not on-demand is refused by name, under the account that
/// holds the stream, in whichever region the request is signed for.
#[tokio::test]
async fn a_source_stream_outside_the_on_demand_mode_is_refused_by_name() {
    let emulator = Emulator::new();

    // (case, the region the request is signed for, the stream's name)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("the default region", REGION, "provisioned-source"),
        ("another region", "eu-west-1", "provisioned-source"),
        (
            "a name at the other end of the name class",
            REGION,
            "A.provisioned_stream-0",
        ),
    ];

    for (index, (case, region, stream)) in rows.iter().enumerate() {
        emulator
            .ok_in(
                case,
                region,
                "CreateStream",
                &json!({
                    "StreamName": stream,
                    "ShardCount": 1,
                    "StreamModeDetails": { "StreamMode": STREAM_MODE_PROVISIONED },
                }),
            )
            .await;
        poll_until_active_in(&emulator, case, region, stream).await;
        let (status, response) = emulator
            .call_in(
                region,
                "CreateChannel",
                &create(
                    &format!("provisioned-{index}"),
                    &stream_arn(region, stream),
                    None,
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the create refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {response}"
        );
        let expected = format!(
            "Request is invalid. Stream {stream} under account {} is in {} mode. Please enable \
             {} mode to use this feature.",
            engine::EMULATOR_ACCOUNT_ID,
            capacity_mode_prose(STREAM_MODE_PROVISIONED),
            capacity_mode_prose(STREAM_MODE_ON_DEMAND),
        );
        assert_eq!(
            error_message(&response),
            expected,
            "case {case:?}: expected {expected:?}"
        );
    }
}

/// The two capacity modes the model declares, as a sentence spells each one.
#[tokio::test]
async fn a_capacity_mode_is_worded_as_a_sentence_spells_it() {
    // (the `StreamMode` value, the word a sentence uses). Source:
    // `smithy_query(query="shape", shape="com.amazonaws.kinesis#StreamMode")`.
    for (mode, word) in [
        (STREAM_MODE_PROVISIONED, "Provisioned"),
        (STREAM_MODE_ON_DEMAND, "On-demand"),
    ] {
        let spelled = capacity_mode_prose(mode);
        assert_eq!(
            spelled, word,
            "case {mode:?}: expected the mode worded {word:?}, got {spelled:?}"
        );
    }
}

/// The bucket helper is shared with the lifecycle suite, and a destination naming one bucket
/// is what every row above delivers to — asserted here so the helper cannot drift away from
/// the ARN form the model states.
#[test]
fn a_destination_bucket_arn_is_inside_the_modelled_form() {
    let arn = bucket_arn("key-template-bucket");
    assert_eq!(
        arn, "arn:aws:s3:::key-template-bucket",
        "the destination bucket ARN is the modelled `BucketARN` form, got {arn:?}"
    );
}
