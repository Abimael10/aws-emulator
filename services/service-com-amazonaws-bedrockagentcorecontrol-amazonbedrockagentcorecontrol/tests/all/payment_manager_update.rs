// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdatePaymentManager` (`PATCH
//! /payments/managers/{paymentManagerId}`), and what a later `GetPaymentManager` reports
//! of what it wrote.
//!
//! The rules under test:
//!
//! * PATCH semantics — "you only need to specify the fields you want to change"
//!   (`smithy_query doc shape=UpdatePaymentManager`): a member the request carries replaces
//!   the stored one, a member it omits leaves the stored one alone, and a member the
//!   update's own response does not declare (the description, the tags, the authorizer
//!   configuration) is reported by the read;
//! * one write at a time — a manager whose own create or update has not settled is still
//!   being written, so a second writer is refused the modelled `ConflictException` (HTTP
//!   409) naming the account, the manager and the version it raced against, and changes
//!   nothing: the manager settles `READY` carrying the values the writer that won
//!   submitted;
//! * an update is asynchronous — it answers 202 with the manager `UPDATING`, and the move
//!   back to `READY` is owed by the clock rather than by the call, so every wait below
//!   POLLS for the status instead of sleeping a fixed span.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised.

use std::sync::Arc;
use std::time::Duration;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every request in this suite is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries, and the one the conflict sentence names.
const ACCOUNT: &str = "000000000000";

/// How long a wait for a clock-owed transition may take before the test gives up. Far
/// longer than the span the service gives the transition, so a loaded host cannot fail
/// this suite; the test waits for the STATUS, never for a span to pass.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How often that wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `PaymentManagerStatus` members this suite expects (`smithy_query
/// shape=PaymentManagerStatus`).
const STATUS_CREATING: &str = "CREATING";
const STATUS_UPDATING: &str = "UPDATING";
const STATUS_READY: &str = "READY";

/// Two IAM role ARNs inside the modelled `RoleArn` pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$` (`smithy_query
/// operation=UpdatePaymentManager`).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/PaymentManagerRole";
const OTHER_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/PaymentManagerRotatedRole";

/// A customer managed KMS key ARN inside the modelled `KmsKeyArn` pattern
/// `^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:123456789012:key/12345678-1234-1234-1234-123456789012";

/// `PaymentsDescription`'s longest published value, `len 1..4096`.
const MAX_DESCRIPTION_LEN: usize = 4096;

/// A `clientToken` inside the modelled `ClientToken` constraints (len 33..256, pattern
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`).
fn client_token(seed: &str) -> String {
    let token = format!("{seed}{}", "0".repeat(40));
    token[..40].to_string()
}

/// The `AuthorizerConfiguration` union's one member, `customJWTAuthorizer`, with a
/// `discoveryUrl` its own pattern admits (`smithy_query
/// shape=CustomJWTAuthorizerConfiguration`).
fn jwt_authorizer() -> Value {
    json!({
        "customJWTAuthorizer": {
            "discoveryUrl": "https://auth.example.com/.well-known/openid-configuration",
            "allowedClients": ["payments-checkout"]
        }
    })
}

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and what
/// the handlers read the ARN Region from.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `CreatePaymentManager`: `POST /payments/managers`.
async fn create_manager(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "POST", "/payments/managers", Some(body)).await
}

/// `UpdatePaymentManager`: `PATCH /payments/managers/{paymentManagerId}`.
async fn update_manager(
    router: &MultiServiceRouter,
    payment_manager_id: &str,
    body: &Value,
) -> (u16, Value) {
    send(
        router,
        "PATCH",
        &format!("/payments/managers/{payment_manager_id}"),
        Some(body),
    )
    .await
}

/// `GetPaymentManager`: `GET /payments/managers/{paymentManagerId}`.
async fn get_manager(router: &MultiServiceRouter, payment_manager_id: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/payments/managers/{payment_manager_id}"),
        None,
    )
    .await
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence a writer refused for a collision reads, for `payment_manager_id` at
/// `version`: fixed text around the account this emulator serves, the manager the request
/// addressed and the version the refused writer raced against.
fn conflict_message(payment_manager_id: &str, version: u64) -> String {
    format!(
        "Conflict while updating payment manager: accountId={ACCOUNT}, \
         managerId={payment_manager_id}, version={version}"
    )
}

