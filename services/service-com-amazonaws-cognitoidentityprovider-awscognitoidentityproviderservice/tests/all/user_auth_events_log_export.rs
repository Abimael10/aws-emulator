// SPDX-License-Identifier: CC0-1.0

//! What a user pool with an accepted `userAuthEvents` log delivery WRITES into the
//! destination log group.
//!
//! The rule under test: a pool whose feature plan includes the source and whose threat
//! protection is active exports every event it logs to each destination its
//! `userAuthEvents` configuration names — one record per TERMINATED attempt, carrying
//! `eventSource` `USER_AUTH_EVENTS`, `logSourceId.userPoolId`, and a `message` naming the
//! logged event: its `eventId`, `eventType`, `eventResponse` (`Pass` for a completed attempt,
//! `Fail` for a refused one), the `userName`, `userPoolId` and `clientId` it ran against and
//! the `challenges` that were checked, each as `<ChallengeName>:<ChallengeResponse>`. The
//! configuration is read when the record is written, so clearing `LogConfigurations` stops the
//! export and re-setting it resumes it, and a pool whose threat protection is not active logs
//! nothing and so exports nothing. The other half of the gate — the feature plan the source
//! needs — is what `SetLogDeliveryConfiguration` refuses the configuration itself with, which
//! [`super::feature_plan_gate`] states.
//!
//! The rows span what the rule generalises over rather than the one recorded sign-in: WHICH
//! surface terminated the attempt (the self-service and the administrative sign-in, a
//! self-service registration), WHAT outcome it reached (completed, refused), WHICH
//! destination group the pool points at (one group, another group, none), and WHICH
//! threat-protection mode the pool runs.
//!
//! Both services are mounted on ONE engine exactly as the assembled emulator mounts them,
//! which is what makes the group the pool writes into the group CloudWatch Logs serves
//! `FilterLogEvents` from — the records are read back only through that operation.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::log_delivery_configuration::{call, ok};
use crate::log_delivery_destinations::{
    given_log_group, json_rpc, log_group_arn, router, set,
};

/// The `X-Amz-Target` prefix CloudWatch Logs serves its `awsJson1_1` wire under, and the
/// name its requests are addressed and signed under.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SERVICE: &str = "logs";

/// The one stream of a destination group that carries no exported record: CloudWatch Logs'
/// own vended-delivery machinery writes its subscription validation line there, so a reader
/// of the exported trail skips it.
const VALIDATION_STREAM: &str = "log_stream_created_by_aws_to_validate_log_delivery_subscriptions";

/// A password satisfying a default pool's password policy, and one no user holds.
const GOOD_PASSWORD: &str = "Passw0rd!2024";
const WRONG_PASSWORD: &str = "N0tTheP4ssword!";

/// The `AdvancedSecurityModeType` values threat protection is ON under, and the one it is off
/// under (`smithy_query(query="shape",shape="AdvancedSecurityModeType")`).
const ENFORCED: &str = "ENFORCED";
const AUDIT: &str = "AUDIT";
const OFF: &str = "OFF";

/// The `eventSource` of every exported user-activity record.
const USER_AUTH_EVENTS: &str = "USER_AUTH_EVENTS";

/// The `EventType` values the rows below export, and the outcomes an attempt reaches
/// (`EventResponseType`).
const SIGN_IN: &str = "SignIn";
const SIGN_UP: &str = "SignUp";
const PASS: &str = "Pass";
const FAIL: &str = "Fail";

/// The challenge results a password sign-in records.
const PASSWORD_SUCCESS: &str = "Password:Success";
const PASSWORD_FAILURE: &str = "Password:Failure";

/// One CloudWatch Logs call, requiring success, over the one awsJson caller this crate's
/// suites drive another service's wire with.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) =
        json_rpc(router, LOGS_SERVICE, LOGS_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// Every exported record `log_group` holds, as a reader of the trail sees them: the stream
/// each arrived in and the JSON the record decodes to, in the order `FilterLogEvents`
/// serves them, with the delivery-validation stream skipped.
async fn exported(router: &MultiServiceRouter, log_group: &str) -> Vec<(String, Value)> {
    let answered = logs_ok(
        router,
        "FilterLogEvents",
        &json!({ "logGroupName": log_group }),
    )
    .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|event| {
            event.get("logStreamName").and_then(Value::as_str) != Some(VALIDATION_STREAM)
        })
        .map(|event| {
            let stream = event
                .get("logStreamName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let decoded = serde_json::from_str(message).unwrap_or_else(|error| {
                panic!("an exported record must be JSON ({error}): {message:?}")
            });
            (stream, decoded)
        })
        .collect()
}

