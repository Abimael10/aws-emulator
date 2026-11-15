// SPDX-License-Identifier: CC0-1.0

//! The ONE rule behind every refusal this service words itself, driven end to end over
//! the service's own `awsJson1_0` wire protocol through the shared
//! [`crate::keyspace_table_lifecycle::Emulator`] harness: a refusal states the FAULT the
//! request hit, in the service's own template for that fault — fixed text plus the values
//! the request supplied and the names the emulator's own state holds, and nothing else.
//!
//! Two consequences of that rule are what this suite pins, each over a SET of inputs:
//!
//! 1. Faults a caller must tell apart are worded apart. One `keyspaceName` reaches three
//!    distinct faults — a create of a name the Region already holds (the modelled
//!    `ConflictException`), a read of a name nothing holds and a DROP of a name nothing
//!    holds (both the modelled `ResourceNotFoundException`) — and each carries its own
//!    template, so no two of the three share a sentence. The rows span the modelled
//!    `keyspaceName` shape (`smithy_query(query="shape", shape="KeyspaceName")`:
//!    `length 1..48`, pattern `^[a-zA-Z0-9][a-zA-Z0-9_]{0,47}$`) — each published bound,
//!    a digit-led name, a name carrying upper case, one carrying underscores, and a prefix
//!    of a name that IS held.
//! 2. A template is FIXED text: the sentence a refusal carries does not grow with the
//!    state behind it. A delete of a user-defined type something still uses is refused in
//!    the same sentence whichever and however many resources refer to the type, so the
//!    sentence stays byte-identical as users are added to the type one at a time, and it
//!    names neither the keyspace nor any sibling resource.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_keyspaces_keyspacesservice::scaffolded;
use service_com_amazonaws_keyspaces_keyspacesservice::state as keyspaces;

use crate::keyspace_table_lifecycle::{assert_ok, error_code, Emulator, REGION};

/// The `message` member of a failure body, as a string.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message member>")
        .to_string()
}

