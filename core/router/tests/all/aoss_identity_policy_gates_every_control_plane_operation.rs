// SPDX-License-Identifier: CC0-1.0

//! Who may call the OpenSearch Serverless CONTROL PLANE, asked of EVERY operation it
//! publishes.
//!
//! RULE: AWS decides a request at the IAM layer, ahead of the service, so a control-plane
//! call is served only where the identity-based policies in force on the principal its SigV4
//! credential names allow `aoss:<OperationName>` on the resource the request addresses — the
//! ARN its own input carries, else every resource — and only where the permissions boundary
//! in force on that principal allows it too; every other call is refused
//! `AccessDeniedException` at HTTP 403, naming the principal, the action, that resource and
//! which of IAM's rules withheld the grant, and performs nothing.
//!
//! Three crates have to agree for that to hold — IAM holds the role and the documents in
//! force on it, STS mints the session credential and names the principal a refusal blames,
//! and the service takes the decision before its handler reads or writes anything — so the
//! suite lives here rather than in one of them.
//!
//! What the rows span:
//!
//! * the OPERATION: all 46 this service publishes
//!   (`smithy_query(query="list_operations")`), each asked of a principal granted nothing of
//!   the service and of one granted the whole namespace — so an operation left outside the
//!   decision is caught whichever family it belongs to and whether it reads, writes or is
//!   still a scaffold. Most of those calls carry an input the operation's own validator would
//!   refuse, which is what shows the decision is taken AHEAD of the service and not by a
//!   handler that got as far as reading its input;
//! * the DOCUMENT in force on the principal: none of this service's actions, another
//!   service's action, another action of this service, the action itself, the whole namespace,
//!   an explicit deny beneath a wildcard allow, and a wildcard allow capped by a permissions
//!   boundary (one allowing nothing of the service, one allowing it, one denying the action);
//! * the RESOURCE: a request that names none, and a tag write that names its target by ARN,
//!   crossed with a statement scoped to that ARN and one scoped to another collection's.
//!
//! The decision itself is `service_common::iam_authorization`, shared with every other
//! service that authorizes a caller; what this suite pins is that the OpenSearch Serverless
//! control plane takes it, for its whole published surface, and that a refused call leaves
//! the store as it was.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// This service's SigV4 signing name, which routes a request to it, and its IAM action
/// namespace (`smithy_query(query="service_info")` -> `sigv4Name` / `arnNamespace`).
const AOSS: &str = "aoss";

/// The `X-Amz-Target` prefix this `awsJson1_0` service's operations are named under (the
/// wire-protocol section of the service crate's `OPERATIONS.md`).
const TARGET_PREFIX: &str = "OpenSearchServerless";

/// The status and wire code AWS refuses a request IAM does not permit with.
const ACCESS_DENIED_STATUS: u16 = 403;
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The long-term key every emulator publishes, which is the account's own credential — used
/// here only to read the service's state back, never as the caller under test.
const ACCOUNT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The session name every role of this suite is assumed under.
const SESSION: &str = "gate-probe";

