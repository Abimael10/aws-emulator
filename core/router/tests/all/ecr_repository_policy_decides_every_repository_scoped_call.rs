// SPDX-License-Identifier: CC0-1.0

//! An Amazon ECR repository policy is an authorization input to every call scoped to that
//! repository, not just to the ones that reach its images.
//!
//! RULE: a repository policy (`SetRepositoryPolicy`) is a resource-based policy Amazon ECR
//! evaluates on EVERY request scoped to that repository — whichever part of the repository the
//! request reaches (its images and layers, its tags, the documents and configuration it
//! carries, the repository itself) and whether the request names it by `repositoryName` or by
//! the `resourceArn` of the tagging operations — so a statement that explicitly DENIES the
//! calling principal that operation's own action (`ecr:<OperationName>`) refuses the call with
//! the service-common `AccessDeniedException` (HTTP 400) before the operation reads or writes
//! anything, and the identical call made while that statement names another action is not
//! refused.
//!
//! The principal a refusal blames, and the principal a statement is matched against, is the
//! one `sts:GetCallerIdentity` answers for the credential the request was signed with, in
//! every spelling a resource-based policy may name it by — its own ARN, its account's id and
//! that account's root, and the `*` wildcard.
//!
//! The suite lives here because the decision reads three stores over ONE engine: Amazon ECR's
//! own (the repository and its policy), IAM's (the identities the document names, which the
//! set resolves) and the token service's (the principal the credential speaks for).
//!
//! The rows differ along what the rule generalises over: WHICH operation is refused (a read, a
//! write, a delete, a tagging call addressed by ARN, a policy call, a lifecycle call, a
//! configuration call), how the statement names the caller, and how it names the action (the
//! action itself, a wildcard prefix over it, every action). Nothing here reaches into a state
//! struct: every value asserted is read back over the wire, and the refusal sentence is
//! spelled out here rather than imported, so a wrong wording in the implementation cannot make
//! a test pass.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML element reader this suite's awsQuery calls need are the
// test crate's own, written once in `all/query_wire.rs`.
use crate::query_wire::{element, form_encode};

/// The region every request here is signed for, which is therefore the region segment of
/// every repository ARN the registry mints.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`engine::emulator_account_id`), which is also
/// this registry's id.
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix Amazon ECR routes on.
const TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// `sts:GetCallerIdentity`'s own API version, for the awsQuery call that reads the caller back.
const STS_API_VERSION: &str = "2011-06-15";

/// The error code an authorization decision refuses with, and its status: the AWS-common
/// authorization refusal, which this service's model declares no shape of.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The tag key the tagging rows write, read back and remove.
const PROBE_KEY: &str = "probe";

/// How many characters of an operation name one row's wildcard action keeps before the `*`:
/// enough to leave a prefix that is still a prefix of that operation and of nothing else this
/// suite calls.
const WILDCARD_PREFIX_LEN: usize = 3;

/// Amazon ECR and the two services its decision reads, over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call, as a client makes it: `POST /` with the parameters in the form body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECRPOLICY/20260101/{REGION}/{service}\
                 /aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsJson1_1 call to Amazon ECR, answered with the status and the parsed body.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("api.ecr.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECRPOLICY/20260101/{REGION}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon ECR call that must be served, answered with its body.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = ecr(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be served, got {status} with {answered}"
    );
    answered
}

/// The awsJson error code (`__type`), without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The member an AWS-COMMON error carries its sentence under: `Message`, the capitalised
/// spelling, which is how this service's common refusals report it — its MODELLED errors use
/// the lowercase `message` their shapes declare (the service crate's
/// `repository_access_authorization` suite pins the two apart).
const COMMON_ERROR_DETAIL: &str = "Message";

