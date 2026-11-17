// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::BedrockAgentCore::WorkloadIdentity` has
//! really created that identity in the Amazon Bedrock AgentCore control plane once it
//! reports `CREATE_COMPLETE` — across the whole range of names the owning service's own
//! model accepts, and never for a name it refuses.
//!
//! The rule under test: **a stack creates every resource its template declares by calling
//! the OWNING service's own create API, so the handle the stack publishes for a resource
//! resolves in that service — through its read AND its listing — for as long as the stack
//! stands, and a declaration that service refuses leaves the stack rolled back rather than
//! publishing a handle nothing created.** Two crates have to agree for that to hold
//! (CloudFormation makes the call, the AgentCore control plane serves it and mints the ARN),
//! so the suite lives here rather than in either crate.
//!
//! `cloudformation_provisions_agentcore_workload_identities.rs` spans the TEMPLATE side of
//! the same rule (the spelling, the intrinsic the output reads the resource back with, how
//! many identities one stack declares, the update and the delete). These rows span the one
//! dimension it does not: the NAME, at each published bound of the member that carries it
//! and one step past each, and the character classes its pattern allows — plus the owning
//! service's own LISTING, which is what says the identity is held rather than only
//! answerable.
//!
//! Source of the bounds and the alphabet: `CreateWorkloadIdentity`'s input member `name` in
//! the AgentCore control plane's model — `@required`, `@length(min: 3, max: 255)`,
//! `@pattern ^[A-Za-z0-9_.-]+$` — read with `smithy_query(query="operation",
//! operation="CreateWorkloadIdentity")`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`), so no suite re-derives the encoding its assertions depend on.
use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stacks and their
/// workload identities belong to.
const REGION: &str = "us-east-1";

/// How long a stack is given to reach a terminal status, and how long between two looks.
///
/// What is waited for is the CONDITION — the stack out of its `_IN_PROGRESS` statuses — and
/// the deadline is only a safety net so a wait that can never be satisfied still ends. It is
/// orders of magnitude above the clock-owed span a stack transition takes, so neither a
/// loaded host nor a host running the whole workspace's tests beside this one can decide the
/// outcome.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The signing name the AgentCore control plane shares with the AgentCore data plane
/// (`sigv4Name: bedrock-agentcore`): the route decides which of the two serves a request.
const AGENTCORE: &str = "bedrock-agentcore";

/// The published bounds of `CreateWorkloadIdentity$name` (`@length(min: 3, max: 255)`).
const NAME_MIN: usize = 3;
const NAME_MAX: usize = 255;

/// What the two length rows below start their name with, so each row's name is padded to
/// exactly the length it is testing off this prefix rather than off a literal of its length.
const NAME_PREFIX: &str = "wi-";

/// One name of exactly `length` characters, made of the characters the member's pattern
/// allows: [`NAME_PREFIX`] padded out, so a change to the prefix or to a bound cannot
/// silently put a row off the bound it names. The padding states its own precondition, so a
/// prefix longer than the length asked for fails here rather than producing a name of the
/// wrong length.
fn name_of_length(length: usize) -> String {
    let prefix = NAME_PREFIX.chars().count();
    assert!(
        length >= prefix,
        "case \"a row's name length\": expected a length of at least the prefix's \
         {prefix} characters, got {length}"
    );
    let name = format!("{NAME_PREFIX}{}", "a".repeat(length - prefix));
    assert_eq!(
        name.chars().count(),
        length,
        "case \"a row's name length\": expected {length} characters, got {name:?}"
    );
    name
}

/// The largest page `ListWorkloadIdentities` serves (`maxResults` `@range(min: 1, max: 20)`),
/// asked for so the walk below reads the listing in as few pages as that service allows.
const LIST_PAGE: u64 = 20;

/// Far above the number of identities any row here creates, so the page walk ends on the
/// listing's own last page rather than on this bound.
const LIST_PAGES: usize = 8;

/// The ARN prefix the control plane stamps on a workload identity
/// (`awsService.arnNamespace: bedrock-agentcore`), in the Region this suite signs for.
fn agentcore_arn_prefix() -> String {
    format!("arn:aws:bedrock-agentcore:{REGION}:")
}

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
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