/// RULE 1: the three faults one keyspace name can reach are refused in three templates.
#[tokio::test]
async fn the_three_faults_one_keyspace_name_reaches_are_worded_apart() {
    let emulator = Emulator::start();
    // Names that span the modelled `keyspaceName` shape. `held` is created, `absent` is
    // not; `absent` is derived from `held` so each row's two names are of one form.
    let longest = "k".repeat(48);
    let rows: Vec<(&str, String, String)> = vec![
        ("shortest", "a".to_string(), "z".to_string()),
        (
            "longest",
            longest.clone(),
            format!("absent_{}", "n".repeat(41)),
        ),
        ("digit_led", "0held_ks".to_string(), "9absent_ks".to_string()),
        ("mixed_case", "Held_Ks9".to_string(), "Absent_Ks9".to_string()),
        (
            "underscores",
            "held__ks".to_string(),
            "absent__ks".to_string(),
        ),
        // A prefix of a name that IS held is still a name nothing holds.
        (
            "prefix_of_a_held_name",
            "prefix_holder_ks".to_string(),
            "prefix_holder".to_string(),
        ),
    ];

    for (case, held, absent) in &rows {
        assert_ok(
            case,
            emulator
                .call("CreateKeyspace", json!({"keyspaceName": held}))
                .await,
        );

        // Fault one: the name is already taken in this Region.
        let (status, body) = emulator
            .call("CreateKeyspace", json!({"keyspaceName": held}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (scaffolded::CLIENT_ERROR_STATUS, "ConflictException"),
            "case {case:?}: expected the modelled conflict, got {status} {body}"
        );
        let taken = message(&body);
        assert_eq!(
            taken,
            format!("Cannot add existing keyspace {held}"),
            "case {case:?}: expected the already-taken sentence, got {taken:?}"
        );

        // Fault two: a read of a name nothing holds.
        let (status, body) = emulator
            .call("GetKeyspace", json!({"keyspaceName": absent}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (scaffolded::CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case:?}: expected the modelled not-found, got {status} {body}"
        );
        let looked_up = message(&body);
        assert_eq!(
            looked_up,
            format!("Keyspace {absent} does not exist"),
            "case {case:?}: expected the failed-lookup sentence, got {looked_up:?}"
        );

        // Fault three: a DROP of a name nothing holds — the same absence as fault two,
        // reached by a request that was trying to remove the keyspace, not read it.
        let (status, body) = emulator
            .call("DeleteKeyspace", json!({"keyspaceName": absent}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (scaffolded::CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case:?}: expected the modelled not-found, got {status} {body}"
        );
        let dropped = message(&body);
        assert_eq!(
            dropped,
            format!("Cannot drop non existing keyspace '{absent}'"),
            "case {case:?}: expected the failed-drop sentence, got {dropped:?}"
        );

        // No two of the three faults share a sentence, so a caller that branches on the
        // refusal text can tell them apart.
        for (left_name, left, right_name, right) in [
            ("already taken", &taken, "failed lookup", &looked_up),
            ("already taken", &taken, "failed drop", &dropped),
            ("failed lookup", &looked_up, "failed drop", &dropped),
        ] {
            assert_ne!(
                left, right,
                "case {case:?}: the {left_name} and the {right_name} faults must not share \
                 one sentence, both read {left:?}"
            );
        }
        // Every sentence is the request's own names and nothing of the caller's
        // credential scope.
        for (fault, sentence) in [
            ("already taken", &taken),
            ("failed lookup", &looked_up),
            ("failed drop", &dropped),
        ] {
            for scoped in [REGION, engine::emulator_account_id()] {
                assert!(
                    !sentence.contains(scoped),
                    "case {case:?}: the {fault} sentence must carry nothing of the \
                     credential scope ({scoped:?}), got {sentence:?}"
                );
            }
        }
    }

    // The refused calls wrote nothing: every keyspace created above is still readable
    // under the name it was created with, and no absent name became readable.
    for (case, held, absent) in &rows {
        let reported = assert_ok(
            case,
            emulator
                .call("GetKeyspace", json!({"keyspaceName": held}))
                .await,
        );
        assert_eq!(
            reported["keyspaceName"],
            json!(held),
            "case {case:?}: a refused create must leave the held keyspace alone, got \
             {reported}"
        );
        let (status, body) = emulator
            .call("GetKeyspace", json!({"keyspaceName": absent}))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (scaffolded::CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case:?}: a refused read or drop must not create the keyspace, got \
             {status} {body}"
        );
    }
}

/// One user a row adds to its type, and the request that adds it.
struct User {
    /// What the user is called in the refusal's own keyspace — a name the sentence must
    /// not disclose.
    name: String,
    /// The operation that creates it, with its request body.
    operation: &'static str,
    request: Value,
}

/// RULE 2: the still-referenced refusal is FIXED text plus the formatted type name, so it
/// stays byte-identical as the type gains users of every kind.
#[tokio::test]
async fn the_still_referenced_sentence_does_not_grow_with_the_users_of_the_type() {
    let emulator = Emulator::start();
    let ks = "fault_template_ks";
    assert_ok(
        ks,
        emulator
            .call("CreateKeyspace", json!({"keyspaceName": ks}))
            .await,
    );

    // The rows differ in the form of the `typeName` the request supplies: an unquoted name
    // the service folds, an already-lower-case one it holds verbatim, and a double-quoted
    // one it preserves letter for letter.
    let submitted_names = ["GrowingUsers", "growing_plain", "\"GrowingQuoted\""];
    for submitted in submitted_names {
        assert_ok(
            submitted,
            emulator
                .call(
                    "CreateType",
                    json!({
                        "keyspaceName": ks,
                        "typeName": submitted,
                        "fieldDefinitions": [{"name": "f", "type": "text"}],
                    }),
                )
                .await,
        );
    }
    // A column or a field may only name a type that is ACTIVE, so wait for the CONDITION
    // the clock owes each type rather than for a span of wall clock.
    for submitted in submitted_names {
        wait_until_type_is_active(&emulator, ks, submitted).await;
    }

    for (row, submitted) in submitted_names.iter().enumerate() {
        let formatted = keyspaces::format_type_name(submitted)
            .expect("every submitted name above is a form a typeName may take");
        // The users are added one at a time, and they differ in kind (a table column, a
        // second table reaching the type through a collection, another type's field) so
        // the refusal is asked again with one, two and three users behind it.
        let users = vec![
            User {
                name: format!("growing_first_tbl_{row}"),
                operation: "CreateTable",
                request: json!({
                    "keyspaceName": ks,
                    "tableName": format!("growing_first_tbl_{row}"),
                    "schemaDefinition": {
                        "allColumns": [
                            {"name": "pk", "type": "text"},
                            {"name": "value", "type": format!("frozen<{submitted}>")},
                        ],
                        "partitionKeys": [{"name": "pk"}],
                    },
                }),
            },
            User {
                name: format!("growing_second_tbl_{row}"),
                operation: "CreateTable",
                request: json!({
                    "keyspaceName": ks,
                    "tableName": format!("growing_second_tbl_{row}"),
                    "schemaDefinition": {
                        "allColumns": [
                            {"name": "pk", "type": "text"},
                            {"name": "value", "type": format!("set<frozen<{submitted}>>")},
                        ],
                        "partitionKeys": [{"name": "pk"}],
                    },
                }),
            },
            User {
                name: format!("growing_parent_type_{row}"),
                operation: "CreateType",
                request: json!({
                    "keyspaceName": ks,
                    "typeName": format!("growing_parent_type_{row}"),
                    "fieldDefinitions": [
                        {"name": "value", "type": format!("list<frozen<{submitted}>>")},
                    ],
                }),
            },
        ];

        let expected = format!("{formatted} is referenced by other UDTs or tables.");
        let mut added: Vec<&str> = Vec::new();
        for user in &users {
            let case = format!("{submitted} with {} user(s)", added.len() + 1);
            assert_ok(&case, emulator.call(user.operation, user.request.clone()).await);
            added.push(&user.name);

            let (status, body) = emulator
                .call(
                    "DeleteType",
                    json!({"keyspaceName": ks, "typeName": submitted}),
                )
                .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (scaffolded::CLIENT_ERROR_STATUS, "ConflictException"),
                "case {case:?}: expected the modelled conflict, got {status} {body}"
            );
            let refusal = message(&body);
            assert_eq!(
                refusal, expected,
                "case {case:?}: the sentence must not vary with the users of the type, got \
                 {refusal:?}"
            );
            // It discloses neither the keyspace nor any user, so it is the fault alone.
            assert!(
                !refusal.contains(ks),
                "case {case:?}: the sentence must not name the keyspace, got {refusal:?}"
            );
            for name in &added {
                assert!(
                    !refusal.contains(name),
                    "case {case:?}: the sentence must not name the user {name:?}, got \
                     {refusal:?}"
                );
            }
        }

        // The refusals wrote nothing: the type is still there, still ACTIVE, and still
        // reports every user the rows added.
        let reported = assert_ok(
            submitted,
            emulator
                .call(
                    "GetType",
                    json!({"keyspaceName": ks, "typeName": submitted}),
                )
                .await,
        );
        assert_eq!(
            reported["status"],
            json!(keyspaces::TYPE_STATUS_ACTIVE),
            "case {submitted:?}: a refused delete must leave the type {}, got {reported}",
            keyspaces::TYPE_STATUS_ACTIVE
        );
        assert_eq!(
            reported["typeName"],
            json!(formatted),
            "case {submitted:?}: expected the formatted type name, got {reported}"
        );
    }
}

/// Wait until Amazon Keyspaces reports the type ACTIVE — the status a type must hold
/// before a column or a field may name it.
///
/// A new type is CREATING until the clock the service stores owes it ACTIVE, so this polls
/// the condition with a deadline far past that span, rather than sleeping a fixed time.
async fn wait_until_type_is_active(emulator: &Emulator, keyspace: &str, type_name: &str) {
    /// How long the wait may take at most: far past the transition the emulator owes the
    /// type, so a loaded machine still reaches it.
    const WITHIN: Duration = Duration::from_secs(30);
    /// How long one look waits before the next.
    const EVERY: Duration = Duration::from_millis(25);

    let deadline = Instant::now() + WITHIN;
    loop {
        let (status, body) = emulator
            .call(
                "GetType",
                json!({"keyspaceName": keyspace, "typeName": type_name}),
            )
            .await;
        if status == 200 && body["status"] == json!(keyspaces::TYPE_STATUS_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {type_name:?}: expected the type to reach {}, last saw {status} {body}",
            keyspaces::TYPE_STATUS_ACTIVE
        );
        tokio::time::sleep(EVERY).await;
    }
}
