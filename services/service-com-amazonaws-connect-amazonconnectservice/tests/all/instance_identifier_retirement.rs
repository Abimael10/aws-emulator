// SPDX-License-Identifier: CC0-1.0

//! Which not-found sentence Connect's instance plane states for an `InstanceId` that
//! resolves to no instance, driven over HTTP through the same `MultiServiceRouter` the
//! emulator serves with.
//!
//! RULE: for an operation whose addressed resource IS an instance, the modelled
//! `ResourceNotFoundException` (HTTP 404,
//! `smithy_query(query="operation",operation="DescribeInstance,DeleteInstance")`) is worded by
//! what the account knows of the identifier: an identifier the account was NEVER given states
//! the service's fixed not-found sentence, `Resource not found`, which has no variable part
//! and so quotes nothing the request carried, while an identifier the account HELD until its
//! `DeleteInstance` retired it names the instance that is gone,
//! `Instance <instanceId> not found`, whose one variable part is the `InstanceId` the request
//! carried in its path label.
//!
//! The suites assert that rule over tables: the never-allocated half over the forms and
//! lengths an `InstanceId` label can take, crossed with both operations of the plane; the
//! retired half over several instances of one account, each named by its own identifier and
//! none by another's; and the two halves side by side in one store, so neither sentence can
//! pass by being the only one the service knows.

use serde_json::Value;

use super::harness::{connect_managed, Connect};

/// The sentence this service states for a resource this account never held, written out here
/// rather than read off the implementation so the suite pins the text itself.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The sentence the instance plane states for an instance this account held until a delete
/// retired it. Its one variable part is the identifier the request carried.
fn instance_not_found(instance_id: &str) -> String {
    format!("Instance {instance_id} not found")
}

/// The two operations whose addressed resource IS an instance, with the HTTP method each
/// travels as over the one URI the model binds for both
/// (`smithy_query(query="operation",operation="DescribeInstance,DeleteInstance")` ->
/// `/instance/{InstanceId}`).
const INSTANCE_PLANE: [(&str, &str); 2] = [("DescribeInstance", "GET"), ("DeleteInstance", "DELETE")];

/// The `Message` member of an answered error — the member name the model gives it.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// Send both instance-plane operations against `instance_id` and assert each answers 404
/// `ResourceNotFoundException` carrying `expected`.
///
/// `quotes_the_identifier` says whether the sentence has the identifier as its variable part;
/// when it does not, the reply must not carry the identifier anywhere at all, which is what
/// "no variable part" means on the wire.
async fn assert_plane_refuses(
    connect: &Connect,
    case: &str,
    instance_id: &str,
    expected: &str,
    quotes_the_identifier: bool,
) {
    for (operation, method) in INSTANCE_PLANE {
        let answer = connect
            .send_answer(method, &format!("/instance/{instance_id}"), None)
            .await;
        let actual = message_of(&answer.body);
        assert_eq!(
            answer.status, 404,
            "case {case:?}/{operation}: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ResourceNotFoundException",
            "case {case:?}/{operation}: expected error code ResourceNotFoundException, got {:?}",
            answer.error_code
        );
        assert_eq!(
            actual, expected,
            "case {case:?}/{operation}: expected message {expected:?}, got {actual:?}"
        );
        // Only a label long enough not to occur inside the sentence by accident is checked
        // for being quoted back: a one-character label is a letter of the sentence itself.
        if !quotes_the_identifier && instance_id.len() >= 4 {
            assert!(
                !answer.body.to_string().contains(instance_id),
                "case {case:?}/{operation}: this refusal quotes no identifier, got {}",
                answer.body
            );
        }
    }
}

