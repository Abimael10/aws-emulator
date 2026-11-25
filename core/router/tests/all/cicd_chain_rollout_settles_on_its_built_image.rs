// SPDX-License-Identifier: CC0-1.0

//! The container CI/CD chain SETTLES: CodeBuild -> Amazon ECR -> Amazon ECS behind an
//! Application Load Balancer, driven end to end the way a client drives it — by POLLING each
//! documented steady state.
//!
//! RULE under test: **every step of this chain reaches a TERMINAL state its model names,
//! within a bounded wait, so a client that polls those states reaches a verdict instead of
//! waiting forever.** `StatusType`
//! (`smithy_query(query="shape", shape="com.amazonaws.codebuild#StatusType")`) gives a build
//! the non-terminal `IN_PROGRESS` and the terminal `SUCCEEDED`/`FAILED`/`FAULT`/`TIMED_OUT`/
//! `STOPPED`, so a build that was started leaves `IN_PROGRESS`; `Deployment$rolloutState` is a
//! state machine with one entry and two ends — "When a service deployment is started, it
//! begins in an `IN_PROGRESS` state … When the service reaches a steady state, the deployment
//! transitions to a `COMPLETED` state … If the service fails to reach a steady state … the
//! deployment transitions to a `FAILED` state", with `DeploymentCircuitBreaker$rollback`
//! deciding where a failed rollout leaves the service ("the service is rolled back to the last
//! deployment that completed successfully") — so a rollout that was started leaves
//! `IN_PROGRESS` too, in one direction or the other. The same holds of the steps between: a
//! push the build made is an image the registry RESOLVES, by the `sha256:` digest
//! `ImageDetail$imageDigest` carries, and a registration of the service's task leaves the
//! `initial` of `TargetHealthStateEnum` for a verdict.
//!
//! What this suite therefore asserts of every step is that it ENDS: no wait here is a sleep,
//! each one looks again until its condition holds or a generous deadline passes, and a
//! deadline that passes fails the row with what the step last reported. That is what a client
//! polling `BatchGetBuilds`, `DescribeImages`, `DescribeServices` and `DescribeTargetHealth`
//! depends on, and it is the property a chain that never settles takes away — such a client
//! can tell neither success from refusal nor a limitation from a hung build.
//!
//! Seven crates have to agree for that to hold, which is why this suite lives here: IAM holds
//! the role the build RUNS AS and the one the task PULLS AS, Amazon ECR holds the registry the
//! push uploads to and the digest the pull resolves, CodeBuild runs the build, CloudWatch Logs
//! holds the build's own account of itself, Amazon EC2 owns the network the tasks are placed
//! in, Amazon ECS places the tasks and runs the rollouts, and Elastic Load Balancing v2 holds
//! the registrations and answers the client. Everything is driven through the real operations
//! over ONE shared engine, through the fixtures the suite that OWNS the
//! load-balancer-in-front-of-an-`awsvpc`-service topology already keeps
//! ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`]), so this suite holds no second copy
//! of the network or the front door.
//!
//! The rows differ along what the rule generalises over: which build of the chain is being
//! rolled out (the first, which the service is created on, and a later one, which updates it),
//! the tag the push addresses, and whether the revision's container serves and passes the probe
//! it declares — which is what decides WHICH terminal `rolloutState` that rollout is owed, and
//! so covers both ends of the state machine with one rule.
//!
//! The last step of the chain that did not settle is the registration a later rollout's task
//! needs: a rollout replaces one task with another, and a replacement placed at an address the
//! deregistered task held is a target the group was still DRAINING — so the group reported it
//! `draining` for the whole deregistration delay and the load balancer sent it nothing, leaving
//! a service in its own steady state reachable through no target. `draining` is the state of a
//! target "in the process of being deregistered and the deregistration delay period has not
//! expired" (`TargetHealth$Reason` `Target.DeregistrationInProgress`), which a target
//! registered AGAIN is not, so the registration now cancels that deregistration and is checked
//! from the moment it is made.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The topology this chain ends in is the fixture suite's, whose own header invites a suite
// standing it up for another rule to call these rather than keeping a second copy of them.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    ecs_ok, front_door, get, health, seed_network, Network, CLUSTER, CONTAINER, HEALTHY,
};
// The ONE percent-encoder and element reader this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for — the one the fixtures this suite calls
/// address their own services in, so the whole chain runs in one Region.
const REGION: &str = "us-east-1";

