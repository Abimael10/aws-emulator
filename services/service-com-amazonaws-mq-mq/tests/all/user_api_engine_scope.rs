// SPDX-License-Identifier: CC0-1.0

//! WHICH BROKERS Amazon MQ's per-user API applies to, driven end to end through the
//! router.
//!
//! RULE: the per-user API — `CreateUser`, `DescribeUser`, `UpdateUser` and `DeleteUser`,
//! each documented as acting on "an ActiveMQ user" — addresses the users of an ActiveMQ
//! broker. A broker of any other engine the model enumerates holds no user the API can
//! address, so every one of those four calls against it is refused with the modelled
//! `ConflictException` (HTTP 409) carrying the service's own fixed sentence
//! (`state::USER_API_ACTIVEMQ_ONLY_MESSAGE`), and the broker is left exactly as it was:
//! the same `users` list, no user carrying a pending change, and the broker still
//! RUNNING.
//!
//! The table spans what the rule generalises over: every engine the model enumerates
//! (`ENGINE_TYPE_VALUES` — `state::user_api_applies` is the predicate under test, so the
//! engine is never spelled out in an assertion) crossed with all four operations of the
//! API, each driven on a RUNNING broker. On the engine the API does apply to, the same
//! four calls are SERVED and read back through `DescribeUser` and `DescribeBroker`, so
//! the refusal is shown to be the engine's and not a dead per-user API.
//!
//! The wire harness is shared with [`crate::broker_lifecycle`], so every call goes
//! through the same `MultiServiceRouter::dispatch` the front door uses, and the wait for
//! a broker to reach RUNNING is [`crate::broker_restart`]'s poll for that CONDITION.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{
    user_api_applies, BROKER_STATE_RUNNING, CHANGE_TYPE_DELETE, CHANGE_TYPE_UPDATE,
    USER_API_ACTIVEMQ_ONLY_MESSAGE,
};

use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::broker_restart::poll_until_state;
use crate::user_deletion::broker_users;

/// The administrative user every broker in this suite is created with.
const ADMIN: &str = "sim2realadmin";

/// A second username the API is asked to add, which exists on no broker to start with.
const ADDED: &str = "second-operator";

/// A password satisfying every rule `User$Password` documents.
const GOOD_PASSWORD: &str = "GoodPassword12";

/// One call of the per-user API: the operation, the HTTP method and URI the model binds
/// it to, the username it addresses and the body it carries.
struct UserCall {
    operation: &'static str,
    method: &'static str,
    username: &'static str,
    body: Option<Value>,
}

/// The four operations of the per-user API, each as the model binds it:
/// `GET`/`PUT`/`DELETE`/`POST` on `/v1/brokers/{BrokerId}/users/{Username}`.
fn user_api_calls() -> Vec<UserCall> {
    vec![
        UserCall {
            operation: "DescribeUser",
            method: "GET",
            username: ADMIN,
            body: None,
        },
        UserCall {
            operation: "UpdateUser",
            method: "PUT",
            username: ADMIN,
            body: Some(json!({"consoleAccess": true, "groups": ["operators"]})),
        },
        UserCall {
            operation: "CreateUser",
            method: "POST",
            username: ADDED,
            body: Some(json!({"password": GOOD_PASSWORD})),
        },
        UserCall {
            operation: "DeleteUser",
            method: "DELETE",
            username: ADDED,
            body: None,
        },
    ]
}

/// A RUNNING broker of `engine_type` holding exactly [`ADMIN`].
async fn running_broker(mq: &Mq, engine_type: &str, name: &str) -> String {
    let mut body = create_body(name);
    body["engineType"] = json!(engine_type);
    body["users"] = json!([{"username": ADMIN, "password": GOOD_PASSWORD}]);
    let (status, created) = mq.create_broker(body).await;
    assert_eq!(
        status, 200,
        "case {engine_type:?}: expected 200 from create-broker, got {status} {created:?}"
    );
    let broker_id = string_at(&created, "/brokerId");
    poll_until_state(mq, &broker_id, BROKER_STATE_RUNNING).await;
    broker_id
}

