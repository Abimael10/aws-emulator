// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what `UpdatePaymentManager` (`PATCH
//! /payments/managers/{paymentManagerId}`) accepts, what it refuses, and what a later
//! `GetPaymentManager` reports of either.
//!
//! The rules under test, beyond the PATCH and conflict rules
//! `payment_manager_update.rs` already pins:
//!
//! 1. THE PUBLISHED BOUNDS. A member inside its modelled `@length` / `@pattern` is
//!    applied and read back; a value one step outside is refused `400`
//!    `ValidationException` with the clause this family words, and such a refusal writes
//!    nothing — the manager keeps every value and the status it already had. Rows span
//!    `description`, `clientToken`, `roleArn` and `kmsKeyArn`, each at both published
//!    bounds the model admits and one past them.
//! 2. THE URI LABEL'S BOUNDS. `paymentManagerId` is a constraint check first and a
//!    lookup second: an id of the modelled form that this account holds no manager
//!    under is the modelled `404`, at both ends of its published length, while an id
//!    outside its pattern never reaches the store at all.
//! 3. AN IDEMPOTENCY RECORD IS THE OPERATION'S OWN. "If this token matches a previous
//!    request, the service ignores the request, but doesn't return an error" is about a
//!    previous request to the SAME operation: a `clientToken` a create recorded does not
//!    suppress an update carrying it, and one an update recorded does not suppress a
//!    create.
//!
//! Sources for every bound and pattern asserted below: `smithy_query(query="operation",
//! operation="UpdatePaymentManager")` and the same for `CreatePaymentManager`.
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

/// How long a wait for a clock-owed transition may take before the test gives up. Far
/// longer than the span the service gives the transition, so a loaded host cannot fail
/// this suite; the test waits for the STATUS, never for a span to pass.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How often that wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `PaymentManagerStatus` members this suite expects (`smithy_query
/// shape=PaymentManagerStatus`).
const STATUS_CREATING: &str = "CREATING";
const STATUS_READY: &str = "READY";

/// `PaymentsDescription`'s published bounds, `@length(min: 1, max: 4096)`.
const MIN_DESCRIPTION_LEN: usize = 1;
const MAX_DESCRIPTION_LEN: usize = 4096;

/// `ClientToken`'s published bounds, `@length(min: 33, max: 256)`. Its pattern
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$` admits a run of alphanumerics of any length in
/// that range, so a row varies the length alone.
const MIN_CLIENT_TOKEN_LEN: usize = 33;
const MAX_CLIENT_TOKEN_LEN: usize = 256;

/// `RoleArn`'s published maximum, `@length(min: 1, max: 2048)`. Its minimum is
/// unreachable on its own: the pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$` admits nothing shorter than the ARN
/// prefix itself, so the only length bound a row can vary alone is the maximum.
const MAX_ROLE_ARN_LEN: usize = 2048;

/// The fixed head of an IAM role ARN inside that pattern, which a row pads to a length.
const ROLE_ARN_PREFIX: &str = "arn:aws:iam::123456789012:role/";

/// An IAM role ARN inside the modelled `RoleArn` pattern, for the requests whose
/// offending member is somewhere else.
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/PaymentManagerRole";

/// A customer managed KMS key ARN inside the modelled `KmsKeyArn` pattern
/// `^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`. The
/// pattern fixes the key id at 36 characters, so that member's rows vary the pattern
/// rather than the length.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:123456789012:key/12345678-1234-1234-1234-123456789012";

/// `PaymentManagerId`'s published bounds, `@length(min: 12, max: 211)`.
const MIN_PAYMENT_MANAGER_ID_LEN: usize = 12;
const MAX_PAYMENT_MANAGER_ID_LEN: usize = 211;

/// A `clientToken` of `len` alphanumerics, which `ClientToken`'s pattern admits.
fn client_token(seed: &str, len: usize) -> String {
    let token = format!("{seed}{}", "0".repeat(len));
    token[..len].to_string()
}