/// Create one `AWS_IAM` payment manager and wait for it to settle, so a row can update a
/// manager that is not already being written. Reports its `paymentManagerId`.
async fn settled_manager(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    body: Value,
) -> String {
    let mut request = json!({"name": name, "authorizerType": "AWS_IAM", "roleArn": ROLE_ARN});
    for (member, value) in body.as_object().expect("the row's members are an object") {
        request[member] = value.clone();
    }
    let (status, created) = create_manager(router, &request).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreatePaymentManager, got {status} ({created})"
    );
    assert_eq!(
        text(&created, "status"),
        STATUS_CREATING,
        "case {case:?}: a created manager is CREATING, got {created}"
    );
    let id = text(&created, "paymentManagerId");
    wait_until_ready(router, case, &id).await;
    id
}

/// Look again until the manager reports `READY`, or the generous [`READY_DEADLINE`]
/// passes. The transition is owed by the clock, so this waits for the CONDITION rather
/// than for a span.
async fn wait_until_ready(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + READY_DEADLINE;
    loop {
        let (status, body) = get_manager(router, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetPaymentManager for {id:?}, got {status} \
             ({body})"
        );
        if text(&body, "status") == STATUS_READY {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the manager {id:?} never reached {STATUS_READY}, last status {:?}",
            text(&body, "status")
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert one `DateTimestamp` member's wire form: a JSON string in the canonical RFC 3339
/// form, naming a real instant (`lastUpdatedAt` targets `DateTimestamp`, which carries
/// `@timestampFormat("date-time")`).
fn assert_date_time(case: &str, member: &str, value: &Value) {
    let rendered = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be a date-time string, got {value:?}")
    });
    let parsed = DateTime::from_str(rendered, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must parse as a date-time ({error}), got {rendered:?}")
    });
    assert!(
        parsed.secs() > 0,
        "case {case:?}: {member} must name a real instant, got {rendered:?}"
    );
}

/// An update changes exactly the members its request carried, and the read reports the
/// result; everything else the manager holds is left as it was.
///
/// The rows differ along which members the PATCH carries and what they hold: the
/// description alone, the description at its longest published length, the role alone, a
/// customer managed key alone, the authorizer type together with the configuration it
/// requires, the authorizer type alone once a configuration is held, two members at once,
/// and a request that carries no changeable member at all. The expectation is kept as this
/// test's OWN model of the PATCH rule, applied row by row, so the read is compared against
/// the accumulated effect rather than against one row's request.
#[tokio::test]
async fn an_update_changes_only_the_members_it_carries_and_the_read_reports_them() {
    let router = router();
    let id = settled_manager(
        &router,
        "the manager the rows patch",
        "patchedManager",
        json!({"description": "the first description", "tags": {"owner": "retail"}}),
    )
    .await;

    // This test's own model of the stored manager, against which the read is checked.
    let mut expected: Map<String, Value> = Map::new();
    expected.insert("description".to_string(), json!("the first description"));
    expected.insert("authorizerType".to_string(), json!("AWS_IAM"));
    expected.insert("roleArn".to_string(), json!(ROLE_ARN));

    let longest_description = "d".repeat(MAX_DESCRIPTION_LEN);

    // (case, the members the PATCH carries)
    let rows: [(&str, Value); 8] = [
        ("the description alone", json!({"description": "the second description"})),
        (
            "the longest description the model admits",
            json!({"description": longest_description}),
        ),
        ("the role alone", json!({"roleArn": OTHER_ROLE_ARN})),
        ("a customer managed KMS key alone", json!({"kmsKeyArn": KMS_KEY_ARN})),
        (
            "the authorizer type with the configuration it requires",
            json!({"authorizerType": "CUSTOM_JWT", "authorizerConfiguration": jwt_authorizer()}),
        ),
        (
            "the authorizer type alone, once a configuration is held",
            json!({"authorizerType": "AWS_IAM"}),
        ),
        (
            "two members at once",
            json!({"description": "renamed and re-roled", "roleArn": ROLE_ARN}),
        ),
        ("no changeable member at all", json!({})),
    ];

    for (case, patch) in rows {
        let before = get_manager(&router, &id).await.1;
        let (status, updated) = update_manager(&router, &id, &patch).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from UpdatePaymentManager, got {status} ({updated})"
        );
        assert_eq!(
            text(&updated, "status"),
            STATUS_UPDATING,
            "case {case:?}: an accepted update reports the manager UPDATING, got {updated}"
        );
        assert_date_time(case, "lastUpdatedAt", &updated["lastUpdatedAt"]);

        for (member, value) in patch.as_object().expect("the row's members are an object") {
            expected.insert(member.clone(), value.clone());
        }

        // `UpdatePaymentManagerResponse` declares the identity, the authorizer type, the
        // role, the key and the status — and no description, tags or authorizer
        // configuration, which the read reports.
        assert_eq!(
            (
                updated["paymentManagerId"].clone(),
                updated["paymentManagerArn"].clone(),
                updated["name"].clone(),
                updated["authorizerType"].clone(),
                updated["roleArn"].clone(),
                updated["kmsKeyArn"].clone(),
            ),
            (
                json!(id),
                before["paymentManagerArn"].clone(),
                before["name"].clone(),
                expected
                    .get("authorizerType")
                    .cloned()
                    .unwrap_or(Value::Null),
                expected.get("roleArn").cloned().unwrap_or(Value::Null),
                expected.get("kmsKeyArn").cloned().unwrap_or(Value::Null),
            ),
            "case {case:?}: the update reports the patched manager, got {updated}"
        );

        let settled = wait_until_ready(&router, case, &id).await;
        for (member, value) in &expected {
            assert_eq!(
                &settled[member], value,
                "case {case:?}: the read reports {member} as {:?}, expected {value:?}",
                settled[member]
            );
        }
        // Members no update carried are the create's still, including the ones the update's
        // own response does not declare.
        assert_eq!(
            (
                settled["name"].clone(),
                settled["createdAt"].clone(),
                settled["tags"].clone(),
                settled["workloadIdentityDetails"].clone(),
            ),
            (
                before["name"].clone(),
                before["createdAt"].clone(),
                before["tags"].clone(),
                before["workloadIdentityDetails"].clone(),
            ),
            "case {case:?}: the update changed a member it did not carry, got {settled}"
        );
    }
}

