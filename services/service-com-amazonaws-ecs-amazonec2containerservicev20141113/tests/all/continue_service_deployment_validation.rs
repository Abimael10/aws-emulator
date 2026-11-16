// SPDX-License-Identifier: CC0-1.0

//! `ContinueServiceDeployment`'s INPUT VALIDATION, driven end to end over the shared wire
//! harness in [`crate::ecs_wire`].
//!
//! The rule under test: each identifier the request carries is checked against the FORM
//! Amazon ECS mints it in before any state is consulted, and a value outside that form is
//! refused `InvalidParameterException` with the fixed sentence that states the form — the
//! `serviceDeploymentArn` first, then the `hookId` — so a caller that passed a value which
//! could name no deployment and no hook at all is told its request is malformed, while a
//! well-formed value that happens to name nothing waiting earns the state refusals instead
//! (`ServiceDeploymentNotFoundException` for a rollout nothing recorded, and the
//! "not the hook waiting for an action" `InvalidParameterException` for a hook that is not
//! the one waiting).
//!
//! The docs the rows are read off:
//!
//! * `ContinueServiceDeployment$hookId`: "The ID of the paused lifecycle hook to act on. You
//!   can find the hookId by calling DescribeServiceDeployments and inspecting the
//!   lifecycleHookDetails field of the service deployment."
//! * `ContinueServiceDeployment$serviceDeploymentArn`: "The ARN of the service deployment to
//!   continue or roll back."
//! * `DeploymentLifecycleHookDetail$hookId`: "The ID of the lifecycle hook. Use this value
//!   when calling ContinueServiceDeployment to continue or roll back a paused deployment."
//! * `DeploymentLifecycleHookDetail$targetType`: "The type of action the lifecycle hook
//!   performs, such as AWS_LAMBDA or PAUSE."
//! * `InvalidParameterException`: "The specified parameter isn't valid."
//! * `ServiceDeploymentNotFoundException`: "The service deploy ARN that you specified in the
//!   ContinueServiceDeployment doesn't exist. You can use ListServiceDeployments to retrieve
//!   the service deployment ARNs."
//!
//! Every case goes through the same front door a client uses, so nothing reaches into the
//! state struct and a response that is not computed from persisted state cannot pass.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, refusal, register_definition, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster every service below is created in.
const CLUSTER: &str = "demo";

/// The modelled error a malformed parameter earns: "The specified parameter isn't valid."
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The modelled error a well-formed ARN that names no stored rollout earns.
const NOT_FOUND: &str = "ServiceDeploymentNotFoundException";

/// The whole message a `hookId` outside the minted form is refused with. Fixed text: it
/// states the format and echoes nothing the request carried.
const INVALID_HOOK_ID_MESSAGE: &str = "Invalid hookId: hookId must follow the format: \
                                       ecs-{hookType}-{id}";

/// The whole message a `serviceDeploymentArn` outside the minted form is refused with, and
/// fixed text for the same reason.
const INVALID_ARN_MESSAGE: &str = "The ServiceDeployment ARN is invalid.";

/// The `DeploymentLifecycleHookStatus` of a `PAUSE` hook the rollout is waiting at.
const AWAITING_ACTION: &str = "AWAITING_ACTION";

/// The `ServiceDeploymentStatus` values a rollout that is still under way reports.
const ONGOING: [&str; 2] = ["PENDING", "IN_PROGRESS"];

/// A deployment configuration that pauses the rollout at `stage`.
fn configuration(stage: &str) -> Value {
    json!({
        "strategy": "BLUE_GREEN",
        // "You must provide this parameter when you use the BLUE_GREEN deployment
        // strategy."
        "bakeTimeInMinutes": 1,
        "lifecycleHooks": [{
            "targetType": "PAUSE",
            "lifecycleStages": [stage],
        }],
    })
}

/// Create a service that pauses its rollout at `stage`, and return the ARN of the service
/// deployment that rollout is recorded as.
async fn paused_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    stage: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
            "deploymentConfiguration": configuration(stage),
        }),
        case,
    )
    .await;
    string(&body["service"], "currentServiceDeployment", case)
}

/// The stored rollout `arn` names, as `DescribeServiceDeployments` reports it.
///
/// Shared with the sibling suites that read a rollout back through the same operation.
pub(crate) async fn describe_deployment(
    router: &MultiServiceRouter,
    arn: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [arn] }),
        case,
    )
    .await;
    list(&body, "serviceDeployments", case)
        .first()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the service deployment {arn} to be described")
        })
        .clone()
}