/// The sentence a refusal reports, under either member: the common error's and a modelled
/// error's, so a row that is NOT refused as unauthorized can be quoted too.
fn message(body: &Value) -> String {
    body.get(COMMON_ERROR_DETAIL)
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ARN `sts:GetCallerIdentity` answers for the credential every call here is signed with:
/// the principal a statement is matched against and a refusal blames.
async fn caller_arn(router: &MultiServiceRouter) -> String {
    let (status, body) = query(router, "sts", STS_API_VERSION, "GetCallerIdentity", &[]).await;
    assert_eq!(
        status, 200,
        "setup: GetCallerIdentity must be served, got {status} with {body}"
    );
    element(&body, "Arn")
}

/// The ARN of the repository `name` in this registry, as every ARN this registry mints spells
/// one: `arn:aws:ecr:<region>:<registry id>:repository/<name>`.
fn repository_arn(name: &str) -> String {
    format!("arn:aws:ecr:{REGION}:{ACCOUNT}:repository/{name}")
}

/// A repository policy with one statement: `effect` on `actions` for `principal`.
fn policy(effect: &str, principal: Value, actions: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DecideTheCall",
            "Effect": effect,
            "Principal": principal,
            "Action": actions,
        }],
    })
    .to_string()
}

/// The minimum length `LifecyclePolicyText` is modelled with
/// (`smithy_query(query="operation",operation="PutLifecyclePolicy")`: `@length(100..30720)`).
const MIN_LIFECYCLE_POLICY_LEN: usize = 100;

/// A lifecycle policy document, padded with insignificant trailing whitespace past the
/// modelled minimum length so the boundary never decides a row.
fn lifecycle_policy() -> String {
    let mut text = json!({
        "rules": [{
            "rulePriority": 1,
            "description": "expire what is past the count",
            "selection": {
                "tagStatus": "any",
                "countType": "imageCountMoreThan",
                "countNumber": 10,
            },
            "action": { "type": "expire" },
        }],
    })
    .to_string();
    while text.len() < MIN_LIFECYCLE_POLICY_LEN {
        text.push(' ');
    }
    text
}

/// One row of the operation table: the operation the policy decides, and the request body it
/// names the repository with — by `repositoryName`, or by the `resourceArn` of a tagging call.
struct Scoped {
    operation: &'static str,
    body: fn(&str) -> Value,
}

/// Every operation of this service scoped to ONE repository, with a request body the boundary
/// validator passes — so what answers the call is the authorization decision and not a
/// malformed request. The actions are the ones an Amazon ECR repository policy governs.
fn repository_scoped_operations() -> Vec<Scoped> {
    let row = |operation: &'static str, body: fn(&str) -> Value| Scoped { operation, body };
    vec![
        row("TagResource", |name| {
            json!({
                "resourceArn": repository_arn(name),
                "tags": [{ "Key": PROBE_KEY, "Value": "denied" }],
            })
        }),
        row("UntagResource", |name| {
            json!({ "resourceArn": repository_arn(name), "tagKeys": [PROBE_KEY] })
        }),
        row("ListTagsForResource", |name| {
            json!({ "resourceArn": repository_arn(name) })
        }),
        row("DescribeImages", |name| {
            json!({ "repositoryName": name })
        }),
        row("ListImages", |name| json!({ "repositoryName": name })),
        row("GetRepositoryPolicy", |name| {
            json!({ "repositoryName": name })
        }),
        row("SetRepositoryPolicy", |name| {
            json!({
                "repositoryName": name,
                "policyText": policy(
                    "Allow",
                    json!({ "AWS": format!("arn:aws:iam::{ACCOUNT}:root") }),
                    json!("ecr:BatchGetImage"),
                ),
            })
        }),
        row("DeleteRepositoryPolicy", |name| {
            json!({ "repositoryName": name })
        }),
        row("PutLifecyclePolicy", |name| {
            json!({ "repositoryName": name, "lifecyclePolicyText": lifecycle_policy() })
        }),
        row("GetLifecyclePolicy", |name| {
            json!({ "repositoryName": name })
        }),
        row("DeleteLifecyclePolicy", |name| {
            json!({ "repositoryName": name })
        }),
        row("StartLifecyclePolicyPreview", |name| {
            json!({ "repositoryName": name, "lifecyclePolicyText": lifecycle_policy() })
        }),
        row("GetLifecyclePolicyPreview", |name| {
            json!({ "repositoryName": name })
        }),
        row("PutImageScanningConfiguration", |name| {
            json!({
                "repositoryName": name,
                "imageScanningConfiguration": { "scanOnPush": true },
            })
        }),
        row("PutImageTagMutability", |name| {
            json!({ "repositoryName": name, "imageTagMutability": "IMMUTABLE" })
        }),
        row("DeleteRepository", |name| {
            json!({ "repositoryName": name })
        }),
    ]
}

