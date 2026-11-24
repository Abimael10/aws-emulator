// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the two things `TestInvokeAuthorizer` settles BEFORE an
//! authorizer's Lambda function is reached: whether the request describes an authorization
//! request at all, and whether that function's resource-based policy admits the invocation
//! AWS IoT would make.
//!
//! Both rules are about the operation as a whole rather than about one authorizer, so each
//! is driven over a TABLE spanning what it generalises over.
//!
//! * **A request with no protocol context is no authorization request.** `httpContext`
//!   "specifies a test HTTP authorization request", `mqttContext` a test MQTT one and
//!   `tlsContext` a test TLS one, and all three are optional
//!   (`smithy_query(query="operation",operation="TestInvokeAuthorizer")`) — so a request
//!   carrying none of them asks for nothing to be authorized and is refused
//!   `InvalidRequestException` (400) with the sentence that refusal carries, whatever token,
//!   signature or authorizer it names, and before any function is invoked. A request
//!   carrying ANY of the three gets past it.
//! * **The invocation is admitted only by the function's resource-based policy.** An
//!   authorizer names only its `authorizerFunctionArn` — there is no role beside it — so
//!   AWS IoT invokes the function as its own service principal, which `lambda:AddPermission`
//!   has to grant ("the identifier of the service that invokes the function",
//!   `AddPermission$Principal`), narrowed where the grant names a `SourceArn` ("for AWS
//!   services, the ARN of the AWS resource that invokes the function"). An invocation no
//!   statement admits is refused by AWS Lambda as a denial — a principal the policy grants
//!   nothing is told nothing about the function — and the operation reports that as
//!   `InvalidResponseException` (400) naming the class the invocation was refused with and
//!   the id of that invocation, and naming neither the authorizer nor the function it
//!   resolved.
//!
//! Every function here is created through Lambda's own API on the same shared engine, every
//! grant is written through Lambda's own `AddPermission`, and every authorizer is created
//! through IoT's own `CreateAuthorizer` — so a row can only pass if the two services agree
//! about the one grant between them. The harness is the one the suite next door uses
//! (`crate::custom_authorizer_invocation`), so there is one spelling of a staged function,
//! a grant and a `TestInvokeAuthorizer` call.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::custom_authorizer_invocation::{
    authorizer_create, create_function, grant_invoke, rest, router, test_invoke, wait_active,
    Reply, ECHO_SOURCE, IOT_PRINCIPAL,
};

/// The refusal AWS IoT answers a `TestInvokeAuthorizer` that names none of the three
/// protocol contexts with — fixed text, with no part of the request in it.
const NO_CONTEXT_MESSAGE: &str = "Request must have Http/Mqtt/Tls context.";

/// The fixed half of the refusal of an invocation AWS Lambda did not admit: the error class
/// it was refused with, then the id of the invocation.
const DENIED_INVOCATION_PREFIX: &str =
    "AccessDeniedException encountered when invoking lambda; requestId: ";

/// Create an authorizer over `function_arn` that validates no token signature, and hand
/// back the ARN IoT minted for it — the ARN a grant narrowed to this authorizer names.
async fn authorizer_over(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    function_arn: &str,
) -> String {
    let created = authorizer_create(router, name, function_arn, &BTreeMap::new(), true).await;
    assert_eq!(
        created.status, 200,
        "CreateAuthorizer {name:?} must succeed, got {} ({})",
        created.status, created.body
    );
    created.body["authorizerArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateAuthorizer must mint an ARN, got {}", created.body))
        .to_string()
}

