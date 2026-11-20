// SPDX-License-Identifier: CC0-1.0

//! The refusal Connect gives for a request that names a resource this account does not
//! hold, driven over HTTP through the same `MultiServiceRouter` the emulator serves
//! with.
//!
//! RULE: Connect reports the resource the operation ADDRESSED as missing. The service
//! answers HTTP 404 (the model's error binding) with `ResourceNotFoundException`, and the
//! sentence follows that resource: an operation whose addressed resource IS the instance — an
//! `InstanceId` that never named one, or one `DeleteInstance` retired — names it,
//! `Instance <instanceId> not found`, while every operation that resolves the instance as the
//! CONTAINER of the resource it addresses states the fixed `Resource not found`, which names
//! no resource kind and quotes no identifier.
//!
//! The suites below assert that rule over a TABLE: every operation of this service that
//! has to resolve an `InstanceId` crossed with identifiers of several forms and lengths,
//! including both ends of the modelled `InstanceId` length bound and an identifier whose
//! instance really existed until it was deleted. The last suite pins the same sentence on
//! the other side of the lookup, where the instance IS there and the resource inside it
//! is not.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::harness::{connect_managed, Connect};

/// A flow content of the documented form — the JSON string of a flow definition — so a
/// `CreateContactFlow` row reaches the instance lookup rather than content validation.
const FLOW_CONTENT: &str =
    r#"{"Version":"2019-10-30","StartAction":"a1","Actions":[{"Identifier":"a1","Type":"DisconnectParticipant","Parameters":{}}]}"#;

/// An identifier of the right modelled form that names no resource inside an instance.
/// A row that sends one only ever reaches the instance lookup, which is what the rule is
/// about; the suite that needs the child lookup itself sends it against a live instance.
const ABSENT_CHILD: &str = "11111111-2222-3333-4444-555555555555";

/// How long the suite below will keep looking for a new instance to report `ACTIVE`
/// before it calls the wait a failure: a deadline, not a wait — the poll leaves as soon
/// as the status holds. Deliberately far longer than the activation span the store
/// records, so a loaded host cannot fail the run.
const ACTIVATION_DEADLINE: Duration = Duration::from_secs(60);