/// The sentence Amazon ECR words an explicit resource-policy deny with: who was refused, which
/// action, on which resource, and which document withheld the grant. Spelled out here rather
/// than imported from the implementation.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in a resource-based policy"
    )
}

/// Install `document` as the repository's policy, signed as the caller every row uses. The
/// policy a document REPLACES may itself deny this, so it is installed while nothing denies
/// the set.
async fn share(router: &MultiServiceRouter, case: &str, repository: &str, document: &str) {
    served(
        router,
        case,
        "SetRepositoryPolicy",
        &json!({ "repositoryName": repository, "policyText": document }),
    )
    .await;
}

/// RULE: every operation scoped to one repository is decided against that repository's policy,
/// so an explicit deny of the operation's own action refuses the call — and the identical call
/// is served while the deny names another action.
#[tokio::test]
async fn an_explicit_deny_refuses_every_repository_scoped_operation() {
    let router = emulator();
    let caller = caller_arn(&router).await;

    for (index, row) in repository_scoped_operations().into_iter().enumerate() {
        let operation = row.operation;
        let action = format!("ecr:{operation}");
        let case = format!("{operation} denied by the repository's policy");

        // The positive control first, on a repository of its own, while the stored document
        // denies an action this call does not perform: the gate lets the call through,
        // whatever the operation itself then makes of the request.
        let elsewhere = format!("{case}: the deny names another action");
        let repository = format!("deny-{index}-control");
        served(
            &router,
            &elsewhere,
            "CreateRepository",
            &json!({ "repositoryName": repository }),
        )
        .await;
        share(
            &router,
            &elsewhere,
            &repository,
            &policy(
                "Deny",
                json!({ "AWS": caller.clone() }),
                json!("ecr:DescribeRegistry"),
            ),
        )
        .await;
        let (status, answered) = ecr(&router, operation, &(row.body)(&repository)).await;
        assert_ne!(
            (status, error_code(&answered).as_str()),
            (400, ACCESS_DENIED),
            "case {elsewhere:?}: a deny of another action must not refuse this call, got \
             {answered}"
        );

        // And then the same call while the document denies ITS action, under each spelling the
        // statement may name that action by. Each spelling gets a repository of its own: a row
        // whose operation deletes the repository, or whose deny locks the policy itself, leaves
        // no state a second row could be set up in.
        for (spelling, (how, actions)) in [
            ("the action itself", json!(action.clone())),
            ("the action in a list", json!([action.clone()])),
            (
                "a wildcard prefix over the action",
                json!(format!("ecr:{}*", &operation[..WILDCARD_PREFIX_LEN])),
            ),
            ("every action of this service", json!("ecr:*")),
        ]
        .into_iter()
        .enumerate()
        {
            let case = format!("{case}: the deny names {how}");
            let repository = format!("deny-{index}-{spelling}");
            served(
                &router,
                &case,
                "CreateRepository",
                &json!({ "repositoryName": repository }),
            )
            .await;
            share(
                &router,
                &case,
                &repository,
                &policy("Deny", json!({ "AWS": caller.clone() }), actions.clone()),
            )
            .await;

            let (status, answered) = ecr(&router, operation, &(row.body)(&repository)).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, ACCESS_DENIED),
                "case {case:?}: an explicit deny of {actions} must refuse this call, got \
                 {status} with {answered}"
            );
            assert_eq!(
                message(&answered),
                explicit_deny_message(&caller, &action, &repository_arn(&repository)),
                "case {case:?}: the refusal must name the caller, the action and the \
                 repository, got {:?}",
                message(&answered)
            );
        }
    }
}

