// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECR registry AUTHENTICATION a launched task needs, and the stop a task whose
//! execution role is refused it earns.
//!
//! RULE: before any manifest is fetched, the Amazon ECS container agent retrieves the Amazon ECR
//! registry's authorization token as the task's execution role
//! (`TaskDefinition$executionRoleArn`, "the task execution role that grants the Amazon ECS
//! container agent permission to make Amazon Web Services API calls on your behalf"). That token
//! addresses no repository — "a client can use the returned token to access any Amazon ECR
//! registry that the IAM principal has access to" (`GetAuthorizationToken`) — so only the
//! identity-based documents in force on the role decide it, and a repository's own
//! resource-based policy is not the document for it. A role no document in force permits it has
//! therefore reached no image at all: the task is reported `STOPPED` under `Task$stopCode`
//! `TaskFailedToStart` with a `Task$stoppedReason` carrying the agent's
//! `ResourceInitializationError` for the execution resource it could not retrieve and Amazon
//! ECR's own refusal of the call, naming no image and reported on the task alone — a resource the
//! agent retrieves for the whole task belongs to no one container, so no `Container$reason` and
//! no `Container$exitCode` is reported for it. A refusal of a REPOSITORY-scoped call of the pull
//! is a different stop: the fetch was made and answered, so what the task reports is the
//! container runtime's `CannotPullContainerError` for the request the registry refused, on the
//! container whose image it was.
//!
//! Three services have to agree for the rule to hold, which is why this suite lives beside the
//! router: IAM owns the execution role and the documents in force on it, Amazon ECR owns the
//! registry, the repository, its policy and its images, and Amazon ECS owns the task and the
//! terminal record it stops with. Every fact is created through the owning service's own API, and
//! every value asserted is read back through `DescribeTasks`.
//!
//! The rows differ along what the rule generalises over: WHICH documents decide the token (an
//! `Allow` naming it, an `Allow` of the repository's calls that does not, a wildcard action
//! covering it, an `Allow` beside an explicit `Deny`), WHERE the grant sits (the role's own
//! policy, the repository's resource-based policy), and whether any image of the task comes from
//! this registry at all — with the repository-scoped refusal beside them, which is the same pull
//! failing one step later.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder this test crate keeps for awsQuery suites (`all/query_wire.rs`:
// "Nothing new should write another one — call this module").
use crate::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region every task runs in.
const REGION: &str = "us-west-2";

/// The account this emulator serves.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The ECS cluster every task below runs in, and the one container every revision declares.
const CLUSTER: &str = "registryauth";
const CONTAINER: &str = "app";

/// The EC2 instance id the cluster's one container instance is registered under: "The EC2 launch
/// type runs your tasks on Amazon EC2 instances registered to your cluster", so a cluster with
/// none has nowhere to place a task.
const CAPACITY_INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c1";

/// The one tag pushed into every repository below.
const TAG: &str = "v1";

/// A reference into a registry this emulator does not hold at all: the row that keeps the rule
/// about a task that pulls from THIS registry rather than about every task.
const FOREIGN_IMAGE: &str = "registry.example.com/library/app:v1";

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all, and of one
/// whose essential container exited — the end of a task that ran
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The fixed text of the `stoppedReason` the agent words for the Amazon ECR registry
/// authentication it could not retrieve, spelled out here rather than imported so a wrong wording
/// in the implementation cannot make this test pass.
const UNABLE_TO_RETRIEVE_REGISTRY_AUTH: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve ecr registry auth: service call has been retried 1 \
     time(s): AccessDeniedException: ";

/// The error class Amazon ECR refuses an unauthorized call with, which the agent's account of the
/// resource it could not retrieve carries.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The error class a container runtime reports a failed image FETCH under, which is the stop a
/// repository-scoped refusal earns instead.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The HTTP status the registry answers a manifest request no authorization covers with, which is
/// what the runtime reports for such a fetch.
const FORBIDDEN_STATUS: &str = "403 Forbidden";

