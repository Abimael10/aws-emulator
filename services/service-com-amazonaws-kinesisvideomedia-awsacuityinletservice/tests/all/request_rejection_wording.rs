// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the two refusals `GetMedia` answers a request it has not yet read any
//! state for: a `StartSelector` missing the companion member its type requires, and a value
//! one of the model's constraints does not admit.
//!
//! The rules under test, each one level above any single recorded call:
//!
//! > 1. A `StartSelector` whose `StartSelectorType` needs a companion member the request
//! >    did not carry is refused HTTP 400 `InvalidArgumentException` with a sentence that
//! >    names the requirement that was not met and then renders the WHOLE selector the
//! >    request carried — the shape's name over its four members, in the order the model
//! >    declares them, each either the value the request stated for it or the literal
//! >    `null`. The requirement's noun is fixed per class, and the refusal precedes every
//! >    lookup, so it does not depend on which stream was addressed or whether that stream
//! >    exists.
//! > 2. A value a modelled constraint does not admit is refused by the boundary validator
//! >    in the shared AWS envelope, whose clause states the member path and the requirement
//! >    and NO value the caller sent — whatever member and whatever constraint class — and
//! >    whose `@pattern` requirement quotes the expression this API PUBLISHES for the
//! >    member's shape rather than the model's trait text.
//!
//! Rule 1's rows span the three classes (and both selector types of the timestamp class),
//! WHICH other members of the selector the request also carried (none, a fragment number, a
//! token, a timestamp) and how the stream was addressed (a live stream by name, a live
//! stream by ARN, a name nothing created). Rule 2's rows span the four pattern-constrained
//! members of this operation's input, several offending values per member — so a message
//! that echoed one would differ between two rows of the same member — and, for the classes
//! whose requirement text is the framework's own, a length and an enumerated-value
//! violation. The calls the issue recorded are one row of each table.

use crate::control_plane_stream_reads::{error_code, status, Harness};
use crate::refusal_message_fidelity::message;

/// The chunk the live stream of each table is seeded with, so a positive control can read
/// it back.
const MEDIA: &[&[u8]] = &[b"chunk-bytes-for-the-rejection-suite"];

