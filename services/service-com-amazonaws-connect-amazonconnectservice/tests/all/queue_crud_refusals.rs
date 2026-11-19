// SPDX-License-Identifier: CC0-1.0

//! How the Amazon Connect queue plane refuses a request it cannot serve, driven over
//! HTTP through the same `MultiServiceRouter` the emulator serves with.
//!
//! Three rules, each asserted over a TABLE of inputs rather than one call:
//!
//! 1. INPUT BEFORE STATE. A path label that is not an identifier the service accepts is
//!    refused with the operation's modelled `InvalidParameterException` (HTTP 400,
//!    `x-amzn-errortype: InvalidParameterException`) whose `Message` is `Invalid
//!    parameter: <member>` — and it is refused BEFORE anything is resolved, so the
//!    answer does not depend on what the store holds. The form a label must have is the
//!    one every Connect identifier has: the `8-4-4-4-12` hex groups `CreateInstance` and
//!    `CreateQueue` hand back.
//! 2. A VALUE PAST A MODELLED LENGTH BOUND. `CreateQueue`'s `Name` targets a shape
//!    bounded at 127 characters; a longer value is `InvalidParameterException` with `The
//!    value for QueueName must be less than or equal to 127`, the service's own name for
//!    that parameter and the bound the request broke.
//! 3. A RESOURCE THAT IS NOT THERE. Once the request is acceptable, an identifier that
//!    resolves to nothing is `ResourceNotFoundException` (404) carrying this service's
//!    one not-found sentence, the fixed `Resource not found` — the same words for an
//!    `InstanceId` that resolves to nothing and for a queue missing from an instance that
//!    IS there. `instance_not_found_refusal` asserts that rule across the whole service;
//!    the rows here pin it against the two refusals above, which is where the queue plane
//!    used to answer the wrong one.

use serde_json::{json, Value};

use super::harness::{connect_managed, Connect};

/// A queue identifier of the right form that names no queue. Used wherever a row has to
/// get PAST label validation to reach the lookup.
const ABSENT_QUEUE: &str = "11111111-2222-3333-4444-555555555555";

/// An `HoursOfOperationId` for a `CreateQueue` body: the member is `@required` and
/// unconstrained, and a queue is stored with whatever it names.
const HOURS_OF_OPERATION: &str = "22222222-3333-4444-5555-666666666666";

/// The `Message` member of an answered error — the member name the model gives it. A
/// body that carries no `Message` at all reports as the empty string, which no rule here
/// accepts.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// A valid `CreateQueue` body naming `name`.
fn queue_body(name: &str) -> Value {
    json!({ "Name": name, "HoursOfOperationId": HOURS_OF_OPERATION })
}

/// One request as a suite sends it: the operation's name, and the HTTP method, URI and
/// body it travels as.
type Call = (&'static str, &'static str, String, Option<Value>);

/// The four queue operations that take an `InstanceId` path label, as a request against
/// `instance_id`: the operation's name, and the method, URI and body it is sent as (the
/// `http` bindings of this service's model,
/// `smithy_query(query="operation",operation="<Name>")`).
fn queue_calls_on_instance(instance_id: &str) -> Vec<Call> {
    vec![
        (
            "CreateQueue",
            "PUT",
            format!("/queues/{instance_id}"),
            Some(queue_body("support")),
        ),
        (
            "DescribeQueue",
            "GET",
            format!("/queues/{instance_id}/{ABSENT_QUEUE}"),
            None,
        ),
        (
            "DeleteQueue",
            "DELETE",
            format!("/queues/{instance_id}/{ABSENT_QUEUE}"),
            None,
        ),
        (
            "ListQueues",
            "GET",
            format!("/queues-summary/{instance_id}"),
            None,
        ),
    ]
}

