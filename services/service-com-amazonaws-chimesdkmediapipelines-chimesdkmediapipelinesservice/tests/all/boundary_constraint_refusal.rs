// SPDX-License-Identifier: CC0-1.0

//! How a value one of this model's own constraints does not admit is refused, end to end
//! over HTTP.
//!
//! Every assertion goes through the wire: the suite reuses the lifecycle suite's `Emulator`
//! harness, which mounts this service on the real `MultiServiceRouter` and dispatches
//! `http::Request`s, so what it pins is the emulator's HTTP behaviour and not an internal
//! helper.
//!
//! The RULE under test, one level above the recorded case (one member, one pattern):
//!
//! > A modelled constraint is enforced at the REQUEST BOUNDARY, ahead of any handler, and
//! > that layer answers `400 ValidationException` carrying AWS's envelope over one clause
//! > per violation — `1 validation error detected: <clause>` for one and
//! > `N validation errors detected: <clause>; <clause>` for several — where a clause names
//! > the submitted value, the member's own lowerCamel wire path and the requirement it
//! > broke, and a `@pattern` requirement quotes the expression the model declares less the
//! > `$` that closes it.
//! > A rule no modelled constraint expresses is refused by the SERVICE instead, as its
//! > modelled `BadRequestException`, so the two layers stay distinguishable.
//!
//! The rows span what the rule generalises over: the KIND of constraint (a `@pattern`, both
//! ends of a `@length`, a `@range`, an enum value set, an absent `@required` member), where
//! the value arrived (a JSON body member, a nested member, a URI label, a query parameter),
//! the operation it arrived on, and how MANY violations one request carries.

use serde_json::{json, Value};

use crate::media_insights_pipeline_configuration_lifecycle::{
    Emulator, Response, BAD_REQUEST_EXCEPTION, ROLE_ARN, VALIDATION_EXCEPTION,
};

/// The collection routes the rows drive, from `smithy_query(query="digest")`.
const CONFIGURATIONS: &str = "/media-insights-pipeline-configurations";
const POOLS: &str = "/media-pipeline-kinesis-video-stream-pools";

/// The `@pattern` of `MediaInsightsPipelineConfigurationNameString` and of
/// `KinesisVideoStreamPoolName`, as this service QUOTES one back: the expression the model
/// declares (`^[0-9a-zA-Z._-]+$`) less the `$` that closes it, which is how the model makes
/// a find total at that end rather than part of the grammar a caller satisfies. Source:
/// `smithy_query(query="operation",
/// operation="CreateMediaInsightsPipelineConfiguration,
/// CreateMediaPipelineKinesisVideoStreamPool")`.
const NAME_PATTERN: &str = "^[0-9a-zA-Z._-]+";

/// The `@pattern` of the `Arn` shape `ResourceAccessRoleArn` targets, from the same query
/// (`^arn[\/\:\-\_\.a-zA-Z0-9]+$`), quoted the same way.
const ARN_PATTERN: &str = r"^arn[\/\:\-\_\.a-zA-Z0-9]+";

