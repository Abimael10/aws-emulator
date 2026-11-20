// SPDX-License-Identifier: CC0-1.0

//! Which sentence a Connect `ResourceNotFoundException` carries, driven over HTTP through
//! the same `MultiServiceRouter` the emulator serves with.
//!
//! RULE: a Connect `ResourceNotFoundException` states the sentence of the resource the
//! operation ADDRESSED: an operation whose addressed resource is an instance names that
//! instance — `Instance <instanceId> not found`, whose one variable part is the identifier the
//! request carried — while an operation that could not resolve a resource INSIDE an instance,
//! or a phone number, states the fixed `Resource not found`, with no variable part at all, so
//! nothing the request carried is quoted back. The error shape and the status (404) are the
//! same either way.
//!
//! Two suites: the first crosses the resource planes with the operations that reach them,
//! which is what shows which sentence follows the addressed resource; the second walks the
//! forms an `InstanceId` label can take and pins that the instance sentence carries the label
//! and nothing else.

use serde_json::{json, Value};

use super::harness::{connect_managed, Connect};

/// An identifier of the form this service mints that names nothing. Used wherever a row
/// has to reach a lookup — the queue plane refuses a label of any other form before it
/// resolves anything.
const ABSENT_ID: &str = "11111111-2222-3333-4444-555555555555";

/// The sentence the resources INSIDE an instance and the phone-number inventory carry,
/// written out here rather than read off the implementation so the suite pins the text itself.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The sentence an operation whose addressed resource IS an instance carries, for the
/// instance `instance_id` it could not resolve.
fn instance_not_found(instance_id: &str) -> String {
    format!("Instance {instance_id} not found")
}

/// The `Message` member of an answered error — the member name the model gives it.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// One request that resolves nothing: the operation's name, the HTTP method and URI
/// it travels as (the `http` bindings of this service's model,
/// `smithy_query(query="operation",operation="<Name>")`), and the sentence its addressed
/// resource is reported missing with.
struct Row {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    message: String,
}

/// RULE: the sentence follows the resource the operation ADDRESSED — the instance plane names
/// the instance, every resource inside an instance and the phone-number inventory state the
/// fixed sentence.
///
/// The rows cross the planes with the operations that reach them: the instance itself
/// through a read, a delete, a list and a configuration read of an absent instance; the
/// resources inside a LIVE instance through their own reads; and the phone-number
/// inventory through a read and a release of a number this account never claimed.
#[tokio::test]
async fn the_not_found_sentence_follows_the_resource_the_operation_addressed() {
    let connect = Connect::new();
    // A live instance, so a child-resource row really reaches the child lookup rather
    // than being refused on the instance.
    let (live_id, _) = connect.create_instance(connect_managed("planes")).await;
    assert_ne!(live_id, ABSENT_ID, "the absent id must name no instance");

    let rows: Vec<Row> = vec![
        Row {
            operation: "DescribeInstance",
            method: "GET",
            uri: format!("/instance/{ABSENT_ID}"),
            body: None,
            message: instance_not_found(ABSENT_ID),
        },
        Row {
            operation: "DeleteInstance",
            method: "DELETE",
            uri: format!("/instance/{ABSENT_ID}"),
            body: None,
            message: instance_not_found(ABSENT_ID),
        },
        Row {
            operation: "ListQueues",
            method: "GET",
            uri: format!("/queues-summary/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeAttachedFilesConfiguration",
            method: "GET",
            uri: format!("/attached-files-configurations/{ABSENT_ID}/CHAT"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "CreateQueue",
            method: "PUT",
            uri: format!("/queues/{ABSENT_ID}"),
            body: Some(json!({ "Name": "support", "HoursOfOperationId": ABSENT_ID })),
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeQueue (an absent instance)",
            method: "GET",
            uri: format!("/queues/{ABSENT_ID}/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DeleteQueue (an absent instance)",
            method: "DELETE",
            uri: format!("/queues/{ABSENT_ID}/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeQueue (a live instance, an absent queue)",
            method: "GET",
            uri: format!("/queues/{live_id}/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeContactFlow (a live instance, an absent flow)",
            method: "GET",
            uri: format!("/contact-flows/{live_id}/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeHoursOfOperation (a live instance, absent hours)",
            method: "GET",
            uri: format!("/hours-of-operations/{live_id}/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribeInstanceStorageConfig (a live instance, no association)",
            method: "GET",
            uri: format!(
                "/instance/{live_id}/storage-config/{ABSENT_ID}?resourceType=CALL_RECORDINGS"
            ),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "DescribePhoneNumber (a number nobody claimed)",
            method: "GET",
            uri: format!("/phone-number/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
        Row {
            operation: "ReleasePhoneNumber (a number nobody claimed)",
            method: "DELETE",
            uri: format!("/phone-number/{ABSENT_ID}"),
            body: None,
            message: RESOURCE_NOT_FOUND.to_string(),
        },
    ];

    for row in rows {
        let answer = connect.send_answer(row.method, &row.uri, row.body).await;
        let actual = message_of(&answer.body);
        let case = row.operation;
        let expected = row.message;
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
            actual, expected,
            "case {case:?}: expected message {expected:?}, got {actual:?}"
        );
        // The instance sentence carries the addressed instance and nothing else; the fixed
        // sentence carries no identifier at all — and neither reports the LIVE instance a
        // child row resolved through.
        assert!(
            !answer.body.to_string().contains(&live_id),
            "case {case:?}: this refusal quotes no other identifier, got {}",
            answer.body
        );
        if expected == RESOURCE_NOT_FOUND {
            assert!(
                !answer.body.to_string().contains(ABSENT_ID),
                "case {case:?}: this refusal quotes no identifier, got {}",
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

/// RULE (the instance plane's variable part): the sentence names the `InstanceId` label the
/// request carried, whatever form that label takes, and carries nothing else.
///
/// The rows walk the forms a label can take inside the identifier's modelled `@length`
/// bound (`InstanceId`, 1..100, from
/// `smithy_query(query="operation",operation="DescribeInstance")`): the minted UUID shape
/// and the same shape in upper case, both ends of that bound, a name, digits only and a
/// prefix of a live instance's own identifier.
#[tokio::test]
async fn the_instance_sentence_names_the_label_the_request_carried() {
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
