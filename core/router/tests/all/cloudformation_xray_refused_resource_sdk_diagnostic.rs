// SPDX-License-Identifier: CC0-1.0

//! What a resource AWS X-RAY refused reports on its own `CREATE_FAILED` stack event.
//!
//! RULE under test: **a CloudFormation resource provider words no diagnosis of its own for a
//! call the owning service refused — what the failed resource's `ResourceStatusReason` quotes
//! is the exception that provider's SDK raised: the service's own sentence followed by that
//! SDK's diagnostic of the call, naming the service as its own SDK names it, the HTTP status
//! that service answered and the id of the call, ahead of the `RequestToken` and
//! `HandlerErrorCode` suffix CloudFormation adds.**
//!
//! Why it matters: tooling that attributes a failed deployment reads the stack event's reason
//! and splits on ` (Service: ` to recover WHICH service refused the call and with WHAT HTTP
//! status — a CDK diagnostic, a deploy pipeline that retries on an upstream 4xx, any matcher
//! anchored on AWS's reason format. A quote that closes straight after the service's own
//! sentence carries neither, so an upstream API rejection is indistinguishable from
//! CloudFormation's own template or handler validation and no retry keyed on an upstream
//! status can fire.
//!
//! Two crates have to agree for the rule to hold (CloudFormation provisions the resource, AWS
//! X-Ray owns it and words the refusal), so the suite lives here rather than in either one.
//!
//! The rows differ along what the rule generalises over: WHICH type of that service the
//! refused resource is (`AWS::XRay::Group` and `AWS::XRay::SamplingRule`), WHICH call of that
//! service was refused (`CreateGroup`, `CreateSamplingRule`) and WHY it was refused — a filter
//! expression that is not of the language, in three different ways, and a sampling rule name
//! the account already holds. The control is the same stack shape with a filter expression the
//! language DOES accept: nothing refuses it, the stack reports `CREATE_COMPLETE` and X-Ray
//! itself reports the declared expression.
//!
//! The sentence inside the quotes is computed from the owning service's OWN answer to the same
//! call — asked directly over X-Ray's wire — never read back out of the stack event being
//! checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The SDK's own name for AWS X-Ray, as the exception its SDK raises states it: that model's
/// `sdkId`, which is also its `awsService.cloudFormationName`
/// (`smithy_query(query="service_info")` on that service's model).
const SDK_SERVICE: &str = "XRay";

/// The status X-Ray answers for a request of these calls it rejects: the `httpError` of the
/// `InvalidRequestException` its `CreateGroup` and `CreateSamplingRule` both model.
const REFUSED_REQUEST_STATUS: u16 = 400;

/// The `HandlerErrorCode` class such a refusal falls in — the owning service refused what the
/// declaration asked for. One of the CloudFormation model's own `HandlerErrorCode` values.
const INVALID_REQUEST: &str = "InvalidRequest";

/// How long a stack is given to reach a terminal status, and how long one look-again waits.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the stack's
/// own clock, and every wait below ends as soon as the condition holds rather than after a
/// fixed span — so a loaded host cannot decide the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
        engine,
    ));
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