/// An IAM role ARN of exactly `len` characters inside `RoleArn`'s pattern: the ARN head
/// above, padded in the `role/.+` tail the pattern leaves free.
fn role_arn_of_len(len: usize) -> String {
    format!(
        "{ROLE_ARN_PREFIX}{}",
        "r".repeat(len - ROLE_ARN_PREFIX.len())
    )
}

/// A `paymentManagerId` of exactly `len` characters inside `PaymentManagerId`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`: a stem of up to 100 `[0-9a-z][-]?` groups,
/// then the `-` and the ten trailing alphanumerics the pattern ends with.
fn payment_manager_id_of_len(len: usize) -> String {
    let stem_len = len - 11;
    let stem: String = (0..stem_len)
        .map(|position| if position % 2 == 0 { 'a' } else { '-' })
        .collect();
    format!("{stem}-0123456789")
}

/// One router over one engine, whose calls are signed as a principal the access decision
/// allows ([`crate::authorized_signing_identity`]): the rows below read what the OPERATION
/// answers, including a `paymentManagerId` no manager can carry, which the published
/// credential alone is refused for (`unnameable_identifier_authorization.rs`).
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::authorized_signing_identity::given_authorized_caller(&engine);
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, what
/// the handlers read the ARN Region from, and the identity it names.
fn authorization() -> String {
    let key = crate::authorized_signing_identity::AUTHORIZED_KEY_ID;
    format!(
        "AWS4-HMAC-SHA256 Credential={key}/20240101/{REGION}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the `x-amzn-errortype` header and the parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status: response.status().as_u16(),
        error_type,
        body: parsed,
    }
}

/// `CreatePaymentManager`: `POST /payments/managers`.
async fn create_manager(router: &MultiServiceRouter, body: &Value) -> Answer {
    send(router, "POST", "/payments/managers", Some(body)).await
}

/// `UpdatePaymentManager`: `PATCH /payments/managers/{paymentManagerId}`.
async fn update_manager(
    router: &MultiServiceRouter,
    payment_manager_id: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/payments/managers/{payment_manager_id}"),
        Some(body),
    )
    .await
}

/// `GetPaymentManager`: `GET /payments/managers/{paymentManagerId}`.
async fn get_manager(router: &MultiServiceRouter, payment_manager_id: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/payments/managers/{payment_manager_id}"),
        None,
    )
    .await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The clause this family words for a value outside its modelled constraint: the member,
/// the requirement, and the value the request carried (`/payments` quotes it, as
/// `validation_message_rendering.rs` pins for the family as a whole).
fn validation_message(submitted: &str, member: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{submitted}' at '{member}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// `@length`'s two requirements, as a clause states them.
fn at_least(bound: usize) -> String {
    format!("Member must have length greater than or equal to {bound}")
}

fn at_most(bound: usize) -> String {
    format!("Member must have length less than or equal to {bound}")
}

/// `@pattern` as a requirement reads, from the modelled expression WITHOUT the
/// whole-value anchors the model spells it with.
fn pattern_requirement(modelled: &str) -> String {
    let unanchored = modelled
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
        .expect("every modelled pattern below is anchored at both ends");
    format!("Member must satisfy regular expression pattern: {unanchored}")
}

/// Create one settled `AWS_IAM` payment manager, so a row updates a manager that is not
/// already being written. Reports its `paymentManagerId`.
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
    let created = create_manager(router, &request).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202 from CreatePaymentManager, got {} ({})",
        created.status, created.body
    );
    assert_eq!(
        text(&created.body, "status"),
        STATUS_CREATING,
        "case {case:?}: a created manager is CREATING, got {}",
        created.body
    );
    let id = text(&created.body, "paymentManagerId");
    wait_until_ready(router, case, &id).await;
    id
}

