// SPDX-License-Identifier: CC0-1.0

//! What a client that deploys an `AWS::ApiGateway::VpcLink` over a target Elastic Load Balancing
//! does not hold is left holding, end to end over the wire.
//!
//! RULE: **a VPC link carries traffic through the network load balancers its `targetArns` name, so
//! a link whose targets the owning service cannot resolve settles `FAILED` rather than
//! `AVAILABLE`; the resource provider that declared it therefore never stabilises it, which fails
//! that resource in the one envelope CloudFormation quotes a provider's answer in, rolls the whole
//! stack back, and leaves API Gateway holding no link at all — so an identifier a stack that did
//! not complete published resolves to nothing.** `VpcLink$status` is the asynchronous
//! `AVAILABLE | PENDING | DELETING | FAILED` the create answers `PENDING` out of, and `targetArns`
//! is "The ARN of the network load balancer of the VPC targeted by the VPC link. The network load
//! balancer must be owned by the same Amazon Web Services account of the API owner"
//! (`smithy_query(query="operation",operation="CreateVpcLink")` on the apigateway model), so which
//! status a link settles in is a question about the RESOURCE a target names and is answered by the
//! service that owns load balancers.
//!
//! Only this test crate can assert it: CloudFormation judges and deploys the declaration, API
//! Gateway mints and provisions the link, and Elastic Load Balancing owns the load balancers the
//! link is resolved against — three separate service crates, which exist together only where one
//! engine carries all of them, as `core/router/src/lib.rs` mounts them. Every load balancer a row
//! resolves against is created through Elastic Load Balancing's OWN API over the same router, so a
//! resolvable target is one the account really holds and an unresolvable one is missing only its
//! resource.
//!
//! Why the LAST leg matters: the rolled-back stack's whole promise is that it left nothing behind.
//! A link the rollback did not take away is a record no stack owns any more, counting against the
//! account's quota and listed to every client that enumerates links, while the identifier the
//! failed stack published still resolves — so a client cannot tell a link it must clean up from one
//! that was never created.
//!
//! The rows differ along what the rule generalises over: how many load balancers the declaration
//! targets, WHERE the unresolvable one sits among them, how it comes to be unresolvable (never
//! created, or created and then deleted) and which further properties the declaration states. The
//! POSITIVE CONTROL deploys the same declaration over a load balancer the account holds: that
//! stack completes, the link is available, and it is still there afterwards — so what the rows pin
//! is a failure to stabilise being reported and undone, not an estate nothing can deploy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, members_of, opt_element};

/// The region every request below is signed for, and so the region the stack, the link and the
/// load balancers belong to.
const REGION: &str = "us-east-1";

/// The API version an ELBv2 client names in its awsQuery envelope, which is what the shared
/// `elasticloadbalancing` endpoint serves the ELBv2 model for.
const ELB_VERSION: &str = "2015-12-01";

/// The subnet a network load balancer of these rows is placed in. Elastic Load Balancing takes the
/// placement as given here: what the rows are about is whether a load balancer EXISTS, not where
/// it sits.
const SUBNET: &str = "subnet-0123456789abcdef0";

/// The logical id every template below declares its VPC link under.
const LINK: &str = "Private";

/// The statuses a stack operation settles in, and the suffix one still running carries.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// The resource statuses a declared VPC link is reported in.
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The status a link that finished being provisioned and carries traffic reports, and the
/// description API Gateway settles such a link with. Both fixed text of that service's.
const AVAILABLE: &str = "AVAILABLE";
const READY_TO_ROUTE: &str = "VPC link is ready to route traffic";

/// The `statusMessage` API Gateway settles a link `FAILED` with when a target names a network load
/// balancer the account does not hold — fixed text, carrying no part of the request.
const NLB_ARN_NOT_FOUND: &str = "NLB ARN not found";

/// How a failure of the owning SERVICE is classified: it accepted the call and then could not
/// bring the resource up.
const SERVICE_FAILED: &str = "GeneralServiceException";