/// The IAM sentence an explicit deny in a RESOURCE-based policy is refused with, which belongs to
/// the registry's own API answer and to no account a container runtime gives of a fetch.
const RESOURCE_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in a resource-based policy";

/// The registry-level call a pull begins with, as the IAM Service Authorization Reference
/// publishes it, and the two repository-scoped calls that follow it: the manifest and the layers
/// it references.
const GET_AUTHORIZATION_TOKEN: &str = "ecr:GetAuthorizationToken";
const REPOSITORY_PULL_ACTIONS: [&str; 2] = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"];

/// Every action of this service, as a policy names them with a wildcard.
const EVERY_ECR_ACTION: &str = "ecr:*";

/// The resource a registry-level call is decided on: it addresses no repository, so the statement
/// that grants it is the one that covers every resource.
const ANY_RESOURCE: &str = "*";

/// The clause IAM's refusal carries when no document in force allows the action, and the one it
/// carries when a document in force denies it outright.
fn no_identity_policy_clause(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}
const IDENTITY_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is which documents
/// decide the registry authentication, not which blobs an image is built from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// How long a row waits for the clock-owed moves it depends on — the start a placement owes, the
/// work a started task does and the stop that follows — and how often it looks. Generous on
/// purpose, by well over an order of magnitude over the spans themselves: the deadline is only
/// here so a failure is a failure rather than a hang, and a run sharing a loaded machine with
/// every other suite of the workspace must not be read as one.
const POLL_BUDGET: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Amazon ECS, Amazon ECR and IAM over ONE shared engine, as `router/src/lib.rs` mounts them, so
/// the registry authentication a launch retrieves is decided by these very documents.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call that must succeed, answered with its parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    json_call(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
        case,
    )
    .await
}

/// One Amazon ECR call that must succeed: the repository, its image and its policy are created
/// through that service's OWN API.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    json_call(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        body,
        case,
    )
    .await
}

/// One awsQuery call to IAM that must succeed: the execution role and the documents in force on
/// it are created through IAM's own API.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, String)], case: &str) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
}

/// A required string member of a response body.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// Which documents the row puts in force ON THE EXECUTION ROLE — the identity-based half of the
/// decision, which is the only half a registry-level call has.
#[derive(Debug, Clone, Copy)]
enum Identity {
    /// Every call the pull makes, the registry token included.
    EveryPullCall,
    /// The repository-scoped calls alone: nothing names the registry token, so IAM's default
    /// deny is what answers it.
    RepositoryCallsOnly,
    /// A wildcard over this service's actions, which covers the registry token without naming
    /// it.
    EveryEcrAction,
    /// That same wildcard with an explicit `Deny` of the registry token beside it.
    EveryEcrActionButTheToken,
}

/// What the row puts on the REPOSITORY as its own resource-based policy.
#[derive(Debug, Clone, Copy)]
enum Shared {
    /// None: the repository carries no policy of its own.
    None,
    /// Every action of this service, granted to the row's role — including the registry token,
    /// which a repository policy is not the document for.
    AllowsEveryEcrAction,
    /// The repository-scoped calls of the pull, denied to the row's role.
    DeniesTheRepositoryCalls,
}

/// Which registry the row's image reference names.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// A repository of this account's registry, holding the tag the reference asks for.
    HeldByThisRegistry,
    /// A registry this emulator does not hold at all.
    AnotherRegistry,
}

/// WHICH of IAM's rules withheld a registry token no document granted.
#[derive(Debug, Clone, Copy)]
enum Withheld {
    /// IAM's default deny: no document in force names the action at all.
    NoIdentityGrant,
    /// An explicit `Deny` in a document in force on the role.
    ExplicitDeny,
}

impl Withheld {
    /// The clause IAM's refusal carries for this rule.
    fn clause(self) -> String {
        match self {
            Self::NoIdentityGrant => no_identity_policy_clause(GET_AUTHORIZATION_TOKEN),
            Self::ExplicitDeny => IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
        }
    }
}