/// RULE: a `TestInvokeAuthorizer` carrying none of `httpContext`, `mqttContext` and
/// `tlsContext` is refused `InvalidRequestException` (400) with the sentence that refusal
/// carries, and one carrying any of them is not refused for it.
#[tokio::test]
async fn a_request_naming_no_protocol_context_is_refused() {
    let router = router();
    let function = create_function(&router, "context-rule-authorizer", ECHO_SOURCE).await;
    wait_active(&router, "context-rule-authorizer").await;
    grant_invoke(
        &router,
        "context-rule-authorizer",
        "iot-invoke",
        IOT_PRINCIPAL,
        None,
    )
    .await;
    let _ = authorizer_over(&router, "contextrule", &function).await;

    // The rows span what the rule generalises over: which of the three contexts a request
    // carries (none, each one alone, all three), and what else it carries beside them — no
    // token at all, a token, a token with a signature, and a long token — since none of
    // that is what the rule reads.
    let signature = "YWJjZA==";
    let long_token = "t".repeat(6_000);
    let rows: [(&str, Value, bool); 10] = [
        ("nothing at all", json!({}), false),
        ("a token alone", json!({ "token": "tok" }), false),
        (
            "a token and a signature",
            json!({ "token": "tok", "tokenSignature": signature }),
            false,
        ),
        (
            "a 6000-character token and a signature",
            json!({ "token": long_token, "tokenSignature": signature }),
            false,
        ),
        (
            "an empty http context",
            json!({ "httpContext": {} }),
            true,
        ),
        (
            "an http context",
            json!({ "httpContext": { "queryString": "?k=v" } }),
            true,
        ),
        (
            "an mqtt context",
            json!({ "mqttContext": { "clientId": "device-01" } }),
            true,
        ),
        (
            "a tls context",
            json!({ "tlsContext": { "serverName": "data.iot.example.test" } }),
            true,
        ),
        (
            "a tls context beside a token and a signature",
            json!({
                "token": "tok",
                "tokenSignature": signature,
                "tlsContext": { "serverName": "data.iot.example.test" },
            }),
            true,
        ),
        (
            "all three contexts at once",
            json!({
                "httpContext": { "queryString": "?k=v" },
                "mqttContext": { "clientId": "device-02" },
                "tlsContext": { "serverName": "both.iot.example.test" },
            }),
            true,
        ),
    ];

    for (case, body, names_a_context) in rows {
        let answer = test_invoke(&router, "contextrule", &body).await;
        if names_a_context {
            // The authorizer's function is staged and granted, so a request that names a
            // context is ANSWERED: whatever this row gets, it is not the refusal above.
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the authorizer to be run, got {} ({})",
                answer.status, answer.body
            );
            assert_ne!(
                answer.message(),
                NO_CONTEXT_MESSAGE,
                "case {case:?}: expected no context refusal, got {}",
                answer.body
            );
            continue;
        }
        assert_eq!(
            (answer.status, answer.code().as_str(), answer.message().as_str()),
            (400, "InvalidRequestException", NO_CONTEXT_MESSAGE),
            "case {case:?}: expected HTTP 400 InvalidRequestException {NO_CONTEXT_MESSAGE:?}, \
             got {} {} ({})",
            answer.status,
            answer.code(),
            answer.body
        );
    }
}

