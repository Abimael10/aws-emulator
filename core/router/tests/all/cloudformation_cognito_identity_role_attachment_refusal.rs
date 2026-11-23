// SPDX-License-Identifier: CC0-1.0

//! An `AWS::Cognito::IdentityPoolRoleAttachment` a stack declares is really applied through
//! Amazon Cognito Identity's own `SetIdentityPoolRoles`, so the owning service — not
//! CloudFormation — decides whether the declaration is acceptable.
//!
//! The rule under test: **a resource of a type this emulator provisions is created by calling
//! the owning service's own API, so a declaration that service REFUSES fails exactly that
//! resource (one `CREATE_FAILED` event, carrying that service's own refusal as the reason)
//! and the create then rolls back — every resource the create had already made is deleted in
//! the service that owns it, the stack's single `ROLLBACK_IN_PROGRESS` event names the failed
//! logical id, and the stack settles in `ROLLBACK_COMPLETE` — while a declaration that
//! service ACCEPTS is applied, and the owning service reports exactly the configuration the
//! template declared.** For a role attachment the refusals are the ones
//! `SetIdentityPoolRoles` states about a `RoleMapping`: the model documents "If you specify
//! Rules as the role mapping type, RulesConfiguration is required." and "If you specify Token
//! or Rules as the Type, AmbiguousRoleResolution is required", so a mapping missing either is
//! a bad input parameter and the stack that declares it cannot complete.
//!
//! Three crates have to agree for that to hold (CloudFormation makes the call, Amazon Cognito
//! Identity serves it, IAM holds the role the attachment names), so the suite lives here
//! rather than in any one crate.
//!
//! The rows differ along what the rule generalises over: which refusal the declaration runs
//! into (a `Rules` mapping with no rules configuration, the same one sitting beside a
//! complete mapping, a mapping stating no ambiguous role resolution), how many mappings the
//! attachment carries (none, one, two), whether the mapping is of type `Token` or of type
//! `Rules` with the rules it requires, and which Region the stack deploys in.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the pool ID itself, which is the owning service's to
//! mint: what is checked about it is that the owning service resolves it while the stack
//! stands and no longer resolves it once the stack has rolled back.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// How long a row goes on polling for an outcome that is already due, and how often: the
/// stack's clock-owed spans are sub-second, and a loaded machine can take far longer than
/// that without the rule being any different, so the deadline is generous and the wait is
/// for the CONDITION rather than for a span of time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The type CloudFormation reports a stack's OWN events under.
const STACK_RESOURCE_TYPE: &str = "AWS::CloudFormation::Stack";

/// The type whose declaration this suite is about.
const ROLE_ATTACHMENT_TYPE: &str = "AWS::Cognito::IdentityPoolRoleAttachment";

/// The logical ids every row's template declares.
const POOL_ID_LOGICAL: &str = "IdentityPool";
/// See [`POOL_ID_LOGICAL`].
const ROLE_LOGICAL: &str = "AuthRole";
/// See [`POOL_ID_LOGICAL`].
const ATTACHMENT_LOGICAL: &str = "RoleAttachment";

/// The `ResourceStatus` and `StackStatus` values the rule above is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them: CloudFormation, the Amazon Cognito Identity
/// service that owns the pool and its role configuration, and IAM, which holds the role the
/// attachment names.
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
        service_com_amazonaws_cognitoidentity_awscognitoidentityservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon Cognito Identity operation over the awsJson1_1 wire its model declares,