/// What the row expects of the task it placed.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// Nothing withheld the registry authentication or the fetch, so the container ran and then
    /// exited.
    Ran,
    /// The registry authentication was withheld by this rule: no container was ever created, and
    /// the stop names no image.
    RegistryAuthWithheld(Withheld),
    /// The registry authentication was retrieved and the MANIFEST request was refused, so the
    /// stop is the container runtime's account of the fetch it made.
    ManifestRefused,
}

/// One row of the table: the documents in force, what the reference names, and what the task is
/// expected to stop as.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    identity: Identity,
    shared: Shared,
    reference: Reference,
    expected: Expected,
}

/// The rule's rows, differing along which documents decide the registry token, where the grant
/// sits, and whether the task pulls from this registry at all.
const ROWS: [Row; 7] = [
    Row {
        case: "every call the pull makes is allowed on the execution role",
        identity: Identity::EveryPullCall,
        shared: Shared::None,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::Ran,
    },
    Row {
        case: "the execution role is allowed the repository's calls but not the registry token",
        identity: Identity::RepositoryCallsOnly,
        shared: Shared::None,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::RegistryAuthWithheld(Withheld::NoIdentityGrant),
    },
    Row {
        case: "the execution role's own policy denies the registry token outright",
        identity: Identity::EveryEcrActionButTheToken,
        shared: Shared::None,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::RegistryAuthWithheld(Withheld::ExplicitDeny),
    },
    Row {
        case: "only the repository's own policy grants the execution role the registry token",
        identity: Identity::RepositoryCallsOnly,
        shared: Shared::AllowsEveryEcrAction,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::RegistryAuthWithheld(Withheld::NoIdentityGrant),
    },
    Row {
        case: "a wildcard action on the execution role covers the registry token",
        identity: Identity::EveryEcrAction,
        shared: Shared::None,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::Ran,
    },
    Row {
        case: "no image of the task comes from a registry this emulator holds",
        identity: Identity::RepositoryCallsOnly,
        shared: Shared::None,
        reference: Reference::AnotherRegistry,
        expected: Expected::Ran,
    },
    Row {
        case: "the repository's own policy denies the execution role the manifest call",
        identity: Identity::EveryEcrAction,
        shared: Shared::DeniesTheRepositoryCalls,
        reference: Reference::HeldByThisRegistry,
        expected: Expected::ManifestRefused,
    },
];

impl Row {
    /// The statements of the inline policy this row's execution role carries.
    fn identity_statements(self) -> Value {
        let mut actions = vec![GET_AUTHORIZATION_TOKEN.to_string()];
        actions.extend(REPOSITORY_PULL_ACTIONS.iter().map(ToString::to_string));
        match self.identity {
            Identity::EveryPullCall => json!([{
                "Effect": "Allow",
                "Action": actions,
                "Resource": ANY_RESOURCE,
            }]),
            Identity::RepositoryCallsOnly => json!([{
                "Effect": "Allow",
                "Action": REPOSITORY_PULL_ACTIONS,
                "Resource": ANY_RESOURCE,
            }]),
            Identity::EveryEcrAction => json!([{
                "Effect": "Allow",
                "Action": EVERY_ECR_ACTION,
                "Resource": ANY_RESOURCE,
            }]),
            Identity::EveryEcrActionButTheToken => json!([
                {
                    "Effect": "Allow",
                    "Action": EVERY_ECR_ACTION,
                    "Resource": ANY_RESOURCE,
                },
                {
                    "Effect": "Deny",
                    "Action": GET_AUTHORIZATION_TOKEN,
                    "Resource": ANY_RESOURCE,
                },
            ]),
        }
    }

    /// The statements of the repository's own policy, for a row that puts one on it. A repository
    /// policy is attached to the repository its statements apply to, so it names no resource of
    /// its own — Amazon ECR refuses one that does.
    fn shared_statements(self, role_arn: &str) -> Option<Value> {
        match self.shared {
            Shared::None => None,
            Shared::AllowsEveryEcrAction => Some(json!([{
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": EVERY_ECR_ACTION,
            }])),
            Shared::DeniesTheRepositoryCalls => Some(json!([{
                "Effect": "Deny",
                "Principal": { "AWS": role_arn },
                "Action": REPOSITORY_PULL_ACTIONS,
            }])),
        }
    }
}