/// The two queue operations that take a `QueueId` path label as well.
fn queue_calls_on_queue(
    instance_id: &str,
    queue_id: &str,
) -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "DescribeQueue",
            "GET",
            format!("/queues/{instance_id}/{queue_id}"),
        ),
        (
            "DeleteQueue",
            "DELETE",
            format!("/queues/{instance_id}/{queue_id}"),
        ),
    ]
}

/// RULE 1, on the `InstanceId` label: a label that is not an identifier of the form this
/// service hands out is `InvalidParameterException` (400) naming `InstanceId`, for every
/// queue operation — and the answer is the same whether the store holds an instance or
/// not, because the label is checked before anything is resolved.
///
/// The table spans the ways a label can fail to be one: a name, digits, a truncated
/// identifier, a value inside the modelled `InstanceId` `@length` bound (1..100
/// characters) at both of its ends, and one past that bound.
#[tokio::test]
async fn an_instance_label_that_is_not_an_identifier_is_an_invalid_parameter() {
    let connect = Connect::new();
    // A live instance stands beside the refused rows, so none of them can pass merely
    // because the store is empty.
    connect
        .create_instance(connect_managed("queue-labels"))
        .await;

    let cases: Vec<(&str, String)> = vec![
        ("one character, the length lower bound", "a".to_string()),
        ("a name, not an identifier", "my-instance".to_string()),
        ("digits only", "9876543210".to_string()),
        ("a truncated identifier", ABSENT_QUEUE[..8].to_string()),
        ("100 characters, the length upper bound", "i".repeat(100)),
        ("101 characters, one past that bound", "i".repeat(101)),
    ];

    for (case, instance_id) in cases {
        for (operation, method, uri, body) in queue_calls_on_instance(&instance_id) {
            let answer = connect.send_answer(method, &uri, body).await;
            let actual = message_of(&answer.body);
            assert_eq!(
                answer.status, 400,
                "case {case:?}/{operation}: expected HTTP 400, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, "InvalidParameterException",
                "case {case:?}/{operation}: expected error code InvalidParameterException, \
                 got {:?} {}",
                answer.error_code, answer.body
            );
            assert_eq!(
                actual, "Invalid parameter: InstanceId",
                "case {case:?}/{operation}: expected message \
                 \"Invalid parameter: InstanceId\", got {actual:?}"
            );
        }
    }
}

/// RULE 1, the accepting side: a label that IS an identifier of that form is acceptable,
/// so it reaches the lookup and is answered as a resource that is not there.
#[tokio::test]
async fn an_instance_label_of_the_identifier_form_reaches_the_lookup() {
    let connect = Connect::new();
    connect
        .create_instance(connect_managed("queue-labels-ok"))
        .await;

    let cases: Vec<(&str, String)> = vec![
        ("a minted identifier", ABSENT_QUEUE.to_string()),
        (
            "another minted identifier",
            "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
        ),
        (
            "the same form in upper case",
            ABSENT_QUEUE.to_ascii_uppercase(),
        ),
    ];

    for (case, instance_id) in cases {
        for (operation, method, uri, body) in queue_calls_on_instance(&instance_id) {
            let answer = connect.send_answer(method, &uri, body).await;
            let actual = message_of(&answer.body);
            // The label reached the lookup and resolved nothing, so the answer is this
            // service's not-found sentence, which quotes nothing the request carried.
            let expected = "Resource not found";
            assert_eq!(
                answer.status, 404,
                "case {case:?}/{operation}: expected HTTP 404, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, "ResourceNotFoundException",
                "case {case:?}/{operation}: expected error code ResourceNotFoundException, \
                 got {:?} {}",
                answer.error_code, answer.body
            );
            assert_eq!(
                actual, expected,
                "case {case:?}/{operation}: expected message {expected:?}, got {actual:?}"
            );
            assert!(
                !answer.body.to_string().contains(&instance_id),
                "case {case:?}/{operation}: the refusal quotes no identifier, got {}",
                answer.body
            );
        }
    }
}