/// The statuses this suite reads: what a control-plane call answers, and what the container's
/// own server answers the document it holds with.
const OK: u16 = 200;

/// `StatusType` of a build that has not settled, and of one whose every command exited zero.
const IN_PROGRESS: &str = "IN_PROGRESS";
const SUCCEEDED: &str = "SUCCEEDED";

/// The terminal marker phase of a build, which is open while the build completes and so
/// carries no `phaseStatus` of its own.
const PHASE_COMPLETED: &str = "COMPLETED";

/// The two ends of `Deployment$rolloutState`: "When the service reaches a steady state, the
/// deployment transitions to a `COMPLETED` state" and "If the service fails to reach a steady
/// state … the deployment transitions to a `FAILED` state".
const ROLLOUT_COMPLETED: &str = "COMPLETED";
const ROLLOUT_FAILED: &str = "FAILED";

/// The ECR repository the chain pushes to. Deliberately not the name of any image this
/// emulator ships a catalogued server for, so what a container of it runs is the revision's
/// own command and nothing inherited.
const REPOSITORY: &str = "cicdchain-app";

/// The family every revision of the chain is registered under, so the rollouts below are
/// successive revisions of one deployable, as a pipeline produces them.
const FAMILY: &str = "cicdchain";

/// The group the chain's builds deliver their logs to, and the project that configures it.
const BUILD_LOG_GROUP: &str = "cicd-chain-rollout-logs";
const PROJECT: &str = "cicd-chain-rollout";

/// The port the revision's container serves on, which is the `containerPort` the binding names
/// and the target group's own port.
const PORT: i64 = 80;

/// The directory the container's own program writes its document into and serves.
const DOCUMENT_ROOT: &str = "/srv/app";

/// The marker every document below opens with, so a body anything but the chain's own
/// container produced fails the row.
const MARKER: &str = "CICD-CHAIN-SERVED";