/// One request that has to resolve an `InstanceId`: the operation's name, and the HTTP
/// method, URI and body it is sent as. The method/URI pairs are the `http` bindings of
/// this service's model (smithy_query(query="operation",operation="<Name>")).
struct Call {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// Every operation this emulator implements that resolves an `InstanceId`, as a request
/// against `instance_id`. Each body is a valid one for its operation, so the instance
/// lookup is what the request is refused on.
fn calls_resolving_an_instance(instance_id: &str) -> Vec<Call> {
    let call = |operation, method, uri: String, body| Call {
        operation,
        method,
        uri,
        body,
    };
    vec![
        call(
            "DescribeInstance",
            "GET",
            format!("/instance/{instance_id}"),
            None,
        ),
        call(
            "DeleteInstance",
            "DELETE",
            format!("/instance/{instance_id}"),
            None,
        ),
        call(
            "ListQueues",
            "GET",
            format!("/queues-summary/{instance_id}"),
            None,
        ),
        call(
            "CreateQueue",
            "PUT",
            format!("/queues/{instance_id}"),
            Some(json!({ "Name": "support", "HoursOfOperationId": ABSENT_CHILD })),
        ),
        call(
            "DescribeQueue",
            "GET",
            format!("/queues/{instance_id}/{ABSENT_CHILD}"),
            None,
        ),
        call(
            "DeleteQueue",
            "DELETE",
            format!("/queues/{instance_id}/{ABSENT_CHILD}"),
            None,
        ),
        call(
            "ListContactFlows",
            "GET",
            format!("/contact-flows-summary/{instance_id}"),
            None,
        ),
        call(
            "CreateContactFlow",
            "PUT",
            format!("/contact-flows/{instance_id}"),
            Some(json!({ "Name": "inbound", "Type": "CONTACT_FLOW", "Content": FLOW_CONTENT })),
        ),
        call(
            "DescribeContactFlow",
            "GET",
            format!("/contact-flows/{instance_id}/{ABSENT_CHILD}"),
            None,
        ),
        call(
            "ListHoursOfOperations",
            "GET",
            format!("/hours-of-operations-summary/{instance_id}"),
            None,
        ),
        call(
            "CreateHoursOfOperation",
            "PUT",
            format!("/hours-of-operations/{instance_id}"),
            Some(json!({
                "Name": "open",
                "TimeZone": "UTC",
                "Config": [{
                    "Day": "MONDAY",
                    "StartTime": { "Hours": 9, "Minutes": 0 },
                    "EndTime": { "Hours": 17, "Minutes": 30 },
                }],
            })),
        ),
        call(
            "DescribeHoursOfOperation",
            "GET",
            format!("/hours-of-operations/{instance_id}/{ABSENT_CHILD}"),
            None,
        ),
        call(
            "AssociateInstanceStorageConfig",
            "PUT",
            format!("/instance/{instance_id}/storage-config"),
            Some(json!({
                "ResourceType": "CALL_RECORDINGS",
                "StorageConfig": {
                    "StorageType": "S3",
                    "S3Config": { "BucketName": "connect-recordings", "BucketPrefix": "calls" },
                },
            })),
        ),
        call(
            "DescribeInstanceStorageConfig",
            "GET",
            format!(
                "/instance/{instance_id}/storage-config/{ABSENT_CHILD}?resourceType=CALL_RECORDINGS"
            ),
            None,
        ),
        call(
            "UpdateAttachedFilesConfiguration",
            "POST",
            format!("/attached-files-configurations/{instance_id}/CHAT"),
            Some(json!({ "MaximumSizeLimitInBytes": 1024 })),
        ),
        call(
            "DescribeAttachedFilesConfiguration",
            "GET",
            format!("/attached-files-configurations/{instance_id}/CHAT"),
            None,
        ),
    ]
}

/// The `Message` member of an answered error — the member name the model gives it —
/// so a row's assertion reports the text rather than the whole envelope.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// The sentence a `ResourceNotFoundException` about a resource INSIDE an instance carries,
/// written out here rather than read off the implementation so the suite pins the text itself.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The sentence a `ResourceNotFoundException` about the INSTANCE ITSELF carries, whose one
/// variable part is the identifier the request carried.
fn instance_not_found(instance_id: &str) -> String {
    format!("Instance {instance_id} not found")
}

/// Whether `operation`'s own addressed resource IS the instance, so a missing instance is
/// reported by naming it rather than with the fixed sentence for a resource inside one.
fn addresses_the_instance_itself(operation: &str) -> bool {
    matches!(operation, "DescribeInstance" | "DeleteInstance")
}

/// Whether an operation checks the FORM of its `InstanceId` label before it resolves
/// anything: the queue plane does, so a label that is not an identifier this service
/// could have minted is a 400 there and never reaches the lookup this suite is about
/// (`queue_crud_refusals` pins that half). Every other operation below resolves the
/// label as sent — the instance plane included: it refuses only a label carrying no
/// identifier at all, or one longer than the modelled bound, neither of which any row
/// here sends (`instance_plane_refusals` pins that half).
fn checks_the_labels_form(operation: &str) -> bool {
    matches!(
        operation,
        "CreateQueue" | "DescribeQueue" | "DeleteQueue" | "ListQueues"
    )
}

/// Drive every instance-resolving call against `instance_id` and assert the rule on each
/// answer: 404, `ResourceNotFoundException`, and the sentence of the resource that operation
/// ADDRESSED — the instance named, or the fixed sentence for a resource inside one.
///
/// `of_the_identifier_form` says whether `instance_id` has the form this service mints;
/// when it does not, the operations that check that form first are left out, since their
/// answer is the parameter refusal rather than the not-found one.
async fn assert_every_call_reports_the_instance_missing(
    connect: &Connect,
    case: &str,
    instance_id: &str,
    of_the_identifier_form: bool,
) {
    for call in calls_resolving_an_instance(instance_id) {
        if !of_the_identifier_form && checks_the_labels_form(call.operation) {
            continue;
        }
        let answer = connect
            .send_answer(call.method, &call.uri, call.body.clone())
            .await;
        let actual = message_of(&answer.body);
        assert_eq!(
            answer.status, 404,
            "case {case:?}/{}: expected HTTP 404, got {} {}",
            call.operation, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ResourceNotFoundException",
            "case {case:?}/{}: expected error code ResourceNotFoundException, got {:?}",
            call.operation, answer.error_code
        );
        let expected = if addresses_the_instance_itself(call.operation) {
            instance_not_found(instance_id)
        } else {
            RESOURCE_NOT_FOUND.to_string()
        };
        assert_eq!(
            actual, expected,
            "case {case:?}/{}: expected message {expected:?}, got {actual:?}",
            call.operation
        );
        // The sentence for a resource inside an instance has no variable part, so the
        // identifier asked for is not quoted back. Only a label long enough not to occur in
        // the sentence by accident is checked.
        if instance_id.len() >= 4 && !addresses_the_instance_itself(call.operation) {
            assert!(
                !answer.body.to_string().contains(instance_id),
                "case {case:?}/{}: the refusal must not quote the identifier the request \
                 carried, got {}",
                call.operation,
                answer.body
            );
        }
    }
}

/// RULE: an `InstanceId` that names no instance is refused with
/// `ResourceNotFoundException` (404) — naming the instance where the addressed resource is the
/// instance itself, and the fixed `Resource not found` where it is a resource inside one — for
/// every operation that resolves an `InstanceId`.
///
/// The table spans the identifier's form and length: the UUID shape Connect mints, both
/// ends of the modelled `InstanceId` length bound (1..100), a mixed-case identifier, one
/// that is a prefix of a UUID, and one carrying the characters an identifier may hold —
/// crossed with every instance-resolving operation. The last element of each row says
/// whether the identifier has the minted form, which decides whether the operations that
/// check that form first are part of the row (see
/// [`assert_every_call_reports_the_instance_missing`]).
#[tokio::test]
async fn an_unknown_instance_id_is_refused_in_awss_sentence() {
    let connect = Connect::new();
    // A live instance stands beside the absent ones, so a row cannot pass merely because
    // the store is empty.
    let (live_id, _) = connect.create_instance(connect_managed("neighbour")).await;

    let longest = "i".repeat(99) + "z"; // InstanceId length bound: 1..100.
    let cases: Vec<(&str, String, bool)> = vec![
        (
            "the UUID shape Connect mints",
            "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30".to_string(),
            true,
        ),
        (
            "the same shape in upper case",
            "7F3C9B21-0D4E-4A86-9C15-2B6F8E5D1A30".to_string(),
            true,
        ),
        (
            "one character, the length lower bound",
            "a".to_string(),
            false,
        ),
        ("100 characters, the length upper bound", longest, false),
        ("mixed case", "AbCdEf-0123".to_string(), false),
        (
            "a prefix of a UUID-shaped identifier",
            "7f3c9b21".to_string(),
            false,
        ),
        (
            "a prefix of the live instance's identifier",
            live_id[..live_id.len() - 1].to_string(),
            false,
        ),
        ("digits only", "9876543210".to_string(), false),
    ];

    for (case, instance_id, of_the_identifier_form) in cases {
        assert_ne!(
            instance_id, live_id,
            "case {case:?}: the row has to name an instance the store does not hold"
        );
        assert_every_call_reports_the_instance_missing(
            &connect,
            case,
            &instance_id,
            of_the_identifier_form,
        )
        .await;
    }
}

/// RULE (the same one, on an identifier that really named an instance): deleting an
/// instance retires its identifier, so every later call carrying it is refused with the
/// same sentence.
#[tokio::test]
async fn a_deleted_instances_id_is_refused_in_awss_sentence() {
    let connect = Connect::new();
    let (instance_id, _) = connect.create_instance(connect_managed("retired")).await;

    // The instance really exists first: it reaches ACTIVE once the activation span the
    // store records falls due. The suite waits for that CONDITION — it polls the read
    // until the status holds, under a deadline far longer than any span the store uses —
    // rather than for a stretch of the clock, so the wait does not depend on how fast the
    // host runs.
    let deadline = Instant::now() + ACTIVATION_DEADLINE;
    loop {
        let read = connect
            .send_answer("GET", &format!("/instance/{instance_id}"), None)
            .await;
        if read.body["Instance"]["InstanceStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the instance has to reach ACTIVE before it is deleted, last read {}",
            read.body
        );
        tokio::task::yield_now().await;
    }

    let deleted = connect
        .send_answer("DELETE", &format!("/instance/{instance_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteInstance of a live instance: got {} {}",
        deleted.status, deleted.body
    );

    assert_every_call_reports_the_instance_missing(
        &connect,
        "a deleted instance's identifier",
        &instance_id,
        true,
    )
    .await;

    // The identifier stays retired: the same refusal answers a second read.
    assert_every_call_reports_the_instance_missing(
        &connect,
        "a deleted instance's identifier, read twice",
        &instance_id,
        true,
    )
    .await;
}

/// RULE (the other side of the lookup): a resource missing from an instance that IS there
/// is the same refusal — `ResourceNotFoundException` (404) carrying the same fixed
/// sentence, which quotes neither the instance nor the resource the request named.
#[tokio::test]
async fn a_present_instance_with_an_absent_child_reports_the_same_sentence() {
    let connect = Connect::new();
    let (instance_id, _) = connect.create_instance(connect_managed("children")).await;

    let cases: Vec<(&str, &str, String)> = vec![
        (
            "DescribeQueue",
            "GET",
            format!("/queues/{instance_id}/{ABSENT_CHILD}"),
        ),
        (
            "DeleteQueue",
            "DELETE",
            format!("/queues/{instance_id}/{ABSENT_CHILD}"),
        ),
        (
            "DescribeContactFlow",
            "GET",
            format!("/contact-flows/{instance_id}/{ABSENT_CHILD}"),
        ),
        (
            "DescribeHoursOfOperation",
            "GET",
            format!("/hours-of-operations/{instance_id}/{ABSENT_CHILD}"),
        ),
        (
            "DescribeInstanceStorageConfig",
            "GET",
            format!(
                "/instance/{instance_id}/storage-config/{ABSENT_CHILD}?resourceType=CALL_RECORDINGS"
            ),
        ),
    ];

    for (case, method, uri) in cases {
        let answer = connect.send_answer(method, &uri, None).await;
        let actual = message_of(&answer.body);
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ResourceNotFoundException",
            "case {case:?}: expected error code ResourceNotFoundException, got {:?}",
            answer.error_code
        );
        assert_eq!(
            actual, RESOURCE_NOT_FOUND,
            "case {case:?}: expected message {RESOURCE_NOT_FOUND:?}, got {actual:?}"
        );
        assert!(
            !answer.body.to_string().contains(ABSENT_CHILD)
                && !answer.body.to_string().contains(&instance_id),
            "case {case:?}: the refusal must quote neither identifier the request \
             carried, got {}",
            answer.body
        );
    }
}
