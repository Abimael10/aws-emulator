// SPDX-License-Identifier: CC0-1.0

//! What a client that deploys an `AWS::ApiGateway::VpcLink` over a target API Gateway cannot
//! resolve reads out of `DescribeStackEvents`.
//!
//! RULE: the `ResourceStatusReason` of a resource whose RESOURCE PROVIDER could not stabilise it
//! is reported only inside the one envelope CloudFormation quotes a provider's answer in —
//! `Resource handler returned message: "<the provider's message>" (RequestToken: <token>,
//! HandlerErrorCode: <class>)` — whose quoted sentence is the provider's own, with its variable
//! parts (the identifier the owning service minted for the resource, the description that
//! service settled it with) read off this emulator's own state, and whose class, for a service
//! that ACCEPTED the call and then could not bring the resource up, is `GeneralServiceException`.
//! That reason is a RECORD of the failure: a later read of the same event reports the same text,
//! the token included. And only a resource a PROVIDER answered for is quoted that way — the
//! stack's own events, where CloudFormation reports on itself, carry no envelope.
//!
//! Why it matters: `HandlerErrorCode` is the machine-readable half of a failed deployment, the
//! field infrastructure tooling branches on to tell "the owning service failed, retry" from "the
//! template is wrong, fix it", and it exists only inside that envelope. A reason worded outside
//! it carries no class at all, so every such parse fails and a service-side failure to stabilise
//! a resource cannot be told from a bad declaration.
//!
//! The rows differ along what the rule generalises over: how many load balancers the declaration
//! targets, WHERE the target that cannot be resolved sits among them (alone, behind one that
//! resolves, ahead of one that resolves) and which further properties the declaration states (a
//! description and tags beside the members the create cannot be made without) — none of which
//! the envelope, the quoted sentence or the class may depend on. The targets that DO resolve are
//! load balancers created through Elastic Load Balancing's own API over the same router, so a
//! resolvable target is one the account really holds.
//!
//! The POSITIVE CONTROL deploys the same declaration over that load balancer alone: the stack
//! completes, API Gateway reports the link available over the ARN the declaration named, and no
//! event of the stack carries a handler failure at all — so what the rows pin is a provider's
//! failure being reported, not an estate nothing can deploy.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::{
    apigateway, elb_arn, is_api_gateway_id, member_text, network_load_balancer, subnet,
};
use crate::harness::{
    call, create_stack, envelope_parts, failed_resource_reason, is_a_request_id, member, router,
    settled_stack, stack_own_events, xml_blocks, xml_text, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The logical id every template below declares its VPC link under.
const LINK: &str = "Private";

/// How many characters a minted VPC link id carries: API Gateway assigns a record of the account
/// ten lowercase alphanumeric characters.
const VPC_LINK_ID_CHARACTERS: usize = 10;

/// The class a failure of the owning SERVICE falls in — it accepted the call and then could not
/// bring the resource up — which is one of the model's own `HandlerErrorCode` values
/// (`smithy_query(query="shape", shape="HandlerErrorCode")`).
const SERVICE_FAILED: &str = "GeneralServiceException";

/// The `statusMessage` API Gateway settles a link `FAILED` with when a declared target names a
/// network load balancer the account does not hold. Fixed text: it carries no part of the
/// request — not the target, not the link's name and not its id.
const NLB_ARN_NOT_FOUND: &str = "NLB ARN not found";

/// The `status` a link that finished being provisioned and carries traffic reports
/// (`VpcLinkStatus`: `AVAILABLE | PENDING | DELETING | FAILED`), and the description
/// ("`VpcLink$statusMessage`") API Gateway settles such a link with — fixed text of that
/// service's, carrying no part of the request.
const AVAILABLE: &str = "AVAILABLE";
const READY_TO_ROUTE: &str = "VPC link is ready to route traffic";

/// The head of the envelope CloudFormation quotes a provider's answer in, for the assertions that
/// a provider's sentence is quoted ONCE and that CloudFormation's own report quotes nobody.
const ENVELOPE_OPENS: &str = "Resource handler returned message: \"";

/// How the `AWS::ApiGateway::VpcLink` provider words a link it could not stabilise: the
/// identifier API Gateway minted for the link, and the description that service settled it with.
fn failed_to_stabilise(vpc_link_id: &str, status_message: &str) -> String {
    format!("Failed to stabilize Vpc Link with id {vpc_link_id} Status Message {status_message}.")
}

/// One provider answer in that envelope, written out in full — so a case asserts the WHOLE
/// reason rather than the parts it chose to look at.
fn handler_envelope(message: &str, token: &str, class: &str) -> String {
    format!("{ENVELOPE_OPENS}{message}\" (RequestToken: {token}, HandlerErrorCode: {class})")
}

/// The ARN of a NETWORK load balancer of this account, in the form Elastic Load Balancing writes
/// one (`loadbalancer/net/<name>/<assigned id>`) — well formed, and held by nobody.
fn unresolvable_target(name: &str) -> String {
    elb_arn(&format!("loadbalancer/net/{name}/0123456789abcdef"))
}

/// One row: a VPC link declaration API Gateway cannot bring the link up for.
struct Row {
    case: &'static str,
    stack: &'static str,
    link_name: &'static str,
    /// The targets the declaration names, in the order it names them.
    targets: Vec<String>,
    /// Whether the declaration states a description and tags beside the required members.
    states_more: bool,
}

/// The rows, over a target that really resolves.
fn rows(resolvable: &str) -> Vec<Row> {
    let absent = unresolvable_target("stabilisation-reason-absent");
    vec![
        Row {
            // The shape a client hits first: one target, naming a load balancer nobody created.
            case: "one target, which nobody holds",
            stack: "stabilisation-reason-alone",
            link_name: "stabilisation-reason-alone-link",
            targets: vec![absent.clone()],
            states_more: false,
        },
        Row {
            case: "a target that resolves ahead of one nobody holds",
            stack: "stabilisation-reason-behind",
            link_name: "stabilisation-reason-behind-link",
            targets: vec![resolvable.to_string(), absent.clone()],
            states_more: false,
        },
        Row {
            case: "a target nobody holds ahead of one that resolves, beside a description and tags",
            stack: "stabilisation-reason-ahead",
            link_name: "stabilisation-reason-ahead-link",
            targets: vec![absent, resolvable.to_string()],
            states_more: true,
        },
    ]
}

/// The template one row deploys: a single `AWS::ApiGateway::VpcLink` over the targets it states.
fn template(link_name: &str, targets: &[String], states_more: bool) -> String {
    let mut properties: Value = json!({ "Name": link_name, "TargetArns": targets });
    if states_more {
        properties["Description"] = json!("the private path to the orders service");
        properties["Tags"] = json!([{ "Key": "owner", "Value": "platform" }]);
    }
    json!({
        "Resources": {
            LINK: { "Type": "AWS::ApiGateway::VpcLink", "Properties": properties },
        },
    })
    .to_string()
}

/// Create a stack and poll it to the status it settles in, answering that read's body.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (code, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack({stack}) must answer 200, got {code} {body}"
    );
    settled_stack(case, router, REGION, stack).await
}