/// The log actions an execution role carries whatever else it is granted: they name no action
/// of this service, so a role carrying only these is the position a policy nobody extended is
/// in.
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// Every operation this service publishes (`smithy_query(query="list_operations")`, which
/// reports these 46 and no other).
const OPERATIONS: &[&str] = &[
    "BatchGetCollection",
    "BatchGetCollectionGroup",
    "BatchGetEffectiveLifecyclePolicy",
    "BatchGetLifecyclePolicy",
    "BatchGetVpcEndpoint",
    "CreateAccessPolicy",
    "CreateCollection",
    "CreateCollectionGroup",
    "CreateIndex",
    "CreateLifecyclePolicy",
    "CreateSecurityConfig",
    "CreateSecurityPolicy",
    "CreateVpcEndpoint",
    "DeleteAccessPolicy",
    "DeleteCollection",
    "DeleteCollectionGroup",
    "DeleteIndex",
    "DeleteLifecyclePolicy",
    "DeleteSecurityConfig",
    "DeleteSecurityPolicy",
    "DeleteVpcEndpoint",
    "GetAccessPolicy",
    "GetAccountSettings",
    "GetIndex",
    "GetPoliciesStats",
    "GetSecurityConfig",
    "GetSecurityPolicy",
    "ListAccessPolicies",
    "ListCollectionGroups",
    "ListCollections",
    "ListLifecyclePolicies",
    "ListSecurityConfigs",
    "ListSecurityPolicies",
    "ListTagsForResource",
    "ListVpcEndpoints",
    "TagResource",
    "UntagResource",
    "UpdateAccessPolicy",
    "UpdateAccountSettings",
    "UpdateCollection",
    "UpdateCollectionGroup",
    "UpdateIndex",
    "UpdateLifecyclePolicy",
    "UpdateSecurityConfig",
    "UpdateSecurityPolicy",
    "UpdateVpcEndpoint",
];

/// The operations whose input names a resource of this service by ARN: the three carrying a
/// `resourceArn` (`smithy_query(query="operation", operation="TagResource,UntagResource,
/// ListTagsForResource")`), which are judged against that ARN where every other operation is
/// judged against every resource.
const ARN_ADDRESSED: [&str; 3] = ["ListTagsForResource", "TagResource", "UntagResource"];

/// A well-formed collection id of this account that nobody created — 20 characters of
/// `CollectionId`'s own alphabet (`^[a-z0-9]{3,40}$`). The decision is about the ARN the
/// request NAMES, not about what exists, so the rows that address one collection address this
/// one.
const ADDRESSED_COLLECTION_ID: &str = "gateaddressed0000001";

/// Another well-formed id of the same shape, which the scoped statements name INSTEAD.
const OTHER_COLLECTION_ID: &str = "gateotherxxxx0000002";

/// The emulator as it is ASSEMBLED, on one engine, so the service under test is the one the
/// router mounts.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature itself is not what this suite is about, as in every other suite of this
    // crate: the credential scope routes the request and the access key id names the caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header presenting `access_key_id`, scoped to `signing_name`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`, made as the account's own credential: the status and the
/// XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// What one control-plane call answered: the status, the wire error code and the body.
#[derive(Debug)]
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    /// The `message` the answer carries, or the empty string.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

/// One OpenSearch Serverless call over its `awsJson1_0` wire, signed as `access_key_id`.
async fn aoss(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{AOSS}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization(access_key_id, AOSS))
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let raw = response.into_body();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = header_code
        .or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_default();
    // An awsJson code can arrive namespaced (`com.amazonaws...#Shape`); a client reads the
    // shape name out of it.
    let code = code
        .split('#')
        .next_back()
        .unwrap_or_default()
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    Answer { status, code, body }
}

/// The ARN of a collection of this account in this Region, as the service mints it.
fn collection_arn(id: &str) -> String {
    format!("arn:aws:{AOSS}:{REGION}:{ACCOUNT}:collection/{id}")
}

/// The trust policy a role this suite assumes carries.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A policy document carrying the log actions every execution role has plus `statements`.
fn document(statements: Vec<Value>) -> String {
    let mut all = vec![json!({
        "Effect": "Allow",
        "Action": LOG_ACTIONS,
        "Resource": "*",
    })];
    all.extend(statements);
    json!({"Version": "2012-10-17", "Statement": all}).to_string()
}

/// One statement.
fn statement(effect: &str, actions: &[&str], resource: &str) -> Value {
    json!({"Effect": effect, "Action": actions, "Resource": resource})
}