/// RULE 1, on the `QueueId` label: a label that is not an identifier of the form the
/// service mints — the only form a stored queue can be keyed by, so a queue ARN is not
/// one of them — is `InvalidParameterException` (400) naming `QueueId`, including when
/// the instance named beside it does not exist, which is the case that used to be
/// answered as a missing resource.
///
/// The table spans the ways a label can fail to be an identifier: a name, a truncated
/// id, a non-hex character and the wrong group layout.
#[tokio::test]
async fn a_queue_label_that_is_not_an_identifier_is_an_invalid_parameter() {
    let connect = Connect::new();
    let (live_instance, _) = connect.create_instance(connect_managed("queue-ids")).await;
    let absent_instance = "33333333-4444-5555-6666-777777777777";
    assert_ne!(
        absent_instance, live_instance,
        "the row has to name an instance the store does not hold"
    );

    let cases: Vec<(&str, String)> = vec![
        ("a name, not an identifier", "support".to_string()),
        ("a truncated identifier", ABSENT_QUEUE[..8].to_string()),
        (
            "a non-hex character in the first group",
            "1111111z-2222-3333-4444-555555555555".to_string(),
        ),
        (
            "the right characters, the wrong groups",
            "11111111-2222-3333-4444555555555555".to_string(),
        ),
    ];

    for (case, queue_id) in cases {
        // Both an instance that exists and one that does not: the label is refused
        // before either is resolved, so the two answer alike.
        for (instance_case, instance_id) in [
            ("a live instance", live_instance.as_str()),
            ("an absent instance", absent_instance),
        ] {
            for (operation, method, uri) in queue_calls_on_queue(instance_id, &queue_id) {
                let answer = connect.send_answer(method, &uri, None).await;
                let actual = message_of(&answer.body);
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}/{instance_case}/{operation}: expected HTTP 400, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code, "InvalidParameterException",
                    "case {case:?}/{instance_case}/{operation}: expected error code \
                     InvalidParameterException, got {:?} {}",
                    answer.error_code, answer.body
                );
                assert_eq!(
                    actual, "Invalid parameter: QueueId",
                    "case {case:?}/{instance_case}/{operation}: expected message \
                     \"Invalid parameter: QueueId\", got {actual:?}"
                );
            }
        }
    }
}

/// RULE 1, when BOTH labels are unacceptable: the refusal names the first member the
/// operation takes, so a caller is told about the label nearest the front of the request
/// rather than a different one each time.
#[tokio::test]
async fn two_unacceptable_labels_are_refused_on_the_first_of_them() {
    let connect = Connect::new();
    let not_an_identifier = "my-instance";
    for (operation, method, uri) in queue_calls_on_queue(not_an_identifier, "support") {
        let answer = connect.send_answer(method, &uri, None).await;
        let actual = message_of(&answer.body);
        assert_eq!(
            answer.status, 400,
            "case {operation}: expected HTTP 400, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            actual, "Invalid parameter: InstanceId",
            "case {operation}: expected the refusal to name InstanceId, got {actual:?}"
        );
    }
}