/// The whole `DescribeStackEvents` body of one stack, as a client reads its deployment back.
async fn described_events(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} {body}"
    );
    body
}

/// The events of one logical resource in one `ResourceStatus`, as blocks — so a case can both
/// COUNT them and read the members of the one it expects.
fn events_of(body: &str, logical_id: &str, reported_status: &str) -> Vec<String> {
    xml_blocks(body, "member")
        .into_iter()
        .filter(|entry| {
            xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id)
                && xml_text(entry, "ResourceStatus").as_deref() == Some(reported_status)
        })
        .collect()
}

/// The one event `logical_id` reached `reported_status` in, which is how many a stack records for
/// a resource that got there once.
fn only_event(case: &str, body: &str, logical_id: &str, reported_status: &str) -> String {
    let found = events_of(body, logical_id, reported_status);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {reported_status} event for {logical_id}, got \
         {found:?}"
    );
    found.into_iter().next().unwrap_or_default()
}

/// A link its provider could not stabilise fails the resource in the envelope CloudFormation
/// quotes a provider's answer in, under the class a service that could not bring an accepted
/// resource up falls in — whatever the declaration named, and unchanged on every later read.
#[tokio::test]
async fn a_link_its_provider_cannot_stabilise_reports_only_the_handler_envelope() {
    let router = router();
    let subnet_id = subnet(&router).await;
    let resolvable =
        network_load_balancer(&router, &subnet_id, "stabilisation-reason-nlb").await;
    for row in rows(&resolvable) {
        let case = row.case;
        let described = deploy(
            &router,
            case,
            row.stack,
            &template(row.link_name, &row.targets, row.states_more),
        )
        .await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the stack to roll back, got {described}"
        );

        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            LINK,
            resource_status::CREATE_FAILED,
        )
        .await;
        let body = described_events(&router, case, row.stack).await;
        let failed = only_event(case, &body, LINK, resource_status::CREATE_FAILED);

        // The link the provider read is the resource the event is about, so the id its sentence
        // names is the one the stack recorded for that resource.
        let vpc_link_id = xml_text(&failed, "PhysicalResourceId").unwrap_or_default();
        assert!(
            is_api_gateway_id(&vpc_link_id, VPC_LINK_ID_CHARACTERS),
            "case {case:?}: expected the stack to have recorded the identifier API Gateway \
             minted, got {vpc_link_id:?}"
        );

        let (quoted, token, class) = envelope_parts(case, &reason);
        assert_eq!(
            class, SERVICE_FAILED,
            "case {case:?}: expected the failure to be classified {SERVICE_FAILED:?}, got \
             {reason:?}"
        );
        assert!(
            is_a_request_id(&token),
            "case {case:?}: expected the token of the resource request, got {token:?} in \
             {reason:?}"
        );
        let stated = failed_to_stabilise(&vpc_link_id, NLB_ARN_NOT_FOUND);
        assert_eq!(
            quoted, stated,
            "case {case:?}: expected the provider's own account of the link it could not \
             stabilise, got {quoted:?}"
        );
        assert!(
            !quoted.contains(ENVELOPE_OPENS),
            "case {case:?}: expected the provider's answer to be quoted ONCE, got {quoted:?}"
        );
        assert_eq!(
            reason,
            handler_envelope(&stated, &token, SERVICE_FAILED),
            "case {case:?}: expected the whole reason to be that envelope and nothing besides, \
             got {reason:?}"
        );

        // The reason is a RECORD of the failure: reading the event again reports the same text,
        // the token included, so a client that parses it twice recovers one request.
        let again = described_events(&router, case, row.stack).await;
        let re_read = only_event(case, &again, LINK, resource_status::CREATE_FAILED);
        assert_eq!(
            xml_text(&re_read, "ResourceStatusReason").unwrap_or_default(),
            reason,
            "case {case:?}: expected a later read to report the recorded reason unchanged, got \
             {re_read}"
        );

        // CloudFormation reporting on ITSELF quotes no provider: the envelope belongs to the
        // resource whose provider answered.
        for event in stack_own_events(&router, case, row.stack).await {
            assert!(
                !event.reason.contains(ENVELOPE_OPENS),
                "case {case:?}: expected the stack's own event to carry CloudFormation's own \
                 words, got {:?}",
                event.reason
            );
        }
    }
}