/// Create a role carrying `inline` as its inline policy, and answer its ARN.
async fn create_role(router: &MultiServiceRouter, name: &str, inline: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN, got {created}"));
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "gate-grants"),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Put `document` in force as `role`'s permissions boundary, through the managed policy IAM
/// requires a boundary to be ("you can use a managed policy that is used to set the
/// permissions boundary", `AttachedPermissionsBoundary$PermissionsBoundaryArn`).
async fn put_boundary(router: &MultiServiceRouter, role: &str, name: &str, document: &str) {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreatePolicy",
        &[("PolicyName", name), ("PolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreatePolicy({name:?}) must succeed, got {status} with {created}"
    );
    let policy_arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreatePolicy({name:?}) must answer the policy's ARN, got {created}")
    });
    let (status, bounded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePermissionsBoundary",
        &[("RoleName", role), ("PermissionsBoundary", &policy_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePermissionsBoundary({role:?}) must succeed, got {status} with {bounded}"
    );
}

/// Assume `role_arn`: the access key id a call of that session signs with, and the principal
/// STS reports for it, which is what a refusal names.
async fn assume_role(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", SESSION)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_arn:?}) must succeed, got {status} with {body}"
    );
    let credentials = block_of(&body, "Credentials")
        .unwrap_or_else(|| panic!("setup: AssumeRole({role_arn:?}) must vend credentials"));
    let access_key_id = element(credentials, "AccessKeyId");
    let assumed = block_of(&body, "AssumedRoleUser").unwrap_or_else(|| {
        panic!("setup: AssumeRole({role_arn:?}) must report the session's own identity")
    });
    let principal = element(assumed, "Arn");
    assert!(
        !access_key_id.is_empty() && !principal.is_empty(),
        "setup: AssumeRole({role_arn:?}) must answer a key id and a principal, got \
         {access_key_id:?} and {principal:?}"
    );
    (access_key_id, principal)
}

/// One role of this suite, and the session that speaks for it.
struct Caller {
    access_key_id: String,
    principal: String,
}

/// Create a role carrying `statements` beside the log actions, optionally capped by
/// `boundary`, and assume it.
async fn caller(
    router: &MultiServiceRouter,
    name: &str,
    statements: Vec<Value>,
    boundary: Option<Vec<Value>>,
) -> Caller {
    let role_arn = create_role(router, name, &document(statements)).await;
    if let Some(boundary) = boundary {
        put_boundary(
            router,
            name,
            &format!("{name}-ceiling"),
            &document(boundary),
        )
        .await;
    }
    let (access_key_id, principal) = assume_role(router, &role_arn).await;
    Caller {
        access_key_id,
        principal,
    }
}

/// Which of IAM's rules withheld a grant, as the clause that closes the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// IAM's default deny: no readable document allows the action.
    NoPolicyAllows,
    /// A `Deny` statement of the principal's own documents covers it.
    PolicyDeny,
    /// The ceiling in force states nothing about the action, so it caps the grant away.
    NoBoundaryAllows,
    /// A `Deny` statement of the ceiling covers it.
    BoundaryDeny,
}

impl Reason {
    fn clause(self, action: &str) -> String {
        match self {
            Self::NoPolicyAllows => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::PolicyDeny => "with an explicit deny in an identity-based policy".to_string(),
            Self::NoBoundaryAllows => {
                format!("because no permissions boundary allows the {action} action")
            }
            Self::BoundaryDeny => "with an explicit deny in a permissions boundary".to_string(),
        }
    }
}

/// The sentence a refusal carries, written here from the request rather than taken from the
/// service's own helper so the wording has something to be wrong about.
fn refusal_message(principal: &str, action: &str, resource: &str, reason: Reason) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {}",
        reason.clause(action)
    )
}

/// Assert `answer` is IAM's refusal of `action` on `resource` for `principal`.
fn assert_refused(case: &str, answer: &Answer, principal: &str, action: &str, resource: &str, reason: Reason) {
    assert_eq!(
        (answer.status, answer.code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {}",
        answer.status,
        answer.code,
        answer.body
    );
    let expected = refusal_message(principal, action, resource, reason);
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected IAM's own sentence, got {}",
        answer.body
    );
}