/// signed for `region`.
async fn cognito_identity(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-identity.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityService.{operation}"),
        )
        .header("authorization", authorization("cognito-identity", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One `StackEvent` as the wire reports it.
///
/// A `StackEvent` holds no list of its own, so each `<member>` of the answer is one event and
/// the shared member reader is what separates them — never two document-ordered element lists
/// zipped, which would misalign the moment one event omitted a member.
#[derive(Debug, Clone)]
struct WireEvent {
    logical_id: String,
    physical_id: Option<String>,
    resource_type: String,
    status: String,
    status_reason: Option<String>,
}

/// Every event one `DescribeStackEvents` answer carries.
fn wire_events(xml: &str) -> Vec<WireEvent> {
    members_of(xml)
        .into_iter()
        .map(|entry| {
            let one = |name: &str| elements(entry, name).into_iter().next();
            WireEvent {
                logical_id: one("LogicalResourceId").unwrap_or_default(),
                physical_id: one("PhysicalResourceId"),
                resource_type: one("ResourceType").unwrap_or_default(),
                status: one("ResourceStatus").unwrap_or_default(),
                status_reason: one("ResourceStatusReason"),
            }
        })
        .collect()
}

/// The events a stack has recorded.
async fn events_of(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> Vec<WireEvent> {
    let (status, body) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
         {body}"
    );
    wire_events(&body)
}

/// The `DescribeStacks` answer for `stack` once its operation has left `*_IN_PROGRESS`.
///
/// The wait is for the terminal status, polled until it holds or the generous
/// [`SETTLE_DEADLINE`] passes — never for a span of wall clock, which would make the row's
/// outcome depend on how fast the host runs.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {described}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// Whether the pool `pool_id` names is one `ListIdentityPools` reports in `region`.
async fn is_listed(router: &MultiServiceRouter, region: &str, pool_id: &str) -> (bool, Value) {
    let (status, listed) = cognito_identity(
        router,
        region,
        "ListIdentityPools",
        // The model makes `MaxResults` required and bounds it at 60, which is more pools
        // than any row here creates.
        &json!({"MaxResults": 60}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListIdentityPools, got {status} with body {listed}"
    );
    let found = listed
        .get("IdentityPools")
        .and_then(Value::as_array)
        .is_some_and(|pools| {
            pools.iter().any(|pool| {
                pool.get("IdentityPoolId").and_then(Value::as_str) == Some(pool_id)
            })
        });
    (found, listed)
}

/// One row of the table: the `RoleMappings` the attachment declares, and what the rule says
/// the stack and the pool must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the pool lives in.
    region: &'static str,
    /// The `RoleMappings` the attachment declares, around the identity provider the row's
    /// mappings are for. `None` declares no mappings at all.
    role_mappings: Option<fn(&str) -> Value>,
    /// The OWNING service's own refusal sentence for the declaration, for a row Amazon
    /// Cognito Identity will not accept; `None` for one it accepts.
    refusal: Option<&'static str>,
    /// For an accepted row, the `RoleMappings` keys the pool must then report: the identity
    /// provider each declared mapping NAMES, which is what `SetIdentityPoolRoles` keys a
    /// mapping by.
    reported_keys: fn(&str) -> Vec<String>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a Rules mapping that carries no rules configuration",
        region: "us-east-1",
        role_mappings: Some(|provider| {
            json!({"userpool": {
                "IdentityProvider": provider,
                "Type": "Rules",
                "AmbiguousRoleResolution": "AuthenticatedRole",
            }})
        }),
        refusal: Some("RuleConfiguration is required for Rules"),
        reported_keys: |_| Vec::new(),
    },
    Row {
        case: "a complete mapping beside a Rules mapping with no rules configuration",
        region: "us-west-2",
        role_mappings: Some(|provider| {
            // The second mapping is keyed by the SAME user pool named WITHOUT its app client
            // — a provider the pool is configured with as well — so the row is refused for
            // what that mapping is missing rather than for the provider it is keyed by.
            let provider_name = provider.rsplit_once(':').map_or(provider, |(name, _)| name);
            json!({
                "userpool": {
                    "IdentityProvider": provider,
                    "Type": "Token",
                    "AmbiguousRoleResolution": "AuthenticatedRole",
                },
                provider_name: {
                    "Type": "Rules",
                    "AmbiguousRoleResolution": "Deny",
                },
            })
        }),
        refusal: Some("RuleConfiguration is required for Rules"),
        reported_keys: |_| Vec::new(),
    },
    Row {
        case: "a mapping that states no ambiguous role resolution",
        region: "eu-west-1",
        role_mappings: Some(|provider| {
            json!({"userpool": {
                "IdentityProvider": provider,
                "Type": "Token",
            }})
        }),
        refusal: Some("AmbiguousRoleResolution must be specified for Token"),
        reported_keys: |_| Vec::new(),
    },
    Row {
        case: "a Token mapping, which the owning service accepts",
        region: "us-east-1",
        role_mappings: Some(|provider| {
            json!({"userpool": {
                "IdentityProvider": provider,
                "Type": "Token",
                "AmbiguousRoleResolution": "AuthenticatedRole",
            }})
        }),
        refusal: None,
        reported_keys: |provider| vec![provider.to_string()],
    },
    Row {
        case: "a Rules mapping carrying the rules it requires",
        region: "us-west-2",
        role_mappings: Some(|provider| {
            json!({provider: {
                "Type": "Rules",
                "AmbiguousRoleResolution": "AuthenticatedRole",
                "RulesConfiguration": {"Rules": [{
                    "Claim": "isAdmin",
                    "MatchType": "Equals",
                    "Value": "yes",
                    "RoleARN": {"Fn::GetAtt": [ROLE_LOGICAL, "Arn"]},
                }]},
            }})
        }),
        refusal: None,
        reported_keys: |provider| vec![provider.to_string()],
    },
    Row {
        case: "an attachment that declares no mapping at all",
        region: "eu-west-1",
        role_mappings: None,
        refusal: None,
        reported_keys: |_| Vec::new(),
    },
];

/// The template every row deploys: the pool, the IAM role its attachment names, and the
/// attachment itself, differing only in the `RoleMappings` the row declares.
fn template(row: &Row, index: usize, provider: &str) -> String {
    let mut attachment = json!({
        "IdentityPoolId": {"Ref": POOL_ID_LOGICAL},
        "Roles": {"authenticated": {"Fn::GetAtt": [ROLE_LOGICAL, "Arn"]}},
    });
    if let Some(mappings) = row.role_mappings {
        attachment["RoleMappings"] = mappings(provider);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an identity pool, its role and the attachment binding them",
        "Resources": {
            POOL_ID_LOGICAL: {
                "Type": "AWS::Cognito::IdentityPool",
                "Properties": {
                    "IdentityPoolName": format!("attachment-pool-{index}"),
                    "AllowUnauthenticatedIdentities": false,
                    "DeveloperProviderName": format!("login.attachment.dev{index}"),
                    "CognitoIdentityProviders": [{
                        "ProviderName": provider_name(row.region),
                        "ClientId": client_id(index),
                        "ServerSideTokenCheck": false,
                    }],
                }
            },
            ROLE_LOGICAL: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("attachment-pool-{index}-authenticated"),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Federated": "cognito-identity.amazonaws.com"},
                            "Action": "sts:AssumeRoleWithWebIdentity",
                        }],
                    },
                }
            },
            ATTACHMENT_LOGICAL: {
                "Type": ROLE_ATTACHMENT_TYPE,
                "Properties": attachment,
            }
        },
        "Outputs": {
            "PoolRef": {"Value": {"Ref": POOL_ID_LOGICAL}},
            "RoleArn": {"Value": {"Fn::GetAtt": [ROLE_LOGICAL, "Arn"]}},
        }
    })
    .to_string()
}

