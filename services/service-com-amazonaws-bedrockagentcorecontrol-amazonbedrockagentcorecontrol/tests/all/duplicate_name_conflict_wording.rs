// SPDX-License-Identifier: CC0-1.0

//! How this control plane refuses a create whose `name` the account already holds, end to
//! end over HTTP, across every resource family that refuses one in its handler.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint walk and the handlers are all exercised against the one shared engine.
//!
//! THE RULE UNDER TEST: a create whose name the account already holds is refused with the
//! operation's modelled conflict error carrying this control plane's one duplicate-name
//! sentence — the family's subject, the name the REQUEST carried in single quotes, and the
//! account the name is held in ("<subject> with name '<name>' already exists in this
//! account") — and the refusal says nothing about the resource that already holds the name:
//! an identifier this service generated for it is a value the caller never sent and was
//! refused the right to learn, and quoting it would make the refusal differ between two runs
//! that asked the very same thing.
//!
//! The rows differ along what the rule generalises over: the family refused (its route, the
//! member its name arrives in, and what else its create requires), and the name the
//! collision is on — its length, from the single character the patterns admit to the
//! forty-character one, its case, and the digits it carries. The families and their routes
//! come from `smithy_query(query="operation", operation="CreateGateway,CreateAgentRuntime,\
//! CreateMemory,CreateHarness,CreateDataset,CreateRegistry,CreateConfigurationBundle")`.
//!
//! The families that already shared this sentence before — the custom browser, the browser
//! profile, the custom code interpreter and the consent portal — are covered by
//! `tests/all/tool_resource_statuses_and_name_conflicts.rs` and their own lifecycle suites.
//! The capacity provider refuses no duplicate name at all — it is identified by the id this
//! service mints, not by its name — so it is not a family here
//! (`tests/all/capacity_provider_lifecycle.rs` pins that rule).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::state;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentCoreRole";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The registry plane is entitled: an account holds the AgentCore Registry entitlement,
    // granted out of band, or every registry call is refused before a handler runs
    // (`tests/all/registry_entitlement_gate.rs` pins that rule). The registry row here is
    // about the words its create is refused with once the account may use the plane.
    assert!(
        state::grant_registry_entitlement(&*engine),
        "setup: the shared engine must carry this service's store"
    );
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = Bytes::from(serde_json::to_vec(body).expect("the request body serializes"));
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com")
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The error code `restJson1` puts in the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The string at `pointer`, or the empty string when the body carries none.
fn at(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One resource family under test: how its create is addressed, which member its name
/// arrives in, what else the create requires, which output member carries the identifier
/// this service minted for it, and the subject this control plane words its duplicate-name
/// refusal with.
struct Family {
    /// What the assertions call this family.
    case: &'static str,
    /// The create's method and route.
    method: &'static str,
    route: &'static str,
    /// The member the name arrives in.
    name_member: &'static str,
    /// The members the create carries beyond the name.
    extra: fn() -> Value,
    /// The HTTP status the create answers with (`@http` on the operation).
    create_status: u16,
    /// A JSON pointer to an identifier the create minted, which the refusal must not carry.
    minted: &'static str,
    /// The error code the refusal is named by.
    code: &'static str,
    /// The HTTP status the refusal answers with.
    refusal_status: u16,
    /// How the duplicate-name sentence names the family.
    subject: &'static str,
}

/// An inline dataset source: one example carrying the fields an AgentCore evaluation schema
/// requires of one (`smithy_query(query="shape", shape="DataSourceType")`).
fn inline_source() -> Value {
    json!({"inlineExamples": {"examples": [{
        "scenario_id": "scenario-0",
        "input": "turn 0",
    }]}})
}

/// Every family whose create refuses a duplicate name, each with the smallest body its
/// create accepts.
fn families() -> Vec<Family> {
    vec![
        Family {
            case: "gateway",
            method: "POST",
            route: "/gateways/",
            name_member: "name",
            extra: || json!({"roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"}),
            create_status: 202,
            minted: "/gatewayId",
            code: "ConflictException",
            refusal_status: 409,
            subject: "A gateway",
        },
        Family {
            case: "agent runtime",
            method: "PUT",
            route: "/runtimes/",
            name_member: "agentRuntimeName",
            extra: || {
                json!({
                    "agentRuntimeArtifact": {"containerConfiguration": {
                        "containerUri": "public.ecr.aws/agentcore/demo:latest"
                    }},
                    "roleArn": ROLE_ARN,
                })
            },
            create_status: 202,
            minted: "/agentRuntimeId",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Agent runtime",
        },
        Family {
            case: "memory",
            method: "POST",
            route: "/memories/create",
            name_member: "name",
            extra: || json!({"eventExpiryDuration": 30}),
            create_status: 202,
            minted: "/memory/id",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Memory",
        },
        Family {
            case: "harness",
            method: "POST",
            route: "/harnesses",
            name_member: "harnessName",
            extra: || json!({"executionRoleArn": ROLE_ARN}),
            create_status: 201,
            minted: "/harness/harnessId",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Harness",
        },
        Family {
            case: "dataset",
            method: "POST",
            route: "/datasets",
            name_member: "datasetName",
            extra: || {
                json!({
                    "schemaType": "AGENTCORE_EVALUATION_PREDEFINED_V1",
                    "source": inline_source(),
                })
            },
            create_status: 202,
            minted: "/datasetId",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Dataset",
        },
        Family {
            case: "registry",
            method: "POST",
            route: "/registries",
            name_member: "name",
            extra: || json!({}),
            create_status: 202,
            // `CreateRegistryResponse` reports only the ARN, which carries the minted id.
            minted: "/registryArn",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Registry",
        },
        Family {
            case: "configuration bundle",
            method: "POST",
            route: "/configuration-bundles/create",
            name_member: "bundleName",
            extra: || json!({"components": {"agent": {"configuration": {"model": "a-model"}}}}),
            create_status: 201,
            minted: "/bundleId",
            code: "ConflictException",
            refusal_status: 409,
            subject: "Configuration bundle",
        },
        Family {
            case: "workload identity",
            method: "POST",
            route: "/identities/CreateWorkloadIdentity",
            name_member: "name",
            extra: || json!({}),
            create_status: 201,
            // The identity is addressed by its own name, so nothing was minted for it.
            minted: "",
            // `CreateWorkloadIdentity` models no conflict error, so the one sentence is
            // carried by its modelled `ValidationException` instead.
            code: "ValidationException",
            refusal_status: 400,
            subject: "Workload identity",
        },
    ]
}

/// The body one family's create carries for `name`.
fn create_body(family: &Family, name: &str) -> Value {
    let mut body = json!({});
    body[family.name_member] = json!(name);
    if let Value::Object(extra) = (family.extra)() {
        for (member, value) in extra {
            body[member] = value;
        }
    }
    body
}

/// The names the collisions are on. Every one satisfies every family's own constraints at
/// once: a leading letter, then letters and digits with no underscore (which
/// `GatewayName`'s `^([0-9a-zA-Z][-]?){1,48}$` does not take), at least the three
/// characters `WorkloadIdentityNameType`'s `@length` requires and at most the forty
/// `HarnessName`'s `^[a-zA-Z][a-zA-Z0-9_]{0,39}$` admits — so one table spans the families.
fn names() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest name every family admits", "abc".to_string()),
        ("a mixed-case name with digits", "Report42".to_string()),
        ("a lower-case word", "sharedname".to_string()),
        (
            "the longest name every family admits (40 characters)",
            format!("n{}", "x".repeat(39)),
        ),
    ]
}