/// The error an API Gateway read of a link nobody holds answers, and the sentence it carries
/// (`Invalid Vpc Link identifier specified`) — fixed text of that service's, which does not echo
/// the identifier asked about.
const NOT_FOUND: &str = "NotFoundException";
const NO_SUCH_LINK: &str = "Invalid Vpc Link identifier specified";

/// How long a wait for a stack to reach a terminal status runs before the case fails, and how
/// often it looks again.
///
/// Generous: a stack's create makes further requests of this emulator, each with the span the clock
/// owes it, so the budget is far above anything that flow takes even on a host running the whole
/// workspace at once. Nothing is waited OUT — every wait looks again until the condition holds.
const STACK_BUDGET: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The emulator with the services this suite needs, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation, which deploys the declaration; API
/// Gateway's control plane, which mints and provisions the link; and both Elastic Load Balancing
/// models — the Classic one, which owns the `elasticloadbalancing` signing name the two APIs share,
/// and the v2 one, whose load balancers a link targets.
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
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what routes
/// the request; the signature is never checked, since this router runs with request authentication
/// off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
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

/// Call one Elastic Load Balancing action at the endpoint the two models SHARE, with the ELBv2 API
/// version in the envelope — exactly how an ELBv2 SDK client addresses it — and answer its XML.
async fn elb(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("elasticloadbalancing.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("elasticloadbalancing"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: Elastic Load Balancing {action} must succeed, got {status} with {payload}"
    );
    payload
}

/// A NETWORK load balancer of this account, created through Elastic Load Balancing's own
/// `CreateLoadBalancer`, answering the ARN that service assigned it.
async fn network_load_balancer(router: &MultiServiceRouter, name: &str) -> String {
    let answered = elb(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", name),
            ("Type", "network"),
            ("Scheme", "internal"),
            ("Subnets.member.1", SUBNET),
        ],
    )
    .await;
    element(&answered, "LoadBalancerArn")
}

/// A well-formed ARN of a network load balancer of this account that NOBODY created: the form
/// Elastic Load Balancing writes (`loadbalancer/net/<name>/<assigned id>`), so only the resource it
/// names is missing.
fn unheld_target(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/net/{name}/0123456789abcdef",
        engine::emulator_account_id()
    )
}

/// Call one API Gateway control-plane operation over its restJson1 wire.
async fn apigateway(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// One string member of a JSON body.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {name:?}, got {body}"))
        .to_string()
}

/// Every link `GetVpcLinks` reports, as the list a client enumerating the account reads.
async fn listed_links(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, body) = apigateway(router, "GET", "/vpclinks?limit=500").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetVpcLinks, got {status} with {body}"
    );
    body.get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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

