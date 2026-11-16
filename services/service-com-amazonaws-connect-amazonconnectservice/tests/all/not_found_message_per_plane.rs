// SPDX-License-Identifier: CC0-1.0

//! Which sentence a Connect `ResourceNotFoundException` carries, driven over HTTP through
//! the same `MultiServiceRouter` the emulator serves with.
//!
//! RULE: Connect words its not-found refusal per resource plane. When the resource that
//! resolves to nothing is the INSTANCE the request's `InstanceId` names, the `Message` is
//! `Instance <InstanceId> not found`, whose one variable part is the identifier the
//! request's path label carried. A plane with no sentence of its own — a resource inside
//! an instance that IS there, or a phone number this account has not claimed — carries
//! the resource-agnostic `Resource not found`, which names no resource kind and quotes no
//! identifier. The status (404) and the error code are the same either way, so the
//! sentence is the only thing that tells a caller WHICH resource was reported gone.
//!
//! Two suites: the first crosses the planes with the operations that reach them, the
//! second walks the forms an `InstanceId` label can take and pins the message's variable
//! part to the label as the request carried it.

use serde_json::{json, Value};

use super::harness::{connect_managed, Connect};

/// An identifier of the form this service mints that names nothing. Used wherever a row
/// has to reach a lookup — the queue plane refuses a label of any other form before it
/// resolves anything.
const ABSENT_ID: &str = "11111111-2222-3333-4444-555555555555";

/// The sentence a plane with no wording of its own carries, written out here rather than
/// read off the implementation so the suite pins the text itself.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The instance plane's sentence as the template it is: the fixed `Instance ` /
/// ` not found` halves around the identifier the request addressed.
fn instance_not_found(instance_id: &str) -> String {
    format!("Instance {instance_id} not found")
}

/// The `Message` member of an answered error — the member name the model gives it.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// One request that resolves nothing: the plane it addresses, the operation's name, and
/// the HTTP method and URI it travels as (the `http` bindings of this service's model,
/// `smithy_query(query="operation",operation="<Name>")`).
struct Row {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    expected: String,
}

/// RULE: the sentence follows the plane of the resource that resolved to nothing — the
/// instance plane names the instance asked for, every other plane carries the
/// resource-agnostic sentence.
///
/// The rows cross the planes with the operations that reach them: the instance plane
/// through a read, a delete, a list and a configuration read of an absent instance; the
/// resources inside a LIVE instance through their own reads; and the phone-number plane
/// through a read and a release of a number this account never claimed.
#[tokio::test]
async fn the_not_found_sentence_is_written_per_resource_plane() {
    let connect = Connect::new();
    // A live instance, so a child-resource row really reaches the child lookup rather
    // than being refused on the instance.
    let (live_id, _) = connect.create_instance(connect_managed("planes")).await;
    assert_ne!(live_id, ABSENT_ID, "the absent id must name no instance");

    let absent_instance = instance_not_found(ABSENT_ID);
    let rows: Vec<Row> = vec![
        Row {
            operation: "DescribeInstance",
            method: "GET",
            uri: format!("/instance/{ABSENT_ID}"),
            body: None,
            expected: absent_instance.clone(),
        },
        Row {
            operation: "DeleteInstance",
            method: "DELETE",
            uri: format!("/instance/{ABSENT_ID}"),
            body: None,
            expected: absent_instance.clone(),
        },
        Row {
            operation: "ListQueues",
            method: "GET",
            uri: format!("/queues-summary/{ABSENT_ID}"),
            body: None,
            expected: absent_instance.clone(),
        },
        Row {
            operation: "DescribeAttachedFilesConfiguration",
            method: "GET",
            uri: format!("/attached-files-configurations/{ABSENT_ID}/CHAT"),
            body: None,
            expected: absent_instance.clone(),
        },
        Row {
            operation: "CreateQueue",
            method: "PUT",
            uri: format!("/queues/{ABSENT_ID}"),
            body: Some(json!({ "Name": "support", "HoursOfOperationId": ABSENT_ID })),
            expected: absent_instance.clone(),
        },
        Row {
            operation: "DescribeQueue (a live instance, an absent queue)",
            method: "GET",
            uri: format!("/queues/{live_id}/{ABSENT_ID}"),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeContactFlow (a live instance, an absent flow)",
            method: "GET",
            uri: format!("/contact-flows/{live_id}/{ABSENT_ID}"),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeHoursOfOperation (a live instance, absent hours)",
            method: "GET",
            uri: format!("/hours-of-operations/{live_id}/{ABSENT_ID}"),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeInstanceStorageConfig (a live instance, no association)",
            method: "GET",
            uri: format!(
                "/instance/{live_id}/storage-config/{ABSENT_ID}?resourceType=CALL_RECORDINGS"
            ),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribePhoneNumber (a number nobody claimed)",
            method: "GET",
            uri: format!("/phone-number/{ABSENT_ID}"),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "ReleasePhoneNumber (a number nobody claimed)",
            method: "DELETE",
            uri: format!("/phone-number/{ABSENT_ID}"),
            body: None,
            expected: RESOURCE_NOT_FOUND.to_string(),
        },
    ];

    for row in rows {
        let answer = connect.send_answer(row.method, &row.uri, row.body).await;
        let actual = message_of(&answer.body);
        let case = row.operation;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ResourceNotFoundException",
            "case {case:?}: expected error code ResourceNotFoundException, got {:?} {}",
            answer.error_code, answer.body
        );
        assert_eq!(
            actual, row.expected,
            "case {case:?}: expected message {:?}, got {actual:?}",
            row.expected
        );
        // A plane with no sentence of its own quotes nothing the request carried, so the
        // live instance's identifier never reaches a reply either way.
        if row.expected == RESOURCE_NOT_FOUND {
            assert!(
                !answer.body.to_string().contains(ABSENT_ID)
                    && !answer.body.to_string().contains(&live_id),
                "case {case:?}: this plane's refusal quotes no identifier, got {}",
                answer.body
            );
        }
    }

    // Every row above left the live instance alone: it is still served.
    let read = connect
        .send_answer("GET", &format!("/instance/{live_id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "the live instance is still readable, got {} {}",
        read.status, read.body
    );
}