/// The trust policy of the role a build RUNS AS: CodeBuild assumes it for the build.
const CODEBUILD_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"codebuild.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The trust policy of the role a TASK pulls as (`TaskDefinition$executionRoleArn`).
const ECS_TASKS_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"ecs-tasks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The actions AWS documents a role needs to PUSH an image to a repository (Amazon ECR User
/// Guide, the pushing permissions it lists for a repository), beside the login the push
/// presents, and the ones its User Guide publishes for a build's own log delivery.
const BUILD_ACTIONS: [&str; 9] = [
    "ecr:GetAuthorizationToken",
    "ecr:BatchCheckLayerAvailability",
    "ecr:InitiateLayerUpload",
    "ecr:UploadLayerPart",
    "ecr:CompleteLayerUpload",
    "ecr:PutImage",
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The two calls a task's image PULL makes, which its execution role has to be allowed.
const PULL_ACTIONS: [&str; 2] = ["ecr:GetAuthorizationToken", "ecr:BatchGetImage"];

/// `HealthCheck$retries`: "the number of times to retry a failed health check before the
/// container is considered unhealthy" — one, so a row spends one execution per verdict — and
/// the interval between executions, the shortest the member admits.
const PROBE_RETRIES: i64 = 1;
const PROBE_INTERVAL_SECONDS: i64 = 5;

/// The probe a revision whose container is well declares, and the one a revision nothing can
/// make healthy declares.
const PASSING_PROBE: &str = "exit 0";
const FAILING_PROBE: &str = "exit 1";

/// Two more `TargetHealthStateEnum` values this suite reads
/// (`smithy_query(query="shape", shape="TargetHealthStateEnum")`): the state a registration
/// passes THROUGH before its checks have a verdict, and the one a registration being taken out
/// of the group is reported in while it is removed.
const INITIAL: &str = "initial";
const DRAINING: &str = "draining";

/// The `deploymentCircuitBreaker` the service is created with: on, rolling back, and failing
/// the rollout at the FIRST task it loses — `ThresholdConfiguration$type` `COUNT` with a value
/// of one — so a rollout nothing can make healthy reaches its verdict after one task rather
/// than after a percentage of a desired count.
fn circuit_breaker() -> Value {
    json!({
        "enable": true,
        "rollback": true,
        "thresholdConfiguration": { "type": "COUNT", "value": 1 },
    })
}

/// How long one clock-owed move of the chain is waited for before a row calls it stalled, and
/// how long the row leaves before looking again.
///
/// Generous on purpose, and the whole point of this suite: the wait ends on the CONDITION, and
/// the deadline is only here so a step that never settles FAILS the row instead of hanging it.
/// It covers the longest move the chain owes by a wide margin — a revision's probe is executed
/// every [`PROBE_INTERVAL_SECONDS`] and the rollout nothing can make healthy takes a verdict of
/// those plus the span a registration settles over — so a step that misses it is stuck rather
/// than slow, and no row is left occupying this host for minutes on end.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(250);

/// One emulator carrying the seven services this chain spans over ONE shared engine, exactly
/// as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call to the service `signing_name` serves, answered with the HTTP status and
/// the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One call to the service `signing_name` serves that has to succeed, answered with its body.
async fn rpc_ok(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = rpc(router, signing_name, target_prefix, operation, body).await;
    assert_eq!(
        status, OK,
        "case {case:?}: {signing_name} {operation} must answer {OK}, got {status} with {answer}"
    );
    answer
}

/// One CodeBuild call that has to succeed.
async fn codebuild_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc_ok(
        router,
        case,
        "codebuild",
        "CodeBuild_20161006",
        operation,
        body,
    )
    .await
}

/// One Amazon ECR call that has to succeed.
async fn ecr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc_ok(
        router,
        case,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        body,
    )
    .await
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
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
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Create the role `name`, trusted by `trust`, whose one inline policy allows `actions`, and
/// answer its ARN — through IAM's own API, so the permissions the other services read are the
/// ones that service persisted.
async fn role_allowing(
    router: &MultiServiceRouter,
    name: &str,
    trust: &str,
    actions: &[&str],
) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust)],
    )
    .await;
    assert_eq!(
        status, OK,
        "setup: CreateRole {name:?} must persist the role, got {status}: {created}"
    );
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": "*"}],
    })
    .to_string();
    let (status, attached) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "chain-permissions"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, OK,
        "setup: PutRolePolicy {name:?} must persist the policy, got {status}: {attached}"
    );
    opt_element(&created, "Arn").unwrap_or_else(|| panic!("no <Arn> in {created}"))
}

/// The Dockerfile the chain's build materialises: the body it bakes travels as a build
/// argument, so each build of the chain composes an image of its own.
const DOCKERFILE: &str = "FROM scratch\nARG BODY\nENV SERVED_BODY=$BODY\nEXPOSE 80\n";

/// The buildspec of a container-image build, as the chain's own commands write it: the
/// Dockerfile travels as a Base64-encoded environment variable and is decoded into the working
/// tree, the build logs in to the registry with the password Amazon ECR issues, builds the
/// image with the body as a build argument and pushes it.
const BUILDSPEC: &str = "version: 0.2\nphases:\n  pre_build:\n    commands:\n      - 'echo \
                         \"$DOCKERFILE_B64\" | base64 -d > Dockerfile'\n      - 'aws ecr \
                         get-login-password --region $AWS_REGION | docker login --username AWS \
                         --password-stdin $REGISTRY'\n  build:\n    commands:\n      - 'docker \
                         build --build-arg \"BODY=$BODY\" -t \"$REPO_URI:$IMAGE_TAG\" .'\n  \
                         post_build:\n    commands:\n      - 'docker push \
                         \"$REPO_URI:$IMAGE_TAG\"'\n";