/// Two writers racing one settled manager: one applies and the other is refused the
/// modelled 409 naming the version it raced against, the refused one changes nothing, and
/// the manager settles `READY` carrying the winner's description.
///
/// Run twice against the same manager, so the version the refusal names is the manager's
/// own state rather than a constant: the first race is against version 1, and the second
/// — after one write has been applied — against version 2.
#[tokio::test]
async fn one_of_two_racing_updates_applies_and_the_other_is_refused_a_conflict() {
    let router = router();
    let id = settled_manager(
        &router,
        "the manager the writers race for",
        "racedManager",
        json!({"description": "the created description"}),
    )
    .await;

    // (case, the version the manager is at when the race starts, the two descriptions)
    let races: [(&str, u64, [&str; 2]); 2] = [
        ("the first race, against the created manager", 1, ["first writer", "second writer"]),
        ("the second race, one write later", 2, ["third writer", "fourth writer"]),
    ];

    for (case, version, descriptions) in races {
        let left_body = json!({"description": descriptions[0]});
        let right_body = json!({"description": descriptions[1]});
        let (left, right) = tokio::join!(
            update_manager(&router, &id, &left_body),
            update_manager(&router, &id, &right_body),
        );

        let answers = [(descriptions[0], left), (descriptions[1], right)];
        let accepted: Vec<&str> = answers
            .iter()
            .filter(|(_, (status, _))| *status == 202)
            .map(|(description, _)| *description)
            .collect();
        let refused: Vec<&(u16, Value)> = answers
            .iter()
            .filter(|(_, (status, _))| *status != 202)
            .map(|(_, answer)| answer)
            .collect();
        assert_eq!(
            (accepted.len(), refused.len()),
            (1, 1),
            "case {case:?}: expected exactly one writer to apply and one to be refused, got \
             {answers:?}"
        );

        let (status, body) = refused[0];
        assert_eq!(
            (*status, error_code(body).as_str()),
            (409, "ConflictException"),
            "case {case:?}: expected HTTP 409 ConflictException for the refused writer, got \
             {status} ({body})"
        );
        assert_eq!(
            text(body, "message"),
            conflict_message(&id, version),
            "case {case:?}: unexpected conflict message, got {body}"
        );

        let settled = wait_until_ready(&router, case, &id).await;
        assert_eq!(
            settled["description"],
            json!(accepted[0]),
            "case {case:?}: the manager must settle carrying the description the writer that \
             applied submitted, got {settled}"
        );
        assert_eq!(
            text(&settled, "status"),
            STATUS_READY,
            "case {case:?}: the manager must settle READY, got {settled}"
        );
    }
}

