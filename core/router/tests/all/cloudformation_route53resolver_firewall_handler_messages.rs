// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::Route53Resolver::FirewallRuleGroupAssociation` Route 53 Resolver refused to
//! act on reports on its own `*_FAILED` stack event.
//!
//! RULE under test: **that type's resource provider reports a refusal OF THE REQUEST as the
//! invalid-request exception of the plugin it is written against, worded around the REQUEST it
//! issued — `Invalid request provided: ` then the name of the shape the called operation's
//! input is modelled under, then the members the provider set, each as `<Member>=<Value>` —
//! with the owning service's own sentence discarded; a refusal of any other class is passed on
//! as that service worded it.**
//!
//! Why it matters: tooling that classifies a failed deployment matches CloudFormation's
//! documented reason shape to recover the call the handler made and the values it made it
//! with. A reason carrying the provider-internal service code and a per-request trace id
//! instead matches nothing, names no call, and differs between two attempts at the same
//! deployment.
//!
//! Two crates have to agree for the rule to hold (CloudFormation issues the call and words the
//! reason; Route 53 Resolver owns the association and refuses the call), so the suite lives
//! here rather than in either one.
//!
//! The cases differ along what the rule generalises over: WHICH call of that service was
//! refused (`DisassociateFirewallRuleGroup` on a delete, `AssociateFirewallRuleGroup` on a
//! create), how many members the refused request carried (one, and every member an associate
//! takes), WHICH stack operation ran into the refusal, how many attempts were made at it, and
//! whether the refusal is of the REQUEST at all — the last case being a refusal of another
//! class, which is reported as the owning service worded it.
//!
//! Every expected value is computed here from what the case declares, never read back out of
//! the answer being checked, except the identifiers Route 53 Resolver itself mints.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The VPC the associations are made with, in the form Amazon EC2 ids take: Route 53 Resolver
/// resolves an association's `VpcId` against the account's own Amazon EC2 store, so the account
/// holds it before any stack naming it is deployed ([`register_vpc`]).
const VPC_ID: &str = "vpc-0a1b2c3d4e5f60718";

/// The opener the plugin's invalid-request exception words, which is the fixed text of every
/// reason the rule covers.
const INVALID_REQUEST_PROVIDED: &str = "Invalid request provided: ";

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, and the class a refusal
/// about a resource that service does not hold falls in. Both are values of the CloudFormation
/// model's own `HandlerErrorCode`.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The status Route 53 Resolver reports a settled DNS Firewall resource in.
const COMPLETE: &str = "COMPLETE";

