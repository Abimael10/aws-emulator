// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack reports for an `AWS::ApiGateway::Authorizer` whose
//! `ProviderARNs` entry names no Amazon Cognito user pool.
//!
//! RULE under test: **a `COGNITO_USER_POOLS` authorizer's `providerARNs` entries each name the
//! user pool whose tokens that authorizer accepts, so API Gateway refuses a write of the member
//! carrying an entry that names none — `400` `BadRequestException`, the member's own fixed
//! sentence followed by the rejected entries — and CloudFormation reports that refusal as the
//! resource FAILING: the Authorizer resource goes `CREATE_FAILED` carrying the exception its
//! provider's SDK raised, the deployment does not complete, and the api holds no authorizer at
//! all. A declaration whose entry DOES name a pool is created, and the api reports it.**
//!
//! Why it matters: a client that deploys an authorizer with a typo'd ARN is otherwise told the
//! deployment succeeded, and every later call through that authorizer is decided by an
//! authorizer bound to nothing — the exact failure CloudFormation exists to report at deploy
//! time. Tooling that attributes a failed deployment also splits the reason on ` (Service: ` to
//! recover which service refused the call and with what status, so the quote has to carry the
//! owning service's own sentence and that SDK's diagnostic of the call.
//!
//! Two crates have to agree for the rule to hold (API Gateway owns the authorizer and words the
//! refusal, CloudFormation provisions the resource and reports it), so the suite lives here
//! rather than in either one.
//!
//! The rows differ along what the rule generalises over: WHAT the declared entry is (a value
//! that is no ARN at all, an ARN of another service's pool, an ARN reaching past one pool to its
//! app client) and, for the control, a `ProviderARNs` resolved from the stack's OWN user pool
//! through `Fn::GetAtt`. Every expected value is computed here from what the row declares,
//! never read back out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources belong
/// to.
const REGION: &str = "us-east-1";

/// The SDK's own name for Amazon API Gateway, as the exception its provider's SDK raises states
/// it: that model's `awsService.cloudFormationName`
/// (`smithy_query(query="service_info")` on that service's model).
const SDK_SERVICE: &str = "ApiGateway";

/// The status API Gateway answers a request it found a fault in with: the `httpError` of its
/// modelled `BadRequestException`, which `CreateAuthorizer` declares
/// (`smithy_query(query="operation",operation="CreateAuthorizer")`).
const REFUSED_REQUEST_STATUS: u16 = 400;

/// How the sentence API Gateway words for a `providerARNs` entry that names no user pool OPENS:
/// fixed text, followed by the rejected entries, one per line.
const INVALID_PROVIDER_ARNS: &str =
    "ProviderARNs need to be valid Cognito Userpools. Invalid ARNs-";

/// The `HandlerErrorCode` the Authorizer type's provider reports such a refusal under — the
/// class that provider maps API Gateway's `BadRequestException` onto. One of the CloudFormation
/// model's own `HandlerErrorCode` values.
const HANDLER_ERROR_CLASS: &str = "AlreadyExists";

/// How long a wait for a stack to reach a terminal status, or for an event to be recorded, is
/// given.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the stack's
/// own clock, and every wait below ends as soon as the condition holds rather than after a fixed
/// span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator as it is ASSEMBLED, on one engine: the resources CloudFormation creates are the
/// resources this suite then reads through their owning service.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Read one API Gateway control-plane resource over its restJson1 wire, which is how a client
/// reads back what the stack reported.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Submit `template` as `stack`, keeping a failed deployment's resources where `keep_failed`
/// asks for it, and asserting the submission itself was accepted.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    keep_failed: bool,
) {
    let mut members = vec![("StackName", stack), ("TemplateBody", template)];
    if keep_failed {
        members.push(("DisableRollback", "true"));
    }
    let (status, body) = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in with the `DescribeStacks` answer that status was read from.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status — rather than on a
/// fixed span, so it ends as soon as the stack has settled.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return (reached, body);
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, or `None` where
/// the stack has recorded no such event.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so
/// a reason is never read off another resource's — or another transition's — event.
async fn recorded_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Option<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body).into_iter().find_map(|member| {
        let named = elements(member, "LogicalResourceId")
            .into_iter()
            .next()
            .is_some_and(|named| named == logical_id);
        let failed = elements(member, "ResourceStatus")
            .into_iter()
            .next()
            .is_some_and(|status| status == reported);
        (named && failed)
            .then(|| elements(member, "ResourceStatusReason").into_iter().next())
            .flatten()
            .filter(|reason| !reason.is_empty())
    })
}

