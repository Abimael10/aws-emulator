// SPDX-License-Identifier: CC0-1.0

//! The ELEMENT-SET rule of a media insights pipeline configuration, end to end over HTTP.
//!
//! Every assertion goes through the wire: the suite reuses the lifecycle suite's `Emulator`
//! harness, which mounts this service on the real `MultiServiceRouter` and dispatches
//! `http::Request`s, so what it pins is the emulator's HTTP behaviour and not an internal
//! helper.
//!
//! The RULE under test, one level above the recorded case (one list, one pair of types):
//!
//! > One media insights pipeline transcribes ONE audio stream, so a configuration may carry
//! > at most one TRANSCRIPTION processor element: a second element whose `Type` is
//! > `AmazonTranscribeProcessor` or `AmazonTranscribeCallAnalyticsProcessor` — either
//! > combination of the two, in any order, at any position — is refused `400
//! > BadRequestException` with the fixed message "Duplicate Transcribe processors are not
//! > allowed", and nothing is created or changed; a list carrying at most one of them is
//! > served, including one holding an element of every other type the model declares.
//!
//! The rows span what the rule generalises over: WHICH of the two processor types repeat,
//! how many of them the list carries, where in the list they sit, and the operation the list
//! arrives on — the rule holds for `CreateMediaInsightsPipelineConfiguration` and for
//! `UpdateMediaInsightsPipelineConfiguration`, which replaces the stored list wholesale.

use serde_json::{json, Value};

use crate::media_insights_pipeline_configuration_lifecycle::{
    Emulator, Response, BAD_REQUEST_EXCEPTION, ROLE_ARN,
};

/// The collection route the configuration operations hang off, from
/// `smithy_query(query="digest")`.
const CONFIGURATIONS: &str = "/media-insights-pipeline-configurations";

/// The two `MediaInsightsPipelineConfigurationElementType` values that name a transcription
/// processor, from `smithy_query(query="shape",
/// shape="MediaInsightsPipelineConfigurationElementType")`.
const TRANSCRIBE: &str = "AmazonTranscribeProcessor";
const CALL_ANALYTICS: &str = "AmazonTranscribeCallAnalyticsProcessor";

/// Every other `Type` that enum declares, which a configuration may carry beside ONE
/// transcription processor.
const OTHER_TYPES: [&str; 7] = [
    "VoiceAnalyticsProcessor",
    "KinesisDataStreamSink",
    "LambdaFunctionSink",
    "SqsQueueSink",
    "SnsTopicSink",
    "S3RecordingSink",
    "VoiceEnhancementSink",
];

/// The `ErrorCode` value the modelled `BadRequestException` carries in its body `Code`
/// member. Source: `smithy_query(query="shape",shape="ErrorCode")` — `BadRequest |
/// Forbidden | NotFound | ResourceLimitExceeded | ServiceFailure | ServiceUnavailable |
/// Throttling`.
const ERROR_CODE_BAD_REQUEST: &str = "BadRequest";

/// The message a list carrying more than one transcription processor is refused with.
/// Written out HERE rather than taken from the service's own constant, so any change to that
/// wording fails this suite. Recorded behaviour: fixed text with no variable parts.
const DUPLICATE_PROCESSORS: &str = "Duplicate Transcribe processors are not allowed";

/// A processor element of `element_type`, carrying the configuration that type declares.
fn processor(element_type: &str, language: &str) -> Value {
    match element_type {
        CALL_ANALYTICS => json!({
            "Type": CALL_ANALYTICS,
            "AmazonTranscribeCallAnalyticsProcessorConfiguration": { "LanguageCode": language },
        }),
        _ => json!({
            "Type": TRANSCRIBE,
            "AmazonTranscribeProcessorConfiguration": { "LanguageCode": language },
        }),
    }
}

