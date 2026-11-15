// SPDX-License-Identifier: CC0-1.0

//! The ASSEMBLED emulator serves a CloudFormation stack that declares an `AWS::Rbin::Rule`: the
//! retention rule is created in Recycle Bin's own control plane, the stack publishes the identity
//! that service minted, and deleting the stack takes the rule away.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and never the `Template format error: Unrecognized
//! resource types` a type CloudFormation does not know is refused with.**
//!
//! This suite is the ROUTER's smoke test of that pairing: it stands the two crates up the way
//! `core/router/src/lib.rs` mounts them and drives one rule through its whole life. The TABLES
//! that span the rule — the three resource types, each type's published retention bound, the
//! selections a rule may carry, the published return values, both template spellings, the
//! declared and propagated tags, a changed declaration, and the refusal the owning service
//! answers for a period a type does not support — live in the CloudFormation crate's own suite
//! (`services/service-com-amazonaws-cloudformation-cloudformation/tests/all/\
//! rbin_rule_provisioning.rs`), whose harness mounts Recycle Bin beside CloudFormation on one
//! engine.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack's resources belong
/// to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it looks
/// again. Generous: the stack's own clock-owed transition has to come due, and the suite waits
/// for the CONDITION (the stack's status, the rule's existence) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with, which
/// is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The resource type under test, and the resource type the declared rule retains.
const RULE: &str = "AWS::Rbin::Rule";
const EBS_SNAPSHOT: &str = "EBS_SNAPSHOT";

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
    router.register_service(
        service_com_amazonaws_rbin_amazonrecyclebin::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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

/// `GetRule` over Recycle Bin's own restJson1 wire (`GET /rules/{Identifier}`) — that service's
/// own read of one retention rule.
async fn get_rule(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/rules/{identifier}"))
        .header("host", format!("rbin.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("rbin"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the stack
/// has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || std::time::Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The identifier Recycle Bin minted, as the ARN it built around it carries it.
fn identifier_in(arn: &str) -> String {
    arn.rsplit_once('/')
        .map(|(_, identifier)| identifier.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_assembled_emulator_deploys_a_retention_rule_and_takes_it_away_with_the_stack() {
    let router = build_router();
    let case = "one tag-level snapshot rule through the assembled router";
    let stack_name = "rbin-rule-router";
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Rule": {
                "Type": RULE,
                "Properties": {
                    "ResourceType": EBS_SNAPSHOT,
                    "RetentionPeriod": {
                        "RetentionPeriodValue": 1,
                        "RetentionPeriodUnit": "DAYS",
                    },
                    "ResourceTags": [
                        {"ResourceTagKey": "retain", "ResourceTagValue": "router"}
                    ],
                }
            }
        },
        "Outputs": {
            "RuleRef": {"Value": {"Ref": "Rule"}},
            "RuleIdentifier": {"Value": {"Fn::GetAtt": ["Rule", "Identifier"]}}
        }
    })
    .to_string();

    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert!(
        !created.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {created}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an {RULE} is a type a template may \
         declare — got {status} with body {created}"
    );

    let body = settled_stack(&router, case, stack_name).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body}"
    );
    let outputs = outputs_of(&body);
    let published = outputs
        .get("RuleRef")
        .unwrap_or_else(|| panic!("case {case:?}: no RuleRef output in {body}"))
        .clone();
    let identifier = identifier_in(&published);
    assert_eq!(
        outputs.get("RuleIdentifier"),
        Some(&identifier),
        "case {case:?}: expected the published identifier to be the one inside the published ARN, \
         got {outputs:?}"
    );

    // The rule the stack claims to have created is a rule Recycle Bin holds, under the identity
    // the stack published.
    let (status, rule) = get_rule(&router, &identifier).await;
    assert_eq!(
        (status, rule.get("RuleArn").and_then(Value::as_str)),
        (200, Some(published.as_str())),
        "case {case:?}: expected Recycle Bin to report the rule under the published identity, got \
         {status} with {rule}"
    );

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );

    // ...and it is gone with the stack, which is what makes a stack's resources the stack's own
    // rather than leftovers. Waits for the CONDITION, under the same generous deadline.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, answered) = get_rule(&router, &identifier).await;
        if status == 404 {
            assert_eq!(
                answered.get("__type").and_then(Value::as_str),
                Some("ResourceNotFoundException"),
                "case {case:?}: expected that service's own not-found refusal, got {answered}"
            );
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the rule {identifier:?} to be released with the stack, it is \
             still held: {answered}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}
