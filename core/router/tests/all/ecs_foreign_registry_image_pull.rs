// SPDX-License-Identifier: CC0-1.0

//! WHOSE Amazon ECR registry a container image reference addresses decides whether a launched
//! Amazon ECS task can fetch that image.
//!
//! RULE: Amazon ECS fetches a container's image as the task's execution role
//! (`TaskDefinition$executionRoleArn`, "the task execution role that grants the Amazon ECS
//! container agent permission to make Amazon Web Services API calls on your behalf"), and an
//! Amazon ECR registry belongs to ONE account ("the Amazon Web Services account ID associated
//! with the registry", `registryId`) whose contents are shared only through that registry's
//! own resource-based policy. So a reference into the registry of an account other than the
//! one this emulator serves is a cross-account pull nothing here serves: the task never runs
//! — it is reported `STOPPED` with `Task$stopCode` `TaskFailedToStart` and a
//! `Task$stoppedReason` carrying the container runtime's `CannotPullContainerError` for the
//! reference, whose clause names what the fetch found: for a pull made as a role, the manifest
//! request the runtime made against the registry that was ADDRESSED and the `403 Forbidden`
//! that registry answered it with, and for a launch that resolved no role to pull as, a
//! reference that resolved to nothing — and nothing of this account's own registry is
//! served in its place.
//!
//! Three services have to agree for the rule to hold, which is why this suite lives beside
//! the router: Amazon ECR owns the registry, the repositories and the images, IAM owns the
//! execution role and the policies in force on it, and Amazon ECS owns the task and the
//! terminal record it stops with. Every fact is created through the owning service's own API
//! and every value asserted is read back through `DescribeTasks` and `DescribeRepositories`.
//!
//! The rows differ along what the rule generalises over: which account the reference's
//! registry host names (this one, two different foreign ones), whether this account's own
//! registry holds a repository of that very name, whether the reference carries a tag, which
//! Region it names, whether the launch resolved a principal for the pull to be made as, and
//! whether the reference addresses an Amazon ECR private registry in the first place.

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
const CLUSTER: &str = "foreignregistry";
const CONTAINER: &str = "app";

/// The EC2 instance id the cluster's one container instance is registered under: "The EC2
/// launch type runs your tasks on Amazon EC2 instances registered to your cluster", so a
/// cluster with none has nowhere to place a task.
const CAPACITY_INSTANCE_ID: &str = "i-0d0ffee0d0ffee0d2";

/// The repository this account's registry holds, and the tag pushed into it.
const REPOSITORY: &str = "app";
const TAG: &str = "1";

/// The tag a container runtime appends to a reference that names none, which is the tag such a
/// reference's manifest request asks for.
const DEFAULT_TAG: &str = "latest";

/// Two accounts that are not the one this emulator serves, so two registries no policy here
/// shares: the rule is about the registry the host names and not about one spelling of it.
const OTHER_ACCOUNT: &str = "210987654321";
const THIRD_ACCOUNT: &str = "432109876543";

/// Another Region of THIS account's registry, which holds no repository of its own here: the
/// row that keeps the rule about the ACCOUNT rather than about the Region.
const OTHER_REGION: &str = "us-east-1";

/// The execution role every row but one pulls as.
const ROLE: &str = "foreignpull";

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
/// part of such a `stoppedReason`.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// The HTTP status a registry answers a manifest request no authorization covers with, which
/// is what the runtime reports for a refused pull: a `docker pull` is refused with a status,
/// not with an IAM sentence.
const FORBIDDEN: &str = "403 Forbidden";

/// A Docker image manifest V2 schema 2 referencing no blob: what is under test is whose
/// registry a reference addresses, not which blobs an image is built from.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
);

/// How long a row waits for the clock-owed moves it depends on — the start a placement owes,
/// the work a started task does and the stop that follows — and how often it looks. Generous
/// on purpose, by well over an order of magnitude over the spans themselves: the deadline is
/// only here so a failure is a failure rather than a hang, and a run sharing a loaded machine
/// with every other suite of the workspace must not be read as one.
const POLL_BUDGET: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Amazon ECS, Amazon ECR and IAM over ONE shared engine, as `router/src/lib.rs` mounts them,
/// so the pull a launch makes reaches this very registry and these very policies.
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

