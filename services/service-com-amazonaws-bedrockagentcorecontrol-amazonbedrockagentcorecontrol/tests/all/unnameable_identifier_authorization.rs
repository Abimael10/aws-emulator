// SPDX-License-Identifier: CC0-1.0

//! What this control plane answers a request that addresses its resource by an identifier the
//! member that names it cannot admit, driven end to end over the wire path (a `restJson1`
//! request through the multi-service router into the mounted service).
//!
//! THE RULE UNDER TEST: a route addresses its resource by the identifier its path carries, and
//! the authorization decision is taken against that resource before the request's members are
//! validated. An identifier outside the modelled format of the member that NAMES the resource
//! is one no resource of the family can be addressed by, so such a call names no resource of
//! this account — and that is the one request the standing exemption of the credential this
//! emulator publishes does not reach, because what it stands in for is the account's authority
//! over its own resources (`src/service/authorization.rs` states the rule and the evidence).
//! The decision is then IAM's own, for every caller alike:
//!
//! * a principal no readable statement allows the action to falls to IAM's default deny and is
//!   refused HTTP 403 `AccessDeniedException`, in the wording that family states a denial in,
//!   with no report about the member at all — including the credential this emulator
//!   publishes, which is an IAM user ARN holding no document and which every call addressing a
//!   resource of the account reaches the operation with;
//! * a principal whose readable documents DO allow the action reaches the operation's own
//!   verdicts, where that same identifier IS the modelled `ValidationException` the member's
//!   `@pattern` states — so the modelled refusal is reachable, and this suite asserts it per
//!   family in that family's own clause form;
//! * a principal carrying a grant this emulator cannot READ keeps its exemption, since such a
//!   grant may cover the action over every resource, and reaches the operation.
//!
//! WHAT THE RULE DOES NOT COVER, asserted beside it so the refusal stays bounded:
//!
//! * an identifier INSIDE its member's format reaches the operation's own verdicts — the
//!   modelled not-found for one that names nothing, and the resource itself for one this
//!   account holds;
//! * a CHILD identifier outside ITS format, beside a parent whose own identifier is inside its
//!   format, leaves the resource the decision is taken against named — so that request is
//!   validated and answered the modelled `ValidationException` naming the child member;
//! * a route that names no resource (a create, a listing of a family) is validated the same
//!   way.
//!
//! The tables span what the rule generalises over: the resource family (runtimes and capacity
//! providers across their own operations, plus memories, gateways, harnesses, payments and
//! configuration bundles), the method (GET, PUT, POST, DELETE), the way the identifier leaves
//! its member's format (no minted suffix at all — the form a caller derives from a vended
//! log-group name — a leading digit, a suffix one character short and one past it, upper case
//! where the grammar admits none), and the caller (the account's own published credential, an
//! IAM user carrying no policy, an IAM user allowed another namespace, an IAM user allowed
//! this namespace, and a principal carrying a grant this emulator cannot read).
//!
//! The router, the identity stores and the IAM seeding are the ones
//! `runtime_identifier_authorization.rs` already drives this same decision through, reused
//! rather than copied; this suite varies the identifier a route addresses its resource by,
//! where that one varies the documents in force on the principal.
//!
//! Sources: `smithy_query(query="operation", operation="GetAgentRuntime,UpdateAgentRuntime,\
//! DeleteAgentRuntime,ListAgentRuntimeEndpoints,GetAgentRuntimeEndpoint,GetCapacityProvider,\
//! UpdateCapacityProvider,GetMemory,GetGateway,GetHarness,GetPaymentManager,\
//! GetConfigurationBundle,CreateAgentRuntime")` for every route, the member each one
//! addresses its resource by and that member's `@pattern`.

use serde_json::{json, Value};

use crate::runtime_identifier_authorization::{
    allow, not_authorized, Harness, AMBIENT_KEY_ID, REGION, USER_KEY_ID, VALID_ROLE_ARN,
};

/// The error codes `restJson1` carries in `__type` and in `x-amzn-errortype`.
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const VALIDATION_EXCEPTION: &str = "ValidationException";
/// See [`ACCESS_DENIED`].
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// This endpoint's IAM action namespace: its `sigv4Name` and `arnNamespace`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "bedrock-agentcore";

/// The fragment a constraint clause states, which no refusal of the rule under test carries.
const CONSTRAINT_CLAUSE: &str = "failed to satisfy constraint";

