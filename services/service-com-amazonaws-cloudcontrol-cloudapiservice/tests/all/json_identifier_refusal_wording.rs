// SPDX-License-Identifier: CC0-1.0

//! The refusal an `Identifier` given in its JSON form carries when it names no identifier of
//! the resource type.
//!
//! The rule under test: "primary identifiers can be specified as a string or JSON"
//! (`smithy_query(query="doc",shape="GetResource",member="Identifier")`), and a JSON object
//! must name exactly the identifier the type's registry schema declares and nothing beside
//! it. One that names a property which identifies no resource of the type, names fewer or
//! more properties than the identifier is made of, or carries an extra key is refused with
//! `ValidationException` whose message is one fixed template:
//!
//! ```text
//! When specifying the identifier as JSON, you must specify exactly one identifier from
//! [[<pointers>]], and no additional properties can be specified
//! ```
//!
//! The pointer list is the per-type fill-in, taken from the type's `primaryIdentifier`; every
//! other word of it is fixed, the closing clause included — it is what states what is
//! disallowed.
//!
//! The rows differ along what the rule generalises over: which type's identifier is named
//! (one made of a single property, one made of several), which operation the reference was
//! handed to, and HOW the object fails to name that identifier.

use serde_json::json;

use super::emulator::{code, message, Emulator};
/// A type whose primary identifier is one property of its schema.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// A type whose primary identifier is a COMBINATION of two of them.
const LOG_STREAM: &str = "AWS::Logs::LogStream";

/// The error code such a refusal carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The template the refusal states, around the pointers of the identifier the type declares.
fn json_identifier_message(pointers: &str) -> String {
    format!(
        "When specifying the identifier as JSON, you must specify exactly one identifier \
         from [[{pointers}]], and no additional properties can be specified"
    )
}

#[tokio::test]
async fn a_json_identifier_naming_no_identifier_of_the_type_is_refused_with_the_template() {
    let emulator = Emulator::with_cloudwatch_logs();
    let group = "addressed-group";
    emulator
        .provision(LOG_GROUP, &json!({ "LogGroupName": group }))
        .await;
    emulator
        .provision(
            LOG_STREAM,
            &json!({ "LogGroupName": group, "LogStreamName": "host-1" }),
        )
        .await;

    // The pointers each type's `primaryIdentifier` declares, which is the one part of the
    // template taken from the type.
    let group_pointers = "/properties/LogGroupName";
    let stream_pointers = "/properties/LogGroupName, /properties/LogStreamName";

    // (case, type, the reference, the pointers the refusal lists)
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "a property that identifies no resource of the type",
            LOG_GROUP,
            json!({ "RetentionInDays": 7 }).to_string(),
            group_pointers,
        ),
        (
            "the identifier and one property beside it",
            LOG_GROUP,
            json!({ "LogGroupName": group, "RetentionInDays": 7 }).to_string(),
            group_pointers,
        ),
        (
            "an object naming nothing at all",
            LOG_GROUP,
            json!({}).to_string(),
            group_pointers,
        ),
        (
            "one part of a combined identifier missing",
            LOG_STREAM,
            json!({ "LogGroupName": group }).to_string(),
            stream_pointers,
        ),
        (
            "both parts and an extra key",
            LOG_STREAM,
            json!({ "LogGroupName": group, "LogStreamName": "host-1", "Extra": "x" })
                .to_string(),
            stream_pointers,
        ),
    ];

    for (case, type_name, reference, pointers) in &rows {
        // Every operation that takes an `Identifier` reads it the same way, so the refusal is
        // the same whichever of them the reference was handed to.
        let answers = [
            ("GetResource", emulator.get(type_name, reference).await),
            (
                "UpdateResource",
                emulator
                    .update(
                        type_name,
                        reference,
                        &json!([{ "op": "add", "path": "/RetentionInDays", "value": 7 }]),
                        None,
                    )
                    .await,
            ),
            (
                "DeleteResource",
                emulator.delete(type_name, reference).await,
            ),
        ];
        for (operation, (status, body)) in answers {
            assert_eq!(
                status, 400,
                "case {case:?} on {operation}: expected HTTP 400, got {status} with body \
                 {body:?}"
            );
            assert!(
                code(&body).contains(VALIDATION_EXCEPTION),
                "case {case:?} on {operation}: expected {VALIDATION_EXCEPTION}, got {:?}",
                code(&body)
            );
            assert_eq!(
                message(&body),
                json_identifier_message(pointers),
                "case {case:?} on {operation}: expected the whole template, got {:?}",
                message(&body)
            );
        }
    }

    // The control: an object that DOES name the type's identifier resolves the resource, so
    // each refusal above is about the object and not about the JSON form itself.
    for (case, type_name, reference, expected) in [
        (
            "the one property a single identifier is made of",
            LOG_GROUP,
            json!({ "LogGroupName": group }).to_string(),
            group.to_string(),
        ),
        (
            "the pointer spelling of that same property",
            LOG_GROUP,
            json!({ "/properties/LogGroupName": group }).to_string(),
            group.to_string(),
        ),
        (
            "both properties a combined identifier is made of",
            LOG_STREAM,
            json!({ "LogGroupName": group, "LogStreamName": "host-1" }).to_string(),
            format!("{group}|host-1"),
        ),
    ] {
        let (status, body) = emulator.get(type_name, &reference).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an object naming the identifier must resolve, got {status} with \
             body {body:?}"
        );
        assert_eq!(
            body["ResourceDescription"]["Identifier"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the resource the object named, got {body:?}"
        );
    }
}
