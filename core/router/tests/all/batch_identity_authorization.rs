// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the AWS Batch API, over its restJson1 wire.
//!
//! The rule under test: every request to Batch is a signed call, and such a call is served
//! only to a caller whose identity-based IAM policies allow THAT operation's own action
//! (`batch:<OperationName>`) on the resource the request addresses — the ARN of this
//! service the request names, and IAM's any-resource wildcard for a request that addresses
//! none. Every other caller is refused HTTP 403 under the error code
//! `AccessDeniedException`, with IAM's own sentence, BEFORE the operation reads or writes
//! anything — so a refused termination leaves its job running, a refused submit mints no
//! job, and a refused reader learns nothing about what the account holds.
//!
//! This is a CROSS-SERVICE rule and only a cross-service suite can pin it: the principals
//! below are made with IAM's and STS's OWN operations over the same engine (`CreateUser` +
//! `PutUserPolicy` + `CreateAccessKey`, `CreateRole` + `PutRolePolicy` + `AssumeRole`), so
//! the documents Batch decides on are the ones those services wrote and the principal a
//! refusal blames is the identity STS reports for the credential that signed.
//!
//! The rows vary what the rule generalises over: the OPERATION asked for (a submit and a
//! snapshot, which address a queue by ARN; a describe and a termination, which address the
//! job their `jobId` names, whose ARN this service builds from that id; and a collection
//! read, which addresses no single resource), the kind of principal (the account's own
//! published credential, an IAM user, an assumed-role session) and what its policies say
//! (nothing about this service, a wildcard `Allow`, a wildcard `Allow` beside an explicit
//! `Deny` of one action — the least-privilege shape — and an `Allow` scoped to ANOTHER
//! queue's ARN).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_batch_awsbatchv20160810::service::authorization::ANY_RESOURCE;
use service_com_amazonaws_batch_awsbatchv20160810::state::service_job_arn;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, so the ARNs the decisions are made about are
/// the ones this Region's resources carry.
pub(crate) const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: neither STS nor
/// IAM holds a record for it, so it resolves to the account's ambient identity, which holds
/// no IAM policies — the position AWS account root credentials are in.
pub(crate) const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The wire name and status of the refusal the authorization layer answers with.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";
pub(crate) const ACCESS_DENIED_STATUS: u16 = 403;

/// Batch's own IAM action prefix, which is its signing name and `arnNamespace` alike.
pub(crate) const ACTION_PREFIX: &str = "batch";

/// The only `ServiceJobType` and `ServiceEnvironmentType` the model declares.
pub(crate) const SAGEMAKER_TRAINING: &str = "SAGEMAKER_TRAINING";

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) error_code: Option<String>,
    pub(crate) body: Value,
}

impl Reply {
    /// The `message` the error document carries.
    pub(crate) fn message(&self) -> String {
        self.body["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
#[derive(Debug)]
pub(crate) struct Principal {
    pub(crate) access_key_id: String,
    pub(crate) arn: String,
}

/// A SigV4 `Authorization` header whose credential scope names `service`. The signature is
/// never checked — the router here runs with request authentication off — and the access key
/// id is what resolves the caller's identity.
fn authorization(access_key_id: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// The emulator these rows drive: Batch beside the IAM and STS services the principals and
/// their policies are made with, on ONE engine, as `core/router/src/lib.rs` mounts them.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One Batch call: `restJson1` under the `batch` credential scope, in Batch's own `/v1/…`
/// path space, signed with `access_key_id`.
pub(crate) async fn batch(
    router: &MultiServiceRouter,
    access_key_id: &str,
    path: &str,
    body: &Value,
) -> Reply {
    let payload = serde_json::to_vec(body).expect("the request body serializes");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{path}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(access_key_id, ACTION_PREFIX))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let payload = response.into_body();
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_code,
        body,
    }
}

/// One Batch call signed as the account itself, which must succeed: the suite's setup and
/// read-back legs.
pub(crate) async fn served(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    body: &Value,
) -> Value {
    let reply = batch(router, ACCOUNT_KEY, path, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the account's own call must succeed, got {reply:?}"
    );
    reply.body
}

/// One awsQuery call to IAM or STS, signed with the account's own credential, which must
/// succeed: these are the suite's setup legs.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// Create an IAM user carrying `policy` as its inline policy, mint a long-term access key
/// for it, and answer that key id with the user's own ARN.
pub(crate) async fn user(
    router: &MultiServiceRouter,
    user_name: &str,
    policy: &str,
) -> Principal {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    let arn = element(&created, "Arn");
    query(
        router,
        "iam",
        "2010-05-08",
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let minted = query(
        router,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Principal {
        access_key_id: element(&minted, "AccessKeyId"),
        arn,
    }
}

/// Create an IAM role carrying `policy` as its inline policy, assume it through STS, and
/// answer the session's access key id with the session ARN.
async fn assumed_role(router: &MultiServiceRouter, role_name: &str, policy: &str) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", "probe".to_string()),
        ],
    )
    .await;
    Principal {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// A policy document allowing `action` on `resource`.
pub(crate) fn allow(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// A policy document allowing every action of this service on everything, and explicitly
/// denying `denied` — the least-privilege shape a caller holds when one action is withheld.
fn allow_every_action_but(denied: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
            { "Effect": "Deny", "Action": denied, "Resource": "*" },
        ],
    })
    .to_string()
}

/// One request a decision is taken about: the call itself, the operation that names the
/// action IAM judges it by, and the resource the request addresses.
struct Gated {
    case: &'static str,
    path: &'static str,
    body: Value,
    operation: &'static str,
    resource: String,
}

impl Gated {
    /// The IAM action this request asks for.
    fn action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.operation)
    }
}