/// An `Allow` of every action of this endpoint's namespace, on every resource.
fn allow_this_namespace() -> String {
    allow(&format!(r#""{ACTION_PREFIX}:*""#), r#""*""#)
}

/// An `Allow` of another service's namespace, which grants nothing here.
fn allow_another_namespace() -> String {
    allow(r#""bedrock:*""#, r#""*""#)
}

/// The principal the account's own published credential speaks for, which is what a refusal of
/// its calls names — the same value `sts:GetCallerIdentity` reports for it.
fn ambient_principal() -> String {
    service_com_amazonaws_sts_awssecuritytokenservicev20110615::state::ambient_identity().arn
}

/// The sentence every family but the runtime one states: the principal, the action, the
/// resource the decision was taken against, and which of IAM's rules withheld the grant —
/// here IAM's own default deny, the rule for a principal no readable statement allows.
fn not_authorized_on(principal: &str, operation: &str, resource: &str) -> String {
    let action = format!("{ACTION_PREFIX}:{operation}");
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The ARN of one resource of this endpoint, as `crate::state` mints each family's
/// (`smithy_query(query="operation", operation="GetMemory,GetGateway,GetHarness,\
/// GetPaymentManager,GetConfigurationBundle,GetCapacityProvider")` — every one is
/// `arn:aws:bedrock-agentcore:<region>:<account>:<type>/<identifier>`), which is the resource
/// a refusal of that family names.
fn family_arn(resource_type: &str, identifier: &str) -> String {
    format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:{resource_type}/{identifier}",
        ::engine::emulator_account_id()
    )
}

/// The ARN of one capacity provider of this account ([`family_arn`]).
fn capacity_provider_arn(identifier: &str) -> String {
    family_arn("capacity-provider", identifier)
}

/// The clause a counted-form family states for a `@pattern` the value broke, with the value
/// quoted — the form `/runtimes` states (`validation_message_rendering.rs` holds the rule).
fn counted_pattern_clause(submitted: &str, member: &str, modelled: &str) -> String {
    let unanchored = modelled
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
        .expect("every modelled pattern below is anchored at both ends");
    format!(
        "1 validation error detected: Value '{submitted}' at '{member}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {unanchored}"
    )
}

/// The same clause in the pointer form the `/capacity-providers` family states
/// (`capacity_provider_refusal_wording.rs` holds that rule): no count, a JSON pointer, no
/// value.
fn pointer_pattern_clause(member: &str, modelled: &str) -> String {
    let unanchored = modelled
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
        .expect("every modelled pattern below is anchored at both ends");
    format!(
        "validation error detected. Value at '/{member}' failed to satisfy constraint: Member \
         must satisfy regular expression pattern: {unanchored}"
    )
}

/// One request of the refusal table: the operation it names, how it is sent, the identifier it
/// addresses its resource by, and the resource a refusal of that family names — `None` for the
/// family whose sentence closes at the action.
struct Addressed {
    case: &'static str,
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    carried: &'static str,
    resource: Option<String>,
}

/// Every row of the refusal table: the operations of two families, across their methods and
/// the ways an identifier leaves its member's format.
fn unnameable_rows() -> Vec<Addressed> {
    vec![
        Addressed {
            // The form a caller derives from a runtime's vended log-group name: a stem with
            // no minted `-<10 characters>` suffix at all.
            case: "GetAgentRuntime, an agentRuntimeId carrying no minted suffix",
            operation: "GetAgentRuntime",
            method: "GET",
            uri: "/runtimes/probe_runtime_from_log_group/".to_string(),
            body: None,
            carried: "probe_runtime_from_log_group",
            resource: None,
        },
        Addressed {
            case: "DeleteAgentRuntime, an agentRuntimeId with a leading digit",
            operation: "DeleteAgentRuntime",
            method: "DELETE",
            uri: "/runtimes/9probeRuntime-abcdefghij/".to_string(),
            body: None,
            carried: "9probeRuntime-abcdefghij",
            resource: None,
        },
        Addressed {
            // A body beside the label: the decision still precedes the body's own walk.
            case: "UpdateAgentRuntime, an agentRuntimeId whose suffix is one character short",
            operation: "UpdateAgentRuntime",
            method: "PUT",
            uri: "/runtimes/probeRuntime-abcdefghi/".to_string(),
            body: Some(json!({
                "agentRuntimeArtifact": {"containerConfiguration": {
                    "containerUri": "public.ecr.aws/agentcore/probe:v2"
                }},
                "networkConfiguration": {"networkMode": "PUBLIC"},
                "roleArn": VALID_ROLE_ARN,
            })),
            carried: "probeRuntime-abcdefghi",
            resource: None,
        },
        Addressed {
            // A child route of the same family: the identifier the DECISION is taken against
            // is the parent's, and it is the parent's that no runtime can carry here.
            case: "ListAgentRuntimeEndpoints, an agentRuntimeId with a leading digit",
            operation: "ListAgentRuntimeEndpoints",
            method: "POST",
            uri: "/runtimes/0notAnId-abcdefghij/runtime-endpoints".to_string(),
            body: None,
            carried: "0notAnId-abcdefghij",
            resource: None,
        },
        Addressed {
            case: "GetCapacityProvider, a capacityProviderId with a leading digit",
            operation: "GetCapacityProvider",
            method: "GET",
            uri: "/capacity-providers/9notaprovider-abcdefghij".to_string(),
            body: None,
            carried: "9notaprovider-abcdefghij",
            resource: Some(capacity_provider_arn("9notaprovider-abcdefghij")),
        },
        Addressed {
            case: "UpdateCapacityProvider, a capacityProviderId whose suffix is one past",
            operation: "UpdateCapacityProvider",
            method: "PUT",
            uri: "/capacity-providers/probeProvider-abcdefghijk".to_string(),
            body: Some(json!({})),
            carried: "probeProvider-abcdefghijk",
            resource: Some(capacity_provider_arn("probeProvider-abcdefghijk")),
        },
    ]
}

/// RULE: a caller no readable statement allows the action to — the credential this emulator
/// publishes included — is refused 403 `AccessDeniedException` in that family's own wording,
/// with no constraint clause and no echo of the identifier it sent.
#[tokio::test]
async fn a_caller_with_no_readable_grant_is_refused_before_the_members_are_validated() {
    let harness = Harness::new();
    let user = harness.given_user();
    let ambient = ambient_principal();

    // (case, the key the request signs with, the document in force, the principal a refusal
    // names) — every caller the gate cannot read a grant for, including the one it otherwise
    // lets through unjudged.
    let callers: [(&str, &str, Option<String>, &str); 3] = [
        (
            "the account's own published credential",
            AMBIENT_KEY_ID,
            None,
            ambient.as_str(),
        ),
        (
            "a principal carrying no policy at all",
            USER_KEY_ID,
            None,
            user.as_str(),
        ),
        (
            "a principal allowed another namespace",
            USER_KEY_ID,
            Some(allow_another_namespace()),
            user.as_str(),
        ),
    ];

    for (caller_case, key, document, principal) in callers {
        if key == USER_KEY_ID {
            harness.given_policy(document.as_deref());
        }
        for row in unnameable_rows() {
            let answer = harness
                .send_as(key, REGION, row.method, &row.uri, row.body.as_ref())
                .await;
            let case = format!("{caller_case}: {}", row.case);
            assert_eq!(
                (answer.status, answer.code.as_str()),
                (403, ACCESS_DENIED),
                "case {case:?}: expected 403 {ACCESS_DENIED}, got {} {:?} with {:?}",
                answer.status,
                answer.code,
                answer.message
            );
            let expected = match &row.resource {
                None => not_authorized(principal, row.operation),
                Some(resource) => not_authorized_on(principal, row.operation, resource),
            };
            assert_eq!(
                answer.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                answer.message
            );
            assert!(
                !answer.message.contains(CONSTRAINT_CLAUSE),
                "case {case:?}: the refusal states no constraint clause, got {:?}",
                answer.message
            );
        }
    }
}

/// RULE's other half: a caller whose readable documents DO allow the action reaches the
/// operation, where the same identifier is the modelled `ValidationException` the member's
/// `@pattern` states — in each family's own clause form.
#[tokio::test]
async fn a_caller_the_policies_allow_reaches_the_modelled_constraint_refusal() {
    let harness = Harness::new();
    harness.given_user();
    harness.given_policy(Some(&allow_this_namespace()));

    // (case, the row, the clause the family states for its addressing member)
    let rows: Vec<(&str, Addressed, String)> = unnameable_rows()
        .into_iter()
        .map(|row| {
            let clause = match row.operation {
                "GetCapacityProvider" | "UpdateCapacityProvider" => pointer_pattern_clause(
                    "capacityProviderId",
                    "^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$",
                ),
                _ => counted_pattern_clause(
                    row.carried,
                    "agentRuntimeId",
                    "^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$",
                ),
            };
            (row.case, row, clause)
        })
        .collect();

    for (case, row, clause) in rows {
        let answer = harness
            .send_as(USER_KEY_ID, REGION, row.method, &row.uri, row.body.as_ref())
            .await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {:?} with {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        assert_eq!(
            answer.message, clause,
            "case {case:?}: expected {clause:?}, got {:?}",
            answer.message
        );
    }
}

/// RULE's other half, for the caller whose grant this emulator cannot READ: an AWS-managed
/// policy may allow the action over every resource, so it keeps the exemption that refusing on
/// it would otherwise deny access AWS permits — and its calls reach the operation, where the
/// identifier is the modelled constraint refusal.
#[tokio::test]
async fn a_caller_carrying_a_grant_this_emulator_cannot_read_reaches_the_operation() {
    let harness = Harness::new();
    harness.given_user();
    // No document of its own, and one attachment of AWS's catalog, which this emulator holds
    // no document for.
    harness.given_policy(None);
    harness.given_attached_policy("arn:aws:iam::aws:policy/AmazonBedrockAgentCoreFullAccess");

    for row in unnameable_rows() {
        let answer = harness
            .send_as(USER_KEY_ID, REGION, row.method, &row.uri, row.body.as_ref())
            .await;
        let case = row.case;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {:?} with {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        assert!(
            answer.message.contains(CONSTRAINT_CLAUSE),
            "case {case:?}: expected the operation's own constraint clause, got {:?}",
            answer.message
        );
    }
}

/// THE RULE'S BOUNDARY, over the families: the decision is the service's, so a family whose
/// backend words a denial differently states ITS sentence for the same request, and every
/// family's addressing member is still the modelled constraint refusal for a caller the
/// policies allow.
#[tokio::test]
async fn every_family_is_decided_alike_and_worded_its_own_way() {
    let harness = Harness::new();
    let user = harness.given_user();

    // (case, method, uri, the operation, the member the clause names, the resource a refusal
    // of that family names) — one family per row, across the five this endpoint fronts beside
    // the two above.
    let rows: [(&str, &str, &str, &str, &str, String); 5] = [
        (
            "GetMemory, a memoryId with a leading digit",
            "GET",
            "/memories/1notamemory-abcdefghij/details",
            "GetMemory",
            "memoryId",
            family_arn("memory", "1notamemory-abcdefghij"),
        ),
        (
            "GetGateway, a gatewayIdentifier carrying upper case",
            "GET",
            "/gateways/BADGATEWAY-abcdefghij/",
            "GetGateway",
            "gatewayIdentifier",
            family_arn("gateway", "BADGATEWAY-abcdefghij"),
        ),
        (
            "GetHarness, a harnessId carrying no minted suffix",
            "GET",
            "/harnesses/1notaharness",
            "GetHarness",
            "harnessId",
            family_arn("harness", "1notaharness"),
        ),
        (
            "GetPaymentManager, a paymentManagerId carrying upper case",
            "GET",
            "/payments/managers/BADMANAGER-abcdefghij",
            "GetPaymentManager",
            "paymentManagerId",
            family_arn("payment-manager", "BADMANAGER-abcdefghij"),
        ),
        (
            "GetConfigurationBundle, a bundleId with a leading digit",
            "GET",
            "/configuration-bundles/9bundle-abcdefghij",
            "GetConfigurationBundle",
            "bundleId",
            family_arn("configuration-bundle", "9bundle-abcdefghij"),
        ),
    ];

    // A principal no readable statement allows: each family states its own sentence, and
    // every one of them names the resource and the rule — only `/runtimes` closes at the
    // action.
    harness.given_policy(None);
    for (case, method, uri, operation, _, resource) in &rows {
        let answer = harness
            .send_as(USER_KEY_ID, REGION, method, uri, None)
            .await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (403, ACCESS_DENIED),
            "case {case:?}: expected 403 {ACCESS_DENIED}, got {} {:?} with {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        let expected = not_authorized_on(&user, operation, resource);
        assert_eq!(
            answer.message, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.message
        );
    }

    // The same requests from a principal the policies allow: the modelled constraint
    // refusal, naming the member the identifier was carried at.
    harness.given_policy(Some(&allow_this_namespace()));
    for (case, method, uri, _, member, _) in &rows {
        let answer = harness
            .send_as(USER_KEY_ID, REGION, method, uri, None)
            .await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {:?} with {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        assert!(
            answer.message.contains(member) && answer.message.contains(CONSTRAINT_CLAUSE),
            "case {case:?}: expected a constraint clause naming {member:?}, got {:?}",
            answer.message
        );
    }
}

/// THE RULE'S BOUNDARY, over the identifier: one INSIDE its member's format is answered by the
/// operation — the resource itself for one this account holds, and the modelled not-found for
/// one that names nothing — and a refused call leaves the held runtime exactly as it was.
#[tokio::test]
async fn an_identifier_inside_the_modelled_format_reaches_the_operation() {
    let harness = Harness::new();
    let held = harness.create_runtime("heldRuntime").await;

    // Every refusal of the table first, so the reads below also show that none of them wrote.
    for row in unnameable_rows() {
        let answer = harness.send(row.method, &row.uri, row.body.as_ref()).await;
        assert_eq!(
            answer.status, 403,
            "case {:?}: expected the access decision's 403, got {} ({})",
            row.case, answer.status, answer.body
        );
    }

    let read = harness.send("GET", &format!("/runtimes/{held}/"), None).await;
    assert_eq!(
        (read.status, read.body["agentRuntimeId"].clone()),
        (200, json!(held)),
        "case \"the runtime this account holds\": expected the stored runtime, got {} ({})",
        read.status,
        read.body
    );

    let absent = harness
        .send("GET", "/runtimes/noSuchRuntime-abcdefghij/", None)
        .await;
    let case = "an identifier inside the format that names nothing";
    assert_eq!(
        (absent.status, absent.code.as_str()),
        (404, RESOURCE_NOT_FOUND),
        "case {case:?}: expected 404 {RESOURCE_NOT_FOUND}, got {} {:?} with {:?}",
        absent.status,
        absent.code,
        absent.message
    );
}

/// THE RULE'S BOUNDARY, over the member: the identifier read is the one the DECISION is taken
/// against. A child identifier outside its own format, beside a parent inside its format,
/// leaves the resource named — so the request is validated and answered the modelled
/// `ValidationException` — and so is a body or query member of a route naming no resource.
#[tokio::test]
async fn a_member_that_names_no_resource_is_still_the_modelled_validation_refusal() {
    let harness = Harness::new();
    let held = harness.create_runtime("validatedRuntime").await;

    // (case, method, uri, body, the member the clause must name)
    let rows: Vec<(&str, &str, String, Option<Value>, &str)> = vec![
        (
            "GetAgentRuntimeEndpoint, a child endpointName outside its format",
            "GET",
            format!("/runtimes/{held}/runtime-endpoints/9endpoint/"),
            None,
            "endpointName",
        ),
        (
            "DeleteAgentRuntimeEndpoint, the same child on another method",
            "DELETE",
            format!("/runtimes/{held}/runtime-endpoints/9endpoint/"),
            None,
            "endpointName",
        ),
        (
            "ListAgentRuntimeEndpoints, a query member outside its range",
            "POST",
            format!("/runtimes/{held}/runtime-endpoints?maxResults=0"),
            None,
            "maxResults",
        ),
        (
            "CreateAgentRuntime, a body member outside its format on a route naming no \
             resource",
            "PUT",
            "/runtimes/".to_string(),
            Some(json!({
                "agentRuntimeName": "9notAName",
                "agentRuntimeArtifact": {"containerConfiguration": {
                    "containerUri": "public.ecr.aws/agentcore/probe:v1"
                }},
                "networkConfiguration": {"networkMode": "PUBLIC"},
                "roleArn": VALID_ROLE_ARN,
            })),
            "agentRuntimeName",
        ),
    ];

    for (case, method, uri, body, member) in rows {
        let answer = harness.send(method, &uri, body.as_ref()).await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} {:?} with {:?}",
            answer.status,
            answer.code,
            answer.message
        );
        assert!(
            answer.message.contains(member) && answer.message.contains(CONSTRAINT_CLAUSE),
            "case {case:?}: expected a constraint clause naming {member:?}, got {:?}",
            answer.message
        );
    }
}