/// One Amazon ECR call that must succeed: every repository and image a pull resolves is
/// created through that service's OWN API.
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

/// One awsQuery call to IAM that must succeed: the execution role and the policies in force
/// on it are created through IAM's own API.
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

/// An image reference naming `repository` in the registry of `account`, in `region`:
/// `<account>.dkr.ecr.<region>.amazonaws.com/<repository>` (Amazon ECR User Guide, the
/// registry URI a pull uses).
fn registry_image(account: &str, region: &str, repository: &str) -> String {
    format!("{account}.dkr.ecr.{region}.amazonaws.com/{repository}")
}

/// The MANIFEST request a pull of `repository` in the registry of `account` makes, addressing
/// `reference` (a tag or a digest): the Docker Registry HTTP API V2 route
/// (`GET /v2/<name>/manifests/<reference>`) on that registry's host, which is the request a
/// registry that will not serve the pull refuses.
fn manifest_request(account: &str, region: &str, repository: &str, reference: &str) -> String {
    format!(
        "https://{account}.dkr.ecr.{region}.amazonaws.com/v2/{repository}/manifests/{reference}"
    )
}

/// Create the IAM role `name`, trusted by Amazon ECS and granted both calls an Amazon ECR
/// pull makes, and answer its ARN. Everything an identity-based policy CAN grant for a pull,
/// so no row fails for want of an identity grant: what is left to decide is whose registry
/// the reference addresses.
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
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["ecr:GetAuthorizationToken", "ecr:BatchGetImage"],
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
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Register one revision of `family` running `image` as `execution_role`, run one task of it,
/// and answer the task's ARN.
async fn run(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    execution_role: Option<&str>,
    case: &str,
) -> String {
    let mut definition = json!({
        "family": family,
        "containerDefinitions": [{ "name": CONTAINER, "image": image, "memory": 128 }],
    });
    if let Some(role) = execution_role {
        definition["executionRoleArn"] = json!(role);
    }
    ecs(router, "RegisterTaskDefinition", definition, case).await;
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

/// Poll one task until it reaches the terminal `STOPPED` status, and answer the record it
/// stopped as. The wait is for the CONDITION, bounded only so a failure is a failure rather
/// than a hang.
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

/// What a row expects of the task it launched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// Nothing refused the fetch, so the task ran and then ended.
    Ran,
    /// The fetch was refused, so the task never started — and the `stoppedReason` carries
    /// every one of these clauses.
    FailedToStart(Vec<String>),
}

/// One row of the table: a revision running `image` as `execution_role`, and the terminal
/// record the task it places must reach.
struct Row {
    case: &'static str,
    family: &'static str,
    image: String,
    execution_role: Option<String>,
    expected: Outcome,
}

/// One task the table launched, waiting to be read back.
struct Launched {
    case: &'static str,
    image: String,
    task_arn: String,
    expected: Outcome,
}