/// Every update this operation refuses leaves the manager exactly as it was: an id this
/// account holds no manager under, a manager whose own create has not settled, and a
/// request that would leave the manager authorized by a bearer token with no
/// configuration to verify it against.
#[tokio::test]
async fn a_refused_update_changes_nothing() {
    let router = router();

    // An id of the modelled form (`^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`) that no manager
    // of this account holds.
    let absent = "nosuchmanager-0123456789";
    let (status, refused) = update_manager(&router, absent, &json!({"description": "x"})).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (404, "ResourceNotFoundException"),
        "an update of an absent manager is the modelled 404, got {status} ({refused})"
    );
    assert_eq!(
        text(&refused, "message"),
        format!("Payment manager with ID {absent} does not exist"),
        "unexpected not-found message, got {refused}"
    );
    let (status, fetched) = get_manager(&router, absent).await;
    assert_eq!(
        (status, error_code(&fetched).as_str()),
        (404, "ResourceNotFoundException"),
        "the refused update minted a manager, got {status} ({fetched})"
    );

    // A manager whose create has not settled is still being written.
    let (status, created) = create_manager(
        &router,
        &json!({
            "name": "unsettledManager",
            "authorizerType": "AWS_IAM",
            "roleArn": ROLE_ARN,
            "description": "the created description",
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "the unsettled manager must be created, got {status} ({created})"
    );
    let id = text(&created, "paymentManagerId");
    let (status, refused) =
        update_manager(&router, &id, &json!({"description": "while still creating"})).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "an update of a manager that is still being created is the modelled 409, got {status} \
         ({refused})"
    );
    assert_eq!(
        text(&refused, "message"),
        conflict_message(&id, 1),
        "unexpected conflict message for the unsettled manager, got {refused}"
    );
    let settled = wait_until_ready(&router, "the unsettled manager", &id).await;
    assert_eq!(
        settled["description"],
        json!("the created description"),
        "the refused update changed the manager, got {settled}"
    );

    // "If you specify CUSTOM_JWT as the authorizerType, you must provide an
    // authorizerConfiguration" (`smithy_query doc shape=CreatePaymentManager`), read
    // against what the manager would carry once the PATCH is applied.
    let (status, refused) =
        update_manager(&router, &id, &json!({"authorizerType": "CUSTOM_JWT"})).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ValidationException"),
        "switching to CUSTOM_JWT with no configuration held is the modelled 400, got {status} \
         ({refused})"
    );
    let (status, fetched) = get_manager(&router, &id).await;
    assert_eq!(
        (
            status,
            fetched["authorizerType"].clone(),
            text(&fetched, "status")
        ),
        (200, json!("AWS_IAM"), STATUS_READY.to_string()),
        "the refused update changed the manager's authorizer or status, got {status} ({fetched})"
    );
}

/// A replayed `clientToken` is ignored rather than applied a second time: "If this token
/// matches a previous request, the service ignores the request, but doesn't return an
/// error."
///
/// The rows differ along what the replay carries: the identical body, and a body asking
/// for a different description — which the service must NOT apply.
#[tokio::test]
async fn a_replayed_client_token_applies_the_update_once() {
    let router = router();
    let id = settled_manager(
        &router,
        "the manager the replay addresses",
        "replayedManager",
        json!({"description": "the created description"}),
    )
    .await;

    let token = client_token("replayedUpdate");
    let first = json!({"description": "the applied description", "clientToken": token});
    let (status, applied) = update_manager(&router, &id, &first).await;
    assert_eq!(
        status, 202,
        "the first update must apply, got {status} ({applied})"
    );

    // (case, the body the replay carries)
    let rows: [(&str, Value); 2] = [
        ("the identical body", first.clone()),
        (
            "a body asking for another description",
            json!({"description": "never applied", "clientToken": token}),
        ),
    ];
    for (case, body) in rows {
        let (status, replayed) = update_manager(&router, &id, &body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: a replayed token answers rather than failing, got {status} \
             ({replayed})"
        );
        assert_eq!(
            (
                replayed["paymentManagerId"].clone(),
                replayed["roleArn"].clone(),
                replayed["lastUpdatedAt"].clone(),
            ),
            (
                applied["paymentManagerId"].clone(),
                applied["roleArn"].clone(),
                applied["lastUpdatedAt"].clone(),
            ),
            "case {case:?}: the replay reports the manager the first update left, got {replayed}"
        );
    }

    let settled = wait_until_ready(&router, "the replayed manager", &id).await;
    assert_eq!(
        settled["description"],
        json!("the applied description"),
        "the replays must not have applied a change of their own, got {settled}"
    );
}