/// Call one AgentCore control-plane operation over its own restJson1 wire: a POST to the URI
/// its `@http` binding names, under the signing name both AgentCore planes share.
async fn agentcore(router: &MultiServiceRouter, uri: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "host",
            format!("bedrock-agentcore-control.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization(AGENTCORE))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetWorkloadIdentity` on one name.
async fn get_identity(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    agentcore(
        router,
        "/identities/GetWorkloadIdentity",
        json!({ "name": name }),
    )
    .await
}

/// Every workload identity the control plane reports for the account as `name -> ARN`,
/// walked page by page the way a client reading the whole listing does.
///
/// The walk ends on the listing's OWN last page — a page carrying no `nextToken` — and
/// [`LIST_PAGES`] only stops a paginator that never terminates, which fails here rather
/// than quietly answering a subset the caller would compare against.
async fn listed_identities(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let mut held = BTreeMap::new();
    let mut token: Option<String> = None;
    let mut whole = false;
    for _ in 0..LIST_PAGES {
        let mut request = json!({ "maxResults": LIST_PAGE });
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let (status, answered) =
            agentcore(router, "/identities/ListWorkloadIdentities", request).await;
        assert_eq!(
            status, 200,
            "case \"the account's listing\": expected HTTP 200 from \
             ListWorkloadIdentities, got {status} with {answered}"
        );
        for entry in answered["workloadIdentities"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let name = entry["name"].as_str().unwrap_or_default().to_string();
            let arn = entry["workloadIdentityArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            held.insert(name, arn);
        }
        match answered["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => {
                whole = true;
                break;
            }
        }
    }
    assert!(
        whole,
        "case \"the account's listing\": expected the page walk to end on a page carrying no \
         nextToken, got {held:?} after {LIST_PAGES} pages"
    );
    held
}

/// The stack's `StackStatus` once it is out of its `_IN_PROGRESS` statuses, with the whole
/// `DescribeStacks` answer it was read from.
///
/// Waits for the CONDITION, looking again every [`POLL_INTERVAL`] until it holds or
/// [`TERMINAL_DEADLINE`] passes, and answers the last status it read either way — so a stack
/// that never settles fails on the status the case asserts rather than on a measured span.
async fn settled_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (_, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return (status, described);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Deploy one stack declaring a single workload identity under `name`, and answer the
/// settled `(StackStatus, DescribeStacks body)`.
///
/// The template publishes both return values the type carries, so what the stack says about
/// the resource can be compared against what the owning service holds.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    name: &str,
) -> (String, String) {
    let template = json!({
        "Resources": {
            "Identity": {
                "Type": "AWS::BedrockAgentCore::WorkloadIdentity",
                "Properties": { "Name": name }
            }
        },
        "Outputs": {
            "IdentityName": {"Value": {"Ref": "Identity"}},
            "IdentityArn": {"Value": {"Fn::GetAtt": ["Identity", "WorkloadIdentityArn"]}}
        }
    })
    .to_string();
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, stack).await
}

/// One name the owning service's model accepts: the case, the stack it deploys in, and the
/// name itself.
struct Accepted {
    case: &'static str,
    stack: &'static str,
    name: String,
}

/// The rows span the member's published bounds and the classes its pattern allows: the
/// shortest name it takes, the longest, and one using every character class
/// (`^[A-Za-z0-9_.-]+$`: upper case, lower case, a digit, `_`, `.` and `-`).
fn accepted() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "the shortest name the member allows",
            stack: "agentcore-wi-shortest-name",
            name: name_of_length(NAME_MIN),
        },
        Accepted {
            case: "the longest name the member allows",
            stack: "agentcore-wi-longest-name",
            name: name_of_length(NAME_MAX),
        },
        Accepted {
            case: "every character class the member's pattern allows",
            stack: "agentcore-wi-pattern-alphabet",
            name: "Wi_09.x-Y".to_string(),
        },
    ]
}

/// One name a read of the control plane refuses: the case, the stack that declares it (where
/// a row declares one), the name itself, and the refusal a read of that name answers with.
struct Refused {
    case: &'static str,
    /// The stack this row deploys to declare the name, or `None` for a row that declares no
    /// stack at all — a name inside the member's constraint that nothing ever created.
    stack: Option<&'static str>,
    name: String,
    /// The status and the modelled client error `GetWorkloadIdentity` answers for this name:
    /// a name outside the member's own `@length`/`@pattern` constraint is refused as a
    /// `ValidationException` before the read can look anything up, and a name inside the
    /// constraint that the account does not hold is its `ResourceNotFoundException`.
    status: u16,
    error: &'static str,
}