/// Assert `answer` came from the OPERATION rather than from the gate: whatever the operation
/// then made of the request, it was not refused for want of a grant.
fn assert_reached_the_operation(case: &str, answer: &Answer) {
    assert!(
        answer.status != ACCESS_DENIED_STATUS && answer.code != ACCESS_DENIED,
        "case {case:?}: expected the call to reach the operation, got {} {:?} with {}",
        answer.status,
        answer.code,
        answer.body
    );
}

/// An input of `operation` that reaches the gate: the three operations whose input names a
/// resource by ARN carry that ARN, and every other one carries the empty document — which its
/// own validator would refuse, so a 403 for it can only have been decided ahead of the
/// service.
fn probe_input(operation: &str, arn: &str) -> Value {
    match operation {
        "ListTagsForResource" => json!({"resourceArn": arn}),
        "TagResource" => json!({"resourceArn": arn, "tags": [{"key": "gate", "value": "yes"}]}),
        "UntagResource" => json!({"resourceArn": arn, "tagKeys": ["gate"]}),
        _ => json!({}),
    }
}

/// The resource `operation` is judged against for [`probe_input`]: the ARN it names, else
/// every resource, which is what IAM writes for a request that addresses none.
fn probe_resource(operation: &str, arn: &str) -> String {
    if ARN_ADDRESSED.contains(&operation) {
        arn.to_string()
    } else {
        "*".to_string()
    }
}

/// RULE: every operation this service publishes is decided against the caller's identity
/// policy before it reads or writes anything — refused for a principal no document grants the
/// action to, and reaching the operation for one granted the namespace.
#[tokio::test]
async fn every_published_operation_is_decided_against_the_callers_identity_policy() {
    assert_eq!(
        OPERATIONS.len(),
        46,
        "case \"the published surface\": expected the 46 operations the model binds, got {}",
        OPERATIONS.len()
    );
    let router = build_router();
    let addressed = collection_arn(ADDRESSED_COLLECTION_ID);
    let ungranted = caller(&router, "gate-surface-ungranted", vec![], None).await;
    let granted = caller(
        &router,
        "gate-surface-granted",
        vec![statement("Allow", &["aoss:*"], "*")],
        None,
    )
    .await;

    for operation in OPERATIONS {
        let input = probe_input(operation, &addressed);
        let resource = probe_resource(operation, &addressed);
        let action = format!("{AOSS}:{operation}");

        let refused = aoss(&router, &ungranted.access_key_id, operation, &input).await;
        assert_refused(
            &format!("a principal granted nothing of this service calls {operation}"),
            &refused,
            &ungranted.principal,
            &action,
            &resource,
            Reason::NoPolicyAllows,
        );

        let served = aoss(&router, &granted.access_key_id, operation, &input).await;
        assert_reached_the_operation(
            &format!("a principal granted {AOSS}:* calls {operation}"),
            &served,
        );
    }
}

/// One row of the document table: what the caller's role states, the ceiling in force on it,
/// which call it then makes and how IAM decides it.
struct Row {
    case: &'static str,
    /// The statements the role's inline document carries beside the log actions.
    statements: fn(&str, &str) -> Vec<Value>,
    /// The statements the permissions boundary carries, where the row puts one in force.
    boundary: Option<fn() -> Vec<Value>>,
    /// The operation the row calls.
    operation: &'static str,
    /// `None` where the call reaches the operation, else the rule that withheld the grant.
    refused: Option<Reason>,
}