/// The control: the same declaration over a load balancer the account really holds comes up, and
/// no event of that stack carries a provider failure at all.
#[tokio::test]
async fn a_link_over_a_target_the_account_holds_records_no_handler_failure() {
    let router = router();
    let case = "a link over a load balancer Elastic Load Balancing holds";
    let stack = "stabilisation-reason-control";
    let subnet_id = subnet(&router).await;
    let resolvable =
        network_load_balancer(&router, &subnet_id, "stabilisation-reason-held").await;
    let described = deploy(
        &router,
        case,
        stack,
        &template(
            "stabilisation-reason-control-link",
            std::slice::from_ref(&resolvable),
            true,
        ),
    )
    .await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the stack to complete, got {described}"
    );

    let body = described_events(&router, case, stack).await;
    let created = only_event(case, &body, LINK, resource_status::CREATE_COMPLETE);
    let vpc_link_id = xml_text(&created, "PhysicalResourceId").unwrap_or_default();
    assert!(
        is_api_gateway_id(&vpc_link_id, VPC_LINK_ID_CHARACTERS),
        "case {case:?}: expected the identifier API Gateway minted, got {vpc_link_id:?}"
    );
    assert!(
        !body.contains(ENVELOPE_OPENS) && !body.contains(resource_status::CREATE_FAILED),
        "case {case:?}: expected no resource of the stack to have failed, got {body}"
    );

    // The link the stack published is one API Gateway holds, available over the ARN the
    // declaration named.
    let (code, link) = apigateway(&router, "GET", &format!("/vpclinks/{vpc_link_id}")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetVpcLink({vpc_link_id}) to answer 200, got {code} {link}"
    );
    assert_eq!(
        member_text(&link, "status").as_deref(),
        Some(AVAILABLE),
        "case {case:?}: expected the link to have come up, got {link}"
    );
    assert_eq!(
        link.get("targetArns"),
        Some(&json!([resolvable])),
        "case {case:?}: expected the link to target the load balancer the declaration named, got \
         {link}"
    );
    assert_eq!(
        member_text(&link, "statusMessage").as_deref(),
        Some(READY_TO_ROUTE),
        "case {case:?}: expected the description that service states for a link that came up, got \
         {link}"
    );
}