/// RULE 1, the accepting side for `QueueId`: the identifier `CreateQueue` hands back
/// resolves the queue it named, so a created queue round-trips through `DescribeQueue`
/// and stops resolving once `DeleteQueue` has taken it.
#[tokio::test]
async fn the_minted_identifier_resolves_the_queue_it_names() {
    let connect = Connect::new();
    let (instance_id, _) = connect.create_instance(connect_managed("queue-forms")).await;

    for case in ["support", "billing"] {
        let created = connect
            .send_answer(
                "PUT",
                &format!("/queues/{instance_id}"),
                Some(queue_body(case)),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateQueue got {} {}",
            created.status, created.body
        );
        let queue_id = created.body["QueueId"]
            .as_str()
            .expect("QueueId")
            .to_string();
        let queue_arn = created.body["QueueArn"]
            .as_str()
            .expect("QueueArn")
            .to_string();

        let read = connect
            .send_answer("GET", &format!("/queues/{instance_id}/{queue_id}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeQueue got {} {}",
            read.status, read.body
        );
        assert_eq!(
            read.body["Queue"]["QueueId"],
            json!(queue_id),
            "case {case:?}: DescribeQueue reported {}",
            read.body
        );
        assert_eq!(
            read.body["Queue"]["QueueArn"],
            json!(queue_arn),
            "case {case:?}: DescribeQueue reported {}",
            read.body
        );
        assert_eq!(
            read.body["Queue"]["Name"],
            json!(case),
            "case {case:?}: DescribeQueue reported {}",
            read.body
        );

        let deleted = connect
            .send_answer("DELETE", &format!("/queues/{instance_id}/{queue_id}"), None)
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteQueue got {} {}",
            deleted.status, deleted.body
        );
        // The delete really removed it: the same read is now the not-found refusal.
        let gone = connect
            .send_answer("GET", &format!("/queues/{instance_id}/{queue_id}"), None)
            .await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: a deleted queue is gone, got {} {}",
            gone.status, gone.body
        );
        assert_eq!(
            message_of(&gone.body),
            "Resource not found",
            "case {case:?}: a deleted queue is reported in this service's one not-found \
             sentence, got {}",
            gone.body
        );
    }
}

/// RULE 2: a `Name` longer than the bound its target shape declares is
/// `InvalidParameterException` (400) stating the parameter the service publishes for
/// that member and the bound the value broke; a `Name` inside the bound is stored.
///
/// The table walks the bound itself: one character, the 127-character maximum
/// (`CommonNameLength127`, from
/// `smithy_query(query="operation",operation="CreateQueue")`), one past it, and far past
/// it — plus the empty value at the other end of the same bound.
#[tokio::test]
async fn a_queue_name_outside_its_modelled_length_bound_is_refused() {
    let connect = Connect::new();
    let (instance_id, _) = connect.create_instance(connect_managed("queue-names")).await;

    // (case, name, expected refusal message — None where the name is acceptable)
    let cases: Vec<(&str, String, Option<String>)> = vec![
        ("one character, the lower bound", "q".to_string(), None),
        ("127 characters, the upper bound", "n".repeat(127), None),
        (
            "128 characters, one past the bound",
            "n".repeat(128),
            Some("The value for QueueName must be less than or equal to 127".to_string()),
        ),
        (
            "1000 characters",
            "n".repeat(1000),
            Some("The value for QueueName must be less than or equal to 127".to_string()),
        ),
        (
            "empty, one short of the lower bound",
            String::new(),
            Some("The value for QueueName must be greater than or equal to 1".to_string()),
        ),
    ];

    for (case, name, refusal) in cases {
        let answer = connect
            .send_answer(
                "PUT",
                &format!("/queues/{instance_id}"),
                Some(queue_body(&name)),
            )
            .await;
        match refusal {
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the name to be accepted, got {} {}",
                    answer.status, answer.body
                );
            }
            Some(expected) => {
                let actual = message_of(&answer.body);
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected HTTP 400, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code, "InvalidParameterException",
                    "case {case:?}: expected error code InvalidParameterException, got {:?} {}",
                    answer.error_code, answer.body
                );
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected message {expected:?}, got {actual:?}"
                );
            }
        }
    }

    // A refused create stored nothing: every queue the instance holds is one the
    // accepted rows created, so no refusal left a record behind.
    let listed = connect
        .send_answer("GET", &format!("/queues-summary/{instance_id}"), None)
        .await;
    let names: Vec<String> = listed.body["QueueSummaryList"]
        .as_array()
        .expect("QueueSummaryList")
        .iter()
        .map(|summary| summary["Name"].as_str().unwrap_or_default().to_string())
        .collect();
    for name in &names {
        assert!(
            name.chars().count() <= 127,
            "a refused create must store nothing, but the instance holds a queue named \
             {name:?}: {}",
            listed.body
        );
    }
}