/// RULE, on the operation the report names: a repository policy that explicitly denies the
/// caller `ecr:TagResource` refuses the tagging and leaves the repository's tags as they were,
/// and the identical call with that statement removed is served and the tag is read back.
///
/// The rows differ along how the statement names the principal — the caller's own ARN, its
/// account's id, that account's root, the `*` wildcard — and along a principal that is NOT the
/// caller, which decides nothing about this call.
#[tokio::test]
async fn a_denied_tagging_changes_nothing_and_the_same_call_without_the_deny_tags() {
    let router = emulator();
    let caller = caller_arn(&router).await;

    // A role of this account the caller is not, so a deny naming it refuses nobody here.
    let (status, body) = query(
        &router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", "someone-else"),
            (
                "AssumeRolePolicyDocument",
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "ecs-tasks.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                })
                .to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole must be served, got {status} with {body}"
    );

    // (case, the principal the deny names, whether it names THIS caller)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("the caller's own ARN", json!({ "AWS": caller.clone() }), true),
        (
            "the caller's ARN in a list",
            json!({ "AWS": [caller.clone()] }),
            true,
        ),
        (
            "the caller's account, by id",
            json!({ "AWS": ACCOUNT }),
            true,
        ),
        (
            "the caller's account, by its root",
            json!({ "AWS": format!("arn:aws:iam::{ACCOUNT}:root") }),
            true,
        ),
        ("every principal, as a typed object", json!({ "AWS": "*" }), true),
        ("every principal, as the bare string", json!("*"), true),
        (
            "another principal of this account",
            json!({ "AWS": format!("arn:aws:iam::{ACCOUNT}:role/someone-else") }),
            false,
        ),
    ];

    for (index, (case, principal, names_caller)) in rows.into_iter().enumerate() {
        let repository = format!("tag-deny-{index}");
        let arn = repository_arn(&repository);
        served(
            &router,
            case,
            "CreateRepository",
            &json!({ "repositoryName": repository }),
        )
        .await;
        share(
            &router,
            case,
            &repository,
            &policy("Deny", principal.clone(), json!(["ecr:TagResource"])),
        )
        .await;

        let value = format!("value-{index}");
        let request = json!({
            "resourceArn": &arn,
            "tags": [{ "Key": PROBE_KEY, "Value": &value }],
        });
        let (status, answered) = ecr(&router, "TagResource", &request).await;
        if names_caller {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, ACCESS_DENIED),
                "case {case:?} ({principal}): the tagging must be refused, got {status} with \
                 {answered}"
            );
            assert_eq!(
                message(&answered),
                explicit_deny_message(&caller, "ecr:TagResource", &arn),
                "case {case:?}: the refusal must name the caller, the action and the \
                 repository, got {:?}",
                message(&answered)
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?} ({principal}): a deny naming another principal decides nothing \
                 about this call, got {status} with {answered}"
            );
        }

        // What the repository holds: nothing was written by a refused call, and the tag the
        // served one wrote is read back.
        let listed = served(
            &router,
            case,
            "ListTagsForResource",
            &json!({ "resourceArn": &arn }),
        )
        .await;
        let probe = listed["tags"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|tag| tag["Key"].as_str() == Some(PROBE_KEY))
            .map(|tag| tag["Value"].as_str().unwrap_or_default().to_string());
        let expected = (!names_caller).then(|| value.clone());
        assert_eq!(
            probe, expected,
            "case {case:?}: expected the repository to carry {expected:?} under {PROBE_KEY:?}, \
             got {probe:?} ({listed})"
        );

        // And removing the deny makes the identical call succeed: the differential a client
        // proves enforcement with, rather than "always refused".
        share(
            &router,
            case,
            &repository,
            &policy(
                "Deny",
                json!({ "AWS": caller.clone() }),
                json!("ecr:DescribeRegistry"),
            ),
        )
        .await;
        served(&router, case, "TagResource", &request).await;
        let listed = served(
            &router,
            case,
            "ListTagsForResource",
            &json!({ "resourceArn": &arn }),
        )
        .await;
        let probe = listed["tags"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|tag| tag["Key"].as_str() == Some(PROBE_KEY))
            .map(|tag| tag["Value"].as_str().unwrap_or_default().to_string());
        assert_eq!(
            probe.as_deref(),
            Some(value.as_str()),
            "case {case:?}: with the deny narrowed, the tagging must land, got {probe:?} \
             ({listed})"
        );
    }
}