/// How long a wait for a stack to settle, for an event to be recorded, or for a resource of the
/// owning service to reach its settled status is given.
///
/// Generous on purpose: what is waited FOR is a condition — the deployment settling, the event
/// being written — and every wait below ends as soon as that condition holds rather than after
/// a fixed span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    register_vpc(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_route53resolver_route53resolver::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// Put the VPC the cases name into the account's own Amazon EC2 store, as the modelled shape
/// that service reports.
fn register_vpc(engine: &engine::InMemoryEngine) {
    use service_com_amazonaws_ec2_amazonec2 as ec2;

    let ec2_state = ec2::state::service_state(engine)
        .expect("the in-memory engine carries the account's Amazon EC2 store");
    let mut store = ec2_state.lock();
    store.vpcs.insert(
        VPC_ID.to_string(),
        ec2::ops::types::Vpc {
            vpc_id: Some(VPC_ID.to_string()),
            cidr_block: Some("10.0.0.0/16".to_string()),
            state: Some(ec2::state::VPC_STATE_AVAILABLE.to_string()),
            ..Default::default()
        },
    );
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

/// Call one Route 53 Resolver operation over the awsJson1_1 wire its model declares.
async fn route53resolver(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("route53resolver.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .header("authorization", authorization("route53resolver"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one Route 53 Resolver operation that must be ACCEPTED, and answer its document.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, answer) = route53resolver(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answer}"
    );
    answer
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

/// Ask for `stack` to be deleted, asserting the submission itself was accepted.
async fn delete_stack(router: &MultiServiceRouter, case: &str, stack: &str) {
    let (status, body) = cloudformation(router, "DeleteStack", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in.
///
/// The wait is on that CONDITION rather than on a span of the clock, so it ends as soon as the
/// stack operation has finished.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.is_empty() && !reached.ends_with("_IN_PROGRESS") {
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

/// The physical id the stack publishes for `logical_id` — the id Route 53 Resolver minted for
/// the resource.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
        .get(logical_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a physical id for {logical_id:?}, got {body}")
        })
}

/// Every `ResourceStatusReason` the stack has recorded for `logical_id` reaching `reported`, in
/// the order `DescribeStackEvents` reports them.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so
/// a reason is never read off another resource's — or another transition's — event.
async fn recorded_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Vec<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
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
        .collect()
}

/// The reasons the stack recorded for `logical_id` reaching `reported`, looked for again until
/// at least `wanted` of them are there or [`SETTLE_DEADLINE`] passes.
async fn failure_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
    wanted: usize,
) -> Vec<String> {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let reasons = recorded_reasons(router, case, stack, logical_id, reported).await;
        if reasons.len() >= wanted {
            return reasons;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported {} of the {wanted} {reported} reasons within \
             {SETTLE_DEADLINE:?}: {reasons:?}",
            reasons.len()
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

/// Hold one quoted exception to carrying nothing of the owning service's OWN report: neither its
/// coded sentence nor the per-request trace clause it closes with, both of which the plugin's
/// invalid-request exception discards.
fn carries_none_of_the_services_own_report(case: &str, quoted: &str) {
    for provider_internal in ["[RSLVR-", "Trace Id"] {
        assert!(
            !quoted.contains(provider_internal),
            "case {case:?}: expected the handler's own wording, with none of the service's own \
             report ({provider_internal:?}) in it, got {quoted:?}"
        );
    }
}

/// Poll until Route 53 Resolver reports the DNS Firewall resource `request` names in the status
/// a settled one carries, so a case that goes on to use it is never refused for the resource
/// still coming up.
async fn settled_firewall_resource(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    member: &str,
    request: &Value,
) {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let answer = served(router, case, operation, request).await;
        let reported = answer
            .get(member)
            .and_then(|resource| resource.get("Status"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if reported == COMPLETE {
            return;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {member} was still {reported:?} after {SETTLE_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A template declaring a domain list, a rule group and the association of that group with the
/// VPC, under the priority and mutation protection stated.
fn association_template(name: &str, priority: i64, mutation_protection: &str) -> String {
    json!({
        "Resources": {
            "DomainList": {
                "Type": "AWS::Route53Resolver::FirewallDomainList",
                "Properties": {
                    "Name": format!("{name}-domains"),
                    "Domains": ["blocked.example.test."]
                }
            },
            "RuleGroup": {
                "Type": "AWS::Route53Resolver::FirewallRuleGroup",
                "Properties": {
                    "Name": format!("{name}-group"),
                    "FirewallRules": [{
                        "FirewallDomainListId": { "Ref": "DomainList" },
                        "Priority": 100,
                        "Action": "ALERT"
                    }]
                }
            },
            "Association": {
                "Type": "AWS::Route53Resolver::FirewallRuleGroupAssociation",
                "Properties": {
                    "Name": name,
                    "FirewallRuleGroupId": { "Ref": "RuleGroup" },
                    "VpcId": VPC_ID,
                    "Priority": priority,
                    "MutationProtection": mutation_protection
                }
            }
        }
    })
    .to_string()
}

/// A delete Route 53 Resolver refuses because the association is under mutation protection: the
/// reason quotes the DISASSOCIATE the handler issued, with the id it addressed the association
/// by as its one member — and it is the same string on every attempt at the delete, where the
/// service's own report closes with a trace id that is not.
#[tokio::test]
async fn a_refused_disassociate_reports_the_request_the_handler_issued() {
    let case = "an association under mutation protection, deleted twice";
    let stack = "firewall-handler-protected-delete";
    let router = build_router();

    create_stack(
        &router,
        case,
        stack,
        &association_template("protected-association", 101, "ENABLED"),
    )
    .await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the stack's DNS Firewall resources to be created, got {settled:?}"
    );
    let association_id = physical_id(&router, case, stack, "Association").await;

    // Two attempts at the delete: the message states the call and the members it carried, so it
    // cannot differ between two attempts at the same delete.
    for attempt in 1..=2 {
        delete_stack(&router, case, stack).await;
        let settled = settled_status(&router, case, stack).await;
        assert_eq!(
            settled, "DELETE_FAILED",
            "case {case:?}: expected attempt {attempt} at the delete to leave the stack \
             DELETE_FAILED, got {settled:?}"
        );
    }
    let reasons = failure_reasons(&router, case, stack, "Association", "DELETE_FAILED", 2).await;

    let expected = format!(
        "{INVALID_REQUEST_PROVIDED}DisassociateFirewallRuleGroupRequest(\
         FirewallRuleGroupAssociationId={association_id})"
    );
    for (attempt, reason) in reasons.iter().enumerate() {
        let (quoted, class) = quoted_and_class(case, reason);
        assert_eq!(
            quoted, expected,
            "case {case:?}: expected attempt {attempt} to report {expected:?}, got {quoted:?} in \
             {reason:?}"
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the class a refusal of the request falls in, got {class:?} \
             in {reason:?}"
        );
        carries_none_of_the_services_own_report(case, &quoted);
    }

    // The refused delete left the association where it was, under the protection that refused
    // it, so what the reason is about is still there to read.
    let answer = served(
        &router,
        case,
        "GetFirewallRuleGroupAssociation",
        &json!({ "FirewallRuleGroupAssociationId": association_id }),
    )
    .await;
    let association = answer
        .get("FirewallRuleGroupAssociation")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        association.get("MutationProtection").and_then(Value::as_str),
        Some("ENABLED"),
        "case {case:?}: expected the association to survive the refused delete under its \
         protection, got {association}"
    );
    assert_eq!(
        association.get("Priority").and_then(Value::as_i64),
        Some(101),
        "case {case:?}: expected the refused delete to change nothing, got {association}"
    );
}

/// The same rule on the CREATE of an association, where the refused request carries every member
/// an associate takes rather than one: the reason states that call and the values the handler
/// made it with, and none of the service's own report.
#[tokio::test]
async fn a_refused_associate_reports_the_members_of_the_request_it_issued() {
    let case = "an association at a priority another association of the VPC holds";
    let stack = "firewall-handler-priority-clash";
    let association_name = "clashing-association";
    let priority: i64 = 142;
    let router = build_router();

    // A rule group and an association of the account's own, which is what holds the priority the
    // stack's declaration then asks for on the same VPC.
    let held = served(
        &router,
        case,
        "CreateFirewallRuleGroup",
        &json!({ "CreatorRequestId": "held-group", "Name": "held-group" }),
    )
    .await;
    let held_group_id = held
        .get("FirewallRuleGroup")
        .and_then(|group| group.get("Id"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted rule group id, got {held}"))
        .to_string();
    settled_firewall_resource(
        &router,
        case,
        "GetFirewallRuleGroup",
        "FirewallRuleGroup",
        &json!({ "FirewallRuleGroupId": held_group_id }),
    )
    .await;
    served(
        &router,
        case,
        "AssociateFirewallRuleGroup",
        &json!({
            "CreatorRequestId": "held-association",
            "FirewallRuleGroupId": held_group_id,
            "VpcId": VPC_ID,
            "Priority": priority,
            "Name": "held-association",
        }),
    )
    .await;

    // The stack declares its own association of that group with the SAME VPC at that priority,
    // which that service refuses.
    let template = json!({
        "Resources": {
            "Association": {
                "Type": "AWS::Route53Resolver::FirewallRuleGroupAssociation",
                "Properties": {
                    "Name": association_name,
                    "FirewallRuleGroupId": held_group_id,
                    "VpcId": VPC_ID,
                    "Priority": priority,
                    "MutationProtection": "DISABLED"
                }
            }
        }
    })
    .to_string();
    create_stack(&router, case, stack, &template).await;
    let settled = settled_status(&router, case, stack).await;
    assert!(
        settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
        "case {case:?}: expected the deployment to fail on the refused association, got \
         {settled:?}"
    );

    let reasons = failure_reasons(&router, case, stack, "Association", "CREATE_FAILED", 1).await;
    let reason = &reasons[0];
    let (quoted, class) = quoted_and_class(case, reason);
    let opens = format!("{INVALID_REQUEST_PROVIDED}AssociateFirewallRuleGroupRequest(");
    assert!(
        quoted.starts_with(&opens) && quoted.ends_with(')'),
        "case {case:?}: expected the rendering of the associate to open with {opens:?} and close, \
         got {quoted:?}"
    );
    // Every member of the call the handler made, as the request carried it: the one
    // CloudFormation decides for the create, the group and the VPC the declaration named, and
    // the priority, name and protection it stated.
    for member in [
        "CreatorRequestId=".to_string(),
        format!("FirewallRuleGroupId={held_group_id}"),
        format!("VpcId={VPC_ID}"),
        format!("Priority={priority}"),
        format!("Name={association_name}"),
        "MutationProtection=DISABLED".to_string(),
    ] {
        assert!(
            quoted.contains(&member),
            "case {case:?}: expected the rendering to state {member:?}, got {quoted:?}"
        );
    }
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the class a refusal of the request falls in, got {class:?} in \
         {reason:?}"
    );
    carries_none_of_the_services_own_report(case, &quoted);
}

/// The other half of the rule: a refusal that is NOT of the request — one about a rule group
/// Route 53 Resolver does not hold — reaches the event as that service worded it, under the
/// class its own error code falls in.
#[tokio::test]
async fn a_refusal_of_another_class_keeps_the_services_own_message() {
    let case = "an association naming a rule group that service does not hold";
    let stack = "firewall-handler-group-not-held";
    // An id of the form that service mints a rule group under, for a rule group nothing
    // created.
    let absent_group_id = "rslvr-frg-00000000000000000";
    let router = build_router();

    let template = json!({
        "Resources": {
            "Association": {
                "Type": "AWS::Route53Resolver::FirewallRuleGroupAssociation",
                "Properties": {
                    "Name": "association-of-an-absent-group",
                    "FirewallRuleGroupId": absent_group_id,
                    "VpcId": VPC_ID,
                    "Priority": 131,
                    "MutationProtection": "DISABLED"
                }
            }
        }
    })
    .to_string();
    create_stack(&router, case, stack, &template).await;
    let settled = settled_status(&router, case, stack).await;
    assert!(
        settled.contains("ROLLBACK") || settled.ends_with("_FAILED"),
        "case {case:?}: expected the deployment to fail on the refused association, got \
         {settled:?}"
    );

    let reasons = failure_reasons(&router, case, stack, "Association", "CREATE_FAILED", 1).await;
    let reason = &reasons[0];
    let (quoted, class) = quoted_and_class(case, reason);
    assert!(
        !quoted.starts_with(INVALID_REQUEST_PROVIDED),
        "case {case:?}: expected a refusal of another class to be reported as the owning service \
         worded it, got {quoted:?}"
    );
    assert!(
        quoted.contains(absent_group_id),
        "case {case:?}: expected the service's own message, which names the rule group it does \
         not hold, got {quoted:?}"
    );
    assert_eq!(
        class, NOT_FOUND,
        "case {case:?}: expected the class a refusal about a resource that service does not hold \
         falls in, got {class:?} in {reason:?}"
    );
}