/// The hook of `deployment` that is waiting for an action.
///
/// Shared with the sibling suites that act on the hook a rollout stands at.
pub(crate) fn awaiting_hook(deployment: &Value, case: &str) -> Value {
    let waiting: Vec<Value> = list(deployment, "lifecycleHookDetails", case)
        .into_iter()
        .filter(|hook| hook["status"] == json!(AWAITING_ACTION))
        .collect();
    assert_eq!(
        waiting.len(),
        1,
        "case {case:?}: expected exactly one hook waiting for an action, got {deployment}"
    );
    waiting[0].clone()
}

/// Re-read the stored rollout `arn` until a `PAUSE` hook of it is waiting for an action,
/// and answer it.
///
/// The stage a rollout stops at is one the CLOCK brings it to, so the wait is for that
/// CONDITION rather than for a span: each read materialises what the clock has made due, so
/// this looks again until a hook is waiting or a generous deadline passes.
///
/// Shared with the sibling suites that wait for a rollout to reach a PAUSE hook.
pub(crate) async fn deployment_awaiting_a_hook(
    router: &MultiServiceRouter,
    arn: &str,
    case: &str,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let deployment = describe_deployment(router, arn, case).await;
        let waiting = list(&deployment, "lifecycleHookDetails", case)
            .iter()
            .any(|hook| hook["status"] == json!(AWAITING_ACTION));
        if waiting {
            return deployment;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the rollout to be waiting at a hook, got {deployment}"
        );
        tokio::task::yield_now().await;
    }
}

/// One refused `ContinueServiceDeployment`: the modelled error code and the message.
async fn refused_continue(
    router: &MultiServiceRouter,
    arn: &str,
    hook_id: &str,
    case: &str,
) -> (String, String) {
    refusal(
        router,
        "ContinueServiceDeployment",
        json!({ "serviceDeploymentArn": arn, "hookId": hook_id }),
        case,
    )
    .await
}

/// A service-deployment ARN built from its parts, as this service mints them:
/// `arn:aws:ecs:<region>:<account>:service-deployment/<cluster>/<service>/<id>`.
fn deployment_arn(resource: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:{resource}")
}

/// A deployment-id token of `width` base62 digits: the fixed-width token a
/// service-deployment ARN ends in, which is 21 digits wide for the one this service mints.
fn deployment_id(width: usize) -> String {
    "0".repeat(width)
}

/// A `hookId` whose format is right but which belongs to no hook of the rollout: the minted
/// form is `ecs-{hookType}-{id}`, and `id` is spelled as a UUID.
fn unminted_hook_id(hook_type: &str) -> String {
    format!("ecs-{hook_type}-00000000-0000-0000-0000-00000000ffff")
}

