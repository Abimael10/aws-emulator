// SPDX-License-Identifier: CC0-1.0

//! The refusal a hub-scoped operation answers an account with no Security Hub CSPM
//! subscription with.
//!
//! RULE: a hub-scoped operation refused because the calling account has no Security
//! Hub CSPM subscription carries the service's fixed not-subscribed wording, whose
//! ONLY variable part is the calling account's id — the Region the subscription would
//! live in is not interpolated, so every operation, method and signing Region gets
//! byte-identical text.
//!
//! The rows span that class: the operations that gate on the subscription (reads,
//! creates, a delete; `GET`, `POST` and `DELETE`), four signing Regions, and both ways
//! an account arrives unsubscribed — never enabled, and enabled then disabled again.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The fixed text of the refusal, with `{account}` standing for the only variable part.
///
/// Written out here rather than read from the handler, so re-wording the handler's
/// message fails this suite.
fn expected_not_subscribed_message(account_id: &str) -> String {
    format!("Account {account_id} is not subscribed to AWS Security Hub CSPM")
}

/// One emulator with the Security Hub service mounted, driven over the restJson1 wire
/// path for one signing Region.
struct Harness {
    router: MultiServiceRouter,
    region: String,
}

impl Harness {
    fn new(region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_securityhub_securityhubapiservice::EmulatedService::new(engine),
        );
        Self {
            router,
            region: region.to_string(),
        }
    }

    /// Dispatch one request and return `(status, parsed body)`. An empty body parses as
    /// `Value::Null`, which is what an operation with no output members returns.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let serialized = body.map(|b| serde_json::to_vec(&b).expect("serializable request body"));
        let region = &self.region;
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/{region}/securityhub/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(serialized.unwrap_or_default()))
            .expect("valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let parsed = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, parsed)
    }
}

/// The refusal's `Message`, under either casing the restJson1 error shape may carry.
fn message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_else(|| panic!("a refusal carries a Message: {body}"))
        .to_string()
}