/// RULE: the authorizer's function is invoked only where the function's own resource-based
/// policy admits this service's invocation of it, for this authorizer; an invocation no
/// grant admits is refused as the denial it is, in the sentence that class carries, and
/// nothing of the authorizer or of the function it resolved is in it.
#[tokio::test]
async fn an_invocation_no_resource_policy_admits_is_refused() {
    let router = router();

    // One staged function per form a grant can take, so what differs between the rows is
    // the policy document Lambda holds and nothing else. Each is created and waited Active
    // the same way; only the `AddPermission` after it differs.
    for name in [
        "granted-authorizer-fn",
        "narrowed-authorizer-fn",
        "ungranted-authorizer-fn",
        "other-principal-authorizer-fn",
        "other-source-authorizer-fn",
    ] {
        create_function(&router, name, ECHO_SOURCE).await;
        wait_active(&router, name).await;
    }
    let granted = function_arn(&router, "granted-authorizer-fn").await;
    let narrowed = function_arn(&router, "narrowed-authorizer-fn").await;
    let ungranted = function_arn(&router, "ungranted-authorizer-fn").await;
    let other_principal = function_arn(&router, "other-principal-authorizer-fn").await;
    let other_source = function_arn(&router, "other-source-authorizer-fn").await;

    // The authorizers come first, because the ARN of the one being tested is what a
    // narrowed grant names as its source.
    let narrowed_authorizer = authorizer_over(&router, "narrowedgrant", &narrowed).await;
    authorizer_over(&router, "othergrant", &other_source).await;
    authorizer_over(&router, "unnarrowedgrant", &granted).await;
    authorizer_over(&router, "nogrant", &ungranted).await;
    authorizer_over(&router, "otherprincipal", &other_principal).await;
    // An authorizer pointed at a function Lambda does not hold at all: nothing grants that
    // invocation either, because there is no policy to grant it.
    authorizer_over(
        &router,
        "absentfn",
        "arn:aws:lambda:us-east-1:000000000000:function:no-such-authorizer-fn",
    )
    .await;

    grant_invoke(
        &router,
        "granted-authorizer-fn",
        "iot-invoke",
        IOT_PRINCIPAL,
        None,
    )
    .await;
    grant_invoke(
        &router,
        "narrowed-authorizer-fn",
        "iot-invoke",
        IOT_PRINCIPAL,
        Some(&narrowed_authorizer),
    )
    .await;
    grant_invoke(
        &router,
        "other-principal-authorizer-fn",
        "events-invoke",
        "events.amazonaws.com",
        None,
    )
    .await;
    // A grant for this service narrowed to ANOTHER authorizer's ARN: it admits that
    // authorizer's invocations and no other's.
    grant_invoke(
        &router,
        "other-source-authorizer-fn",
        "iot-invoke",
        IOT_PRINCIPAL,
        Some(&narrowed_authorizer),
    )
    .await;

    // (case, the authorizer tested, whether its function's policy admits the invocation)
    let rows: [(&str, &str, bool); 6] = [
        ("a grant for this service with no narrowing", "unnarrowedgrant", true),
        (
            "a grant for this service narrowed to this authorizer",
            "narrowedgrant",
            true,
        ),
        ("a function carrying no policy at all", "nogrant", false),
        (
            "a policy that grants another service principal",
            "otherprincipal",
            false,
        ),
        (
            "a grant narrowed to another authorizer",
            "othergrant",
            false,
        ),
        ("a function this account does not hold", "absentfn", false),
    ];

    let request = json!({ "mqttContext": { "clientId": "device-01" } });
    let mut refusal_ids: Vec<String> = Vec::new();
    for (case, authorizer, admitted) in rows {
        let answer = test_invoke(&router, authorizer, &request).await;
        if admitted {
            // The function ran, so the answer is the one it computed from the event.
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the authorizer's function to answer, got {} ({})",
                answer.status, answer.body
            );
            assert_eq!(
                answer.body["disconnectAfterInSeconds"],
                json!(3600),
                "case {case:?}: expected the function's own answer, got {}",
                answer.body
            );
            continue;
        }
        assert_eq!(
            (answer.status, answer.code().as_str()),
            (400, "InvalidResponseException"),
            "case {case:?}: expected HTTP 400 InvalidResponseException, got {} {} ({})",
            answer.status,
            answer.code(),
            answer.body
        );
        let message = answer.message();
        let Some(request_id) = message.strip_prefix(DENIED_INVOCATION_PREFIX) else {
            panic!("case {case:?}: expected a message opening {DENIED_INVOCATION_PREFIX:?}, got {message:?}");
        };
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the refusal to carry the invocation's id, got {message:?}"
        );
        // Which function an authorizer authorizes with is the account's configuration, not
        // a statement about the refusal: neither it nor the authorizer is in the message.
        assert!(
            !message.contains(authorizer) && !message.contains("arn:aws:lambda"),
            "case {case:?}: expected the refusal to name neither the authorizer nor the \
             function, got {message:?}"
        );
        refusal_ids.push(request_id.to_string());
    }

    // The id identifies the INVOCATION, so a second refused call carries a second id
    // rather than one fixed value.
    let repeated = test_invoke(&router, "nogrant", &request).await;
    let repeated_id = repeated
        .message()
        .strip_prefix(DENIED_INVOCATION_PREFIX)
        .unwrap_or_default()
        .to_string();
    assert!(
        !repeated_id.is_empty() && !refusal_ids.contains(&repeated_id),
        "case \"a second refused invocation\": expected an id of its own, got {:?} among {:?}",
        repeated.message(),
        refusal_ids
    );
}

/// The ARN AWS Lambda reports for the function `name`, read back through its own
/// configuration rather than built here.
async fn function_arn(router: &smithy_server_runtime::MultiServiceRouter, name: &str) -> String {
    let read: Reply = rest(
        router,
        "lambda",
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "GetFunctionConfiguration {name:?} must answer 200, got {} ({})",
        read.status, read.body
    );
    read.body["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the configuration must carry an ARN, got {}", read.body))
        .to_string()
}