/// Look again until the manager reports `READY`, or the generous [`READY_DEADLINE`]
/// passes. The transition is owed by the clock, so this waits for the CONDITION rather
/// than for a span.
async fn wait_until_ready(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + READY_DEADLINE;
    loop {
        let fetched = get_manager(router, id).await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: expected HTTP 200 from GetPaymentManager for {id:?}, got {} ({})",
            fetched.status, fetched.body
        );
        if text(&fetched.body, "status") == STATUS_READY {
            return fetched.body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the manager {id:?} never reached {STATUS_READY}, last status {:?}",
            text(&fetched.body, "status")
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE 1, the accepting half: a member at either published bound its model admits is
/// applied, and the read reports it.
///
/// The rows differ along the member and which bound it sits at: the description at its
/// shortest and its longest, a token at its shortest and its longest, and the role at its
/// longest.
#[tokio::test]
async fn an_update_at_a_published_bound_is_applied_and_read_back() {
    let router = router();
    let id = settled_manager(
        &router,
        "the manager the bounds are patched into",
        "boundedManager",
        json!({"description": "the created description"}),
    )
    .await;

    let shortest_description = "d".repeat(MIN_DESCRIPTION_LEN);
    let longest_description = "d".repeat(MAX_DESCRIPTION_LEN);
    let longest_role_arn = role_arn_of_len(MAX_ROLE_ARN_LEN);

    // (case, the members the PATCH carries)
    let rows: [(&str, Value); 5] = [
        (
            "the description at its published minimum length",
            json!({"description": shortest_description}),
        ),
        (
            "the description at its published maximum length",
            json!({"description": longest_description}),
        ),
        (
            "a clientToken at its published minimum length",
            json!({
                "description": "patched with the shortest token",
                "clientToken": client_token("shortToken", MIN_CLIENT_TOKEN_LEN),
            }),
        ),
        (
            "a clientToken at its published maximum length",
            json!({
                "description": "patched with the longest token",
                "clientToken": client_token("longToken", MAX_CLIENT_TOKEN_LEN),
            }),
        ),
        (
            "the role at its published maximum length",
            json!({"roleArn": longest_role_arn}),
        ),
    ];

    for (case, patch) in rows {
        let before = get_manager(&router, &id).await.body;
        let accepted = update_manager(&router, &id, &patch).await;
        assert_eq!(
            accepted.status, 202,
            "case {case:?}: a value at its published bound is accepted, got {} ({})",
            accepted.status, accepted.body
        );

        let settled = wait_until_ready(&router, case, &id).await;
        for (member, value) in patch.as_object().expect("the row's members are an object") {
            // `clientToken` is the request's own idempotency key; no response declares it.
            if member == "clientToken" {
                continue;
            }
            assert_eq!(
                &settled[member], value,
                "case {case:?}: the read reports {member} as {:?}, expected {value:?}",
                settled[member]
            );
        }
        assert_eq!(
            (
                settled["paymentManagerId"].clone(),
                settled["name"].clone(),
                settled["createdAt"].clone(),
            ),
            (
                before["paymentManagerId"].clone(),
                before["name"].clone(),
                before["createdAt"].clone(),
            ),
            "case {case:?}: the update changed the manager's identity, got {settled}"
        );
    }
}

/// RULE 1, the refusing half: a value one step outside its published constraint is
/// refused `400 ValidationException` with the clause the family words, and the refused
/// update writes nothing at all — the read after it is byte for byte the read before it.
///
/// The rows differ along the member and the constraint class broken: a length below the
/// minimum, a length above the maximum, and a value outside a pattern, on four members.
#[tokio::test]
async fn an_update_outside_a_published_bound_is_refused_and_changes_nothing() {
    let router = router();
    let id = settled_manager(
        &router,
        "the manager the refusals address",
        "refusedManager",
        json!({"description": "the created description", "kmsKeyArn": KMS_KEY_ARN}),
    )
    .await;

    let too_long_description = "d".repeat(MAX_DESCRIPTION_LEN + 1);
    let too_short_token = client_token("tinyToken", MIN_CLIENT_TOKEN_LEN - 1);
    let too_long_token = client_token("hugeToken", MAX_CLIENT_TOKEN_LEN + 1);
    let too_long_role_arn = role_arn_of_len(MAX_ROLE_ARN_LEN + 1);

    // (case, the offending member, the value it carried, the requirement the clause states)
    let rows: [(&str, &str, String, String); 7] = [
        (
            "a description below its published minimum length",
            "description",
            String::new(),
            at_least(MIN_DESCRIPTION_LEN),
        ),
        (
            "a description above its published maximum length",
            "description",
            too_long_description,
            at_most(MAX_DESCRIPTION_LEN),
        ),
        (
            "a clientToken below its published minimum length",
            "clientToken",
            too_short_token,
            at_least(MIN_CLIENT_TOKEN_LEN),
        ),
        (
            "a clientToken above its published maximum length",
            "clientToken",
            too_long_token,
            at_most(MAX_CLIENT_TOKEN_LEN),
        ),
        (
            "a roleArn above its published maximum length",
            "roleArn",
            too_long_role_arn,
            at_most(MAX_ROLE_ARN_LEN),
        ),
        (
            "a roleArn outside its published pattern",
            "roleArn",
            "arn:aws:iam::123456789012:user/NotARole".to_string(),
            pattern_requirement("^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$"),
        ),
        (
            "a kmsKeyArn outside its published pattern",
            "kmsKeyArn",
            "arn:aws:kms:us-east-1:123456789012:alias/payments".to_string(),
            pattern_requirement(
                "^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$",
            ),
        ),
    ];

    for (case, member, submitted, requirement) in rows {
        let before = get_manager(&router, &id).await.body;
        let body = json!({member: submitted});
        let refused = update_manager(&router, &id, &body).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a value outside its published constraint is refused 400 \
             ValidationException, got {} {:?} ({})",
            refused.status,
            refused.error_type,
            refused.body
        );
        let expected = validation_message(&submitted, member, &requirement);
        assert_eq!(
            text(&refused.body, "message"),
            expected,
            "case {case:?}: expected {expected:?}, got {}",
            refused.body
        );

        let after = get_manager(&router, &id).await;
        assert_eq!(
            (after.status, after.body.clone()),
            (200, before.clone()),
            "case {case:?}: the refused update changed the manager, got {} ({})",
            after.status,
            after.body
        );
        assert_eq!(
            text(&after.body, "status"),
            STATUS_READY,
            "case {case:?}: a refused update must not move the manager's status, got {}",
            after.body
        );
    }
}

/// RULE 2: the `paymentManagerId` label is a constraint check first and a lookup second.
///
/// At either end of its published length an id of the modelled form passes validation and
/// is answered by the store — the modelled `404` for an id this account holds no manager
/// under — while an id outside the pattern is refused `400` and never reaches the store.
/// The pattern admits nothing shorter than 12 nor longer than 211 characters, so the two
/// published length bounds are exactly the ids a row can send and have validated.
#[tokio::test]
async fn an_absent_id_of_the_modelled_form_is_answered_by_the_store_at_either_bound() {
    let router = router();

    // (case, the id the path carries)
    let absent_rows: [(&str, String); 3] = [
        (
            "an id at its published minimum length",
            payment_manager_id_of_len(MIN_PAYMENT_MANAGER_ID_LEN),
        ),
        (
            "an id at its published maximum length",
            payment_manager_id_of_len(MAX_PAYMENT_MANAGER_ID_LEN),
        ),
        (
            "an id between the two bounds",
            "nosuchmanager-0123456789".to_string(),
        ),
    ];

    for (case, absent) in absent_rows {
        assert!(
            (MIN_PAYMENT_MANAGER_ID_LEN..=MAX_PAYMENT_MANAGER_ID_LEN).contains(&absent.len()),
            "case {case:?}: the row's own id {absent:?} must sit inside the published length, \
             got {}",
            absent.len()
        );
        let refused = update_manager(&router, &absent, &json!({"description": "x"})).await;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: an update of an absent manager is the modelled 404, got {} {:?} ({})",
            refused.status,
            refused.error_type,
            refused.body
        );
        // The one sentence the payments plane words a manager it cannot find with, shared by
        // every operation that resolves one (`payment_manager_not_found_wording.rs`): the fixed
        // text, then the id the request's path carried.
        assert_eq!(
            text(&refused.body, "message"),
            format!("Payment manager not found: {absent}"),
            "case {case:?}: unexpected not-found message, got {}",
            refused.body
        );
    }

    // One past the pattern: upper case, which `PaymentManagerId` does not admit.
    let malformed = "BADMANAGER-abcdefghij";
    let refused = update_manager(&router, malformed, &json!({"description": "x"})).await;
    let case = "an id outside the published pattern";
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (400, "ValidationException"),
        "case {case:?}: an id outside its pattern is refused 400 ValidationException, got {} \
         {:?} ({})",
        refused.status,
        refused.error_type,
        refused.body
    );
    let expected = validation_message(
        malformed,
        "paymentManagerId",
        &pattern_requirement("^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$"),
    );
    assert_eq!(
        text(&refused.body, "message"),
        expected,
        "case {case:?}: expected {expected:?}, got {}",
        refused.body
    );
}