/// The envelope one refused value is stated in, as the shared AWS constraint envelope
/// renders it. The clause states the member path and the requirement; the VALUE half AWS
/// states for some services is absent here, which is rule 2's own half of the sentence.
fn one_violation(member_path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member_path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement clause a `@pattern` violation states, over the expression the API
/// publishes.
fn must_match(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// RULE 1: a `StartSelector` missing the companion member its type requires is refused 400
/// `InvalidArgumentException`, stating the requirement and then the selector as sent.
#[tokio::test]
async fn every_start_selector_missing_its_companion_states_the_requirement_and_the_selector() {
    let harness = Harness::new();
    let live_arn = harness.create_stream("rejection-live").await;
    let fragments = harness.produce(Some("rejection-live"), None, MEDIA);
    let on_stream = fragments
        .first()
        .cloned()
        .expect("the seeded stream holds one fragment");

    // How the request addresses a stream: the refusal is about the SELECTOR, so it is the
    // same answer whichever identifier form carried it and whether or not that identifier
    // resolves.
    let addressings: Vec<(&str, serde_json::Value)> = vec![
        (
            "a live stream by name",
            serde_json::json!({ "StreamName": "rejection-live" }),
        ),
        (
            "a live stream by ARN",
            serde_json::json!({ "StreamARN": live_arn.clone() }),
        ),
        (
            "a name nothing created",
            serde_json::json!({ "StreamName": "rejection-never-created" }),
        ),
    ];

    // (case, the selector sent, the whole message expected)
    //
    // The `startTimestamp` row sends the epoch instant itself, which the date-time form
    // renders as the start of 1970 — a value chosen so the rendering is readable here
    // rather than computed by the code under test.
    let rows: Vec<(&str, serde_json::Value, String)> = vec![
        (
            "FRAGMENT_NUMBER carrying nothing else",
            serde_json::json!({ "StartSelectorType": "FRAGMENT_NUMBER" }),
            "FRAGMENT_NUMBER must be specified for startSelector : StartSelector(\
             startSelectorType=FRAGMENT_NUMBER, afterFragmentNumber=null, startTimestamp=null, \
             continuationToken=null)"
                .to_string(),
        ),
        (
            "FRAGMENT_NUMBER carrying a continuation token",
            serde_json::json!({
                "StartSelectorType": "FRAGMENT_NUMBER",
                "ContinuationToken": "a.token-shaped_value",
            }),
            "FRAGMENT_NUMBER must be specified for startSelector : StartSelector(\
             startSelectorType=FRAGMENT_NUMBER, afterFragmentNumber=null, startTimestamp=null, \
             continuationToken=a.token-shaped_value)"
                .to_string(),
        ),
        (
            "FRAGMENT_NUMBER carrying a start timestamp",
            serde_json::json!({
                "StartSelectorType": "FRAGMENT_NUMBER",
                "StartTimestamp": 0,
            }),
            "FRAGMENT_NUMBER must be specified for startSelector : StartSelector(\
             startSelectorType=FRAGMENT_NUMBER, afterFragmentNumber=null, \
             startTimestamp=1970-01-01T00:00:00Z, continuationToken=null)"
                .to_string(),
        ),
        (
            "PRODUCER_TIMESTAMP carrying nothing else",
            serde_json::json!({ "StartSelectorType": "PRODUCER_TIMESTAMP" }),
            "Start Timestamp must be specified for StartSelector : StartSelector(\
             startSelectorType=PRODUCER_TIMESTAMP, afterFragmentNumber=null, \
             startTimestamp=null, continuationToken=null)"
                .to_string(),
        ),
        (
            "SERVER_TIMESTAMP carrying nothing else",
            serde_json::json!({ "StartSelectorType": "SERVER_TIMESTAMP" }),
            "Start Timestamp must be specified for StartSelector : StartSelector(\
             startSelectorType=SERVER_TIMESTAMP, afterFragmentNumber=null, \
             startTimestamp=null, continuationToken=null)"
                .to_string(),
        ),
        (
            "SERVER_TIMESTAMP carrying a fragment number",
            serde_json::json!({
                "StartSelectorType": "SERVER_TIMESTAMP",
                "AfterFragmentNumber": "91000000000000000000000000000001",
            }),
            "Start Timestamp must be specified for StartSelector : StartSelector(\
             startSelectorType=SERVER_TIMESTAMP, \
             afterFragmentNumber=91000000000000000000000000000001, startTimestamp=null, \
             continuationToken=null)"
                .to_string(),
        ),
        (
            "CONTINUATION_TOKEN carrying nothing else",
            serde_json::json!({ "StartSelectorType": "CONTINUATION_TOKEN" }),
            "ContinuationToken must be specified for StartSelector : StartSelector(\
             startSelectorType=CONTINUATION_TOKEN, afterFragmentNumber=null, \
             startTimestamp=null, continuationToken=null)"
                .to_string(),
        ),
        (
            "CONTINUATION_TOKEN carrying a fragment number and a timestamp",
            serde_json::json!({
                "StartSelectorType": "CONTINUATION_TOKEN",
                "AfterFragmentNumber": "7",
                "StartTimestamp": 0,
            }),
            "ContinuationToken must be specified for StartSelector : StartSelector(\
             startSelectorType=CONTINUATION_TOKEN, afterFragmentNumber=7, \
             startTimestamp=1970-01-01T00:00:00Z, continuationToken=null)"
                .to_string(),
        ),
    ];

    for (selector_case, selector, expected) in &rows {
        for (addressing_case, addressing) in &addressings {
            let case = format!("{selector_case}, addressed by {addressing_case}");
            let response = harness.get_media(addressing, selector.clone()).await;
            assert_eq!(
                status(&response),
                400,
                "case {case:?}: expected HTTP 400, got {} ({})",
                status(&response),
                error_code(&response)
            );
            assert_eq!(
                error_code(&response),
                "InvalidArgumentException",
                "case {case:?}: expected InvalidArgumentException, got {:?}",
                error_code(&response)
            );
            let answered = message(&response);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
            assert!(
                !answered.contains("rejection-live")
                    && !answered.contains("rejection-never-created")
                    && !answered.contains(live_arn.as_str()),
                "case {case:?}: the sentence states the selector and no stream identifier, \
                 but {answered:?} names one"
            );
        }
    }

    // The positive control: a selector that DOES carry its companion member reads the
    // stream, so the rows above refuse an incomplete selector rather than every selector.
    let read = harness
        .get_media(
            &serde_json::json!({ "StreamName": "rejection-live" }),
            serde_json::json!({
                "StartSelectorType": "FRAGMENT_NUMBER",
                "AfterFragmentNumber": on_stream,
            }),
        )
        .await;
    assert_eq!(
        status(&read),
        200,
        "case \"FRAGMENT_NUMBER carrying a fragment number on the stream\": expected HTTP 200, \
         got {} ({})",
        status(&read),
        error_code(&read)
    );
}

/// RULE 2: a value a modelled constraint does not admit is refused in the shared AWS
/// envelope, stating the member path and the requirement and no value the caller sent, with
/// a `@pattern` quoted as this API publishes it.
#[tokio::test]
async fn every_refused_value_is_stated_as_the_published_requirement_and_no_value() {
    let harness = Harness::new();
    harness.create_stream("rejection-constraints").await;
    harness.produce(Some("rejection-constraints"), None, MEDIA);

    // The expressions this API publishes for the four pattern-constrained members of
    // `GetMedia`'s input, one per shape: `StreamName`, `ResourceARN`,
    // `FragmentNumberString` and `ContinuationToken`
    // (`smithy_query(query="closure", operation="GetMedia", model_path="services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/smithy-model/com-amazonaws-kinesisvideomedia-awsacuityinletservice.json")`
    // lists the trait each one carries; the text below is what the API's reference
    // publishes for that shape). Written out here rather than read off the handler's own
    // table, so a test re-asserting that table could not follow its drift.
    let published_stream_name = "[a-zA-Z0-9_.-]+";
    let published_stream_arn =
        r"arn:[a-z\d-]+:kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+";
    let published_fragment_number = "^[0-9]+$";
    let published_continuation_token = r"^[a-zA-Z0-9_\.\-]+$";

    let earliest = serde_json::json!({ "StartSelectorType": "EARLIEST" });

    // (case, the whole request body, the member path, the published expression)
    //
    // Several offending values per member: the clause states no value, so every row of one
    // member answers the very same sentence — which a message that interpolated the value
    // could not do.
    let rows: Vec<(&str, serde_json::Value, &str, &str)> = vec![
        (
            "a stream name carrying a space and a bang",
            serde_json::json!({
                "StreamName": "not a valid stream name!",
                "StartSelector": earliest.clone(),
            }),
            "streamName",
            published_stream_name,
        ),
        (
            "a stream name carrying a slash",
            serde_json::json!({
                "StreamName": "streams/one",
                "StartSelector": earliest.clone(),
            }),
            "streamName",
            published_stream_name,
        ),
        (
            "a stream name carrying a colon",
            serde_json::json!({
                "StreamName": "stream:one",
                "StartSelector": earliest.clone(),
            }),
            "streamName",
            published_stream_name,
        ),
        (
            "an ARN of another service",
            serde_json::json!({
                "StreamARN": "arn:aws:not-a-real-service:us-east-1:123456789012:whatever",
                "StartSelector": earliest.clone(),
            }),
            "streamARN",
            published_stream_arn,
        ),
        (
            "an ARN with no resource path",
            serde_json::json!({
                "StreamARN": "arn:aws:kinesisvideo:us-east-1:123456789012:stream",
                "StartSelector": earliest.clone(),
            }),
            "streamARN",
            published_stream_arn,
        ),
        (
            "a value that is no ARN at all",
            serde_json::json!({
                "StreamARN": "rejection-constraints",
                "StartSelector": earliest.clone(),
            }),
            "streamARN",
            published_stream_arn,
        ),
        (
            "a fragment number that is not numeric",
            serde_json::json!({
                "StreamName": "rejection-constraints",
                "StartSelector": {
                    "StartSelectorType": "FRAGMENT_NUMBER",
                    "AfterFragmentNumber": "not-a-fragment-number",
                },
            }),
            "startSelector.afterFragmentNumber",
            published_fragment_number,
        ),
        (
            "a fragment number carrying a space",
            serde_json::json!({
                "StreamName": "rejection-constraints",
                "StartSelector": {
                    "StartSelectorType": "FRAGMENT_NUMBER",
                    "AfterFragmentNumber": "12 34",
                },
            }),
            "startSelector.afterFragmentNumber",
            published_fragment_number,
        ),
        (
            "a continuation token carrying a character the pattern excludes",
            serde_json::json!({
                "StreamName": "rejection-constraints",
                "StartSelector": {
                    "StartSelectorType": "CONTINUATION_TOKEN",
                    "ContinuationToken": "token/one",
                },
            }),
            "startSelector.continuationToken",
            published_continuation_token,
        ),
        (
            "a continuation token carrying a space",
            serde_json::json!({
                "StreamName": "rejection-constraints",
                "StartSelector": {
                    "StartSelectorType": "CONTINUATION_TOKEN",
                    "ContinuationToken": "token one",
                },
            }),
            "startSelector.continuationToken",
            published_continuation_token,
        ),
    ];

    for (case, body, member_path, published) in &rows {
        let response = harness.call("/getMedia", body).await;
        assert_eq!(
            status(&response),
            400,
            "case {case:?}: expected HTTP 400, got {} ({})",
            status(&response),
            error_code(&response)
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            error_code(&response)
        );
        let expected = one_violation(member_path, &must_match(published));
        let answered = message(&response);
        assert_eq!(
            answered, expected,
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
    }

    // The other constraint classes of the same envelope: their requirement text is the
    // framework's own, so what is asserted here is the half this endpoint owns — the clause
    // names the member and carries no value the request sent.
    //
    // `StreamName` is `@length(min: 1, max: 256)` and `StartSelector$StartSelectorType` is
    // an enumerated type of six values (`smithy_query(query="closure",
    // operation="GetMedia")`), so a name one character past the maximum and a type outside
    // the set are each one violation of a class that is not `@pattern`.
    let over_maximum = "n".repeat(257);
    let outside_the_set = "SOMETIME_SOON";
    let other_classes: Vec<(&str, serde_json::Value, &str, String)> = vec![
        (
            "a stream name one character past the modelled maximum",
            serde_json::json!({
                "StreamName": over_maximum.clone(),
                "StartSelector": earliest.clone(),
            }),
            "streamName",
            over_maximum.clone(),
        ),
        (
            "a selector type outside the modelled set",
            serde_json::json!({
                "StreamName": "rejection-constraints",
                "StartSelector": { "StartSelectorType": outside_the_set },
            }),
            "startSelector.startSelectorType",
            outside_the_set.to_string(),
        ),
    ];

    for (case, body, member_path, submitted) in &other_classes {
        let response = harness.call("/getMedia", body).await;
        assert_eq!(
            status(&response),
            400,
            "case {case:?}: expected HTTP 400, got {} ({})",
            status(&response),
            error_code(&response)
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            error_code(&response)
        );
        let answered = message(&response);
        let opening = one_violation(member_path, "");
        assert!(
            answered.starts_with(&opening),
            "case {case:?}: expected a clause opening {opening:?}, got {answered:?}"
        );
        assert!(
            !answered.contains(submitted.as_str()),
            "case {case:?}: the clause states no value the request sent, but {answered:?} \
             carries {submitted:?}"
        );
    }

    // The positive control: a value every constraint admits is read rather than refused, so
    // the rows above refuse what the model excludes and not every request.
    let read = harness
        .get_media(
            &serde_json::json!({ "StreamName": "rejection-constraints" }),
            earliest,
        )
        .await;
    assert_eq!(
        status(&read),
        200,
        "case \"a request every constraint admits\": expected HTTP 200, got {} ({})",
        status(&read),
        error_code(&read)
    );
    assert!(
        !read.body().is_empty(),
        "case \"a request every constraint admits\": it reads the seeded chunk, so the payload \
         must carry bytes"
    );
}