/// What a principal's policies grant, as the rows below vary it.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// The account's own published credential, which holds no IAM policies at all.
    AccountCredential,
    /// A document that says nothing about this service.
    NothingAboutThisService,
    /// Every action of this service on every resource.
    Everything,
    /// Every action of this service, with ONE of them explicitly denied.
    EverythingBut(&'static str),
    /// Every action of this service, scoped to another queue's ARN.
    AnotherResource,
}

impl Grant {
    /// Whether this grant lets `gate` through.
    ///
    /// A grant scoped to ANOTHER resource still lets through a request that addresses no
    /// resource of this service, where the grant question is the weaker one — whether any
    /// readable `Allow` covers the action at all — because a request naming its target by
    /// name carries no ARN to match a resource element against.
    fn serves(self, gate: &Gated) -> bool {
        match self {
            Self::AccountCredential | Self::Everything => true,
            Self::NothingAboutThisService => false,
            Self::EverythingBut(denied) => gate.action() != denied,
            Self::AnotherResource => gate.resource == ANY_RESOURCE,
        }
    }
}

/// Whether the service job `job_id` has been terminated, read back as the account.
async fn is_terminated(router: &MultiServiceRouter, case: &str, job_id: &str) -> bool {
    let detail = served(
        router,
        case,
        "/v1/describeservicejob",
        &json!({"jobId": job_id}),
    )
    .await;
    detail["isTerminated"].as_bool().unwrap_or(false)
}

/// How many service jobs the queue `queue_arn` holds, read back as the account.
async fn service_jobs_on(router: &MultiServiceRouter, case: &str, queue_arn: &str) -> usize {
    let listed = served(
        router,
        case,
        "/v1/listservicejobs",
        &json!({"jobQueue": queue_arn}),
    )
    .await;
    listed["jobSummaryList"]
        .as_array()
        .map_or(0, Vec::len)
}