/// RULE (the never-allocated half): an `InstanceId` this account was never given is refused
/// with the fixed sentence, whatever form and length the label has, and the reply carries
/// nothing the request sent.
///
/// The rows differ along the FORM and LENGTH of the identifier: the UUID shape Connect mints
/// and that shape in upper case, both ends of the identifier's modelled `@length` bound
/// (`InstanceId`, 1..100), a name, digits only, a prefix of a UUID, and a prefix of the LIVE
/// instance's own identifier — each crossed with both operations of the plane.
#[tokio::test]
async fn an_identifier_the_account_never_held_states_the_fixed_sentence() {
    let connect = Connect::new();
    // A live instance stands beside the absent identifiers, so no row can pass merely
    // because the store is empty.
    let (live_id, _) = connect.create_instance(connect_managed("neighbour")).await;

    let minted = "7f3c9b21-0d4e-4a86-9c15-2b6f8e5d1a30";
    let rows: Vec<(&str, String)> = vec![
        ("the UUID shape Connect mints", minted.to_string()),
        ("the same shape in upper case", minted.to_ascii_uppercase()),
        ("one character, the modelled minimum length", "a".to_string()),
        ("100 characters, the modelled maximum length", "i".repeat(100)),
        ("a name, not an identifier", "support".to_string()),
        ("digits only", "9876543210".to_string()),
        ("a prefix of a UUID-shaped identifier", "7f3c9b21".to_string()),
        (
            "a prefix of the live instance's identifier",
            live_id[..live_id.len() - 1].to_string(),
        ),
    ];

    for (case, instance_id) in rows {
        assert_ne!(
            instance_id, live_id,
            "case {case:?}: the row has to name an instance the store does not hold"
        );
        assert_plane_refuses(&connect, case, &instance_id, RESOURCE_NOT_FOUND, false).await;
    }

    // Every row above left the live instance alone: the plane still serves it.
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

/// RULE (the retired half): an identifier this account held until `DeleteInstance` retired it
/// names THAT instance, and keeps naming it however often it is asked for.
///
/// The rows differ along the COUNT and the identity of the retired instances: several
/// instances of one account are created and deleted, and each one's refusal has to carry its
/// own identifier and no other's — which is what makes the sentence's variable part the
/// identifier the REQUEST carried rather than anything the store picked.
#[tokio::test]
async fn an_identifier_a_delete_retired_names_that_instance() {
    let connect = Connect::new();
    // One instance stays live throughout, so a retired identifier's sentence cannot be the
    // only answer this plane knows.
    let (live_id, _) = connect.create_instance(connect_managed("survivor")).await;

    let mut retired: Vec<String> = Vec::new();
    for index in 0..3 {
        let (instance_id, _) = connect
            .create_instance(connect_managed(&format!("retired{index}")))
            .await;
        let deleted = connect
            .send_answer("DELETE", &format!("/instance/{instance_id}"), None)
            .await;
        assert_eq!(
            deleted.status, 200,
            "DeleteInstance of the live instance {instance_id}: got {} {}",
            deleted.status, deleted.body
        );
        retired.push(instance_id);
    }

    for (index, instance_id) in retired.iter().enumerate() {
        let case = format!("the identifier of retired instance {index}");
        let expected = instance_not_found(instance_id);
        // Asked for twice: the identifier stays retired, so the answer does not change.
        for attempt in 0..2 {
            let case = format!("{case}, attempt {attempt}");
            assert_plane_refuses(&connect, &case, instance_id, &expected, true).await;
        }
        // The sentence names the identifier the request carried and no other retired one.
        for other in retired.iter().filter(|other| *other != instance_id) {
            assert!(
                !expected.contains(other.as_str()),
                "case {case:?}: the sentence must name only the identifier the request \
                 carried, got {expected:?} which carries {other:?}"
            );
        }
    }

    // The live instance is untouched by every refusal above.
    let read = connect
        .send_answer("GET", &format!("/instance/{live_id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "the live instance is still readable, got {} {}",
        read.status, read.body
    );
}

/// RULE (both halves in one store): the two classes are told apart by the account's own
/// history of the identifier, not by its form — a retired identifier and a never-allocated
/// one of the SAME minted form, asked for in the same store, get the two different sentences.
#[tokio::test]
async fn the_two_classes_are_told_apart_in_one_store() {
    let connect = Connect::new();
    let (retired_id, _) = connect.create_instance(connect_managed("retired")).await;
    let deleted = connect
        .send_answer("DELETE", &format!("/instance/{retired_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "DeleteInstance of a live instance: got {} {}",
        deleted.status, deleted.body
    );

    // The same minted form as the retired identifier, but an identifier this account was
    // never given.
    let never_allocated = "11111111-2222-3333-4444-555555555555";
    assert_ne!(
        never_allocated, retired_id,
        "the never-allocated identifier must name no instance this account held"
    );

    assert_plane_refuses(
        &connect,
        "an identifier a delete retired",
        &retired_id,
        &instance_not_found(&retired_id),
        true,
    )
    .await;
    assert_plane_refuses(
        &connect,
        "an identifier of the same form the account never held",
        never_allocated,
        RESOURCE_NOT_FOUND,
        false,
    )
    .await;

    // A fresh instance created after the delete is served, not refused: retirement is about
    // one identifier, not about the plane.
    let (fresh_id, _) = connect.create_instance(connect_managed("fresh")).await;
    let read = connect
        .send_answer("GET", &format!("/instance/{fresh_id}"), None)
        .await;
    assert_eq!(
        read.body["Instance"]["Id"].as_str(),
        Some(fresh_id.as_str()),
        "the instance created after the delete still reads back, got {}",
        read.body
    );
}