/// The user pool a row's identity provider is, and the app client it issues tokens to: the
/// two halves of the `<provider name>:<client id>` identifier a role mapping of such a
/// provider is keyed by.
fn provider_name(region: &str) -> String {
    format!("cognito-idp.{region}.amazonaws.com/{region}_attached1")
}

/// See [`provider_name`].
fn client_id(index: usize) -> String {
    format!("attachedclient{index}")
}

/// Every row: a declaration Amazon Cognito Identity refuses fails exactly the attachment and
/// rolls the whole create back, leaving nothing of the stack in either owning service; one it
/// accepts leaves the pool reporting exactly the configuration the template declared.
#[tokio::test]
async fn a_refused_role_attachment_rolls_the_whole_stack_back() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("role-attachment-stack-{index}");
        let provider = format!("{}:{}", provider_name(region), client_id(index));
        let body = template(row, index, &provider);

        let (status, created) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );

        let described = settled_stack(&router, region, case, &stack_name).await;
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        let events = events_of(&router, region, case, &stack_name).await;

        match row.refusal {
            Some(sentence) => {
                assert_eq!(
                    reached, ROLLBACK_COMPLETE,
                    "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got \
                     {reached:?} with events {events:?}"
                );
                a_refused_attachment_is_the_only_failure(case, &events, sentence);
                one_rollback_event_names_the_failed_resource(case, &events);
                the_created_resources_are_deleted_again(case, &events);
                the_pool_is_gone_from_the_owning_service(&router, region, case, &events).await;
            }
            None => {
                assert_eq!(
                    reached, CREATE_COMPLETE,
                    "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got \
                     {reached:?} with events {events:?}"
                );
                let failed: Vec<&WireEvent> = events
                    .iter()
                    .filter(|event| event.status == CREATE_FAILED)
                    .collect();
                assert!(
                    failed.is_empty(),
                    "case {case:?}: expected no failed resource, got {failed:?}"
                );
                let outputs = outputs_of(&described);
                let pool_id = outputs
                    .get("PoolRef")
                    .unwrap_or_else(|| panic!("case {case:?}: no PoolRef output in {described}"))
                    .clone();
                let role_arn = outputs
                    .get("RoleArn")
                    .unwrap_or_else(|| panic!("case {case:?}: no RoleArn output in {described}"))
                    .clone();
                the_pool_reports_the_declared_configuration(
                    &router,
                    region,
                    row,
                    &pool_id,
                    &role_arn,
                    &provider,
                )
                .await;
                let (listed, listing) = is_listed(&router, region, &pool_id).await;
                assert!(
                    listed,
                    "case {case:?}: expected {pool_id} to be listed by the owning service, got \
                     {listing}"
                );
            }
        }
    }
}