/// RULE (the variable part): the identifier in the instance plane's sentence is the
/// request's `InstanceId` label as it was carried — not normalised, not a value the
/// service chose, and different for different labels.
///
/// The rows walk the forms a label can take inside the identifier's modelled `@length`
/// bound (`InstanceId`, 1..100, from
/// `smithy_query(query="operation",operation="DescribeInstance")`): the minted UUID shape
/// and the same shape in upper case, both ends of that bound, a name, digits only and a
/// prefix of a live instance's own identifier.
#[tokio::test]
async fn the_instance_sentence_quotes_the_label_the_request_carried() {
    let connect = Connect::new();
    let (live_id, _) = connect.create_instance(connect_managed("labels")).await;

    let minted = "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30";
    let labels: Vec<(&str, String)> = vec![
        ("the UUID shape Connect mints", minted.to_string()),
        ("the same shape in upper case", minted.to_ascii_uppercase()),
        ("one character, the length lower bound", "a".to_string()),
        ("100 characters, the length upper bound", "i".repeat(100)),
        ("a name, not an identifier", "support".to_string()),
        ("digits only", "9876543210".to_string()),
        (
            "a prefix of the live instance's identifier",
            live_id[..live_id.len() - 1].to_string(),
        ),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (case, label) in labels {
        assert_ne!(
            label, live_id,
            "case {case:?}: the row has to name an instance the store does not hold"
        );
        let answer = connect
            .send_answer("GET", &format!("/instance/{label}"), None)
            .await;
        let actual = message_of(&answer.body);
        let expected = instance_not_found(&label);
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            actual, expected,
            "case {case:?}: expected message {expected:?}, got {actual:?}"
        );
        assert!(
            !seen.contains(&actual),
            "case {case:?}: a different label reports a different message, got {actual:?} \
             again"
        );
        seen.push(actual);
    }

    // The instance that IS there is still read, not refused: the sentence above belongs
    // to the labels that resolve nothing.
    let read = connect
        .send_answer("GET", &format!("/instance/{live_id}"), None)
        .await;
    assert_eq!(
        read.body["Instance"]["Id"].as_str(),
        Some(live_id.as_str()),
        "the live instance still reads back, got {}",
        read.body
    );
}