/// One step past each published bound, one character outside the published alphabet, and one
/// name inside the constraint that no stack ever declared.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            case: "one character short of the member's minimum",
            stack: Some("agentcore-wi-too-short"),
            name: "a".repeat(NAME_MIN - 1),
            status: 400,
            error: "ValidationException",
        },
        Refused {
            case: "one character past the member's maximum",
            stack: Some("agentcore-wi-too-long"),
            name: name_of_length(NAME_MAX + 1),
            status: 400,
            error: "ValidationException",
        },
        Refused {
            case: "a character the member's pattern excludes",
            stack: Some("agentcore-wi-bad-character"),
            name: "wi/slash".to_string(),
            status: 400,
            error: "ValidationException",
        },
        Refused {
            case: "a name inside the constraint that no stack declared",
            stack: None,
            name: name_of_length(NAME_MIN + 9),
            status: 404,
            error: "ResourceNotFoundException",
        },
    ]
}

#[tokio::test]
async fn a_stack_publishes_a_name_the_control_plane_resolves_across_the_whole_name_range() {
    let router = build_router();
    let rows = accepted();
    let mut created: BTreeMap<String, String> = BTreeMap::new();

    for row in &rows {
        let case = row.case;
        let (status, body) = deploy(&router, case, row.stack, &row.name).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete, got {status:?} with {body}"
        );
        let outputs = outputs_of(&body);
        let published_name = outputs
            .get("IdentityName")
            .unwrap_or_else(|| panic!("case {case:?}: no IdentityName output in {body}"))
            .clone();
        assert_eq!(
            published_name, row.name,
            "case {case:?}: expected Ref to publish the declared name, got \
             {published_name:?}"
        );
        let published_arn = outputs
            .get("IdentityArn")
            .unwrap_or_else(|| panic!("case {case:?}: no IdentityArn output in {body}"))
            .clone();

        // The owning service resolves the name the stack published, and the ARN it minted is
        // the one the stack published for it.
        let (status, reported) = get_identity(&router, &published_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the control plane to resolve the published name \
             {published_name:?}, got {status} with {reported}"
        );
        let held_arn = reported["workloadIdentityArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            published_arn, held_arn,
            "case {case:?}: expected the published ARN to be the one the control plane \
             minted ({held_arn:?}), got {published_arn:?}"
        );
        let prefix = agentcore_arn_prefix();
        assert!(
            held_arn.starts_with(&prefix) && held_arn.ends_with(&published_name),
            "case {case:?}: expected an ARN in the AgentCore namespace naming the identity, \
             got {held_arn:?}"
        );
        assert_eq!(
            reported["name"].as_str(),
            Some(published_name.as_str()),
            "case {case:?}: expected the identity to report the published name, got \
             {reported}"
        );
        created.insert(published_name, held_arn);
    }

    // And the account's own listing holds exactly the identities these stacks created: the
    // read resolving is one thing, the owning service HOLDING the resource is what the rule
    // asks for.
    let held = listed_identities(&router).await;
    assert_eq!(
        held, created,
        "case \"every deployed identity in the account's listing\": expected {created:?}, \
         got {held:?}"
    );
}

#[tokio::test]
async fn a_name_the_control_plane_refuses_leaves_the_stack_rolled_back_and_nothing_created() {
    let router = build_router();

    for row in refused() {
        let case = row.case;
        // A row that declares a stack has to see it rolled back: the control plane refuses
        // the declaration, so there is no resource and no output to publish. The row that
        // declares no stack is the other half of the same rule — a name nothing created.
        if let Some(stack) = row.stack {
            let (status, body) = deploy(&router, case, stack, &row.name).await;
            assert_eq!(
                status, "ROLLBACK_COMPLETE",
                "case {case:?}: expected the failed create to be rolled back rather than \
                 publish a name the control plane never created, got {status:?} with {body}"
            );
            assert!(
                outputs_of(&body).is_empty(),
                "case {case:?}: expected no output for a resource nothing created, got {body}"
            );
        }

        // Nothing was created under the name, so a read of it is refused under the status
        // and the client-error class the row names rather than answering an identity.
        let (status, answered) = get_identity(&router, &row.name).await;
        let reported = answered["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (status, reported.as_str()),
            (row.status, row.error),
            "case {case:?}: expected the control plane to refuse a read of {:?} with a \
             {} {}, got {status} with {answered}",
            row.name,
            row.status,
            row.error
        );
    }

    // And the account holds no workload identity at all: no row's declaration was created.
    let held = listed_identities(&router).await;
    assert!(
        held.is_empty(),
        "case \"a refused declaration creates nothing\": expected the control plane to hold \
         no workload identity, got {held:?}"
    );
}