/// The failed resource is exactly the attachment, and its reason carries the owning service's
/// own refusal.
fn a_refused_attachment_is_the_only_failure(case: &str, events: &[WireEvent], sentence: &str) {
    let failed: Vec<(&str, &str)> = events
        .iter()
        .filter(|event| event.status == CREATE_FAILED)
        .map(|event| (event.logical_id.as_str(), event.resource_type.as_str()))
        .collect();
    assert_eq!(
        failed,
        vec![(ATTACHMENT_LOGICAL, ROLE_ATTACHMENT_TYPE)],
        "case {case:?}: expected exactly the role attachment to have failed, got {failed:?}"
    );
    let reason = events
        .iter()
        .find(|event| event.status == CREATE_FAILED)
        .and_then(|event| event.status_reason.clone())
        .unwrap_or_default();
    assert!(
        reason.contains(sentence),
        "case {case:?}: expected the failure reason to carry the owning service's own refusal \
         {sentence:?}, got {reason:?}"
    );
}

/// The stack records ONE rollback transition, and its reason names the logical id that failed.
fn one_rollback_event_names_the_failed_resource(case: &str, events: &[WireEvent]) {
    let rollbacks: Vec<&WireEvent> = events
        .iter()
        .filter(|event| {
            event.resource_type == STACK_RESOURCE_TYPE && event.status == ROLLBACK_IN_PROGRESS
        })
        .collect();
    assert_eq!(
        rollbacks.len(),
        1,
        "case {case:?}: expected exactly one stack-level {ROLLBACK_IN_PROGRESS} event, got \
         {rollbacks:?}"
    );
    let reason = rollbacks[0].status_reason.clone().unwrap_or_default();
    let names = format!("[{ATTACHMENT_LOGICAL}]");
    assert!(
        reason.contains(&names),
        "case {case:?}: expected the rollback reason to name {names:?}, got {reason:?}"
    );
}

/// What the create had already made is deleted again: the pool and the role each record one
/// `DELETE_COMPLETE`, and the resource that never existed records none.
fn the_created_resources_are_deleted_again(case: &str, events: &[WireEvent]) {
    for logical_id in [POOL_ID_LOGICAL, ROLE_LOGICAL] {
        let deleted = events
            .iter()
            .filter(|event| event.logical_id == logical_id && event.status == DELETE_COMPLETE)
            .count();
        assert_eq!(
            deleted, 1,
            "case {case:?}: expected exactly one {DELETE_COMPLETE} event for {logical_id}, got \
             {deleted} in {events:?}"
        );
    }
}