/// The refusal's error code, under either casing the wire shape may carry.
fn error_code(body: &Value) -> String {
    body["Code"]
        .as_str()
        .or_else(|| body["code"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// One hub-scoped call: the operation's name, its HTTP method and URI (from this
/// service's Smithy model, e.g. `smithy_query(query="operation",
/// operation="DescribeHub")`), a request body that satisfies its required members, and
/// the refusal it raises when the caller holds no subscription.
struct HubScopedCall {
    operation: &'static str,
    method: &'static str,
    path: &'static str,
    body: Option<Value>,
    status: u16,
    code: &'static str,
}

/// The hub-scoped calls this suite drives: every HTTP method the subscription gate sits
/// behind, reads as well as writes, across the finding, insight, standards, action
/// target, member, aggregator, automation rule and hub planes.
///
/// `DisableSecurityHub` is the one row whose refusal class differs (404
/// `ResourceNotFoundException`): the message is the same, which is the point of the
/// rule.
fn hub_scoped_calls() -> Vec<HubScopedCall> {
    vec![
        HubScopedCall {
            operation: "DescribeHub",
            method: "GET",
            path: "/accounts",
            body: None,
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "GetFindings",
            method: "POST",
            path: "/findings",
            body: Some(json!({})),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "GetEnabledStandards",
            method: "POST",
            path: "/standards/get",
            body: Some(json!({})),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "DescribeActionTargets",
            method: "POST",
            path: "/actionTargets/get",
            body: Some(json!({})),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "ListAutomationRules",
            method: "GET",
            path: "/automationrules/list",
            body: None,
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "ListFindingAggregators",
            method: "GET",
            path: "/findingAggregator/list",
            body: None,
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "GetMembers",
            method: "POST",
            path: "/members/get",
            body: Some(json!({ "AccountIds": ["111111111111"] })),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "CreateActionTarget",
            method: "POST",
            path: "/actionTargets",
            body: Some(json!({
                "Id": "Remediation",
                "Name": "Custom action",
                "Description": "Created by a test",
            })),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "CreateInsight",
            method: "POST",
            path: "/insights",
            body: Some(json!({
                "Name": "An insight",
                "GroupByAttribute": "SeverityLabel",
                "Filters": {},
            })),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "GetFindingHistory",
            method: "POST",
            path: "/findingHistory/get",
            body: Some(json!({
                "FindingIdentifier": {
                    "Id": "finding-1",
                    "ProductArn": "arn:aws:securityhub:us-east-1::product/aws/securityhub",
                },
            })),
            status: 401,
            code: "InvalidAccessException",
        },
        HubScopedCall {
            operation: "DisableSecurityHub",
            method: "DELETE",
            path: "/accounts",
            body: None,
            status: 404,
            code: "ResourceNotFoundException",
        },
    ]
}

/// Assert one hub-scoped call's refusal against the rule.
fn assert_not_subscribed(case: &str, call: &HubScopedCall, region: &str, answer: &(u16, Value)) {
    let (status, body) = answer;
    let account_id = engine::emulator_account_id();
    assert_eq!(
        *status, call.status,
        "case {case:?}: expected HTTP {}, got {status} with {body}",
        call.status
    );
    assert_eq!(
        error_code(body),
        call.code,
        "case {case:?}: expected {}, got {body}",
        call.code
    );
    let expected = expected_not_subscribed_message(account_id);
    let actual = message(body);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected message {expected:?}, got {actual:?}"
    );
    assert!(
        !actual.contains(region),
        "case {case:?}: the message interpolates no Region, got {actual:?}"
    );
    assert!(
        actual.contains(account_id),
        "case {case:?}: the message names the calling account {account_id}, got {actual:?}"
    );
}

/// RULE (never subscribed): every hub-scoped operation refuses an account that never
/// enabled Security Hub CSPM with the fixed not-subscribed text, in every signing
/// Region.
#[tokio::test]
async fn every_hub_scoped_operation_refuses_an_account_that_never_subscribed() {
    // Four signing Regions, including the emulator's default: the message is the same
    // in all of them, which is how a Region clause would be caught.
    for region in ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-2"] {
        for call in hub_scoped_calls() {
            let case = format!("{} in {region}", call.operation);
            // A fresh emulator per row: the refusal is about there being no hub at all.
            let harness = Harness::new(region);
            let answer = harness
                .call(call.method, call.path, call.body.clone())
                .await;
            assert_not_subscribed(&case, &call, region, &answer);
        }
    }
}

/// RULE (subscribed, then disabled): an account whose subscription was removed is
/// refused with exactly the same text as one that never had a subscription, and an
/// account subscribed in ANOTHER Region is still unsubscribed here.
#[tokio::test]
async fn an_unsubscribed_region_is_refused_with_the_same_text_after_a_disable() {
    const SUBSCRIBED_REGION: &str = "us-west-2";
    const OTHER_REGION: &str = "eu-central-1";

    for call in hub_scoped_calls() {
        // Enable, then disable: the account held a subscription and holds none now.
        let harness = Harness::new(SUBSCRIBED_REGION);
        let (status, body) = harness.call("POST", "/accounts", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "case {:?}: EnableSecurityHub expected 200, got {status} with {body}",
            call.operation
        );
        let (status, body) = harness.call("DELETE", "/accounts", None).await;
        assert_eq!(
            status, 200,
            "case {:?}: DisableSecurityHub expected 200, got {status} with {body}",
            call.operation
        );

        let case = format!("{} after a disable", call.operation);
        let answer = harness
            .call(call.method, call.path, call.body.clone())
            .await;
        assert_not_subscribed(&case, &call, SUBSCRIBED_REGION, &answer);

        // A hub in one Region does not subscribe another: the same refusal, same text.
        let harness = Harness::new(SUBSCRIBED_REGION);
        let (status, body) = harness.call("POST", "/accounts", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "case {:?}: EnableSecurityHub expected 200, got {status} with {body}",
            call.operation
        );
        let elsewhere = Harness {
            router: harness.router,
            region: OTHER_REGION.to_string(),
        };
        let case = format!("{} in an unsubscribed Region", call.operation);
        let answer = elsewhere
            .call(call.method, call.path, call.body.clone())
            .await;
        assert_not_subscribed(&case, &call, OTHER_REGION, &answer);
    }
}
