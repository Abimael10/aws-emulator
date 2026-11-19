// SPDX-License-Identifier: CC0-1.0

//! How the payments plane words a payment manager it cannot find, end to end over HTTP.
//!
//! One rule: every operation that addresses ONE payment manager by the
//! `paymentManagerId` its path carries reports an id this account holds no manager under
//! as `ResourceNotFoundException` (HTTP 404) whose message is the fixed text "Payment
//! manager not found: " followed by that very id, taken verbatim from the request's path
//! label — so the sentence depends on neither the asking operation nor the shape of the
//! id, and a manager that IS held is untouched by the refusal. The model states no
//! message text, so the wording is the recorded one; what is under test here is that the
//! wording is one sentence shared by the whole payment-manager registry and that the
//! identifier is echoed from the request rather than from any one stored value.
//!
//! Sources: `smithy_query(query="operation",operation="GetPaymentManager,\
//! UpdatePaymentManager,CreatePaymentManager")` for the routes, the `@required`
//! `@httpLabel` `paymentManagerId`, the modelled `ResourceNotFoundException` (httpError
//! 404) and `PaymentManagerId`'s constraints (`length 12..211`, pattern
//! `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`) the rows span, and
//! `smithy_query(query="operation",operation="GetBrowser")` for the sibling browser route
//! whose own sentence must not be dragged along.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/PaymentManagerRole";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the error code `restJson1` puts in `__type`, the `message` the
/// body carries and the parsed body itself.
struct Answer {
    status: u16,
    code: String,
    message: String,
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
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status,
        code: parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        message: parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body: parsed,
    }
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence the payments plane owes for a manager no stored id resolves to.
fn manager_not_found(payment_manager_id: &str) -> String {
    format!("Payment manager not found: {payment_manager_id}")
}

/// `GET /payments/managers/{paymentManagerId}` and
/// `PATCH /payments/managers/{paymentManagerId}`: the two operations whose route
/// addresses ONE manager. Both must answer the identical sentence, so neither the verb
/// nor the operation can appear in it. The PATCH carries the smallest body
/// `UpdatePaymentManager` accepts and no `clientToken`, so nothing but the path label
/// decides the answer.
fn manager_operations() -> [(&'static str, &'static str, Option<Value>); 2] {
    [
        ("GetPaymentManager", "GET", None),
        (
            "UpdatePaymentManager",
            "PATCH",
            Some(json!({"description": "a changed description"})),
        ),
    ]
}

/// THE RULE: an id no manager of this account is stored under is reported as the fixed
/// sentence plus that id, whichever operation addresses it and whatever form the id takes.
///
/// The rows differ along the id the path carries: the shortest `PaymentManagerId` the
/// model admits (`length 12..211`), one whose stem is all digits, one carrying interior
/// hyphens, one whose stem matches a manager this account DOES hold but whose suffix does
/// not, and the longest stem the pattern's `{1,100}` repetition admits.
#[tokio::test]
async fn an_operation_addressing_one_payment_manager_reports_the_id_it_was_given() {
    let router = router();

    // A manager that IS held, so a row can carry its stem with a different suffix and
    // the suite can prove no refusal touched it.
    let created = send(
        &router,
        "POST",
        "/payments/managers",
        Some(&json!({
            "name": "storedManager",
            "authorizerType": "AWS_IAM",
            "roleArn": ROLE_ARN,
        })),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup: the create must succeed for this suite to say anything, got {} with {}",
        created.status, created.body
    );
    let stored_id = text(&created.body, "paymentManagerId");
    let (head, last) = stored_id.split_at(stored_id.len() - 1);
    let neighbour_id = format!("{head}{}", if last == "a" { "b" } else { "a" });

    // `PaymentManagerId` is `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` with `length 12..211`:
    // the shortest admissible id is one stem character, a hyphen and 10 more characters,
    // and the longest stem the repetition admits is 100 characters.
    let shortest_id = "a-0123456789".to_string();
    let longest_stem_id = format!("{}-0123456789", "a".repeat(100));

    // (case, the paymentManagerId the path carries)
    let rows: Vec<(&str, String)> = vec![
        (
            "the shortest id the model admits, which nothing created",
            shortest_id,
        ),
        (
            "an id whose stem is all digits",
            "90210manager-0123456789".to_string(),
        ),
        (
            "an id carrying interior hyphens",
            "ab-cd-ef-0123456789".to_string(),
        ),
        (
            "a stored manager's stem with a different suffix",
            neighbour_id,
        ),
        (
            "the longest stem the pattern admits, which nothing created",
            longest_stem_id,
        ),
    ];

    for (case, id) in rows {
        let expected = manager_not_found(&id);
        let mut sentences: Vec<(&str, String)> = Vec::new();
        for (operation, method, body) in manager_operations() {
            let answer = send(
                &router,
                method,
                &format!("/payments/managers/{id}"),
                body.as_ref(),
            )
            .await;
            assert_eq!(
                (answer.status, answer.code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} ({operation}): a manager that is not there is the modelled 404 \
                 ResourceNotFoundException, got {} {:?} with {}",
                answer.status,
                answer.code,
                answer.body
            );
            assert_eq!(
                answer.message, expected,
                "case {case:?} ({operation}): expected {expected:?}, got {:?}",
                answer.message
            );
            sentences.push((operation, answer.message));
        }
        let (first_operation, first) = &sentences[0];
        for (operation, sentence) in &sentences[1..] {
            assert_eq!(
                sentence, first,
                "case {case:?}: {operation} must answer exactly what {first_operation} answers, \
                 so the asking operation cannot appear in the sentence, got {sentence:?} against \
                 {first:?}"
            );
        }
    }

    // None of the refusals above wrote anything: the manager this account holds still
    // reads back, and the refused update minted nothing.
    let held = send(
        &router,
        "GET",
        &format!("/payments/managers/{stored_id}"),
        None,
    )
    .await;
    assert_eq!(
        (held.status, text(&held.body, "paymentManagerId").as_str()),
        (200, stored_id.as_str()),
        "case \"the manager this account holds\": the refusals above changed nothing, got {} with \
         {}",
        held.status,
        held.body
    );
}

/// The sentence belongs to the payment-manager registry, not to the whole service: the
/// browser plane, whose route addresses its own resource by id, keeps its own wording, so
/// a client branching on the payments sentence cannot be answered it for a browser.
#[tokio::test]
async fn the_payments_sentence_does_not_spread_to_a_sibling_resource() {
    let router = router();

    // `BrowserId` is `^(aws\.browser\.v1|[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10})$`;
    // this one names no stored browser.
    let absent_browser = "noSuchBrowser-abcdefghij";
    let browser = send(
        &router,
        "GET",
        &format!("/browsers/{absent_browser}"),
        None,
    )
    .await;
    assert_eq!(
        (browser.status, browser.code.as_str()),
        (404, "ResourceNotFoundException"),
        "case \"an absent browser\": expected the modelled 404 ResourceNotFoundException, got {} \
         {:?} with {}",
        browser.status,
        browser.code,
        browser.body
    );
    assert_ne!(
        browser.message,
        manager_not_found(absent_browser),
        "case \"an absent browser\": the payments plane's sentence must not be answered for a \
         browser, got {:?}",
        browser.message
    );

    let manager_id = "absentmanager-0123456789";
    let manager = send(
        &router,
        "GET",
        &format!("/payments/managers/{manager_id}"),
        None,
    )
    .await;
    assert_eq!(
        manager.message,
        manager_not_found(manager_id),
        "case \"an absent payment manager\": expected {:?}, got {:?}",
        manager_not_found(manager_id),
        manager.message
    );
}