/// A `hookId` outside the `ecs-{hookType}-{id}` form is refused for its SHAPE, with the
/// fixed sentence that states the form — while a well-formed id that is not the one waiting
/// earns the state refusal, which names what the request asked for.
#[tokio::test]
async fn a_hook_id_outside_the_minted_form_is_refused_by_shape() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "hook id shapes").await;
    let definition = register_definition(&router, "hook-id-shapes", "hook id shapes").await;
    let arn = paused_service(
        &router,
        "hook-id-shapes",
        &definition,
        "POST_SCALE_UP",
        "hook id shapes",
    )
    .await;
    let deployment = deployment_awaiting_a_hook(&router, &arn, "hook id shapes").await;
    let waiting_hook_id = string(
        &awaiting_hook(&deployment, "hook id shapes"),
        "hookId",
        "hook id shapes",
    );

    // The rows differ along WHAT about the form is wrong: the leading segment, the
    // separator, an empty segment, a missing segment, the letter case of the literal
    // segment, an id segment that is not spelled as a UUID, and a bare id of the kind the
    // format wraps.
    let malformed = [
        ("no segment of the form at all", "not-a-paused-hook"),
        ("an empty hookId", ""),
        ("only the literal segment", "ecs"),
        ("the literal segment and a separator", "ecs-"),
        ("no id segment", "ecs-pause"),
        ("an empty id segment", "ecs-pause-"),
        (
            "an empty hook-type segment",
            "ecs--00000000-0000-0000-0000-000000000001",
        ),
        ("the literal segment in upper case", "ECS-pause-0001"),
        ("an id segment that is not a UUID", "ecs-p-1"),
        (
            "an id segment of a UUID's groups in the wrong widths",
            "ecs-pause-0000000-0000-0000-0000-000000000001",
        ),
        (
            "an id segment whose groups are not hexadecimal",
            "ecs-pause-0000000g-0000-0000-0000-000000000001",
        ),
        (
            "a bare id with no literal segment",
            "00000000-0000-0000-0000-000000000001",
        ),
        ("the hook type without the literal segment", "pause-0001"),
        ("underscores for the separators", "ecs_pause_0001"),
        ("another service's prefix", "lambda-pause-0001"),
    ];
    for (case, hook_id) in malformed {
        let (code, message) = refused_continue(&router, &arn, hook_id, case).await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected a {INVALID_PARAMETER}, got {code} with {message:?}"
        );
        assert_eq!(
            message, INVALID_HOOK_ID_MESSAGE,
            "case {case:?}: expected the format refusal, got {message:?}"
        );
    }

    // A hookId of the right FORM that names no waiting hook is a state condition, not a
    // syntax one: it earns the refusal that names the hook and the rollout it was asked
    // for. The hook type varies with the row, because the form fixes only that there IS
    // one.
    for (case, hook_id) in [
        ("a well-formed id of a PAUSE hook", unminted_hook_id("pause")),
        (
            "a well-formed id of another hook type",
            unminted_hook_id("aws_lambda"),
        ),
        (
            "a well-formed id spelled in upper-case hexadecimal",
            "ecs-pause-ABCDEF00-0000-0000-0000-0000000000FF".to_string(),
        ),
    ] {
        let (code, message) = refused_continue(&router, &arn, &hook_id, case).await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected a {INVALID_PARAMETER}, got {code} with {message:?}"
        );
        assert_ne!(
            message, INVALID_HOOK_ID_MESSAGE,
            "case {case:?}: a well-formed hookId is not a format refusal, got {message:?}"
        );
        assert!(
            message.contains(&hook_id) && message.contains(&arn),
            "case {case:?}: expected the refusal to name {hook_id} and {arn}, got {message:?}"
        );
    }

    // Nothing any refusal touched moved: the same hook is still waiting at the same stage,
    // and the rollout is still under way.
    let case = "after the refusals";
    let deployment = describe_deployment(&router, &arn, case).await;
    let hook = awaiting_hook(&deployment, case);
    assert_eq!(
        string(&hook, "hookId", case),
        waiting_hook_id,
        "case {case:?}: expected the same hook to be waiting still, got {deployment}"
    );
    let status = string(&deployment, "status", case);
    assert!(
        ONGOING.contains(&status.as_str()),
        "case {case:?}: expected a refused request to leave the rollout ongoing, got {status}"
    );

    // And the id that rollout reports is one this operation ACCEPTS, so the form the
    // refusal states is the form the service hands out.
    let answered = ok(
        &router,
        "ContinueServiceDeployment",
        json!({ "serviceDeploymentArn": arn, "hookId": waiting_hook_id }),
        case,
    )
    .await;
    assert_eq!(
        string(&answered, "serviceDeploymentArn", case),
        arn,
        "case {case:?}: expected the continued rollout's ARN back, got {answered}"
    );
}

/// The `hookId` a rollout reports is minted in the form the refusal states: the literal
/// segment, the hook's own target type, and an id of its own.
#[tokio::test]
async fn a_reported_hook_id_carries_the_hook_type_of_the_hook_it_names() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "minted form").await;
    let definition = register_definition(&router, "minted-form", "minted form").await;

    // One row per stage a hook may be configured for, so the id's form is asserted on more
    // than the one stage the recorded case used.
    for (index, stage) in ["PRE_SCALE_UP", "POST_SCALE_UP", "PRODUCTION_TRAFFIC_SHIFT"]
        .into_iter()
        .enumerate()
    {
        let case = stage;
        let arn = paused_service(
            &router,
            &format!("minted-form-{index}"),
            &definition,
            stage,
            case,
        )
        .await;
        let deployment = deployment_awaiting_a_hook(&router, &arn, case).await;
        let hook = awaiting_hook(&deployment, case);
        let hook_id = string(&hook, "hookId", case);
        let target_type = string(&hook, "targetType", case);
        let expected_prefix = format!("ecs-{}-", target_type.to_ascii_lowercase());
        assert!(
            hook_id.starts_with(&expected_prefix),
            "case {case:?}: expected a hookId opening with {expected_prefix:?}, got {hook_id:?}"
        );
        assert!(
            hook_id.len() > expected_prefix.len(),
            "case {case:?}: expected a hookId carrying an id of its own, got {hook_id:?}"
        );
        // The same id, read back through the operation that acts on it.
        let answered = ok(
            &router,
            "ContinueServiceDeployment",
            json!({ "serviceDeploymentArn": arn, "hookId": hook_id }),
            case,
        )
        .await;
        assert_eq!(
            string(&answered, "serviceDeploymentArn", case),
            arn,
            "case {case:?}: expected the hookId a describe reports to be accepted, got {answered}"
        );
    }
}