/// A create whose name the account already holds is refused with this control plane's
/// duplicate-name sentence around the requested name.
#[tokio::test]
async fn a_duplicate_name_is_refused_with_the_control_planes_one_sentence() {
    for family in families() {
        for (name_case, name) in names() {
            // One router per row, so the name is free in the account each row starts from.
            let router = router();
            let body = create_body(&family, &name);
            let case = family.case;

            let created = send(&router, family.method, family.route, &body).await;
            assert_eq!(
                created.status,
                family.create_status,
                "case {case:?} / {name_case:?}: the first create of a name succeeds, got {} ({})",
                created.status,
                created.body
            );
            let minted = if family.minted.is_empty() {
                String::new()
            } else {
                let minted = at(&created.body, family.minted);
                assert!(
                    !minted.is_empty(),
                    "case {case:?} / {name_case:?}: the create reports an identifier at {}, got \
                     {}",
                    family.minted,
                    created.body
                );
                minted
            };

            let refused = send(&router, family.method, family.route, &body).await;
            assert_eq!(
                (refused.status, error_code(&refused.body).as_str()),
                (family.refusal_status, family.code),
                "case {case:?} / {name_case:?}: a name the account already holds is refused HTTP \
                 {} {}, got {} {} ({})",
                family.refusal_status,
                family.code,
                refused.status,
                error_code(&refused.body),
                refused.body
            );
            let message = text(&refused.body, "message");
            assert_eq!(
                message,
                format!(
                    "{} with name '{name}' already exists in this account",
                    family.subject
                ),
                "case {case:?} / {name_case:?}: expected this control plane's duplicate-name \
                 sentence around the requested name, got {message}"
            );
            assert!(
                minted.is_empty() || !message.contains(&minted),
                "case {case:?} / {name_case:?}: the refusal discloses {minted}, the identifier \
                 minted for the resource already holding the name, in {message}"
            );
        }
    }
}

/// A name that differs from a held one only in case, or only in a trailing character, is its
/// own resource: the refusal is keyed to the name the request carried and not to a normalised
/// form of it, so the sentence names the second name and nothing else.
#[tokio::test]
async fn a_name_differing_from_a_held_one_is_its_own_resource() {
    // (case, the name created first, the name asked for second)
    let pairs: [(&str, &str, &str); 3] = [
        ("differing in the leading character's case", "oneName", "OneName"),
        ("differing in a trailing digit", "twoName2", "twoName3"),
        ("differing by a trailing letter", "threeName", "threeNamez"),
    ];

    for family in families() {
        for (case, first, second) in pairs {
            let router = router();
            let created = send(
                &router,
                family.method,
                family.route,
                &create_body(&family, first),
            )
            .await;
            assert_eq!(
                created.status,
                family.create_status,
                "case {:?} / {case:?}: the first create succeeds, got {} ({})",
                family.case,
                created.status,
                created.body
            );

            let second_answer = send(
                &router,
                family.method,
                family.route,
                &create_body(&family, second),
            )
            .await;
            assert_eq!(
                second_answer.status,
                family.create_status,
                "case {:?} / {case:?}: a name no resource holds is created, got {} ({})",
                family.case,
                second_answer.status,
                second_answer.body
            );

            // And the SECOND name is now the one a repeat of it collides with.
            let refused = send(
                &router,
                family.method,
                family.route,
                &create_body(&family, second),
            )
            .await;
            let message = text(&refused.body, "message");
            assert_eq!(
                message,
                format!(
                    "{} with name '{second}' already exists in this account",
                    family.subject
                ),
                "case {:?} / {case:?}: the refusal names the name the request carried, got \
                 {message}",
                family.case
            );
        }
    }
}