/// A pool and its app client, and the user every attempt below is made for.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Create a pool running threat protection in `mode` (and none at all where `mode` is
/// `None`) on `tier`, one app client admitting the password flows, and one `CONFIRMED` user
/// holding [`GOOD_PASSWORD`].
async fn given_pool(
    router: &MultiServiceRouter,
    tier: &str,
    mode: Option<&str>,
    username: &str,
) -> Fixture {
    let mut pool = Map::new();
    pool.insert("PoolName".to_string(), json!("exported-activity-pool"));
    pool.insert("UserPoolTier".to_string(), json!(tier));
    if let Some(mode) = mode {
        pool.insert(
            "UserPoolAddOns".to_string(),
            json!({ "AdvancedSecurityMode": mode }),
        );
    }
    let created = ok(router, "CreateUserPool", &Value::Object(pool)).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "exported-activity-client",
            "GenerateSecret": false,
            "ExplicitAuthFlows": [
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string();
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// Point `user_pool_id`'s `userAuthEvents` delivery at the log group `log_group`, at the
/// level that source's own documentation recommends.
async fn given_delivery_to(router: &MultiServiceRouter, user_pool_id: &str, log_group: &str) {
    let (status, payload) = set(
        router,
        user_pool_id,
        &json!([{
            "LogLevel": "INFO",
            "EventSource": "userAuthEvents",
            "CloudWatchLogsConfiguration": { "LogGroupArn": log_group_arn(log_group) },
        }]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the delivery to {log_group} must be accepted, got {status} with {payload}"
    );
}

/// Clear `user_pool_id`'s log delivery, so it exports nothing.
async fn given_delivery_cleared(router: &MultiServiceRouter, user_pool_id: &str) {
    let (status, payload) = set(router, user_pool_id, &json!([])).await;
    assert_eq!(
        status, 200,
        "clearing the delivery must be accepted, got {status} with {payload}"
    );
}

/// Which surface a row terminates its attempt through.
#[derive(Clone, Copy, Debug)]
enum Attempt {
    /// `InitiateAuth` `USER_PASSWORD_AUTH`, with the stored password or with a wrong one.
    SignIn(bool),
    /// `AdminInitiateAuth` `ADMIN_USER_PASSWORD_AUTH`: the administrative surface of the
    /// same attempt.
    AdminSignIn(bool),
    /// `SignUp`: a self-service registration of another user through the same app client.
    Registration,
}

impl Attempt {
    /// Make this attempt against `fixture` and report the status it was answered with.
    async fn make(self, router: &MultiServiceRouter, fixture: &Fixture, username: &str) -> u16 {
        let (operation, body) = match self {
            Self::SignIn(correct) => (
                "InitiateAuth",
                json!({
                    "ClientId": fixture.client_id,
                    "AuthFlow": "USER_PASSWORD_AUTH",
                    "AuthParameters": {
                        "USERNAME": username,
                        "PASSWORD": if correct { GOOD_PASSWORD } else { WRONG_PASSWORD },
                    },
                }),
            ),
            Self::AdminSignIn(correct) => (
                "AdminInitiateAuth",
                json!({
                    "UserPoolId": fixture.user_pool_id,
                    "ClientId": fixture.client_id,
                    "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                    "AuthParameters": {
                        "USERNAME": username,
                        "PASSWORD": if correct { GOOD_PASSWORD } else { WRONG_PASSWORD },
                    },
                }),
            ),
            Self::Registration => (
                "SignUp",
                json!({
                    "ClientId": fixture.client_id,
                    "Username": username,
                    "Password": GOOD_PASSWORD,
                }),
            ),
        };
        call(router, operation, &body).await.0
    }
}

/// What one row expects the exported record to say about the attempt it made.
#[derive(Debug)]
struct Expected {
    event_type: &'static str,
    event_response: &'static str,
    challenges: Vec<&'static str>,
}

/// Assert that `record` is the record `expected` describes, for the attempt `username` made
/// against `fixture`.
fn assert_record(case: &str, record: &Value, fixture: &Fixture, username: &str, expected: &Expected) {
    let string_at = |pointer: &str| -> Option<String> {
        record
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    assert_eq!(
        string_at("/eventSource").as_deref(),
        Some(USER_AUTH_EVENTS),
        "case {case:?}: expected eventSource {USER_AUTH_EVENTS}, got {record}"
    );
    assert_eq!(
        string_at("/logSourceId/userPoolId").as_deref(),
        Some(fixture.user_pool_id.as_str()),
        "case {case:?}: expected the record to name the pool it is about, got {record}"
    );
    for (member, expected_value) in [
        ("eventType", expected.event_type),
        ("eventResponse", expected.event_response),
        ("userName", username),
        ("userPoolId", fixture.user_pool_id.as_str()),
        ("clientId", fixture.client_id.as_str()),
    ] {
        let found = string_at(&format!("/message/{member}"));
        assert_eq!(
            found.as_deref(),
            Some(expected_value),
            "case {case:?}: expected message.{member} to be {expected_value:?}, got {found:?} \
             in {record}"
        );
    }
    let event_id = string_at("/message/eventId").unwrap_or_default();
    assert!(
        !event_id.is_empty(),
        "case {case:?}: the record must carry the id of the event it exports, got {record}"
    );
    let challenges: Vec<&str> = record
        .pointer("/message/challenges")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        challenges, expected.challenges,
        "case {case:?}: expected the challenges {:?}, got {challenges:?} in {record}",
        expected.challenges
    );
}

/// RULE: every terminated attempt a pool with threat protection active logs is exported as
/// ONE record into the destination its `userAuthEvents` delivery names, whichever surface
/// terminated it, carrying the outcome it reached and the challenges that were checked.
#[tokio::test]
async fn each_terminated_attempt_is_exported_as_one_record() {
    let router = router();
    let log_group = "/aws/vendedlogs/cognito-activity";
    given_log_group(&router, log_group).await;
    let username = "marker-user";
    let fixture = given_pool(&router, "PLUS", Some(AUDIT), username).await;
    given_delivery_to(&router, &fixture.user_pool_id, log_group).await;

    // (case, the attempt, the user it is made for, what the exported record says)
    let rows: Vec<(&str, Attempt, &str, Expected)> = vec![
        (
            "a self-service sign-in with the stored password",
            Attempt::SignIn(true),
            username,
            Expected {
                event_type: SIGN_IN,
                event_response: PASS,
                challenges: vec![PASSWORD_SUCCESS],
            },
        ),
        (
            "a self-service sign-in with a password the user does not hold",
            Attempt::SignIn(false),
            username,
            Expected {
                event_type: SIGN_IN,
                event_response: FAIL,
                challenges: vec![PASSWORD_FAILURE],
            },
        ),
        (
            "an administrative sign-in with the stored password",
            Attempt::AdminSignIn(true),
            username,
            Expected {
                event_type: SIGN_IN,
                event_response: PASS,
                challenges: vec![PASSWORD_SUCCESS],
            },
        ),
        (
            "an administrative sign-in with a password the user does not hold",
            Attempt::AdminSignIn(false),
            username,
            Expected {
                event_type: SIGN_IN,
                event_response: FAIL,
                challenges: vec![PASSWORD_FAILURE],
            },
        ),
        (
            "a self-service registration",
            Attempt::Registration,
            "registered-user",
            Expected {
                event_type: SIGN_UP,
                event_response: PASS,
                challenges: Vec::new(),
            },
        ),
    ];

    let mut event_ids: Vec<String> = Vec::new();
    for (written, (case, attempt, attempted_for, expected)) in rows.into_iter().enumerate() {
        let status = attempt.make(&router, &fixture, attempted_for).await;
        // The outcome the record reports is the one the call reached: a completed attempt is
        // answered, a refused one is an HTTP 400 of this API (`src/service/error_wire.rs`).
        let expected_status = if expected.event_response == PASS { 200 } else { 400 };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected the attempt itself to be answered {expected_status}, got \
             {status}"
        );
        let records = exported(&router, log_group).await;
        assert_eq!(
            records.len(),
            written + 1,
            "case {case:?}: the attempt must add exactly one record, got {records:?}"
        );
        let (stream, record) = &records[written];
        assert_eq!(
            stream, &fixture.user_pool_id,
            "case {case:?}: the records of a pool arrive in that pool's own stream, got \
             {stream:?}"
        );
        assert_record(case, record, &fixture, attempted_for, &expected);
        let event_id = record
            .pointer("/message/eventId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !event_ids.contains(&event_id),
            "case {case:?}: each attempt is its own event, so {event_id:?} must not repeat in \
             {event_ids:?}"
        );
        event_ids.push(event_id);
    }
}

/// RULE: the stored configuration decides where the records go and whether they go at all,
/// read when each record is written — so clearing the delivery stops the export for the
/// attempts made while it is cleared, re-pointing it moves the records to the new group, and
/// a group the pool does not name receives none.
#[tokio::test]
async fn the_stored_delivery_decides_where_each_record_goes() {
    let router = router();
    let first = "/aws/vendedlogs/cognito-first";
    let second = "/aws/vendedlogs/cognito-second";
    given_log_group(&router, first).await;
    given_log_group(&router, second).await;
    let username = "marker-user";
    let fixture = given_pool(&router, "PLUS", Some(AUDIT), username).await;

    // (case, what the delivery is set to before the attempt, the records each group holds
    // after it)
    let rows: [(&str, Option<&str>, usize, usize); 5] = [
        ("delivery pointed at the first group", Some(first), 1, 0),
        ("a second attempt under the same delivery", Some(first), 2, 0),
        ("delivery cleared", None, 2, 0),
        ("delivery re-pointed at the second group", Some(second), 2, 1),
        ("delivery pointed back at the first", Some(first), 3, 1),
    ];

    for (case, destination, expected_first, expected_second) in rows {
        match destination {
            Some(log_group) => given_delivery_to(&router, &fixture.user_pool_id, log_group).await,
            None => given_delivery_cleared(&router, &fixture.user_pool_id).await,
        }
        let status = Attempt::SignIn(true)
            .make(&router, &fixture, username)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the sign-in itself must succeed whatever the delivery is"
        );
        for (log_group, expected) in [(first, expected_first), (second, expected_second)] {
            let records = exported(&router, log_group).await;
            assert_eq!(
                records.len(),
                expected,
                "case {case:?}: expected {log_group} to hold {expected} records, got \
                 {records:?}"
            );
        }
    }
}