/// The rows: one per thing the rule generalises over, built over the ARN the call addresses
/// (`addressed`) and another collection's (`other`).
const ROWS: &[Row] = &[
    Row {
        case: "a role granted only the log actions",
        statements: |_addressed, _other| vec![],
        boundary: None,
        operation: "ListCollections",
        refused: Some(Reason::NoPolicyAllows),
    },
    Row {
        case: "another service's action",
        statements: |_addressed, _other| vec![statement("Allow", &["es:ListDomainNames"], "*")],
        boundary: None,
        operation: "ListCollections",
        refused: Some(Reason::NoPolicyAllows),
    },
    Row {
        case: "another action of this service",
        statements: |_addressed, _other| {
            vec![statement("Allow", &["aoss:BatchGetCollection"], "*")]
        },
        boundary: None,
        operation: "ListCollections",
        refused: Some(Reason::NoPolicyAllows),
    },
    Row {
        case: "the action itself",
        statements: |_addressed, _other| vec![statement("Allow", &["aoss:ListCollections"], "*")],
        boundary: None,
        operation: "ListCollections",
        refused: None,
    },
    Row {
        case: "a statement of the action scoped to one collection still permits a call that \
               names no resource",
        statements: |addressed, _other| {
            vec![statement("Allow", &["aoss:ListCollections"], addressed)]
        },
        boundary: None,
        operation: "ListCollections",
        refused: None,
    },
    Row {
        case: "an explicit deny beneath a wildcard allow",
        statements: |_addressed, _other| {
            vec![
                statement("Allow", &["aoss:*"], "*"),
                statement("Deny", &["aoss:ListCollections"], "*"),
            ]
        },
        boundary: None,
        operation: "ListCollections",
        refused: Some(Reason::PolicyDeny),
    },
    Row {
        case: "a wildcard allow capped by a boundary stating nothing of this service",
        statements: |_addressed, _other| vec![statement("Allow", &["aoss:*"], "*")],
        boundary: Some(Vec::new),
        operation: "ListCollections",
        refused: Some(Reason::NoBoundaryAllows),
    },
    Row {
        case: "a wildcard allow a boundary allows too",
        statements: |_addressed, _other| vec![statement("Allow", &["aoss:*"], "*")],
        boundary: Some(|| vec![statement("Allow", &["aoss:*"], "*")]),
        operation: "ListCollections",
        refused: None,
    },
    Row {
        case: "a wildcard allow a boundary explicitly denies",
        statements: |_addressed, _other| vec![statement("Allow", &["aoss:*"], "*")],
        boundary: Some(|| {
            vec![
                statement("Allow", &["aoss:*"], "*"),
                statement("Deny", &["aoss:ListCollections"], "*"),
            ]
        }),
        operation: "ListCollections",
        refused: Some(Reason::BoundaryDeny),
    },
    Row {
        case: "a tag write granted on the collection it addresses",
        statements: |addressed, _other| vec![statement("Allow", &["aoss:TagResource"], addressed)],
        boundary: None,
        operation: "TagResource",
        refused: None,
    },
    Row {
        case: "a tag write granted on another collection only",
        statements: |_addressed, other| vec![statement("Allow", &["aoss:TagResource"], other)],
        boundary: None,
        operation: "TagResource",
        refused: Some(Reason::NoPolicyAllows),
    },
];

/// RULE over the TABLE: the decision follows IAM's own order — an explicit deny refuses
/// whatever else is granted, a grant is needed otherwise, a statement's resource decides which
/// calls it covers, and the permissions boundary caps the grant — and the refusal names which
/// of those rules withheld it.
#[tokio::test]
async fn the_refusal_names_which_of_iams_rules_withheld_the_grant() {
    let router = build_router();
    let addressed = collection_arn(ADDRESSED_COLLECTION_ID);
    let other = collection_arn(OTHER_COLLECTION_ID);

    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("gate-rule-{index}");
        let probe = caller(
            &router,
            &name,
            (row.statements)(&addressed, &other),
            row.boundary.map(|statements| statements()),
        )
        .await;
        let input = probe_input(row.operation, &addressed);
        let answer = aoss(&router, &probe.access_key_id, row.operation, &input).await;
        match row.refused {
            Some(reason) => assert_refused(
                row.case,
                &answer,
                &probe.principal,
                &format!("{AOSS}:{}", row.operation),
                &probe_resource(row.operation, &addressed),
                reason,
            ),
            None => assert_reached_the_operation(row.case, &answer),
        }
    }
}