/// One rollout of the chain: the build it starts, the tag that build pushes, whether the
/// revision pinned to the pushed image starts the server its document is served by, the probe
/// that revision declares, and the terminal `Deployment$rolloutState` that rollout is therefore
/// owed.
struct Rollout {
    case: &'static str,
    tag: &'static str,
    serves: bool,
    probe: &'static str,
    terminal: &'static str,
}

const ROLLOUTS: [Rollout; 3] = [
    Rollout {
        case: "the chain's first build, which the service is created on",
        tag: "v1",
        serves: true,
        probe: PASSING_PROBE,
        terminal: ROLLOUT_COMPLETED,
    },
    Rollout {
        case: "a later build of the same chain, deployed by its own digest",
        tag: "v2",
        serves: true,
        probe: PASSING_PROBE,
        terminal: ROLLOUT_COMPLETED,
    },
    Rollout {
        case: "a build whose container serves nothing and never passes the probe it declares",
        tag: "v3",
        serves: false,
        probe: FAILING_PROBE,
        terminal: ROLLOUT_FAILED,
    },
];

/// The `(phaseType, phaseStatus)` pairs a build reports, in the order it reports them.
fn phases_of(build: &Value) -> Vec<(String, Option<String>)> {
    build["phases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|phase| {
            (
                phase["phaseType"].as_str().unwrap_or_default().to_string(),
                phase["phaseStatus"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// Look again at one build until it reports itself complete, and answer the record
/// `BatchGetBuilds` then reports — the bounded wait a client's own poll makes.
async fn settled_build(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = codebuild_ok(router, case, "BatchGetBuilds", &json!({"ids": [id]})).await;
        let build = read["builds"][0].clone();
        if build["buildComplete"] == json!(true) {
            return build;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the build {id} to leave {IN_PROGRESS} for a terminal \
             buildStatus, it last reported {build}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// One service, as `DescribeServices` reports it now.
async fn describe_service(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let read = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
    )
    .await;
    let service = read["services"][0].clone();
    assert!(
        service.is_object(),
        "case {case:?}: expected the service {name} back, got {read}"
    );
    service
}

/// The deployment of `service` that is rolling out `revision`, whichever position it is
/// reported in — a rollback supersedes an entry as `PRIMARY` while leaving it listed.
fn deployment_of(service: &Value, revision: &str) -> Option<Value> {
    service["deployments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|deployment| deployment["taskDefinition"].as_str() == Some(revision))
}

/// Look again at the rollout of `revision` until its `rolloutState` is TERMINAL, and answer
/// that deployment with the state it reached.
async fn settled_rollout(
    router: &MultiServiceRouter,
    case: &str,
    service_name: &str,
    revision: &str,
) -> (Value, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let service = describe_service(router, case, service_name).await;
        if let Some(deployment) = deployment_of(&service, revision) {
            let state = deployment["rolloutState"].as_str().unwrap_or_default();
            if state == ROLLOUT_COMPLETED || state == ROLLOUT_FAILED {
                let state = state.to_string();
                return (deployment, state);
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rollout of {revision} to leave IN_PROGRESS for \
             {ROLLOUT_COMPLETED} or {ROLLOUT_FAILED}, the service last reported {service}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Look again at `service_name` until its PRIMARY deployment is a COMPLETED rollout of
/// `revision` — the steady state a rollback puts the service back into.
async fn settled_on(router: &MultiServiceRouter, case: &str, service_name: &str, revision: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let service = describe_service(router, case, service_name).await;
        let primary = service["deployments"][0].clone();
        let settled = service["taskDefinition"].as_str() == Some(revision)
            && primary["taskDefinition"].as_str() == Some(revision)
            && primary["rolloutState"].as_str() == Some(ROLLOUT_COMPLETED)
            && service["runningCount"].as_i64() == Some(1);
        if settled {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service to settle on {revision} with a \
             {ROLLOUT_COMPLETED} rollout running the one task it wants, it last reported \
             {service}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The private IPv4 address of the interface of every RUNNING task of `service_name`, which is
/// what Amazon ECS registers for an `awsvpc` task — read so a row that never converges names
/// which addresses the service is actually running on.
async fn running_task_addresses(router: &MultiServiceRouter, service_name: &str) -> Vec<String> {
    let listed = ecs_ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": service_name, "desiredStatus": "RUNNING" }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| {
            task["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|attachment| attachment["details"].as_array())
                .flatten()
                .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                .and_then(|detail| detail["value"].as_str())
                .map(str::to_string)
        })
        .collect()
}

/// Look again at `group` until exactly ONE of its registrations has reached the verdict
/// `HEALTHY` and none of them is still waiting for a verdict — the other end of the chain's
/// convergence, read while a rollout replaces one registration with another, so the
/// registration of a task being taken out of service (`DRAINING`) is counted as settled too.
async fn settled_target(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    service_name: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = health(router, group).await;
        let serving = reported
            .iter()
            .filter(|(_, state)| state == HEALTHY)
            .count();
        if serving == 1 && reported.iter().all(|(_, state)| state != INITIAL) {
            return;
        }
        if Instant::now() >= deadline {
            // What the service itself says about the tasks it placed, so a row that never
            // converges names the reason rather than only the registrations.
            let service = describe_service(router, case, service_name).await;
            let tasks = running_task_addresses(router, service_name).await;
            panic!(
                "case {case:?}: the service reports {service}; expected exactly one registration \
                 reported {HEALTHY} and none left in {INITIAL} (a registration being removed is \
                 reported {DRAINING}), its running tasks hold {tasks:?} and the group last \
                 reported {reported:?}"
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Look again at the address the load balancer published until it answers `document` — the one
/// observation that says the client is being served the bytes of the revision in service.
async fn served_document(router: &MultiServiceRouter, case: &str, dns_name: &str, document: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answered = get(router, dns_name, "/").await;
        if answered.status == OK && answered.body.trim() == document {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the published address to answer {OK} with {document:?}, it \
             last answered {} with {:?}",
            answered.status,
            answered.body
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Register one `awsvpc`, Fargate-compatible revision of [`FAMILY`] pinned to `image`, whose
/// container writes `document` into the directory it serves when `serves` and declares `probe`
/// as its own health check, and answer its ARN.
async fn register_pinned_revision(
    router: &MultiServiceRouter,
    case: &str,
    image: &str,
    execution_role: &str,
    document: &str,
    serves: bool,
    probe: &str,
) -> String {
    // A container that serves starts the interpreter's own HTTP server module over the
    // directory it wrote its document into; one that serves nothing writes the same document
    // and starts no server, so the address its task is registered at answers nothing.
    let server = if serves {
        format!("python3 -m http.server {PORT} --directory {DOCUMENT_ROOT}")
    } else {
        "sleep infinity".to_string()
    };
    let command = format!(
        "mkdir -p {DOCUMENT_ROOT} && echo \"{document}\" > {DOCUMENT_ROOT}/index.html && {server}"
    );
    let registered = ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": image,
                "essential": true,
                "memory": 128,
                "portMappings": [{ "containerPort": PORT, "protocol": "tcp" }],
                "command": ["/bin/sh", "-c", command],
                "healthCheck": {
                    "command": ["CMD-SHELL", probe],
                    "interval": PROBE_INTERVAL_SECONDS,
                    "timeout": 2,
                    "retries": PROBE_RETRIES,
                },
            }],
        }),
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the registration must answer an ARN"))
        .to_string()
}

/// Create the chain's service on `revision`, bound to `group`, with the circuit breaker on.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    service_name: &str,
    revision: &str,
    group: &str,
) {
    ecs_ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "taskDefinition": revision,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "deploymentController": { "type": "ECS" },
            "deploymentConfiguration": { "deploymentCircuitBreaker": circuit_breaker() },
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0]],
                    "securityGroups": [network.task_group],
                    "assignPublicIp": "ENABLED",
                }
            },
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": PORT,
            }],
        }),
    )
    .await;
}

/// RULE: every step of the CodeBuild -> Amazon ECR -> Amazon ECS/Application Load Balancer
/// chain reaches a terminal state its model names within a bounded wait — the build leaves
/// `IN_PROGRESS`, the registry resolves the pushed tag to a `sha256:` digest, the rollout of a
/// revision pinned to that digest leaves `IN_PROGRESS` for `COMPLETED` when its tasks pass
/// their probe and for `FAILED` when nothing can make them pass, the circuit breaker then puts
/// the service back on the revision that completed, and the client is served the bytes of
/// whichever revision is in service.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_step_of_the_container_cicd_chain_settles_on_a_terminal_state() {
    let router = emulator();
    let setup = "the chain's own resources";
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // The registry the chain pushes to, and the address it published for it: that URI is what
    // the build's commands push to and what the task definitions pin.
    let created = ecr_ok(
        &router,
        setup,
        "CreateRepository",
        &json!({ "repositoryName": REPOSITORY }),
    )
    .await;
    let repository_uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {setup:?}: the create reports a repositoryUri, got {created}")
        })
        .to_string();
    let registry = repository_uri
        .split('/')
        .next()
        .unwrap_or(&repository_uri)
        .to_string();

    let build_role =
        role_allowing(&router, "cicd-chain-build", CODEBUILD_TRUST, &BUILD_ACTIONS).await;
    let execution_role =
        role_allowing(&router, "cicd-chain-task", ECS_TASKS_TRUST, &PULL_ACTIONS).await;
    rpc_ok(
        &router,
        setup,
        "logs",
        "Logs_20140328",
        "CreateLogGroup",
        &json!({ "logGroupName": BUILD_LOG_GROUP }),
    )
    .await;
    codebuild_ok(
        &router,
        setup,
        "CreateProject",
        &json!({
            "name": PROJECT,
            "serviceRole": build_role,
            "source": { "type": "NO_SOURCE", "buildspec": BUILDSPEC },
            "artifacts": { "type": "NO_ARTIFACTS" },
            "environment": {
                "type": "LINUX_CONTAINER",
                "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                "computeType": "BUILD_GENERAL1_SMALL",
                "privilegedMode": true,
            },
            "logsConfig": { "cloudWatchLogs": {
                "status": "ENABLED",
                "groupName": BUILD_LOG_GROUP,
            }},
        }),
    )
    .await;
    let (group, dns_name) = front_door(
        &router,
        &network,
        setup,
        "cicd-chain",
        "cicd-chain-lb",
        PORT,
    )
    .await;

    let service_name = "cicdchainsvc";
    // What the client is being served now, and by which revision: the rollback of a failed
    // rollout has to put both back.
    let mut in_service: Option<(String, String)> = None;
    let mut digests: Vec<String> = Vec::new();

    for (index, row) in ROLLOUTS.iter().enumerate() {
        let case = row.case;

        // The build. Its own commands are what push the image the rest of the row pins.
        let started = codebuild_ok(
            &router,
            case,
            "StartBuild",
            &json!({
                "projectName": PROJECT,
                "environmentVariablesOverride": [
                    { "name": "REGISTRY", "value": registry, "type": "PLAINTEXT" },
                    { "name": "REPO_URI", "value": repository_uri, "type": "PLAINTEXT" },
                    { "name": "IMAGE_TAG", "value": row.tag, "type": "PLAINTEXT" },
                    {
                        "name": "BODY",
                        "value": format!("{MARKER} {}", row.tag),
                        "type": "PLAINTEXT",
                    },
                    {
                        "name": "DOCKERFILE_B64",
                        "value": aws_smithy_types::base64::encode(DOCKERFILE),
                        "type": "PLAINTEXT",
                    },
                ],
            }),
        )
        .await;
        assert_eq!(
            started["build"]["buildStatus"].as_str(),
            Some(IN_PROGRESS),
            "case {case:?}: the start answers a build that is under way, got {started}"
        );
        let build_id = started["build"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the start reports a build id, got {started}"))
            .to_string();

        let build = settled_build(&router, case, &build_id).await;
        assert_eq!(
            build["buildStatus"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: every command of the build exited zero, so the build succeeded, got \
             {build}"
        );
        for (phase, status) in phases_of(&build) {
            if phase == PHASE_COMPLETED {
                continue;
            }
            assert_eq!(
                status.as_deref(),
                Some(SUCCEEDED),
                "case {case:?}: the {phase} phase of a build whose commands succeeded is \
                 {SUCCEEDED}, got {build}"
            );
        }

        // The build's own LOG is its account of what it did, delivered to the group the
        // project configured and the stream the record names — so a client reads there how
        // each phase of the build ended.
        let group_name = build["logs"]["groupName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the build reports its log group, got {build}")
            })
            .to_string();
        let stream = build["logs"]["streamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the build reports its log stream, got {build}")
            })
            .to_string();
        assert_eq!(
            group_name, BUILD_LOG_GROUP,
            "case {case:?}: the build's log goes to the group its project configured, got \
             {group_name:?}"
        );
        let read = rpc_ok(
            &router,
            case,
            "logs",
            "Logs_20140328",
            "GetLogEvents",
            &json!({ "logGroupName": group_name, "logStreamName": stream }),
        )
        .await;
        let messages: Vec<String> = read["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message == "Phase complete: BUILD State: SUCCEEDED"),
            "case {case:?}: the build's log carries how its BUILD phase ended, got {messages:?}"
        );

        // What the REGISTRY holds is what says the push happened, and the digest it reports is
        // what the revision pins.
        let described = ecr_ok(
            &router,
            case,
            "DescribeImages",
            &json!({
                "repositoryName": REPOSITORY,
                "imageIds": [{ "imageTag": row.tag }],
            }),
        )
        .await;
        let digest = described["imageDetails"][0]["imageDigest"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the registry resolves the pushed tag, got {described}")
            })
            .to_string();
        assert!(
            digest.starts_with("sha256:"),
            "case {case:?}: an image is addressed by the digest of its manifest, got {digest:?}"
        );
        digests.push(digest.clone());

        // The revision pinned to that digest, and the rollout of it.
        let document = format!("{MARKER} {} {build_id}", row.tag);
        let revision = register_pinned_revision(
            &router,
            case,
            &format!("{repository_uri}@{digest}"),
            &execution_role,
            &document,
            row.serves,
            row.probe,
        )
        .await;
        if index == 0 {
            create_service(&router, &network, service_name, &revision, &group).await;
        } else {
            ecs_ok(
                &router,
                "UpdateService",
                json!({
                    "cluster": CLUSTER,
                    "service": service_name,
                    "taskDefinition": revision,
                }),
            )
            .await;
        }

        // The one thing this suite is about: the rollout ENDS, at one of the two ends the
        // state machine has, and at the one this row's revision is owed.
        let (deployment, state) = settled_rollout(&router, case, service_name, &revision).await;
        assert_eq!(
            state, row.terminal,
            "case {case:?}: expected the rollout to end at {}, got {deployment}",
            row.terminal
        );

        if row.terminal == ROLLOUT_COMPLETED {
            settled_target(&router, case, &group, service_name).await;
            served_document(&router, case, &dns_name, &document).await;
            in_service = Some((revision, document));
        } else {
            // A failed rollout REPORTS why it failed, and the breaker then puts the service
            // back on the last revision that completed — which is the one the client keeps
            // being served.
            assert!(
                deployment["rolloutStateReason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "case {case:?}: a {ROLLOUT_FAILED} rollout reports why it failed, got \
                 {deployment}"
            );
            let (restored, restored_document) = in_service
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: a rollout completed before this one"));
            settled_on(&router, case, service_name, &restored).await;
            settled_target(&router, case, &group, service_name).await;
            served_document(&router, case, &dns_name, &restored_document).await;
        }
    }

    // Each build of the chain passed its own body to a Dockerfile that resolves it, so the
    // registry's own answer is a digest of its own per build — which is what makes a rollout
    // "by digest" name one build's image rather than whatever the tag last pointed at.
    let distinct: std::collections::BTreeSet<&String> = digests.iter().collect();
    assert_eq!(
        distinct.len(),
        digests.len(),
        "expected one distinct digest per build of the chain, got {digests:?}"
    );
}