/// Create a stack and read it until it reports a status that is no longer in progress, answering
/// `(StackStatus, the whole answer)`.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> (String, String) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack({stack}), got {status} with {body}"
    );
    let started = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks({stack}), got {status} with \
             {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with(IN_PROGRESS_SUFFIX) {
            return (reported, described);
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: the deployment of {stack} never settled; last status {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One event of a stack, as a client branching on a deployment reads it.
struct Event {
    logical_id: String,
    status: String,
    reason: String,
    physical_id: String,
}

/// One member of an event's own block, with the entities the XML serializer escaped decoded back —
/// so a reason that quotes a provider's answer compares as CloudFormation worded it rather than as
/// its escaping.
fn event_text(block: &str, name: &str) -> String {
    elements(block, name).into_iter().next().unwrap_or_default()
}

/// Every event `DescribeStackEvents` reports for one stack, each member read INSIDE its own block.
async fn events(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<Event> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents({stack}), got {status} with \
         {body}"
    );
    members_of(&body)
        .into_iter()
        .map(|event| Event {
            logical_id: event_text(event, "LogicalResourceId"),
            status: event_text(event, "ResourceStatus"),
            reason: event_text(event, "ResourceStatusReason"),
            physical_id: event_text(event, "PhysicalResourceId"),
        })
        .collect()
}

/// The ONE event in which `logical_id` reached `reported`, which is how many a stack records for a
/// resource that got there once.
async fn only_event(router: &MultiServiceRouter, case: &str, stack: &str, reported: &str) -> Event {
    let mut found: Vec<Event> = events(router, case, stack)
        .await
        .into_iter()
        .filter(|event| event.logical_id == LINK && event.status == reported)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {reported} event for {LINK}, got {:?}",
        found
            .iter()
            .map(|event| (&event.status, &event.reason))
            .collect::<Vec<_>>()
    );
    found.remove(0)
}

/// The envelope CloudFormation quotes a provider's answer in, written out in full, so a case
/// asserts the WHOLE reason rather than the parts it chose to look at.
fn handler_envelope(message: &str, token: &str, class: &str) -> String {
    format!(
        "Resource handler returned message: \"{message}\" (RequestToken: {token}, \
         HandlerErrorCode: {class})"
    )
}

/// How the `AWS::ApiGateway::VpcLink` provider words a link it could not stabilise: the identifier
/// API Gateway minted for the link, and the description that service settled it with.
fn failed_to_stabilise(vpc_link_id: &str, status_message: &str) -> String {
    format!("Failed to stabilize Vpc Link with id {vpc_link_id} Status Message {status_message}.")
}

/// The per-run request token out of one such envelope — the only variable part a case cannot
/// predict.
fn token_of(case: &str, reason: &str) -> String {
    let after = reason
        .split_once("(RequestToken: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a handler envelope, got {reason:?}"))
        .1;
    after
        .split_once(',')
        .unwrap_or_else(|| panic!("case {case:?}: expected a token in {reason:?}"))
        .0
        .to_string()
}

/// Whether `id` is an identifier API Gateway mints for a record of the account: ten lowercase
/// alphanumeric characters.
fn is_api_gateway_id(id: &str) -> bool {
    id.len() == 10 && id.chars().all(|letter| letter.is_ascii_lowercase() || letter.is_ascii_digit())
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

#[tokio::test]
async fn a_stack_whose_vpc_link_cannot_be_stabilised_rolls_back_and_leaves_no_link() {
    let router = build_router();

    // The load balancers the rows resolve against: two this account holds, and one it held and
    // then deleted — each created through Elastic Load Balancing's own API.
    let held = network_load_balancer(&router, "rollback-held-one").await;
    let removed = network_load_balancer(&router, "rollback-removed").await;
    elb(&router, "DeleteLoadBalancer", &[("LoadBalancerArn", &removed)]).await;

    let rows = vec![
        Row {
            // The shape a client hits first: one target, naming a load balancer nobody created.
            case: "one target, which nobody holds",
            stack: "vpclink-rollback-alone",
            link_name: "vpclink-rollback-alone-link",
            targets: vec![unheld_target("rollback-absent")],
            states_more: false,
        },
        Row {
            case: "a target that resolves ahead of one nobody holds, beside a description and tags",
            stack: "vpclink-rollback-behind",
            link_name: "vpclink-rollback-behind-link",
            targets: vec![held.clone(), unheld_target("rollback-absent-last")],
            states_more: true,
        },
        Row {
            case: "one target, a load balancer the account created and then deleted",
            stack: "vpclink-rollback-removed",
            link_name: "vpclink-rollback-removed-link",
            targets: vec![removed.clone()],
            states_more: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let (settled, described) = deployed(
            &router,
            case,
            row.stack,
            &template(row.link_name, &row.targets, row.states_more),
        )
        .await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll back, got {described}"
        );

        // The resource failed, once, with the provider's own account of the link it could not
        // stabilise — inside the one envelope CloudFormation quotes a provider's answer in.
        let failed = only_event(&router, case, row.stack, RESOURCE_CREATE_FAILED).await;
        let vpc_link_id = failed.physical_id.clone();
        assert!(
            is_api_gateway_id(&vpc_link_id),
            "case {case:?}: expected the stack to have recorded the identifier API Gateway minted \
             for the link, got {vpc_link_id:?}"
        );
        let token = token_of(case, &failed.reason);
        assert_eq!(
            failed.reason,
            handler_envelope(
                &failed_to_stabilise(&vpc_link_id, NLB_ARN_NOT_FOUND),
                &token,
                SERVICE_FAILED,
            ),
            "case {case:?}: expected the provider's failure to stabilise the link, got {:?}",
            failed.reason
        );

        // ...and nothing of the link survives the rollback: the account holds no link under the
        // declared name, and the identifier the failed stack published resolves to nothing.
        let listed = listed_links(&router, case).await;
        let names: Vec<&str> = listed
            .iter()
            .filter_map(|link| link.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            !names.contains(&row.link_name),
            "case {case:?}: a rolled-back stack must leave no link, got {names:?}"
        );
        let ids: Vec<&str> = listed
            .iter()
            .filter_map(|link| link.get("id").and_then(Value::as_str))
            .collect();
        assert!(
            !ids.contains(&vpc_link_id.as_str()),
            "case {case:?}: expected {vpc_link_id} to be gone from the listing, got {ids:?}"
        );
        let (status, answered) =
            apigateway(&router, "GET", &format!("/vpclinks/{vpc_link_id}")).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from GetVpcLink({vpc_link_id}), got {status} with \
             {answered}"
        );
        assert_eq!(
            answered.get("__type").and_then(Value::as_str),
            Some(NOT_FOUND),
            "case {case:?}: expected {NOT_FOUND} for a link nobody holds, got {answered}"
        );
        assert_eq!(
            member(case, &answered, "message"),
            NO_SUCH_LINK,
            "case {case:?}: expected the sentence API Gateway states for an identifier that \
             resolves to nothing, got {answered}"
        );
    }
}