/// The rolled-back pool is gone through the owning service's own API: the id the stack
/// published for it resolves to nothing, and nothing lists it.
async fn the_pool_is_gone_from_the_owning_service(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    events: &[WireEvent],
) {
    let pool_id = events
        .iter()
        .find(|event| event.logical_id == POOL_ID_LOGICAL && event.status == CREATE_COMPLETE)
        .and_then(|event| event.physical_id.clone())
        .unwrap_or_else(|| {
            panic!("case {case:?}: no created pool id among the stack's events {events:?}")
        });
    let (status, gone) = cognito_identity(
        router,
        region,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": &pool_id}),
    )
    .await;
    assert_eq!(
        (
            status,
            gone.get("__type").and_then(Value::as_str).unwrap_or_default(),
            gone.get("message").and_then(Value::as_str).unwrap_or_default(),
        ),
        (
            400,
            "ResourceNotFoundException",
            format!("IdentityPool '{pool_id}' not found.").as_str()
        ),
        "case {case:?}: expected the rolled-back pool to resolve to nothing, got {status} with \
         body {gone}"
    );
    let (listed, listing) = is_listed(router, region, &pool_id).await;
    assert!(
        !listed,
        "case {case:?}: expected {pool_id} to be gone from the owning service's listing, got \
         {listing}"
    );
}

/// An accepted attachment really reached the owning service: the pool reports the roles and
/// the mappings the template declared, keyed the way `SetIdentityPoolRoles` keys them.
async fn the_pool_reports_the_declared_configuration(
    router: &MultiServiceRouter,
    region: &str,
    row: &Row,
    pool_id: &str,
    role_arn: &str,
    provider: &str,
) {
    let case = row.case;
    let (status, roles) = cognito_identity(
        router,
        region,
        "GetIdentityPoolRoles",
        &json!({"IdentityPoolId": pool_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetIdentityPoolRoles, got {status} with body \
         {roles}"
    );
    assert_eq!(
        roles
            .get("Roles")
            .and_then(|roles| roles.get("authenticated"))
            .and_then(Value::as_str),
        Some(role_arn),
        "case {case:?}: expected the declared authenticated role to be in force, got {roles}"
    );
    let mappings = roles.get("RoleMappings").cloned().unwrap_or(Value::Null);
    let keys: Vec<String> = mappings
        .as_object()
        .map(|mappings| mappings.keys().cloned().collect())
        .unwrap_or_default();
    let expected = (row.reported_keys)(provider);
    assert_eq!(
        keys, expected,
        "case {case:?}: expected the pool to report the mappings keyed by the provider each \
         names, got {roles}"
    );
    for key in &expected {
        let entry = mappings.get(key).cloned().unwrap_or(Value::Null);
        let declared = (row.role_mappings.expect("a row with a reported mapping"))(provider);
        // The template may hold a mapping under the provider itself or under a key of its
        // own, the provider then being the mapping's `IdentityProvider` member.
        let declared_entry = declared
            .get(key)
            .cloned()
            .or_else(|| {
                declared.as_object().and_then(|declared| {
                    declared
                        .values()
                        .find(|mapping| {
                            mapping.get("IdentityProvider").and_then(Value::as_str)
                                == Some(key.as_str())
                        })
                        .cloned()
                })
            })
            .unwrap_or(Value::Null);
        assert_eq!(
            entry.get("Type"),
            declared_entry.get("Type"),
            "case {case:?}: expected the declared mapping type to be in force for {key:?}, got \
             {roles}"
        );
        assert_eq!(
            entry.get("AmbiguousRoleResolution"),
            declared_entry.get("AmbiguousRoleResolution"),
            "case {case:?}: expected the declared resolution to be in force for {key:?}, got \
             {roles}"
        );
        let declared_rules = declared_entry
            .get("RulesConfiguration")
            .and_then(|configuration| configuration.get("Rules"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let reported_rules = entry
            .get("RulesConfiguration")
            .and_then(|configuration| configuration.get("Rules"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported_rules.len(),
            declared_rules.len(),
            "case {case:?}: expected the declared rules to be in force for {key:?}, got {roles}"
        );
        for (declared_rule, reported_rule) in declared_rules.iter().zip(&reported_rules) {
            for member in ["Claim", "MatchType", "Value"] {
                assert_eq!(
                    reported_rule.get(member),
                    declared_rule.get(member),
                    "case {case:?}: expected the rule's {member} to be in force for {key:?}, \
                     got {roles}"
                );
            }
            assert_eq!(
                reported_rule.get("RoleARN").and_then(Value::as_str),
                Some(role_arn),
                "case {case:?}: expected the rule to name the role the template resolved for \
                 it, got {roles}"
            );
        }
    }
}