/// RULE: on a broker whose engine the per-user API does not apply to, all four user
/// operations are refused with `ConflictException` (409) and the service's fixed
/// sentence, and nothing about the broker's users changes.
#[tokio::test]
async fn the_per_user_api_is_refused_on_a_broker_it_does_not_apply_to() {
    let mq = Mq::new("us-east-1");

    let mut rows = 0usize;
    for (index, engine_type) in ENGINE_TYPE_VALUES.iter().enumerate() {
        if user_api_applies(engine_type) {
            continue;
        }
        let broker_id = running_broker(&mq, engine_type, &format!("user-api-refused-{index}")).await;

        for call in user_api_calls() {
            let case = format!("{} on a {engine_type} broker", call.operation);
            let (status, code, answer) = mq
                .call_with_code(
                    call.method,
                    &format!("/v1/brokers/{broker_id}/users/{}", call.username),
                    call.body.clone(),
                )
                .await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected 409, got {status} {code:?} {answer:?}"
            );
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?} {answer:?}"
            );
            assert_eq!(
                string_at(&answer, "/message"),
                USER_API_ACTIVEMQ_ONLY_MESSAGE,
                "case {case:?}: expected the service's own sentence, got {answer:?}"
            );
            rows += 1;
        }

        // The broker is left exactly as it was: the one user it was created with, with
        // no change staged on it, and the broker still RUNNING.
        let reported = broker_users(&mq, &broker_id).await;
        assert_eq!(
            reported,
            vec![(ADMIN.to_string(), None)],
            "case {engine_type:?}: a refused call changes no user of the broker, got \
             {reported:?}"
        );
        let (status, described) = mq.describe_broker(&broker_id).await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: expected 200 from the read, got {status} {described:?}"
        );
        assert_eq!(
            string_at(&described, "/brokerState"),
            BROKER_STATE_RUNNING,
            "case {engine_type:?}: the refusals leave the broker running, got {described:?}"
        );
    }

    let expected_rows = ENGINE_TYPE_VALUES
        .iter()
        .filter(|engine_type| !user_api_applies(engine_type))
        .count()
        * user_api_calls().len();
    assert_eq!(
        rows, expected_rows,
        "every operation of the API must be covered on every engine it does not apply \
         to, covered {rows}"
    );
}

