// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeletePaymentManager` (`DELETE
//! /payments/managers/{paymentManagerId}`), and what a later `GetPaymentManager` reports
//! of the deletion it accepted.
//!
//! The rules under test:
//!
//! * a deletion is ACCEPTED, not carried out — the operation "initiates the deletion
//!   process asynchronously" (`smithy_query doc shape=DeletePaymentManager`) and answers
//!   202 with the manager `DELETING`, which is what a read reports until the removal the
//!   clock owes it comes due, and only then is every read the modelled 404;
//! * the children come first — "All payment connectors associated with the payment
//!   manager must be deleted before the payment manager can be deleted", so a manager
//!   that still holds one is refused the modelled `ConflictException` (HTTP 409) and left
//!   exactly as it was, and a manager already going away takes no new connector;
//! * `@idempotent` — a repeat while the deletion is under way reports the same manager
//!   going away rather than pushing its removal further out.
//!
//! Every wait below polls for the CONDITION it is about (the status, or the manager being
//! gone) until it holds or a generous deadline passes, so nothing here depends on how
//! fast the host runs.
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every request in this suite is signed for.
const REGION: &str = "us-east-1";

/// How long a wait for a clock-owed transition or removal may take before the test gives
/// up. Far longer than the span the service gives either, so a loaded host cannot fail a
/// row; every wait below waits for the CONDITION, never for a span to pass.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How often such a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `PaymentManagerStatus` members this suite expects (`smithy_query
/// shape=PaymentManagerStatus`).
const STATUS_CREATING: &str = "CREATING";
const STATUS_UPDATING: &str = "UPDATING";
const STATUS_DELETING: &str = "DELETING";
const STATUS_READY: &str = "READY";

/// `PaymentConnectorProvisionMode`'s value for a connector the service provisions after
/// OAuth consent, which needs no credential provider of the caller's to exist
/// (`smithy_query shape=PaymentConnectorProvisionMode`).
const MODE_QUICK_CREATE: &str = "QUICK_CREATE";

/// `PaymentConnectorType`'s two values (`smithy_query shape=PaymentConnectorType`).
const TYPE_COINBASE_CDP: &str = "CoinbaseCDP";
const TYPE_STRIPE_PRIVY: &str = "StripePrivy";

/// An IAM role ARN inside the modelled `RoleArn` pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$` (`smithy_query
/// operation=CreatePaymentManager`).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/PaymentManagerRole";

/// A customer managed KMS key ARN inside the modelled `KmsKeyArn` pattern
/// `^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:123456789012:key/12345678-1234-1234-1234-123456789012";

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

/// `DeletePaymentManager`: `DELETE /payments/managers/{paymentManagerId}`, with the
/// `clientToken` the model carries in the query string.
async fn delete_manager(
    router: &MultiServiceRouter,
    payment_manager_id: &str,
    client_token: Option<&str>,
) -> (u16, Value) {
    let uri = match client_token {
        Some(token) => format!("/payments/managers/{payment_manager_id}?clientToken={token}"),
        None => format!("/payments/managers/{payment_manager_id}"),
    };
    send(router, "DELETE", &uri, None).await
}

/// `CreatePaymentConnector`: `POST /payments/managers/{paymentManagerId}/connectors`, in
/// the `QUICK_CREATE` mode whose credential provider the service provisions after
/// consent, so a row needs no token-vault resource of its own.
async fn create_connector(
    router: &MultiServiceRouter,
    payment_manager_id: &str,
    name: &str,
    connector_type: &str,
) -> (u16, Value) {
    send(
        router,
        "POST",
        &format!("/payments/managers/{payment_manager_id}/connectors"),
        Some(&json!({
            "name": name,
            "type": connector_type,
            "credentialProviderConfigurations": [],
            "provisionMode": MODE_QUICK_CREATE,
        })),
    )
    .await
}

/// `GetPaymentConnector`:
/// `GET /payments/managers/{paymentManagerId}/connectors/{paymentConnectorId}`.
async fn get_connector(
    router: &MultiServiceRouter,
    payment_manager_id: &str,
    payment_connector_id: &str,
) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/payments/managers/{payment_manager_id}/connectors/{payment_connector_id}"),
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

/// The sentence a delete refused for the connectors the manager still holds reads: fixed
/// text around the number of connectors still on it.
fn still_contains_message(payment_connectors: usize) -> String {
    format!(
        "Payment manager still contains {payment_connectors} payment connector and cannot be \
         deleted"
    )
}