/// Call one AWS X-Ray operation over the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding names.
async fn xray(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("xray.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("xray"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// X-Ray's OWN answer to `request`: the sentence that service words for a call it refuses,
/// asked directly over its wire, so the expected text comes from the owning service rather
/// than from the stack event under test.
async fn xray_refuses(router: &MultiServiceRouter, case: &str, uri: &str, request: &Value) -> String {
    let (status, answered) = xray(router, uri, request).await;
    assert_eq!(
        status, REFUSED_REQUEST_STATUS,
        "case {case:?}: expected X-Ray itself to refuse {uri} with {REFUSED_REQUEST_STATUS}, got \
         {status} with {answered}"
    );
    answered
        .get("Message")
        .or_else(|| answered.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message from {uri}, got {answered}"))
        .to_string()
}

/// Submit `template` as `stack`, asserting the submission itself was accepted.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status — rather than on
/// a fixed span, so it ends as soon as the stack has settled.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
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
            return reached;
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

/// Hold one quoted exception to the shape an AWS X-Ray SDK raises: that service's own sentence,
/// then the diagnostic of the refused call — the SDK's name for the service, the HTTP status it
/// answered and the id of the call, and nothing past the call itself. Answers the sentence.
fn asserted_sentence(case: &str, quoted: &str, status: u16) -> String {
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
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], SDK_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
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

/// A template declaring one `AWS::XRay::Group` under the logical id `TraceGroup`.
fn group_template(name: &str, expression: &str) -> String {
    json!({
        "Resources": {
            "TraceGroup": {
                "Type": "AWS::XRay::Group",
                "Properties": { "GroupName": name, "FilterExpression": expression }
            }
        }
    })
    .to_string()
}

/// A template declaring one `AWS::XRay::SamplingRule` under the logical id `SamplingRule`.
fn sampling_rule_template(name: &str) -> String {
    json!({
        "Resources": {
            "SamplingRule": {
                "Type": "AWS::XRay::SamplingRule",
                "Properties": { "SamplingRule": sampling_rule(name) }
            }
        }
    })
    .to_string()
}

/// The sampling rule both the template above and the direct probe below state.
fn sampling_rule(name: &str) -> Value {
    json!({
        "RuleName": name,
        "ResourceARN": "*",
        "Priority": 1_000,
        "FixedRate": 0.05,
        "ReservoirSize": 1,
        "ServiceName": "*",
        "ServiceType": "*",
        "Host": "*",
        "HTTPMethod": "*",
        "URLPath": "*",
        // The model documents the sampling rule format as `1`.
        "Version": 1,
    })
}

/// One row whose resource X-Ray refuses to create: what the stack declares, and the call this
/// suite asks X-Ray directly to learn the sentence that service words for it.
struct RefusedCreate {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the resource the row is about.
    logical_id: &'static str,
    template: String,
    /// The X-Ray call the same declaration makes, as (URI, request), asked directly so the
    /// expected sentence is that service's own.
    probe: (&'static str, Value),
    /// A sampling rule this row needs X-Ray to already hold, by name.
    held_rule: Option<&'static str>,
}

/// One refused create per type, per refused call and per reason the rule covers.
fn refused_creates() -> Vec<RefusedCreate> {
    // Three ways a filter expression is not of X-Ray's filter-expression language: an
    // unbalanced parenthesis, a keyword the language does not have, and a comparison with no
    // value. Each is refused by that service's own compiler, which words its own sentence.
    let unbalanced = "(annotation.scenario = \"x\"";
    let unknown_keyword = "whatever = 1";
    let dangling = "fault = true AND";
    vec![
        RefusedCreate {
            case: "a group whose filter expression leaves a parenthesis open",
            stack: "xray-diag-unbalanced",
            logical_id: "TraceGroup",
            template: group_template("xray-diag-unbalanced", unbalanced),
            probe: (
                "/CreateGroup",
                json!({ "GroupName": "xray-diag-probe-1", "FilterExpression": unbalanced }),
            ),
            held_rule: None,
        },
        RefusedCreate {
            case: "a group whose filter expression names a keyword the language does not have",
            stack: "xray-diag-unknown-keyword",
            logical_id: "TraceGroup",
            template: group_template("xray-diag-unknown", unknown_keyword),
            probe: (
                "/CreateGroup",
                json!({ "GroupName": "xray-diag-probe-2", "FilterExpression": unknown_keyword }),
            ),
            held_rule: None,
        },
        RefusedCreate {
            case: "a group whose filter expression ends on a dangling operator",
            stack: "xray-diag-dangling",
            logical_id: "TraceGroup",
            template: group_template("xray-diag-dangling", dangling),
            probe: (
                "/CreateGroup",
                json!({ "GroupName": "xray-diag-probe-3", "FilterExpression": dangling }),
            ),
            held_rule: None,
        },
        RefusedCreate {
            case: "a sampling rule whose name the account already holds",
            stack: "xray-diag-rule-taken",
            logical_id: "SamplingRule",
            template: sampling_rule_template("xray-diag-rule-taken"),
            probe: (
                "/CreateSamplingRule",
                json!({ "SamplingRule": sampling_rule("xray-diag-rule-taken") }),
            ),
            held_rule: Some("xray-diag-rule-taken"),
        },
    ]
}

#[tokio::test]
async fn a_create_xray_refuses_reports_the_xray_sdk_exception() {
    for row in refused_creates() {
        let case = row.case;
        let router = build_router();

        // What the row needs X-Ray to already hold, created through that service's own API.
        if let Some(name) = row.held_rule {
            let (status, body) = xray(
                &router,
                "/CreateSamplingRule",
                &json!({ "SamplingRule": sampling_rule(name) }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the sampling rule the row needs held must be created, got \
                 {status} with {body}"
            );
        }

        // What X-Ray itself says about the same call, which is what the stack event has to
        // quote.
        let (uri, request) = &row.probe;
        let says = xray_refuses(&router, case, uri, request).await;

        create_stack(&router, case, row.stack, &row.template).await;
        let settled = settled_status(&router, case, row.stack).await;
        assert!(
            settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
            "case {case:?}: expected the deployment to fail on the refused resource, got \
             {settled:?}"
        );

        let reason =
            failure_reason(&router, case, row.stack, row.logical_id, "CREATE_FAILED").await;
        let sentence = {
            let (quoted, class) = quoted_and_class(case, &reason);
            assert_eq!(
                class, INVALID_REQUEST,
                "case {case:?}: expected the class a refusal of the request falls in, got \
                 {class:?} in {reason:?}"
            );
            asserted_sentence(case, &quoted, REFUSED_REQUEST_STATUS)
        };
        assert_eq!(
            sentence, says,
            "case {case:?}: expected the sentence X-Ray itself words for this call, got \
             {sentence:?} in {reason:?}"
        );
    }
}

/// The CONTROL: the same stack shape with a filter expression the language DOES accept.
/// Nothing refuses it, so the stack reports the create complete, the resource records no
/// failure and X-Ray itself reports the declared expression — which is what makes each refusal
/// above a fact about the declaration rather than a deployment that fails whatever it states.
#[tokio::test]
async fn a_group_the_owning_service_accepts_deploys_and_records_no_refusal() {
    let case = "a group whose filter expression is of the language";
    let stack = "xray-diag-accepted";
    let name = "xray-diag-accepted";
    let expression = "annotation.scenario = \"ok\"";
    let router = build_router();

    create_stack(&router, case, stack, &group_template(name, expression)).await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the group to be created, got {settled:?}"
    );
    let failed = recorded_reason(&router, case, stack, "TraceGroup", "CREATE_FAILED").await;
    assert!(
        failed.is_none(),
        "case {case:?}: expected no refused create, got {failed:?}"
    );

    let (status, answered) = xray(&router, "/GetGroup", &json!({ "GroupName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected X-Ray's own read to resolve the group, got {status} with \
         {answered}"
    );
    assert_eq!(
        answered
            .get("Group")
            .and_then(|group| group.get("FilterExpression"))
            .and_then(Value::as_str),
        Some(expression),
        "case {case:?}: expected X-Ray to report the declared expression, got {answered}"
    );
}