/// The other half of the same rule: on a broker whose engine the per-user API DOES apply
/// to, those same four calls are served and what they change reads back — so the refusal
/// above belongs to the engine and not to a per-user API that answers nobody.
#[tokio::test]
async fn the_per_user_api_is_served_on_a_broker_it_applies_to() {
    let mq = Mq::new("eu-west-1");

    let mut rows = 0usize;
    for (index, engine_type) in ENGINE_TYPE_VALUES.iter().enumerate() {
        if !user_api_applies(engine_type) {
            continue;
        }
        let broker_id = running_broker(&mq, engine_type, &format!("user-api-served-{index}")).await;

        for call in user_api_calls() {
            let case = format!("{} on a {engine_type} broker", call.operation);
            let (status, code, answer) = mq
                .call_with_code(
                    call.method,
                    &format!("/v1/brokers/{broker_id}/users/{}", call.username),
                    call.body.clone(),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {code:?} {answer:?}"
            );
            rows += 1;
        }

        // What `UpdateUser` changed is on the stored user, and the broker still owes the
        // change: the members sent carry their new values, and the user reports the
        // pending UPDATE.
        let (status, admin) = mq
            .call("GET", &format!("/v1/brokers/{broker_id}/users/{ADMIN}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: expected the updated user to read back, got {status} \
             {admin:?}"
        );
        for (pointer, expected) in [
            ("/username", json!(ADMIN)),
            ("/consoleAccess", json!(true)),
            ("/groups", json!(["operators"])),
            ("/pending/pendingChange", json!(CHANGE_TYPE_UPDATE)),
        ] {
            assert_eq!(
                admin.pointer(pointer).cloned().unwrap_or(Value::Null),
                expected,
                "case {engine_type:?}: expected {pointer} {expected:?}, got {admin:?}"
            );
        }

        // And the user `CreateUser` added, then `DeleteUser` asked for, is still served
        // with the deletion staged on it — the broker applies it on its next reboot.
        let (status, added) = mq
            .call("GET", &format!("/v1/brokers/{broker_id}/users/{ADDED}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: expected the added user to read back, got {status} {added:?}"
        );
        assert_eq!(
            added.pointer("/pending/pendingChange").and_then(Value::as_str),
            Some(CHANGE_TYPE_DELETE),
            "case {engine_type:?}: expected the staged deletion, got {added:?}"
        );

        // The broker reports both users and the change each owes.
        let reported = broker_users(&mq, &broker_id).await;
        assert_eq!(
            reported,
            vec![
                (ADDED.to_string(), Some(CHANGE_TYPE_DELETE.to_string())),
                (ADMIN.to_string(), Some(CHANGE_TYPE_UPDATE.to_string())),
            ],
            "case {engine_type:?}: expected the broker to report both users and their \
             staged changes, got {reported:?}"
        );
    }

    let expected_rows = ENGINE_TYPE_VALUES
        .iter()
        .filter(|engine_type| user_api_applies(engine_type))
        .count()
        * user_api_calls().len();
    assert_eq!(
        rows, expected_rows,
        "every operation of the API must be covered on every engine it applies to, \
         covered {rows}"
    );
}

/// RULE: a member `UpdateUser` does not carry is not part of the change — the stored user
/// keeps the value it had — and a member it carries replaces that value, whatever it was.
#[tokio::test]
async fn update_user_changes_only_the_members_it_carries() {
    let mq = Mq::new("us-west-2");
    let engine_type = ENGINE_TYPE_VALUES
        .iter()
        .find(|engine_type| user_api_applies(engine_type))
        .expect("an engine whose brokers hold users");
    let broker_id = running_broker(&mq, engine_type, "user-api-partial").await;

    // Rows: each member of `UpdateUser` sent on its own, then several at once, then a
    // request that carries none of them at all — the user is read back after each.
    let rows: &[(&str, Value, Value)] = &[
        (
            "consoleAccess alone",
            json!({"consoleAccess": true}),
            json!({"consoleAccess": true, "groups": [], "replicationUser": false}),
        ),
        (
            "groups alone",
            json!({"groups": ["admins", "operators"]}),
            json!({"consoleAccess": true, "groups": ["admins", "operators"], "replicationUser": false}),
        ),
        (
            "replicationUser alone",
            json!({"replicationUser": true}),
            json!({"consoleAccess": true, "groups": ["admins", "operators"], "replicationUser": true}),
        ),
        (
            "every member at once",
            json!({"consoleAccess": false, "groups": [], "replicationUser": false, "password": "AnotherPass34"}),
            json!({"consoleAccess": false, "groups": [], "replicationUser": false}),
        ),
        (
            "no member at all",
            json!({}),
            json!({"consoleAccess": false, "groups": [], "replicationUser": false}),
        ),
    ];

    for (case, change, expected) in rows {
        let (status, code, answer) = mq
            .call_with_code(
                "PUT",
                &format!("/v1/brokers/{broker_id}/users/{ADMIN}"),
                Some(change.clone()),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {code:?} {answer:?}"
        );
        let (status, user) = mq
            .call("GET", &format!("/v1/brokers/{broker_id}/users/{ADMIN}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the user to read back, got {status} {user:?}"
        );
        for (member, want) in expected.as_object().expect("an object of members") {
            assert_eq!(
                user.pointer(&format!("/{member}")).cloned().unwrap_or(Value::Null),
                *want,
                "case {case:?}: expected {member} {want:?}, got {user:?}"
            );
        }
    }

    // A `Password` the member's own rules reject is refused, and the user is left as it
    // was — the update is answered, not half-applied.
    let (status, code, answer) = mq
        .call_with_code(
            "PUT",
            &format!("/v1/brokers/{broker_id}/users/{ADMIN}"),
            Some(json!({"password": "short", "consoleAccess": true})),
        )
        .await;
    assert_eq!(
        status, 400,
        "a refused password: expected 400, got {status} {code:?} {answer:?}"
    );
    let (_, user) = mq
        .call("GET", &format!("/v1/brokers/{broker_id}/users/{ADMIN}"), None)
        .await;
    assert_eq!(
        user.pointer("/consoleAccess"),
        Some(&json!(false)),
        "a refused update changes nothing about the user, got {user:?}"
    );

    // A username the broker does not hold is the missing resource it is, and creates
    // nothing.
    let (status, code, answer) = mq
        .call_with_code(
            "PUT",
            &format!("/v1/brokers/{broker_id}/users/nobody"),
            Some(json!({"consoleAccess": true})),
        )
        .await;
    assert_eq!(
        status, 404,
        "an unknown username: expected 404, got {status} {code:?} {answer:?}"
    );
    assert_eq!(
        code, "NotFoundException",
        "an unknown username: expected NotFoundException, got {code:?} {answer:?}"
    );
    let reported = broker_users(&mq, &broker_id).await;
    assert_eq!(
        reported.len(),
        1,
        "an update of a user that does not exist creates none, got {reported:?}"
    );
}