/// RULE: the export is gated on the pool's own threat protection — a pool whose
/// `UserPoolAddOns` run it in a mode that logs activity exports its events, and a pool that
/// runs none logs nothing and so exports nothing, however its delivery is configured.
#[tokio::test]
async fn only_a_pool_running_threat_protection_exports_its_activity() {
    let router = router();
    let log_group = "/aws/vendedlogs/cognito-gated";
    given_log_group(&router, log_group).await;

    // (case, the AdvancedSecurityMode the pool runs, whether its activity is exported)
    //
    // The modes are `AdvancedSecurityModeType`'s
    // (`smithy_query(query="shape",shape="AdvancedSecurityModeType")`: AUDIT | ENFORCED |
    // OFF), plus the pool that configures no add-ons at all. Every pool here is on the
    // feature plan the source needs, so what each row turns on is the mode and nothing else.
    let rows: [(&str, Option<&str>, bool); 4] = [
        ("threat protection logging activity", Some(AUDIT), true),
        ("threat protection acting on activity", Some(ENFORCED), true),
        ("threat protection switched off", Some(OFF), false),
        ("no add-ons configured at all", None, false),
    ];

    for (case, mode, exports) in rows {
        let username = "marker-user";
        let fixture = given_pool(&router, "PLUS", mode, username).await;
        given_delivery_to(&router, &fixture.user_pool_id, log_group).await;
        let status = Attempt::SignIn(true)
            .make(&router, &fixture, username)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the sign-in itself must succeed whatever the pool exports"
        );

        let records: Vec<Value> = exported(&router, log_group)
            .await
            .into_iter()
            .map(|(_, record)| record)
            .filter(|record| {
                record.pointer("/message/userPoolId").and_then(Value::as_str)
                    == Some(fixture.user_pool_id.as_str())
            })
            .collect();
        let expected = usize::from(exports);
        assert_eq!(
            records.len(),
            expected,
            "case {case:?}: expected {expected} records for this pool, got {records:?}"
        );
    }
}