/// The control: the same declaration over a load balancer the account really holds comes up, the
/// stack completes, no resource of it fails, and the link is still there afterwards.
#[tokio::test]
async fn a_stack_whose_vpc_link_targets_a_held_balancer_completes_and_keeps_the_link() {
    let router = build_router();
    let case = "one target, a load balancer Elastic Load Balancing holds";
    let stack = "vpclink-rollback-control";
    let link_name = "vpclink-rollback-control-link";
    let held = network_load_balancer(&router, "rollback-control-nlb").await;

    let (settled, described) = deployed(
        &router,
        case,
        stack,
        &template(link_name, std::slice::from_ref(&held), true),
    )
    .await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to complete, got {described}"
    );
    let reported = events(&router, case, stack).await;
    let failures: Vec<&str> = reported
        .iter()
        .filter(|event| event.status.ends_with("_FAILED"))
        .map(|event| event.reason.as_str())
        .collect();
    assert!(
        failures.is_empty(),
        "case {case:?}: expected no resource of the stack to have failed, got {failures:?}"
    );

    // The link the stack published is one API Gateway holds, available over the ARN the
    // declaration named — and the listing reports it under the declared name.
    let created = only_event(&router, case, stack, RESOURCE_CREATE_COMPLETE).await;
    let vpc_link_id = created.physical_id.clone();
    assert!(
        is_api_gateway_id(&vpc_link_id),
        "case {case:?}: expected the identifier API Gateway minted, got {vpc_link_id:?}"
    );
    let (status, link) = apigateway(&router, "GET", &format!("/vpclinks/{vpc_link_id}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetVpcLink({vpc_link_id}), got {status} with {link}"
    );
    assert_eq!(
        (
            member(case, &link, "status"),
            member(case, &link, "statusMessage"),
        ),
        (AVAILABLE.to_string(), READY_TO_ROUTE.to_string()),
        "case {case:?}: expected the link to have come up, got {link}"
    );
    assert_eq!(
        link.get("targetArns"),
        Some(&json!([held])),
        "case {case:?}: expected the link to target the load balancer the declaration named, got \
         {link}"
    );
    let listed = listed_links(&router, case).await;
    assert!(
        listed.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(vpc_link_id.as_str())
                && entry.get("name").and_then(Value::as_str) == Some(link_name)
        }),
        "case {case:?}: expected GetVpcLinks to list {vpc_link_id} under {link_name:?}, got \
         {listed:?}"
    );
}