/// RULE: a Batch request is served only to a caller whose identity-based policies allow its
/// operation's own action on the resource it addresses; every other caller is refused HTTP
/// 403 `AccessDeniedException` with IAM's sentence, and the call leaves the account exactly
/// as it was.
#[tokio::test]
async fn batch_serves_only_the_caller_iam_allows() {
    let router = build_router();

    // The fixture, made by the account's own credential: a service environment, a
    // SAGEMAKER_TRAINING queue over it, and one service job on that queue.
    served(
        &router,
        "fixture",
        "/v1/createserviceenvironment",
        &json!({
            "serviceEnvironmentName": "se-authz",
            "serviceEnvironmentType": SAGEMAKER_TRAINING,
            "capacityLimits": [{"maxCapacity": 10, "capacityUnit": "NUM_INSTANCES"}],
        }),
    )
    .await;
    let queue = served(
        &router,
        "fixture",
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "q-authz",
            "priority": 1,
            "jobQueueType": SAGEMAKER_TRAINING,
            "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": "se-authz"}],
        }),
    )
    .await;
    let queue_arn = queue["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let submitted = served(
        &router,
        "fixture",
        "/v1/submitservicejob",
        &json!({
            "jobName": "sj-authz",
            "jobQueue": &queue_arn,
            "serviceJobType": SAGEMAKER_TRAINING,
            "serviceRequestPayload": "{}",
        }),
    )
    .await;
    let job_id = submitted["jobId"].as_str().unwrap_or_default().to_string();
    // Another queue of the same account, which no row below addresses: what the
    // resource-scoped grant is written about.
    let elsewhere = served(
        &router,
        "fixture",
        "/v1/createjobqueue",
        &json!({
            "jobQueueName": "q-elsewhere",
            "priority": 1,
            "jobQueueType": SAGEMAKER_TRAINING,
            "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": "se-authz"}],
        }),
    )
    .await;
    let elsewhere_arn = elsewhere["jobQueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // One row per kind of request: two that address the job the request names by id (whose
    // ARN this service builds from that id), two that address a queue by ARN, and one that
    // addresses no single resource at all.
    let gates = vec![
        Gated {
            case: "a termination, addressing its job by id",
            path: "/v1/terminateservicejob",
            body: json!({"jobId": &job_id, "reason": "probe"}),
            operation: "TerminateServiceJob",
            resource: service_job_arn(REGION, &job_id),
        },
        Gated {
            case: "a read of one job, addressing it by id",
            path: "/v1/describeservicejob",
            body: json!({"jobId": &job_id}),
            operation: "DescribeServiceJob",
            resource: service_job_arn(REGION, &job_id),
        },
        Gated {
            case: "a submit, addressing its queue by ARN",
            path: "/v1/submitservicejob",
            body: json!({
                "jobName": "sj-gated",
                "jobQueue": &queue_arn,
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": "{}",
            }),
            operation: "SubmitServiceJob",
            resource: queue_arn.clone(),
        },
        Gated {
            case: "a snapshot, addressing its queue by ARN",
            path: "/v1/getjobqueuesnapshot",
            body: json!({"jobQueue": &queue_arn}),
            operation: "GetJobQueueSnapshot",
            resource: queue_arn.clone(),
        },
        Gated {
            case: "a read of a collection, addressing no single resource",
            path: "/v1/describejobqueues",
            body: json!({"jobQueues": [&queue_arn]}),
            operation: "DescribeJobQueues",
            resource: ANY_RESOURCE.to_string(),
        },
    ];

    // The principals, each carrying one of the grants the rule generalises over.
    let principals: Vec<(Grant, Principal)> = vec![
        (
            Grant::AccountCredential,
            Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: String::new(),
            },
        ),
        (
            Grant::NothingAboutThisService,
            user(&router, "u-nothing", &allow("sqs:*", "*")).await,
        ),
        (
            Grant::Everything,
            user(&router, "u-everything", &allow(&format!("{ACTION_PREFIX}:*"), "*")).await,
        ),
        (
            Grant::EverythingBut("batch:TerminateServiceJob"),
            user(
                &router,
                "u-no-terminate",
                &allow_every_action_but("batch:TerminateServiceJob"),
            )
            .await,
        ),
        (
            Grant::AnotherResource,
            user(
                &router,
                "u-another-queue",
                &allow(&format!("{ACTION_PREFIX}:*"), &elsewhere_arn),
            )
            .await,
        ),
        (
            Grant::Everything,
            assumed_role(&router, "r-everything", &allow(&format!("{ACTION_PREFIX}:*"), "*")).await,
        ),
        (
            Grant::NothingAboutThisService,
            assumed_role(&router, "r-nothing", &allow("sqs:*", "*")).await,
        ),
    ];

    for (grant, principal) in &principals {
        for gate in &gates {
            let case = format!("{:?} asking for {}", grant, gate.case);
            let before_jobs = service_jobs_on(&router, &case, &queue_arn).await;
            let before_terminated = is_terminated(&router, &case, &job_id).await;
            let reply = batch(&router, &principal.access_key_id, gate.path, &gate.body).await;

            if grant.serves(gate) {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: expected the operation's own answer, got {reply:?}"
                );
                continue;
            }

            assert_eq!(
                reply.status, ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {reply:?}"
            );
            assert_eq!(
                reply.error_code.as_deref(),
                Some(ACCESS_DENIED),
                "case {case:?}: expected the {ACCESS_DENIED} code, got {reply:?}"
            );
            let message = reply.message();
            let opening = format!(
                "User: {} is not authorized to perform: {}",
                principal.arn,
                gate.action()
            );
            assert!(
                message.starts_with(&opening),
                "case {case:?}: expected the message to open {opening:?}, got {message:?}"
            );
            if gate.resource == ANY_RESOURCE {
                assert!(
                    !message.contains(" on resource: "),
                    "case {case:?}: a request addressing no resource names none, got \
                     {message:?}"
                );
            } else {
                assert!(
                    message.contains(&format!(" on resource: {}", gate.resource)),
                    "case {case:?}: expected the resource the request addressed, got \
                     {message:?}"
                );
            }
            // A refused call changes nothing: no job is minted and the job it named is
            // left exactly as it was.
            assert_eq!(
                service_jobs_on(&router, &case, &queue_arn).await,
                before_jobs,
                "case {case:?}: a refused call mints no service job"
            );
            assert_eq!(
                is_terminated(&router, &case, &job_id).await,
                before_terminated,
                "case {case:?}: a refused call terminates nothing"
            );
        }
    }
}