/// AWS's envelope over the clauses of one refusal.
fn envelope(clauses: &[&str]) -> String {
    match clauses {
        [single] => format!("1 validation error detected: {single}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// The clause a `@pattern` violation is stated with: the submitted value, the member's wire
/// path and the expression the value had to match.
fn pattern_clause(value: &str, member_path: &str, pattern: &str) -> String {
    format!(
        "Value '{value}' at '{member_path}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {pattern}"
    )
}

/// The same clause for a member whose shape is `@sensitive`, which states NO value: the
/// `Arn` shape `ResourceAccessRoleArn` targets carries that trait
/// (`smithy_query(query="shape",shape="Arn")`), so the refusal names the member and the
/// requirement and echoes nothing of what the caller submitted.
fn withheld_pattern_clause(member_path: &str, pattern: &str) -> String {
    format!(
        "Value at '{member_path}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {pattern}"
    )
}

/// A create body for a configuration named `name`, otherwise valid.
fn configuration(name: &str) -> Value {
    json!({
        "MediaInsightsPipelineConfigurationName": name,
        "ResourceAccessRoleArn": ROLE_ARN,
        "Elements": [{
            "Type": "AmazonTranscribeProcessor",
            "AmazonTranscribeProcessorConfiguration": { "LanguageCode": "en-US" },
        }],
    })
}

/// Assert `response` is the boundary's refusal, carrying `expected_message`.
fn assert_validation(case: &str, response: &Response, expected_message: &str) {
    assert_eq!(
        response.status, 400,
        "case {case:?}: expected 400, got {} body {:?}",
        response.status, response.body
    );
    assert_eq!(
        response.error_code(),
        VALIDATION_EXCEPTION,
        "case {case:?}: expected the boundary's error code, got {:?}",
        response.body
    );
    let message = response
        .body
        .pointer("/message")
        .or_else(|| response.body.pointer("/Message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected AWS's validation message, got {message:?}"
    );
}

/// RULE: a `@pattern` violation is the boundary's `ValidationException`, with AWS's envelope
/// over a clause that names the value, the member's wire path and the expression the model
/// declares — for every member of this service a `@pattern` is declared on, wherever the
/// value arrived.
#[tokio::test]
async fn a_pattern_violation_is_the_boundarys_validation_exception() {
    let emulator = Emulator::new();

    // A configuration NAME, for several values the expression excludes: the recorded case's
    // `@`, and a character class, a space and a path separator beside it.
    for bad in [
        "sim2real-invalid@name-suffix",
        "name with spaces",
        "name/with/slashes",
        "näme-with-non-ascii",
        "name+plus",
    ] {
        let refused = emulator.create(configuration(bad)).await;
        assert_validation(
            bad,
            &refused,
            &envelope(&[&pattern_clause(
                bad,
                "mediaInsightsPipelineConfigurationName",
                NAME_PATTERN,
            )]),
        );
    }

    // The same rule on another operation's name member …
    let refused = emulator
        .call(
            "POST",
            POOLS,
            Some(json!({
                "PoolName": "pool name",
                "StreamConfiguration": { "Region": "us-west-2", "DataRetentionInHours": 2 },
            })),
        )
        .await;
    assert_validation(
        "a PoolName the expression excludes",
        &refused,
        &envelope(&[&pattern_clause("pool name", "poolName", NAME_PATTERN)]),
    );

    // … and on an ARN member, whose expression is a different one and whose `@sensitive`
    // shape keeps the submitted value out of the refusal.
    let mut body = configuration("valid-name");
    body["ResourceAccessRoleArn"] = json!("not an arn");
    let refused = emulator.create(body).await;
    assert_validation(
        "a ResourceAccessRoleArn the ARN expression excludes",
        &refused,
        &envelope(&[&withheld_pattern_clause(
            "resourceAccessRoleArn",
            ARN_PATTERN,
        )]),
    );
    assert!(
        !message_of(&refused).contains("not an arn"),
        "case \"a ResourceAccessRoleArn the ARN expression excludes\": a sensitive member's \
         value must not be echoed, got {:?}",
        refused.body
    );

    // The dialect itself: the quoted expression keeps the model's opening `^` and drops the
    // `$` that closes it.
    let refused = emulator.create(configuration("bad@name")).await;
    let message = message_of(&refused);
    assert!(
        message.ends_with(NAME_PATTERN),
        "case \"the quoted expression\": expected the clause to close with {NAME_PATTERN:?}, \
         got {message:?}"
    );
    assert!(
        message.contains("pattern: ^"),
        "case \"the quoted expression\": expected the model's opening anchor to be kept, got \
         {message:?}"
    );
}

/// RULE: every other kind of modelled constraint is refused by the same layer, in the same
/// envelope: both ends of a `@length`, a `@range`, an enum value set, and a `@required`
/// member the request leaves out.
#[tokio::test]
async fn every_constraint_kind_is_refused_in_the_same_envelope() {
    let emulator = Emulator::new();

    // Both ends of `MediaInsightsPipelineConfigurationNameString`'s 2..64 `@length`, one
    // past each, and the lengths themselves, which are served.
    for bad in ["a", &"c".repeat(65)] {
        let refused = emulator.create(configuration(bad)).await;
        assert_eq!(
            refused.status, 400,
            "case {bad:?}: expected 400, got {} body {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            VALIDATION_EXCEPTION,
            "case {bad:?}: expected the boundary's error code, got {:?}",
            refused.body
        );
        let message = message_of(&refused);
        assert!(
            message.starts_with("1 validation error detected: ")
                && message.contains("at 'mediaInsightsPipelineConfigurationName'"),
            "case {bad:?}: expected AWS's envelope over a clause naming the member, got \
             {message:?}"
        );
    }
    for good in ["ab", &"c".repeat(64)] {
        let served = emulator.create(configuration(good)).await;
        assert_eq!(
            served.status, 201,
            "case {:?}: a name at a published bound must be served, got {} body {:?}",
            good.len(),
            served.status,
            served.body
        );
    }

    // A `@range` on a query parameter, at both published bounds and one past each.
    for bad in ["0", "101"] {
        let refused = emulator
            .call("GET", &format!("{CONFIGURATIONS}?max-results={bad}"), None)
            .await;
        let message = message_of(&refused);
        assert_eq!(
            refused.error_code(),
            VALIDATION_EXCEPTION,
            "case \"max-results={bad}\": expected the boundary's error code, got {:?}",
            refused.body
        );
        assert!(
            message.starts_with("1 validation error detected: ") && message.contains("'maxResults'"),
            "case \"max-results={bad}\": expected a clause naming the member, got {message:?}"
        );
    }

    // An enum value set, and an absent `@required` member: both the boundary's, both in the
    // same envelope.
    let refused = emulator
        .call(
            "POST",
            "/sdk-media-capture-pipelines",
            Some(json!({
                "SourceType": "NotAKnownSource",
                "SourceArn": "arn:aws:chime:us-west-2:000000000000:meeting/ok",
                "SinkType": "S3Bucket",
                "SinkArn": "arn:aws:s3:::ok-bucket/captures",
            })),
        )
        .await;
    let message = message_of(&refused);
    assert_eq!(
        refused.error_code(),
        VALIDATION_EXCEPTION,
        "case \"a SourceType outside its value set\": expected the boundary's error code, got \
         {:?}",
        refused.body
    );
    assert!(
        message.starts_with("1 validation error detected: ")
            && message.contains("at 'sourceType'")
            && message.contains("enum value set"),
        "case \"a SourceType outside its value set\": expected a clause naming the value set, \
         got {message:?}"
    );

    let mut missing = configuration("valid-name");
    missing
        .as_object_mut()
        .expect("the body is an object")
        .remove("ResourceAccessRoleArn");
    let refused = emulator.create(missing).await;
    let message = message_of(&refused);
    assert_eq!(
        refused.error_code(),
        VALIDATION_EXCEPTION,
        "case \"a required member left out\": expected the boundary's error code, got {:?}",
        refused.body
    );
    assert!(
        message.starts_with("1 validation error detected: ")
            && message.contains("'resourceAccessRoleArn'"),
        "case \"a required member left out\": expected a clause naming the member, got \
         {message:?}"
    );
}

/// RULE: a request breaking SEVERAL constraints at once carries one clause per violation,
/// under the plural envelope with AWS's separator.
#[tokio::test]
async fn several_violations_are_counted_in_one_envelope() {
    let emulator = Emulator::new();
    let refused = emulator
        .create(json!({
            "MediaInsightsPipelineConfigurationName": "bad@name",
            "ResourceAccessRoleArn": "not an arn",
            "Elements": [],
        }))
        .await;
    assert_eq!(
        refused.error_code(),
        VALIDATION_EXCEPTION,
        "case \"two violations\": expected the boundary's error code, got {:?}",
        refused.body
    );
    let message = message_of(&refused);
    assert!(
        message.starts_with("2 validation errors detected: "),
        "case \"two violations\": expected the plural envelope, got {message:?}"
    );
    for clause in [
        pattern_clause(
            "bad@name",
            "mediaInsightsPipelineConfigurationName",
            NAME_PATTERN,
        ),
        withheld_pattern_clause("resourceAccessRoleArn", ARN_PATTERN),
    ] {
        assert!(
            message.contains(&clause),
            "case \"two violations\": expected the clause {clause:?}, got {message:?}"
        );
    }
    assert!(
        message.contains("; "),
        "case \"two violations\": expected AWS's clause separator, got {message:?}"
    );
}

/// RULE (the other half of the boundary rule): a rule no modelled constraint expresses is
/// refused by the SERVICE, as its own modelled `BadRequestException` — never as the
/// boundary's code — so a client can tell a malformed request from a request the service
/// would not serve.
#[tokio::test]
async fn a_rule_the_model_cannot_express_stays_this_services_bad_request() {
    let emulator = Emulator::new();
    for (case, method, path, body) in [
        (
            "a pagination token this service never issued",
            "GET",
            format!("{CONFIGURATIONS}?next-token=not-a-token-this-service-issued"),
            None,
        ),
        (
            "a name another live configuration already holds",
            "POST",
            CONFIGURATIONS.to_string(),
            Some(configuration("held-name")),
        ),
    ] {
        if case.starts_with("a name") {
            let first = emulator.create(configuration("held-name")).await;
            assert_eq!(
                first.status, 201,
                "setup: the first create must be served, got {} body {:?}",
                first.status, first.body
            );
        }
        let refused = emulator.call(method, &path, body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} body {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            BAD_REQUEST_EXCEPTION,
            "case {case:?}: expected this service's modelled error, got {:?}",
            refused.body
        );
        assert!(
            !message_of(&refused).contains("validation error"),
            "case {case:?}: a service refusal must not carry the boundary's envelope, got {:?}",
            refused.body
        );
    }
}

/// The `message` of an error answer, whichever casing the layer that produced it spells.
fn message_of(response: &Response) -> String {
    response
        .body
        .pointer("/message")
        .or_else(|| response.body.pointer("/Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