/// The sink every row carries, so a refused list differs from a served one only in its
/// processors.
fn sink() -> Value {
    json!({
        "Type": "KinesisDataStreamSink",
        "KinesisDataStreamSinkConfiguration": {
            "InsightsTarget": "arn:aws:kinesis:us-west-2:000000000000:stream/insights",
        },
    })
}

/// The `Elements` lists the rule REFUSES, each with the case it stands for.
fn refused_lists() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one of each processor type, the sink last",
            json!([processor(TRANSCRIBE, "en-US"), processor(CALL_ANALYTICS, "en-US"), sink()]),
        ),
        (
            "the same two in the opposite order",
            json!([processor(CALL_ANALYTICS, "en-US"), processor(TRANSCRIBE, "en-US"), sink()]),
        ),
        (
            "the two processors separated by the sink",
            json!([processor(TRANSCRIBE, "en-US"), sink(), processor(CALL_ANALYTICS, "fr-FR")]),
        ),
        (
            "two elements of the same processor type",
            json!([processor(TRANSCRIBE, "en-US"), processor(TRANSCRIBE, "fr-FR"), sink()]),
        ),
        (
            "two call-analytics processors",
            json!([
                processor(CALL_ANALYTICS, "en-US"),
                processor(CALL_ANALYTICS, "en-GB"),
                sink(),
            ]),
        ),
        (
            "three transcription processors",
            json!([
                processor(TRANSCRIBE, "en-US"),
                processor(CALL_ANALYTICS, "en-US"),
                processor(TRANSCRIBE, "de-DE"),
            ]),
        ),
        (
            "a bare second processor element carrying no configuration of its own",
            json!([processor(TRANSCRIBE, "en-US"), json!({ "Type": CALL_ANALYTICS }), sink()]),
        ),
    ]
}

/// The `Elements` lists the rule SERVES, each with the case it stands for.
fn served_lists() -> Vec<(&'static str, Value)> {
    let every_other_type: Vec<Value> = OTHER_TYPES
        .iter()
        .map(|element_type| json!({ "Type": element_type }))
        .collect();
    let mut with_one_processor = vec![processor(CALL_ANALYTICS, "en-US")];
    with_one_processor.extend(every_other_type.clone());
    vec![
        (
            "one transcription processor and a sink",
            json!([processor(TRANSCRIBE, "en-US"), sink()]),
        ),
        (
            "one call-analytics processor and a sink",
            json!([processor(CALL_ANALYTICS, "en-US"), sink()]),
        ),
        ("a sink alone, no processor at all", json!([sink()])),
        (
            "every other type the model declares, and no processor",
            Value::Array(every_other_type),
        ),
        (
            "one processor beside every other type the model declares",
            Value::Array(with_one_processor),
        ),
    ]
}

/// A create body carrying `elements` under `name`.
fn create_body(name: &str, elements: &Value) -> Value {
    json!({
        "MediaInsightsPipelineConfigurationName": name,
        "ResourceAccessRoleArn": ROLE_ARN,
        "Elements": elements,
    })
}

/// Assert `response` is the element-set refusal.
fn assert_duplicate_processors(case: &str, response: &Response) {
    assert_eq!(
        response.status, 400,
        "case {case:?}: expected the modelled 400, got {} body {:?}",
        response.status, response.body
    );
    assert_eq!(
        response.error_code(),
        BAD_REQUEST_EXCEPTION,
        "case {case:?}: expected the modelled error shape, got {:?}",
        response.body
    );
    assert_eq!(
        response.body.pointer("/Code").and_then(Value::as_str),
        Some(ERROR_CODE_BAD_REQUEST),
        "case {case:?}: expected the modelled ErrorCode, got {:?}",
        response.body
    );
    let message = response
        .body
        .pointer("/Message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        message, DUPLICATE_PROCESSORS,
        "case {case:?}: expected the element-set wording, got {message:?}"
    );
}