/// RULE 3: an idempotency record belongs to the operation that minted it.
///
/// A `clientToken` a create recorded does not suppress an update carrying the same token,
/// and a token an update recorded does not suppress a create carrying it: each answers
/// from its own operation's record, so the update applies and the create mints a manager
/// of its own.
#[tokio::test]
async fn an_idempotency_record_is_the_operations_own() {
    let router = router();
    let shared_token = client_token("sharedAcrossOperations", MIN_CLIENT_TOKEN_LEN);

    let id = settled_manager(
        &router,
        "the manager created under the shared token",
        "sharedTokenManager",
        json!({"description": "the created description", "clientToken": shared_token}),
    )
    .await;

    // The create holds a record for this token; the update must still apply.
    let case = "an update carrying the token the create recorded";
    let applied = update_manager(
        &router,
        &id,
        &json!({"description": "applied under the create's token", "clientToken": shared_token}),
    )
    .await;
    assert_eq!(
        applied.status, 202,
        "case {case:?}: expected HTTP 202, got {} ({})",
        applied.status, applied.body
    );
    let settled = wait_until_ready(&router, case, &id).await;
    assert_eq!(
        settled["description"],
        json!("applied under the create's token"),
        "case {case:?}: the update must apply rather than be answered from the create's \
         record, got {settled}"
    );

    // The mirror image: a token an update recorded must not answer a create.
    let update_token = client_token("recordedByAnUpdate", MIN_CLIENT_TOKEN_LEN);
    let case = "a create carrying the token an update recorded";
    let applied = update_manager(
        &router,
        &id,
        &json!({"description": "recorded by an update", "clientToken": update_token}),
    )
    .await;
    assert_eq!(
        applied.status, 202,
        "case {case:?}: the update that records the token must apply, got {} ({})",
        applied.status, applied.body
    );
    wait_until_ready(&router, case, &id).await;

    let created = create_manager(
        &router,
        &json!({
            "name": "otherTokenManager",
            "authorizerType": "AWS_IAM",
            "roleArn": ROLE_ARN,
            "description": "minted under the update's token",
            "clientToken": update_token,
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected HTTP 202, got {} ({})",
        created.status, created.body
    );
    let minted = text(&created.body, "paymentManagerId");
    assert_ne!(
        minted, id,
        "case {case:?}: the create must mint a manager of its own rather than answer from the \
         update's record, got {}",
        created.body
    );
    let fetched = wait_until_ready(&router, case, &minted).await;
    assert_eq!(
        (
            fetched["name"].clone(),
            fetched["description"].clone(),
            text(&fetched, "status")
        ),
        (
            json!("otherTokenManager"),
            json!("minted under the update's token"),
            STATUS_READY.to_string()
        ),
        "case {case:?}: the read must report the manager the create minted, got {fetched}"
    );
}