/// A `serviceDeploymentArn` outside the form this service mints is refused for its SHAPE,
/// before the rollout it would name is looked up — while a well-formed ARN that names no
/// stored rollout earns the not-found refusal.
#[tokio::test]
async fn a_service_deployment_arn_outside_the_minted_form_is_refused_by_shape() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "arn shapes").await;
    let definition = register_definition(&router, "arn-shapes", "arn shapes").await;
    let arn = paused_service(
        &router,
        "arn-shapes",
        &definition,
        "PRE_SCALE_UP",
        "arn shapes",
    )
    .await;
    let deployment = deployment_awaiting_a_hook(&router, &arn, "arn shapes").await;
    let waiting_hook_id =
        string(&awaiting_hook(&deployment, "arn shapes"), "hookId", "arn shapes");
    let minted_width = arn
        .rsplit_once('/')
        .map(|(_, id)| id.len())
        .unwrap_or_default();

    // The rows differ along WHAT about the ARN's shape is wrong: the ARN frame itself, the
    // namespace, the resource type, the number of resource segments, the names those
    // segments carry, and the width and alphabet of the deployment-id token — the token's
    // width is a bound, so it is tested one digit either side of the minted width too.
    let malformed = [
        ("not an ARN at all", arn.replace("arn:aws:", "")),
        ("a plain string", "not-an-arn".to_string()),
        (
            "another service's namespace",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width)
            ))
            .replace(":ecs:", ":ec2:"),
        ),
        (
            "another resource type",
            deployment_arn(&format!(
                "service-revision/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a resource type the segment only starts with",
            deployment_arn(&format!(
                "service-deployments/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "no deployment-id segment",
            deployment_arn(&format!("service-deployment/{CLUSTER}/arn-shapes")),
        ),
        (
            "a segment too many",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}/extra",
                deployment_id(minted_width)
            )),
        ),
        (
            "an empty service segment",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}//{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a cluster name no cluster may carry",
            deployment_arn(&format!(
                "service-deployment/not a cluster/arn-shapes/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a deployment-id token one digit short",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width - 1)
            )),
        ),
        (
            "a deployment-id token one digit long",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width + 1)
            )),
        ),
        (
            "a deployment-id token twice the width",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width * 2)
            )),
        ),
        (
            "a deployment-id token that is not a token",
            format!("{arn}-absent"),
        ),
        (
            "a deployment-id token with a character outside the alphabet",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/-{}",
                deployment_id(minted_width - 1)
            )),
        ),
    ];
    for (case, bad_arn) in &malformed {
        let (code, message) = refused_continue(&router, bad_arn, &waiting_hook_id, case).await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected a {INVALID_PARAMETER} for {bad_arn}, got {code} with \
             {message:?}"
        );
        assert_eq!(
            message, INVALID_ARN_MESSAGE,
            "case {case:?}: expected the ARN format refusal for {bad_arn}, got {message:?}"
        );
    }

    // The ARN is checked FIRST: a request whose ARN and whose hookId are both malformed is
    // refused for the ARN, which is what says which rollout a hook would belong to.
    let case = "a malformed ARN beside a malformed hookId";
    let (code, message) =
        refused_continue(&router, &format!("{arn}-absent"), "not-a-paused-hook", case).await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "case {case:?}: expected a {INVALID_PARAMETER}, got {code} with {message:?}"
    );
    assert_eq!(
        message, INVALID_ARN_MESSAGE,
        "case {case:?}: expected the ARN format refusal to come first, got {message:?}"
    );

    // A WELL-FORMED ARN is looked up: one that names nothing stored is not found, whichever
    // part of its identity is the one nothing holds.
    let unknown = [
        (
            "a deployment-id token nothing was minted under",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-shapes/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a service nothing holds",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/never-created/{}",
                deployment_id(minted_width)
            )),
        ),
        ("another Region", arn.replace(SIGNED_REGION, "eu-west-1")),
    ];
    for (case, unknown_arn) in &unknown {
        let (code, message) = refused_continue(&router, unknown_arn, &waiting_hook_id, case).await;
        assert_eq!(
            code, NOT_FOUND,
            "case {case:?}: expected a {NOT_FOUND} for {unknown_arn}, got {code} with {message:?}"
        );
        assert!(
            message.contains(unknown_arn),
            "case {case:?}: expected the refusal to name {unknown_arn}, got {message:?}"
        );
    }

    // Nothing any of those refusals touched moved.
    let case = "after the ARN refusals";
    let deployment = describe_deployment(&router, &arn, case).await;
    assert_eq!(
        string(&awaiting_hook(&deployment, case), "hookId", case),
        waiting_hook_id,
        "case {case:?}: expected the same hook to be waiting still, got {deployment}"
    );
    let status = string(&deployment, "status", case);
    assert!(
        ONGOING.contains(&status.as_str()),
        "case {case:?}: expected a refused request to leave the rollout ongoing, got {status}"
    );
}