/// The collections this emulator holds under `name`, read back as the account's own
/// credential — which the identity policies of the roles above do not bound.
async fn held_collections(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let read = aoss(
        router,
        ACCOUNT_KEY,
        "ListCollections",
        &json!({"collectionFilters": {"name": name}}),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: ListCollections({name:?}) must succeed, got {} with {}",
        read.status, read.body
    );
    read.body["collectionSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: a refused call performs NOTHING — the refusal is taken ahead of the state write, so
/// a denied create answers no detail and leaves no collection behind, while the same create by
/// a granted principal is held.
#[tokio::test]
async fn a_refused_create_answers_no_detail_and_leaves_no_collection() {
    let router = build_router();
    let ungranted = caller(&router, "gate-create-ungranted", vec![], None).await;
    let granted = caller(
        &router,
        "gate-create-granted",
        vec![statement("Allow", &["aoss:*"], "*")],
        None,
    )
    .await;

    // A collection cannot be created without an `encryption` security policy covering its
    // name, so the granted principal writes one for both names first.
    let policy = aoss(
        &router,
        &granted.access_key_id,
        "CreateSecurityPolicy",
        &json!({
            "type": "encryption",
            "name": "gate-encryption",
            "policy": json!({
                "Rules": [{"ResourceType": "collection", "Resource": ["collection/*"]}],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;
    assert_eq!(
        policy.status, 200,
        "setup: the encryption policy must be accepted, got {} with {}",
        policy.status, policy.body
    );

    // (case, the principal that creates, the name it creates, whether it is granted)
    let legs: &[(&str, &Caller, &str, bool)] = &[
        (
            "a principal granted nothing of this service",
            &ungranted,
            "gate-denied-create",
            false,
        ),
        (
            "a principal granted the namespace",
            &granted,
            "gate-allowed-create",
            true,
        ),
    ];

    for (case, principal, name, allowed) in legs {
        let answer = aoss(
            &router,
            &principal.access_key_id,
            "CreateCollection",
            &json!({
                "name": name,
                "type": "SEARCH",
                "encryptionConfig": {"aWSOwnedKey": true},
            }),
        )
        .await;
        if *allowed {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the create served, got {} {:?} with {}",
                answer.status, answer.code, answer.body
            );
            assert!(
                answer.body.get("createCollectionDetail").is_some(),
                "case {case:?}: expected a created detail, got {}",
                answer.body
            );
        } else {
            assert_refused(
                case,
                &answer,
                &principal.principal,
                &format!("{AOSS}:CreateCollection"),
                "*",
                Reason::NoPolicyAllows,
            );
            assert!(
                answer.body.get("createCollectionDetail").is_none(),
                "case {case:?}: expected no created detail in the refusal, got {}",
                answer.body
            );
        }
        let held = held_collections(&router, case, name).await;
        assert_eq!(
            held.len(),
            usize::from(*allowed),
            "case {case:?}: expected the collection {name:?} to be held={allowed}, got {held:?}"
        );
    }

    // And the listing itself is decided the same way: the ungranted principal cannot even
    // read the collection the granted one created.
    let listing = aoss(
        &router,
        &ungranted.access_key_id,
        "ListCollections",
        &json!({}),
    )
    .await;
    assert_refused(
        "the ungranted principal lists collections",
        &listing,
        &ungranted.principal,
        &format!("{AOSS}:ListCollections"),
        "*",
        Reason::NoPolicyAllows,
    );
    assert!(
        listing.body.get("collectionSummaries").is_none(),
        "case \"the ungranted principal lists collections\": expected no listing payload, got {}",
        listing.body
    );
}