/// The ARN of the role `name` in the account this emulator serves.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The ARN of the SESSION the agent assumes an execution role in for ONE task: the retrieval is
/// made under an assumed-role session of that role named after the task it is for, which is what
/// AWS reports as the caller of a call made under one.
fn session_arn(role: &str, task_arn: &str) -> String {
    let task_id = task_arn
        .rsplit('/')
        .next()
        .expect("a task ARN carries its id last");
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{task_id}")
}

/// An image reference naming `path` in this account's registry, in the Region every request is
/// signed for: `<account>.dkr.ecr.<region>.amazonaws.com/<path>` (Amazon ECR User Guide, the
/// registry URI a pull uses).
fn image(path: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{path}")
}

/// Create the IAM role `name`, trusted by Amazon ECS and carrying `statements` as its one inline
/// policy, and answer its ARN.
async fn execution_role(router: &MultiServiceRouter, name: &str, statements: Value) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        name,
    )
    .await;
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "pull".to_string()),
            ("PolicyDocument", document),
        ],
        name,
    )
    .await;
    role_arn(name)
}

/// Poll one task until it reaches the terminal `STOPPED` status, and answer the record it stopped
/// as. The wait is for the CONDITION, bounded only so a failure is a failure rather than a hang.
async fn stopped(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ecs(
            router,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
            case,
        )
        .await;
        let task = body["tasks"]
            .as_array()
            .and_then(|tasks| tasks.first().cloned())
            .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"));
        if string(&task, "lastStatus", case) == STOPPED {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {STOPPED}, it was still {:?}: {task}",
            string(&task, "lastStatus", case)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One task the sequence below launched, waiting to be read back.
struct Launched {
    row: Row,
    role: String,
    image: String,
    task_arn: String,
}

#[tokio::test]
async fn the_registry_token_is_decided_by_the_execution_roles_own_documents() {
    let router = emulator();
    let case = "the account's own records";
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    // The capacity the cluster places a task on, registered the way the container agent registers
    // it: the identity document carries the account and instance it speaks for.
    ecs(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": json!({
                "accountId": ACCOUNT,
                "instanceId": CAPACITY_INSTANCE_ID,
                "region": REGION,
                "instanceType": "m5.large",
                "availabilityZone": format!("{REGION}a"),
            }).to_string(),
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
        case,
    )
    .await;

    // Each row gets its OWN role and its own repository, so the documents one row puts in force
    // decide that row's launch alone and every task below can be placed before any is polled.
    let mut launched: Vec<Launched> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let role_name = format!("registryauthrole{index}");
        let role = execution_role(&router, &role_name, row.identity_statements()).await;
        let repository = format!("registryauth{index}");
        let reference = match row.reference {
            Reference::HeldByThisRegistry => {
                ecr(
                    &router,
                    "CreateRepository",
                    json!({ "repositoryName": repository }),
                    case,
                )
                .await;
                ecr(
                    &router,
                    "PutImage",
                    json!({
                        "repositoryName": repository,
                        "imageManifest": MANIFEST,
                        "imageTag": TAG,
                    }),
                    case,
                )
                .await;
                if let Some(statements) = row.shared_statements(&role) {
                    ecr(
                        &router,
                        "SetRepositoryPolicy",
                        json!({
                            "repositoryName": repository,
                            "policyText": json!({
                                "Version": "2012-10-17",
                                "Statement": statements,
                            }).to_string(),
                        }),
                        case,
                    )
                    .await;
                }
                image(&format!("{repository}:{TAG}"))
            }
            Reference::AnotherRegistry => FOREIGN_IMAGE.to_string(),
        };
        let family = format!("registryauth{index}");
        ecs(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "executionRoleArn": role,
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": reference,
                    "memory": 128,
                    // A program that ENDS: a task is stopped as one whose essential
                    // container exited only once that container really exits
                    // (`Container$exitCode` is "the exit code returned from the
                    // container"), so a row that reads the stop of a task that RAN gives
                    // its container something to run to the end. What the program does is
                    // beside the point here — every row is about the retrieval before it.
                    "command": ["sh", "-c", "exit 0"],
                }],
            }),
            case,
        )
        .await;
        let placed = ecs(
            &router,
            "RunTask",
            json!({ "cluster": CLUSTER, "taskDefinition": family }),
            case,
        )
        .await;
        let task = placed["tasks"]
            .as_array()
            .and_then(|tasks| tasks.first().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected RunTask to place one task, got {placed}")
            });
        launched.push(Launched {
            row: *row,
            role: role_name,
            image: reference,
            task_arn: string(&task, "taskArn", case),
        });
    }

    // Every task was placed first and is polled afterwards: each settles on its own clock, so the
    // whole table costs one run rather than one per row.
    for Launched {
        row,
        role,
        image,
        task_arn,
    } in launched
    {
        let case = row.case;
        let task = stopped(&router, &task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
        let reason = task["stoppedReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let container = task["containers"]
            .as_array()
            .and_then(|containers| containers.first().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the task's one container, got {task}")
            });
        match row.expected {
            Expected::Ran => {
                assert_eq!(
                    stop_code, ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: expected a task whose registry authentication was retrieved \
                     to end with {ESSENTIAL_CONTAINER_EXITED}, got {task}"
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
                for class in [ACCESS_DENIED, CANNOT_PULL] {
                    assert!(
                        !reason.contains(class),
                        "case {case:?}: expected no {class} on a task that ran, got {reason:?}"
                    );
                }
            }
            Expected::RegistryAuthWithheld(withheld) => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task refused the registry authentication to stop \
                     with {TASK_FAILED_TO_START}, got {task}"
                );
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
                // The whole reason: the agent's account of the execution resource it could not
                // retrieve, carrying Amazon ECR's own refusal of the registry-level call — the
                // session of the execution role it was made as, the action, the resource a call
                // addressing no repository is decided on, and the rule that withheld it.
                let expected = format!(
                    "{UNABLE_TO_RETRIEVE_REGISTRY_AUTH}User: {} is not authorized to perform: \
                     {GET_AUTHORIZATION_TOKEN} on resource: {ANY_RESOURCE} {}",
                    session_arn(&role, &task_arn),
                    withheld.clause()
                );
                assert_eq!(
                    reason, expected,
                    "case {case:?}: expected the stoppedReason {expected:?}, got {reason:?}"
                );
                // A resource the agent retrieves for the whole task belongs to no one container,
                // and nothing ran to fetch an image or to exit.
                for member in ["reason", "exitCode", "imageDigest"] {
                    assert!(
                        container[member].is_null(),
                        "case {case:?}: expected no {member} on a container that was never \
                         created, got {container}"
                    );
                }
                assert!(
                    !reason.contains(&image),
                    "case {case:?}: expected a stop that names no image, got {reason:?}"
                );
            }
            Expected::ManifestRefused => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task whose manifest request was refused to stop \
                     with {TASK_FAILED_TO_START}, got {task}"
                );
                // The fetch WAS made, so what the task reports is the runtime's account of the
                // answer it got — on the task and on the container whose image it was — rather
                // than the agent's account of a resource it could not retrieve or the registry's
                // own IAM sentence.
                for (member, reported) in [
                    ("stoppedReason", reason.clone()),
                    ("the container's reason", string(&container, "reason", case)),
                ] {
                    for clause in [CANNOT_PULL, FORBIDDEN_STATUS, image.as_str()] {
                        assert!(
                            reported.contains(clause),
                            "case {case:?}: expected {member} to carry {clause:?}, got \
                             {reported:?}"
                        );
                    }
                    for clause in [ACCESS_DENIED, RESOURCE_POLICY_EXPLICIT_DENY] {
                        assert!(
                            !reported.contains(clause),
                            "case {case:?}: expected {member} to carry no {clause:?}, got \
                             {reported:?}"
                        );
                    }
                }
            }
        }
    }
}