/// How many entries the listing reports under `name`.
async fn listed_under(emulator: &Emulator, name: &str) -> usize {
    let listed = emulator.call("GET", CONFIGURATIONS, None).await;
    assert_eq!(
        listed.status, 200,
        "the listing must answer 200, got {} body {:?}",
        listed.status, listed.body
    );
    listed
        .body
        .pointer("/MediaInsightsPipelineConfigurations")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| {
                    entry["MediaInsightsPipelineConfigurationName"].as_str() == Some(name)
                })
                .count()
        })
        .unwrap_or_default()
}

/// RULE: a create whose `Elements` carries more than one transcription processor is the
/// modelled 400 with the fixed element-set wording, and nothing is created; a list carrying
/// at most one is served.
#[tokio::test]
async fn a_second_transcription_processor_is_refused_and_nothing_is_created() {
    let emulator = Emulator::new();

    for (index, (case, elements)) in refused_lists().into_iter().enumerate() {
        let name = format!("refused-{index}");
        let refused = emulator.create(create_body(&name, &elements)).await;
        assert_duplicate_processors(case, &refused);

        // Nothing was created: the name resolves to no configuration and the listing holds
        // no entry under it.
        let read_back = emulator.get(&name).await;
        assert_eq!(
            read_back.status, 404,
            "case {case:?}: a refused create must store nothing, got {} body {:?}",
            read_back.status, read_back.body
        );
        assert_eq!(
            listed_under(&emulator, &name).await,
            0,
            "case {case:?}: a refused create must leave the listing without that name"
        );
    }

    for (index, (case, elements)) in served_lists().into_iter().enumerate() {
        let name = format!("served-{index}");
        let served = emulator.create(create_body(&name, &elements)).await;
        assert_eq!(
            served.status, 201,
            "case {case:?}: expected the create to be served, got {} body {:?}",
            served.status, served.body
        );
        let stored = emulator.get(&name).await;
        assert_eq!(
            stored
                .body
                .pointer("/MediaInsightsPipelineConfiguration/Elements"),
            served
                .body
                .pointer("/MediaInsightsPipelineConfiguration/Elements"),
            "case {case:?}: the stored elements must be what the create reported"
        );
    }
}

/// RULE (the same rule on the UPDATE, which replaces the stored list wholesale): an update
/// whose `Elements` carries more than one transcription processor is the same refusal, and
/// the stored configuration is left exactly as it was.
#[tokio::test]
async fn an_update_to_two_transcription_processors_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let name = "update-element-rule";
    let created = emulator
        .create(create_body(
            name,
            &json!([processor(TRANSCRIBE, "en-US"), sink()]),
        ))
        .await;
    assert_eq!(
        created.status, 201,
        "setup: the create must be served, got {} body {:?}",
        created.status, created.body
    );
    let before = emulator.get(name).await;

    for (case, elements) in refused_lists() {
        let refused = emulator
            .call(
                "PUT",
                &format!("{CONFIGURATIONS}/{name}"),
                Some(json!({
                    "ResourceAccessRoleArn": ROLE_ARN,
                    "Elements": elements,
                })),
            )
            .await;
        assert_duplicate_processors(case, &refused);
        let after = emulator.get(name).await;
        assert_eq!(
            after.body.pointer("/MediaInsightsPipelineConfiguration"),
            before.body.pointer("/MediaInsightsPipelineConfiguration"),
            "case {case:?}: a refused update must leave the stored configuration untouched"
        );
    }

    // And an update carrying ONE transcription processor is served, so the refusal is about
    // the duplicate alone.
    let served = emulator
        .call(
            "PUT",
            &format!("{CONFIGURATIONS}/{name}"),
            Some(json!({
                "ResourceAccessRoleArn": ROLE_ARN,
                "Elements": [processor(CALL_ANALYTICS, "en-GB"), sink()],
            })),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"one processor on the update\": expected 200, got {} body {:?}",
        served.status, served.body
    );
}