/// Create one payment manager and report its `paymentManagerId`.
async fn given_manager(router: &MultiServiceRouter, case: &str, name: &str, extra: &Value) -> String {
    let mut request = json!({"name": name, "authorizerType": "AWS_IAM", "roleArn": ROLE_ARN});
    for (member, value) in extra.as_object().expect("the row's members are an object") {
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
    text(&created, "paymentManagerId")
}

/// Look again until the manager reports `READY`, or the generous [`POLL_DEADLINE`]
/// passes: the transition is owed by the clock, so this waits for the CONDITION rather
/// than for a span.
async fn wait_until_ready(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
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

/// Look again until the manager is GONE — the modelled 404 — or the generous
/// [`POLL_DEADLINE`] passes. The removal is owed by the clock, so this waits for the
/// CONDITION rather than for a span, and reports the refusal it ended on.
async fn wait_until_gone(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = get_manager(router, id).await;
        if status == 404 {
            return body;
        }
        assert_eq!(
            (status, text(&body, "status").as_str()),
            (200, STATUS_DELETING),
            "case {case:?}: a manager whose deletion is under way reads back 200 {STATUS_DELETING} \
             until it is gone, got {status} ({body})"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the manager {id:?} was never removed"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert the modelled not-found refusal for `id`, in the sentence this plane words a
/// missing payment manager with.
fn assert_not_found(case: &str, id: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (404, "ResourceNotFoundException"),
        "case {case:?}: expected the modelled 404 for {id:?}, got {status} ({body})"
    );
    assert_eq!(
        text(body, "message"),
        format!("Payment manager not found: {id}"),
        "case {case:?}: unexpected not-found message, got {body}"
    );
}

/// One row of the acceptance table: the manager the delete is asked of, and whether its
/// own create or update is still in flight when the delete arrives.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The members the create request carries beyond `name`, `authorizerType` and
    /// `roleArn`.
    extra: Value,
    /// Whether the row waits for the manager to settle `READY` before deleting it.
    settled: bool,
    /// A `PATCH` applied straight before the delete, so the delete arrives while a write
    /// of the manager's own is still in flight.
    updated: Option<Value>,
    /// The `clientToken` the delete carries, or `None` for a delete that sends none.
    client_token: Option<String>,
}

/// An accepted deletion answers 202 with the manager `DELETING`, the manager reads back
/// `DELETING` while the removal is still owed, and the manager is gone once the clock
/// owes its removal.
///
/// The rows span what the rule generalises over: the shape of the manager being deleted
/// (the required members alone, a described and tagged manager, one encrypted under a
/// customer managed key, one authorized by a bearer token), the status the delete finds
/// it in (`READY`, `CREATING` while its create is still in flight, `UPDATING` while an
/// update is), and whether the delete carries an idempotency token. Every row's manager
/// is deleted before any row waits, so the windows the clock owes run together.
#[tokio::test]
async fn an_accepted_deletion_reports_deleting_and_then_the_manager_is_gone() {
    let router = router();

    let rows = [
        Row {
            case: "a settled manager with only the required members",
            name: "plainManager",
            extra: json!({}),
            settled: true,
            updated: None,
            client_token: None,
        },
        Row {
            case: "a settled manager with tags, a description and a customer managed key",
            name: "taggedManager",
            extra: json!({
                "description": "the storefront's payment manager",
                "tags": {"owner": "retail", "environment": "test"},
                "kmsKeyArn": KMS_KEY_ARN,
            }),
            settled: true,
            updated: None,
            client_token: Some(client_token("taggedDelete")),
        },
        Row {
            case: "a settled manager authorized by a bearer token",
            name: "jwtManager",
            extra: json!({
                "authorizerType": "CUSTOM_JWT",
                "authorizerConfiguration": jwt_authorizer(),
            }),
            settled: true,
            updated: None,
            client_token: None,
        },
        Row {
            case: "a manager whose own create has not settled yet",
            name: "creatingManager",
            extra: json!({"description": "deleted while still creating"}),
            settled: false,
            updated: None,
            client_token: Some(client_token("creatingDelete")),
        },
        Row {
            case: "a manager whose own update is still in flight",
            name: "updatingManager",
            extra: json!({"description": "the created description"}),
            settled: true,
            updated: Some(json!({"description": "the updated description"})),
            client_token: None,
        },
        Row {
            case: "a one-letter name",
            name: "m",
            extra: json!({}),
            settled: true,
            updated: None,
            client_token: None,
        },
    ];

    let mut deleted = Vec::new();
    for row in rows {
        let id = given_manager(&router, row.case, row.name, &row.extra).await;
        if row.settled {
            wait_until_ready(&router, row.case, &id).await;
        }
        if let Some(patch) = &row.updated {
            let (status, updated) = update_manager(&router, &id, patch).await;
            assert_eq!(
                (status, text(&updated, "status").as_str()),
                (202, STATUS_UPDATING),
                "case {:?}: the update must be accepted with the manager UPDATING, got {status} \
                 ({updated})",
                row.case
            );
        }

        let (status, accepted) =
            delete_manager(&router, &id, row.client_token.as_deref()).await;
        assert_eq!(
            status, 202,
            "case {:?}: expected HTTP 202 from DeletePaymentManager, got {status} ({accepted})",
            row.case
        );
        assert_eq!(
            text(&accepted, "status"),
            STATUS_DELETING,
            "case {:?}: an accepted deletion reports {STATUS_DELETING}, got {accepted}",
            row.case
        );
        assert_eq!(
            text(&accepted, "paymentManagerId"),
            id,
            "case {:?}: the response names the manager the path addressed, got {accepted}",
            row.case
        );

        // A caller acting straight after the delete still sees the manager going away:
        // the removal is the clock's, and the span is never zero.
        let (status, read) = get_manager(&router, &id).await;
        assert_eq!(
            (status, text(&read, "status").as_str()),
            (200, STATUS_DELETING),
            "case {:?}: a read straight after the delete reports the manager DELETING, got \
             {status} ({read})",
            row.case
        );
        deleted.push((row.case, id));
    }

    for (case, id) in deleted {
        let gone = wait_until_gone(&router, case, &id).await;
        assert_not_found(case, &id, 404, &gone);
    }
}

/// `DeletePaymentManager` is `@idempotent`: a repeat while the deletion is under way
/// reports the same manager going away, and does not push its removal further out.
#[tokio::test]
async fn a_repeated_delete_reports_the_same_deletion_and_does_not_re_arm_the_clock() {
    let router = router();
    let case = "the repeated delete";

    let id = given_manager(&router, case, "repeatedManager", &json!({})).await;
    wait_until_ready(&router, case, &id).await;

    // The same token twice, then a different token, then none at all: every repeat is
    // the same accepted deletion.
    let tokens = [
        Some(client_token("repeatedDelete")),
        Some(client_token("repeatedDelete")),
        Some(client_token("anotherDelete")),
        None,
    ];
    for (attempt, token) in tokens.into_iter().enumerate() {
        let (status, accepted) = delete_manager(&router, &id, token.as_deref()).await;
        assert_eq!(
            (status, text(&accepted, "status").as_str()),
            (202, STATUS_DELETING),
            "case {case:?}: delete attempt {attempt} must report the same accepted deletion, got \
             {status} ({accepted})"
        );
        assert_eq!(
            text(&accepted, "paymentManagerId"),
            id,
            "case {case:?}: delete attempt {attempt} must name the same manager, got {accepted}"
        );
    }

    // The repeats did not re-arm the removal: it still comes due, within the deadline.
    let gone = wait_until_gone(&router, case, &id).await;
    assert_not_found(case, &id, 404, &gone);

    // Past the removal the manager is gone, so a further delete of it is the modelled
    // 404 rather than a third acceptance.
    let (status, refused) = delete_manager(&router, &id, None).await;
    assert_not_found(case, &id, status, &refused);
}

/// "All payment connectors associated with the payment manager must be deleted before
/// the payment manager can be deleted": a manager that still holds connectors is refused
/// the modelled 409, and the refusal changes nothing — the manager stays `READY` and
/// every connector it holds is still readable.
///
/// The rows span how many connectors are still on the manager and which vendors they
/// integrate with, since the count is what the refusal reports.
#[tokio::test]
async fn a_manager_that_still_holds_connectors_cannot_be_deleted() {
    let router = router();

    let rows: [(&str, &str, &[&str]); 3] = [
        (
            "one connector left on the manager",
            "oneConnector",
            &[TYPE_COINBASE_CDP],
        ),
        (
            "two connectors of the two vendors left on the manager",
            "twoConnectors",
            &[TYPE_COINBASE_CDP, TYPE_STRIPE_PRIVY],
        ),
        (
            "three connectors left on the manager",
            "threeConnectors",
            &[TYPE_STRIPE_PRIVY, TYPE_COINBASE_CDP, TYPE_STRIPE_PRIVY],
        ),
    ];

    for (case, name, connector_types) in rows {
        let id = given_manager(&router, case, name, &json!({})).await;
        wait_until_ready(&router, case, &id).await;

        let mut connector_ids = Vec::new();
        for (index, connector_type) in connector_types.iter().enumerate() {
            let (status, created) =
                create_connector(&router, &id, &format!("connector{index}"), connector_type).await;
            assert_eq!(
                status, 202,
                "case {case:?}: expected HTTP 202 from CreatePaymentConnector, got {status} \
                 ({created})"
            );
            connector_ids.push(text(&created, "paymentConnectorId"));
        }

        let (status, refused) = delete_manager(&router, &id, None).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (409, "ConflictException"),
            "case {case:?}: a manager still holding connectors cannot be deleted, got {status} \
             ({refused})"
        );
        assert_eq!(
            text(&refused, "message"),
            still_contains_message(connector_types.len()),
            "case {case:?}: unexpected conflict message, got {refused}"
        );

        // Nothing was written: the manager is still there and still READY, and so is
        // every connector it holds.
        let (status, read) = get_manager(&router, &id).await;
        assert_eq!(
            (status, text(&read, "status").as_str()),
            (200, STATUS_READY),
            "case {case:?}: the refused delete must leave the manager READY, got {status} ({read})"
        );
        for connector_id in &connector_ids {
            let (status, connector) = get_connector(&router, &id, connector_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the connector {connector_id:?} must survive the refused delete, \
                 got {status} ({connector})"
            );
        }
    }
}

/// A manager whose deletion is already under way is going away, so it takes no new
/// connector and no update: both are refused the modelled 409, in the sentence that
/// states what the manager is doing.
#[tokio::test]
async fn a_manager_being_deleted_accepts_no_further_write() {
    let router = router();
    let case = "a manager already going away";

    let id = given_manager(&router, case, "doomedManager", &json!({})).await;
    wait_until_ready(&router, case, &id).await;
    let (status, accepted) = delete_manager(&router, &id, None).await;
    assert_eq!(
        (status, text(&accepted, "status").as_str()),
        (202, STATUS_DELETING),
        "case {case:?}: the deletion must be accepted, got {status} ({accepted})"
    );

    let (status, refused) = create_connector(&router, &id, "lateConnector", TYPE_COINBASE_CDP).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case {case:?}: a connector cannot be created on a manager being deleted, got {status} \
         ({refused})"
    );
    assert_eq!(
        text(&refused, "message"),
        format!("Payment manager {id} is in {STATUS_DELETING} status and cannot be modified."),
        "case {case:?}: unexpected conflict message, got {refused}"
    );

    let (status, refused) =
        update_manager(&router, &id, &json!({"description": "too late"})).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case {case:?}: a manager being deleted cannot be updated, got {status} ({refused})"
    );

    // The removal still comes due, and nothing the refused writes asked for survives it.
    let gone = wait_until_gone(&router, case, &id).await;
    assert_not_found(case, &id, 404, &gone);
}

/// A delete of a manager this account holds none of is the modelled 404, for every
/// well-formed `paymentManagerId` that names nothing: one no create ever minted, and one
/// whose manager has already been removed.
#[tokio::test]
async fn a_delete_of_an_absent_manager_is_the_modelled_not_found() {
    let router = router();

    // Ids of the modelled form `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that no manager of
    // this account holds.
    let absent = [
        ("an id no create ever minted", "nosuchmanager-0123456789"),
        ("the shortest id the model admits", "a-0123456789"),
        (
            "an id carrying the separators the pattern admits",
            "a-b-c-manager-9876543210",
        ),
    ];
    for (case, id) in absent {
        let (status, refused) = delete_manager(&router, id, None).await;
        assert_not_found(case, id, status, &refused);
        let (status, read) = get_manager(&router, id).await;
        assert_not_found(case, id, status, &read);
    }
}