#[tokio::test]
async fn a_reference_into_another_accounts_registry_is_not_served_to_the_execution_role() {
    let router = emulator();
    let case = "the account's own records";
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    // The capacity the cluster places a task on, registered the way the container agent
    // registers it: the identity document carries the account and instance it speaks for.
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

    // This account's registry holds ONE repository, with one tagged image in it.
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
    let tagged = format!("{REPOSITORY}:{TAG}");
    // What a refusal carries when only the ADDRESSED registry's own resource-based policy
    // could have allowed the call: that registry answers the manifest request the fetch makes
    // with `403 Forbidden`, and the runtime reports the request and the status — the
    // authorization decision itself is the registry's own and is not on this wire.
    let cross_account = |account: &str, repository: &str, reference: &str| {
        Outcome::FailedToStart(vec![
            CANNOT_PULL.to_string(),
            format!(
                "unexpected status from HEAD request to {}: {FORBIDDEN}",
                manifest_request(account, REGION, repository, reference)
            ),
        ])
    };
    let rows = vec![
        Row {
            case: "this account's own registry, holding the image the reference names",
            family: "ownregistry",
            image: registry_image(ACCOUNT, REGION, &tagged),
            execution_role: Some(role.clone()),
            expected: Outcome::Ran,
        },
        Row {
            case: "another account's registry, naming a repository THIS registry holds",
            family: "foreignheld",
            image: registry_image(OTHER_ACCOUNT, REGION, &tagged),
            execution_role: Some(role.clone()),
            expected: cross_account(OTHER_ACCOUNT, REPOSITORY, TAG),
        },
        Row {
            case: "another account's registry, naming a repository nothing here holds",
            family: "foreignabsent",
            image: registry_image(OTHER_ACCOUNT, REGION, "absent:7"),
            execution_role: Some(role.clone()),
            expected: cross_account(OTHER_ACCOUNT, "absent", "7"),
        },
        Row {
            case: "a third account's registry, so the rule is not about one account id",
            family: "thirdaccount",
            image: registry_image(THIRD_ACCOUNT, REGION, &tagged),
            execution_role: Some(role.clone()),
            expected: cross_account(THIRD_ACCOUNT, REPOSITORY, TAG),
        },
        Row {
            case: "a foreign reference with no tag, which asks the registry's default tag",
            family: "foreignuntagged",
            image: registry_image(OTHER_ACCOUNT, REGION, REPOSITORY),
            execution_role: Some(role.clone()),
            expected: cross_account(OTHER_ACCOUNT, REPOSITORY, DEFAULT_TAG),
        },
        Row {
            // This account's registry in another Region is still this account's: the fetch is
            // refused for what the registry does not HOLD there, not as a cross-account pull
            // — a different answer, which is what keeps the rule about the account.
            case: "this account's registry in another Region, holding nothing of that name",
            family: "otherregion",
            image: registry_image(ACCOUNT, OTHER_REGION, &tagged),
            execution_role: Some(role.clone()),
            expected: Outcome::FailedToStart(vec![CANNOT_PULL.to_string()]),
        },
        Row {
            // A reference that addresses no Amazon ECR private registry is not that
            // registry's to answer, whichever account it might have named.
            case: "a public registry reference, which no private registry answers for",
            family: "publicregistry",
            image: "public.ecr.aws/nginx/nginx:latest".to_string(),
            execution_role: Some(role.clone()),
            expected: Outcome::Ran,
        },
        Row {
            // Whose registry a reference names does not depend on who is pulling: with no
            // principal there is nobody for an authorization sentence to blame, so what the
            // agent reports is a reference that resolved to nothing.
            case: "a foreign registry reference on a launch that resolved no role",
            family: "foreignnorole",
            image: registry_image(OTHER_ACCOUNT, REGION, &tagged),
            execution_role: None,
            expected: Outcome::FailedToStart(vec![
                CANNOT_PULL.to_string(),
                "not found".to_string(),
            ]),
        },
    ];

    // Every task is launched first and polled afterwards: each settles on its own clock, so
    // the whole table costs one run rather than one per row.
    let mut launched = Vec::new();
    for row in &rows {
        let task_arn = run(
            &router,
            row.family,
            &row.image,
            row.execution_role.as_deref(),
            row.case,
        )
        .await;
        launched.push(Launched {
            case: row.case,
            image: row.image.clone(),
            task_arn,
            expected: row.expected.clone(),
        });
    }

    for Launched {
        case,
        image,
        task_arn,
        expected,
    } in launched
    {
        let task = stopped(&router, &task_arn, case).await;
        let stop_code = string(&task, "stopCode", case);
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
            }
            Outcome::FailedToStart(clauses) => {
                assert_eq!(
                    stop_code, TASK_FAILED_TO_START,
                    "case {case:?}: expected a task that could not fetch {image} to stop with \
                     {TASK_FAILED_TO_START}, got {task}"
                );
                let reason = string(&task, "stoppedReason", case);
                assert!(
                    reason.contains(&image),
                    "case {case:?}: expected the stoppedReason to name the image {image}, got \
                     {reason:?}"
                );
                for clause in clauses {
                    assert!(
                        reason.contains(clause.as_str()),
                        "case {case:?}: expected the stoppedReason to carry {clause:?}, got \
                         {reason:?}"
                    );
                }
                assert!(
                    task["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got {task}"
                );
            }
        }
    }

    // A refused cross-account fetch resolved nothing in THIS account's registry: no
    // repository a foreign reference named was created here, and the one that was pushed is
    // the only one the registry holds.
    let case = "the registry's own records after the table";
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