/// The same reason, looked for again until the stack's own clock has recorded it or
/// [`SETTLE_DEADLINE`] passes.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(reason) = recorded_reason(router, case, stack, logical_id, reported).await {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {reported} reason within {SETTLE_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape this provider's SDK raises — the service's own
/// sentence, then the diagnostic of the refused call (the SDK's name for the service, the status
/// it answered, the id of the call) and the attempt count past it — and answer the sentence.
fn asserted_sentence(case: &str, quoted: &str) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    assert!(
        !sentence.trim().is_empty(),
        "case {case:?}: expected the service's own sentence ahead of the diagnostic, got \
         {quoted:?}"
    );
    let (envelope, attempts) = diagnostic.split_once(") (").unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic and the attempt count, got {quoted:?}")
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id, got {envelope:?}"
    );
    assert_eq!(
        fields[0], SDK_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_REQUEST_STATUS}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    let counted = attempts
        .strip_prefix("SDK Attempt Count: ")
        .and_then(|count| count.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected an attempt count, got {quoted:?}"));
    assert!(
        counted.parse::<u32>().is_ok_and(|count| count >= 1),
        "case {case:?}: expected the call to have been attempted at least once, got {counted:?}"
    );
    sentence.to_string()
}

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// The template every row deploys: the stack's own user pool, a REST API, and the
/// `COGNITO_USER_POOLS` authorizer whose `ProviderARNs` the row declares — plus the outputs a
/// client reads the api and the pool's own ARN from.
fn template(stack: &str, provider_arns: Value) -> String {
    json!({
        "Resources": {
            "Pool": {
                "Type": "AWS::Cognito::UserPool",
                "Properties": { "UserPoolName": format!("{stack}-pool") }
            },
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "Auth": {
                "Type": "AWS::ApiGateway::Authorizer",
                "Properties": {
                    "Name": format!("{stack}-gate"),
                    "RestApiId": { "Ref": "Api" },
                    "Type": "COGNITO_USER_POOLS",
                    "IdentitySource": "method.request.header.Authorization",
                    "ProviderARNs": provider_arns
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "PoolArn": { "Value": { "Fn::GetAtt": ["Pool", "Arn"] } }
        }
    })
    .to_string()
}

/// The authorizers the api `api_id` holds, under the `item` member that service's model gives a
/// collection page (`jsonName: item`); an empty collection carries no `item` at all, which is
/// the same claim as an empty list.
async fn authorizers(router: &MultiServiceRouter, case: &str, api_id: &str) -> Vec<Value> {
    let (status, page) = apigateway(router, &format!("/restapis/{api_id}/authorizers")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAuthorizers({api_id}), got {status} with \
         {page}"
    );
    page.get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A value that is no ARN at all, as a template typo leaves one.
const NOT_AN_ARN: &str = "not-an-arn";

/// An identity pool's ARN: Amazon Cognito Identity's resource, which is not a user pool.
const IDENTITY_POOL_ARN: &str =
    "arn:aws:cognito-identity:us-east-1:123456789012:identitypool/us-east-1:a1b2c3d4";

/// A user pool's APP CLIENT, an ARN that reaches PAST one pool.
const APP_CLIENT_ARN: &str =
    "arn:aws:cognito-idp:us-east-1:123456789012:userpool/us-east-1_pool/client/1a2b3c";

/// One row whose declared `ProviderARNs` API Gateway refuses: the stack it deploys under and the
/// entries the refusal has to list, in the order the declaration wrote them.
struct Refused {
    case: &'static str,
    stack: &'static str,
    declared: &'static [&'static str],
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "an entry that is no ARN at all",
        stack: "apigw-authorizer-provider-not-an-arn",
        declared: &[NOT_AN_ARN],
    },
    Refused {
        case: "an identity pool, which is another service's resource",
        stack: "apigw-authorizer-provider-identity-pool",
        declared: &[IDENTITY_POOL_ARN],
    },
    Refused {
        case: "a pool's app client, which is not the pool",
        stack: "apigw-authorizer-provider-app-client",
        declared: &[APP_CLIENT_ARN],
    },
];

#[tokio::test]
async fn an_authorizer_whose_provider_arn_names_no_pool_fails_its_stack_resource() {
    for row in REFUSED {
        let case = row.case;
        let router = build_router();
        let declared: Vec<&str> = row.declared.to_vec();
        create_stack(
            &router,
            case,
            row.stack,
            &template(row.stack, json!(declared)),
            true,
        )
        .await;

        let (status, body) = settled(&router, case, row.stack).await;
        assert_eq!(
            status, "CREATE_FAILED",
            "case {case:?}: expected the refused authorizer to fail the deployment, got \
             {status:?} with {body}"
        );

        let reason = failure_reason(&router, case, row.stack, "Auth", "CREATE_FAILED").await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let sentence = asserted_sentence(case, &quoted);
        let expected = {
            let mut message = String::from(INVALID_PROVIDER_ARNS);
            for arn in &declared {
                message.push('\n');
                message.push_str(arn);
            }
            message
        };
        assert_eq!(
            sentence, expected,
            "case {case:?}: expected the sentence API Gateway words for the entries it refused, \
             got {sentence:?} in {reason:?}"
        );
        assert_eq!(
            class, HANDLER_ERROR_CLASS,
            "case {case:?}: expected the class this provider reports the refusal under, got \
             {class:?} in {reason:?}"
        );

        // Nothing was created: the api the stack did build holds no authorizer at all. A
        // deployment that did not complete publishes no outputs, so the api is read from the
        // resource the stack DID create.
        let api_id = api_physical_id(&router, case, row.stack).await;
        let held = authorizers(&router, case, &api_id).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected the api to hold no authorizer, got {held:?}"
        );
    }
}

/// The physical id the stack recorded for its api, read from `DescribeStackResources`.
async fn api_physical_id(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
        .get("Api")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a physical id for the api, got {body}"))
}

#[tokio::test]
async fn an_authorizer_over_the_stacks_own_pool_is_created_and_reported() {
    let case = "ProviderARNs resolved from the stack's own user pool";
    let stack = "apigw-authorizer-provider-own-pool";
    let router = build_router();
    create_stack(
        &router,
        case,
        stack,
        &template(stack, json!([{ "Fn::GetAtt": ["Pool", "Arn"] }])),
        false,
    )
    .await;

    let (status, body) = settled(&router, case, stack).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {case:?}: expected the declared authorizer to be created, got {status:?} with \
         {body}"
    );
    let outputs = outputs_of(&body);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the api's id among the outputs: {body}"));
    let pool_arn = outputs
        .get("PoolArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the pool's ARN among the outputs: {body}"));

    let held = authorizers(&router, case, &api_id).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the api to hold the declared authorizer, got {held:?}"
    );
    assert_eq!(
        held[0].get("providerARNs"),
        Some(&json!([pool_arn])),
        "case {case:?}: expected the authorizer to carry the pool's own ARN, got {:?}",
        held[0]
    );
}
