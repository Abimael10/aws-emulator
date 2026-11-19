// SPDX-License-Identifier: CC0-1.0

//! The documents in force on a repository WHEN a task is launched decide whether that task's
//! containers ever run.
//!
//! RULE: a container's image is fetched before the container starts, and Amazon ECS makes that
//! fetch as the task's execution role (`TaskDefinition$executionRoleArn`, "the task execution
//! role that grants the Amazon ECS container agent permission to make Amazon Web Services API
//! calls on your behalf"), so the registry decides it over the documents in force on that role
//! at the instant of the launch — the identity-based policies and the repository's OWN
//! resource-based policy, where "if only one statement denies that operation, then the explicit
//! deny overrides any allow" — and resolves the reference against what it holds. A launch whose
//! fetch the registry will not serve never starts a container: the task is reported `STOPPED`
//! with `Task$stopCode` `TaskFailedToStart`, a `Task$stoppedReason` and a `Container$reason`
//! carrying the container runtime's `CannotPullContainerError` for the reference it could not
//! fetch, the instants the attempt took (`Task$pullStartedAt`/`pullStoppedAt`) and no
//! `Task$startedAt`; a launch whose fetch it serves runs the container, which then exits.
//!
//! Three services have to agree for the rule to hold, which is why this suite lives beside the
//! router: Amazon ECR owns the registry, the repository, its policy and its images, IAM owns the
//! execution role and the policies in force on it, and Amazon ECS owns the task and the terminal
//! record it stops with. Every fact is created through the owning service's own API, and every
//! value asserted is read back through `DescribeTasks` and `DescribeRepositories`.
//!
//! The rows differ along what the rule generalises over: WHICH principal the repository's policy
//! denies the pull to (the execution role itself, the account that role belongs to, another role
//! altogether), whether a policy is in force at the instant of the launch at all (none yet set,
//! one set, one deleted again), and whether the reference resolves to content the registry holds
//! (a tag it holds, a tag it does not, a repository it does not hold) — one revision, launched
//! repeatedly, so what differs between two of its tasks is only what the registry answered.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder this test crate keeps for awsQuery suites (`all/query_wire.rs`:
// "Nothing new should write another one — call this module").
use crate::query_wire::form_encode;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account this emulator serves (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The ECS cluster every task below runs in, and the container every revision declares.
const CLUSTER: &str = "policypull";
const CONTAINER: &str = "app";

/// The EC2 instance id the cluster's one container instance is registered under: "The EC2 launch
/// type runs your tasks on Amazon EC2 instances registered to your cluster", so a cluster with
/// none has nowhere to place a task.
const CAPACITY_INSTANCE_ID: &str = "i-0e0ffee0e0ffee0e3";

/// The repository this account's registry holds, and the one tag pushed into it.
const REPOSITORY: &str = "app";
const TAG: &str = "v1";

/// A tag nothing is ever pushed under, and a repository the registry is never asked to create:
/// the two references the registry answers for and resolves to nothing.
const ABSENT_TAG: &str = "does-not-exist-tag";
const ABSENT_REPOSITORY: &str = "absent";

/// The execution role every task below is launched with, and another role of the same account,
/// which no task is launched with: the row that keeps the rule about WHICH principal a policy
/// denies rather than about a policy carrying a deny at all.
const ROLE: &str = "policypullrole";
const BYSTANDER_ROLE: &str = "policypullbystander";

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Task$stopCode` of a task whose essential container exited — the end of a task that ran.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The error class a container runtime reports a failed image fetch under, which is the fixed
/// part of such a reason.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The clause a refusal carries when the REPOSITORY's own policy denies the call the pull makes:
/// the HTTP status the registry answered the manifest request with.
///
/// What the task reports for such a pull is the container agent's own account of it — the ECS
/// crate's `state::image_pull::pull_access_denied_message`, whose reading is that "the
/// authorization decision itself is the registry's own and is not on this wire: a `docker pull`
/// is answered with an HTTP status, not with an IAM sentence". So the clause held to here is that
/// status, which is what a client reads off `stoppedReason`; the IAM wording of an explicit deny
/// in a resource-based policy belongs to the registry's own API answer, which the row about the
/// registry AUTHENTICATION step covers.
const FORBIDDEN_STATUS: &str = "403 Forbidden";

/// The clause a refusal carries when the registry resolved the reference to nothing: the
/// container runtime's words for a reference no content answers for.
const NOT_FOUND: &str = "not found";

/// The two calls a pull makes on the repository, as the IAM Service Authorization Reference
/// publishes them: the manifest and the layers it references.
const REPOSITORY_PULL_ACTIONS: [&str; 2] = ["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"];

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is which
/// documents decide a pull and what the registry holds, not which blobs an image is built from.
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
/// the pull a launch makes reaches this very registry and these very policies.
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

/// One Amazon ECR call that must succeed: the repository, its images and its policy are created
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

/// One awsQuery call to IAM that must succeed: the execution role and the policies in force on it
/// are created through IAM's own API.
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

/// Create the IAM role `name`, trusted by Amazon ECS and granted every call an Amazon ECR pull
/// makes, and answer its ARN. The identity side grants everything, so no row below fails for want
/// of an identity grant: what is left to decide is the repository's own policy and what the
/// registry holds.
async fn pulling_role(router: &MultiServiceRouter, name: &str) -> String {
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
    let mut actions = vec!["ecr:GetAuthorizationToken".to_string()];
    actions.extend(REPOSITORY_PULL_ACTIONS.iter().map(ToString::to_string));
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": "*",
        }],
    })
    .to_string();
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

/// The ARN of the role `name` in the account this emulator serves.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// An image reference naming `path` in this account's registry, in the Region every request is
/// signed for: `<account>.dkr.ecr.<region>.amazonaws.com/<path>` (Amazon ECR User Guide, the
/// registry URI a pull uses).
fn image(path: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{path}")
}

/// Put `statements` on the repository as its resource-based policy, through Amazon ECR's own API.
async fn set_policy(router: &MultiServiceRouter, statements: Value, case: &str) {
    ecr(
        router,
        "SetRepositoryPolicy",
        json!({
            "repositoryName": REPOSITORY,
            "policyText": json!({
                "Version": "2012-10-17",
                "Statement": statements,
                // A repository policy is attached to the repository its statements apply to, so
                // it names no resource of its own — Amazon ECR refuses one that does.
            }).to_string(),
        }),
        case,
    )
    .await;
}

/// A statement DENYING `principal` both calls a pull makes on the repository.
fn deny_pull(principal: &str) -> Value {
    json!([{
        "Effect": "Deny",
        "Principal": { "AWS": principal },
        "Action": REPOSITORY_PULL_ACTIONS,
    }])
}

/// Register one revision of `family` running `image` as the execution role `execution_role`, run
/// one task of it, and answer the task's ARN.
async fn run(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    execution_role: &str,
    case: &str,
) -> String {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "executionRoleArn": execution_role,
            "containerDefinitions": [{ "name": CONTAINER, "image": image, "memory": 128 }],
        }),
        case,
    )
    .await;
    place(router, family, case).await
}

/// Run ONE task of the latest revision of `family`, and answer its ARN.
async fn place(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ecs(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family }),
        case,
    )
    .await;
    let task = body["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(&task, "taskArn", case)
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

/// What a launch expects of the task it placed.
enum Outcome {
    /// Nothing refused the fetch, so the container ran and then exited.
    Ran,
    /// The fetch was refused, so no container ever ran — and every one of these clauses is in
    /// the reason the task and its container report.
    FailedToStart(Vec<&'static str>),
}

/// One task the sequence below launched, waiting to be read back.
struct Launched {
    case: &'static str,
    image: String,
    task_arn: String,
    expected: Outcome,
}

#[tokio::test]
async fn a_task_is_launched_only_when_the_policy_in_force_serves_its_pull() {
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

    // The registry's own records: one repository, holding one tagged image and carrying no
    // policy of its own yet.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": REPOSITORY }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": TAG,
        }),
        case,
    )
    .await;

    let role = pulling_role(&router, ROLE).await;
    let bystander = pulling_role(&router, BYSTANDER_ROLE).await;
    let held = image(&format!("{REPOSITORY}:{TAG}"));
    let mut launched: Vec<Launched> = Vec::new();

    // ONE revision, launched again after each change to the repository's policy: the fetch is
    // decided at the launch, so what differs between these tasks is only what the registry
    // answered at the instant each of them was placed.
    let family = "policypull";
    let case = "no repository policy in force";
    launched.push(Launched {
        case,
        image: held.clone(),
        task_arn: run(&router, family, &held, &role, case).await,
        expected: Outcome::Ran,
    });

    let case = "the repository's policy denies the execution role itself";
    set_policy(&router, deny_pull(&role), case).await;
    launched.push(Launched {
        case,
        image: held.clone(),
        task_arn: place(&router, family, case).await,
        expected: Outcome::FailedToStart(vec![CANNOT_PULL, FORBIDDEN_STATUS]),
    });

    // The ACCOUNT the role belongs to is another spelling of a principal that covers it, so a
    // deny written that way withholds the same pull.
    let case = "the repository's policy denies the account the role belongs to";
    set_policy(
        &router,
        deny_pull(&format!("arn:aws:iam::{ACCOUNT}:root")),
        case,
    )
    .await;
    launched.push(Launched {
        case,
        image: held.clone(),
        task_arn: place(&router, family, case).await,
        expected: Outcome::FailedToStart(vec![CANNOT_PULL, FORBIDDEN_STATUS]),
    });

    // A deny that names ANOTHER role withholds nothing from this one: what decides the pull is
    // which principal the statement is about, not that the policy carries a deny.
    let case = "the repository's policy denies another role of the same account";
    set_policy(&router, deny_pull(&bystander), case).await;
    launched.push(Launched {
        case,
        image: held.clone(),
        task_arn: place(&router, family, case).await,
        expected: Outcome::Ran,
    });

    // And with the policy gone the identical revision pulls again: the decision is taken from
    // the documents in force at the launch rather than remembered from an earlier one.
    let case = "the repository's policy deleted again";
    ecr(
        &router,
        "DeleteRepositoryPolicy",
        json!({ "repositoryName": REPOSITORY }),
        case,
    )
    .await;
    launched.push(Launched {
        case,
        image: held.clone(),
        task_arn: place(&router, family, case).await,
        expected: Outcome::Ran,
    });

    // The other half of the fetch, with nothing withholding it: what the registry RESOLVES the
    // reference to. A tag that repository does not hold, and a repository the registry does not
    // hold at all, each resolve to nothing.
    for (case, family, path) in [
        (
            "a tag the repository does not hold",
            "policypullmissingtag",
            format!("{REPOSITORY}:{ABSENT_TAG}"),
        ),
        (
            "a repository the registry does not hold",
            "policypullabsentrepo",
            format!("{ABSENT_REPOSITORY}:{TAG}"),
        ),
    ] {
        let reference = image(&path);
        launched.push(Launched {
            case,
            image: reference.clone(),
            task_arn: run(&router, family, &reference, &role, case).await,
            expected: Outcome::FailedToStart(vec![CANNOT_PULL, NOT_FOUND]),
        });
    }

    // Every task was placed first and is polled afterwards: each settles on its own clock, so the
    // whole sequence costs one run rather than one per launch.
    for Launched {
        case,
        image,
        task_arn,
        expected,
    } in launched
    {
        let task = stopped(&router, &task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
        // The fetch is ATTEMPTED either way, so both instants are reported either way.
        for member in ["pullStartedAt", "pullStoppedAt"] {
            assert!(
                !task[member].is_null(),
                "case {case:?}: expected the attempted fetch's {member}, got {task}"
            );
        }
        match &expected {
            Outcome::Ran => {
                assert_eq!(
                    stop_code, ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: expected a task that fetched {image} to end with \
                     {ESSENTIAL_CONTAINER_EXITED}, got {task}"
                );
                assert!(
                    !task["startedAt"].is_null(),
                    "case {case:?}: expected a startedAt on a task that ran, got {task}"
                );
                let reason = task["stoppedReason"].as_str().unwrap_or_default().to_string();
                assert!(
                    !reason.contains(CANNOT_PULL),
                    "case {case:?}: expected no fetch failure on a task that ran, got {reason:?}"
                );
            }
            Outcome::FailedToStart(clauses) => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task that could not fetch {image} to stop with \
                     {TASK_FAILED_TO_START}, got {task}"
                );
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
                // The failure is reported on the task and on the container whose image it was.
                let container = task["containers"]
                    .as_array()
                    .and_then(|containers| containers.first().cloned())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected the task's one container, got {task}")
                    });
                for (member, reported) in [
                    ("stoppedReason", string(&task, "stoppedReason", case)),
                    ("the container's reason", string(&container, "reason", case)),
                ] {
                    assert!(
                        reported.contains(image.as_str()),
                        "case {case:?}: expected {member} to name the image {image}, got \
                         {reported:?}"
                    );
                    for clause in clauses {
                        assert!(
                            reported.contains(clause),
                            "case {case:?}: expected {member} to carry {clause:?}, got \
                             {reported:?}"
                        );
                    }
                }
            }
        }
    }

    // A refused fetch resolved nothing into the registry: the repository that was pushed is the
    // only one it holds, so the reference nothing answered for created no repository of its own.
    let case = "the registry's own records after the sequence";
    let described = ecr(&router, "DescribeRepositories", json!({}), case).await;
    let names: Vec<String> = described["repositories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|repository| string(repository, "repositoryName", case))
        .collect();
    assert_eq!(
        names,
        vec![REPOSITORY.to_string()],
        "case {case:?}: expected only the pushed repository, got {names:?}"
    );
}
