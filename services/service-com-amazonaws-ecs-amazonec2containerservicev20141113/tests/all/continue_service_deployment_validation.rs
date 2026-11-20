// SPDX-License-Identifier: CC0-1.0

//! `ContinueServiceDeployment`'s INPUT VALIDATION, driven end to end over the shared wire
//! harness in [`crate::ecs_wire`].
//!
//! The rule under test: the `serviceDeploymentArn` is checked against the FORM Amazon ECS
//! mints it in before any state is consulted, and a value outside that form is refused
//! `InvalidParameterException` with the fixed sentence that states the form. The rollout
//! that ARN names is then resolved — a `hookId` names a hook OF one service deployment, so
//! a well-formed ARN that names no stored rollout is reported as that (`ClientException`)
//! whatever the `hookId` beside it spells — and only then is the `hookId` read, for its own
//! form first and then against the hooks that rollout is waiting at.
//!
//! The same two readings of the ARN are asserted here for the SIBLING operations that
//! address a service deployment by ARN (`DescribeServiceDeployments` and
//! `StopServiceDeployment`), so one ARN cannot be malformed for one of them and merely
//! absent for another — and so that a caller can tell "I built the ARN wrong" from "the
//! deployment is gone".
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
//! * `ClientException`: "These errors are usually caused by a client action. ... it might be
//!   specifying an identifier that isn't valid."
//! * `DescribeServiceDeployments$failures`: "Any failures associated with the call ... the
//!   reason set to MISSING."
//! * `StopServiceDeployment$serviceDeploymentArn`: "The ARN of the service deployment that
//!   you want to stop."
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
const NOT_FOUND: &str = "ClientException";

/// The whole message `ContinueServiceDeployment` carries for that condition: one fixed
/// sentence, echoing neither the ARN nor the service the request named.
const DOES_NOT_EXIST_MESSAGE: &str = "The service deployment does not exist.";

/// The `Failure$reason` for an identifier that names nothing: "MISSING — the described
/// resource could not be found".
const MISSING: &str = "MISSING";

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
        assert_eq!(
            message, DOES_NOT_EXIST_MESSAGE,
            "case {case:?}: expected the not-found refusal for {unknown_arn}, got {message:?}"
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

/// A `hookId` of the minted form that belongs to no hook of any rollout, so a row about the
/// ARN is never refused for the `hookId` instead.
const UNMINTED_HOOK_ID: &str = "ecs-pause-00000000-0000-0000-0000-0000000000ff";

/// The operations that address ONE service deployment by its ARN.
const DEPLOYMENT_OPERATIONS: [&str; 3] = [
    "DescribeServiceDeployments",
    "ContinueServiceDeployment",
    "StopServiceDeployment",
];

/// The request each of [`DEPLOYMENT_OPERATIONS`] sends for `arn`, beside the members that
/// operation requires.
fn deployment_request(operation: &str, arn: &str) -> Value {
    match operation {
        "DescribeServiceDeployments" => json!({ "serviceDeploymentArns": [arn] }),
        "ContinueServiceDeployment" => {
            json!({ "serviceDeploymentArn": arn, "hookId": UNMINTED_HOOK_ID })
        }
        _ => json!({ "serviceDeploymentArn": arn, "stopType": "ROLLBACK" }),
    }
}

/// Every operation that takes a `serviceDeploymentArn` reads it against the SAME form, and
/// refuses a value outside that form with the same sentence — before the cluster the ARN
/// names, or the rollout it would name, is resolved.
#[tokio::test]
async fn every_operation_that_takes_a_service_deployment_arn_reads_the_same_form() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register_definition(&router, "arn-forms", case).await;
    let stored = paused_service(&router, "arn-forms", &definition, "PRE_SCALE_UP", case).await;
    let minted_width = stored
        .rsplit_once('/')
        .map(|(_, id)| id.len())
        .unwrap_or_default();
    assert!(
        minted_width > 0,
        "case {case:?}: expected the stored ARN {stored} to end in a deployment-id token"
    );

    // The rows differ along WHAT about the form is wrong: the ARN frame itself, the vendor,
    // the resource type, the number of resource segments, the names those segments carry,
    // and the width and the alphabet of the deployment-id token — the token's width is a
    // bound, so it is tested one digit either side of the minted width as well as far from
    // it.
    let malformed: Vec<(&str, String)> = vec![
        ("not an ARN at all", "not-an-arn".to_string()),
        ("an empty value", String::new()),
        (
            "the resource field with no ARN frame",
            format!(
                "service-deployment/{CLUSTER}/arn-forms/{}",
                deployment_id(minted_width)
            ),
        ),
        ("another vendor's namespace", stored.replace(":ecs:", ":ec2:")),
        (
            "another resource type",
            deployment_arn(&format!(
                "service-revision/{CLUSTER}/arn-forms/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a resource type the segment only starts with",
            deployment_arn(&format!(
                "service-deployments/{CLUSTER}/arn-forms/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "no deployment-id segment",
            deployment_arn(&format!("service-deployment/{CLUSTER}/arn-forms")),
        ),
        (
            "the cluster segment alone",
            deployment_arn(&format!("service-deployment/{CLUSTER}")),
        ),
        ("a segment too many", format!("{stored}/extra")),
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
                "service-deployment/not a cluster/arn-forms/{}",
                deployment_id(minted_width)
            )),
        ),
        (
            "a deployment-id token one digit short",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-forms/{}",
                deployment_id(minted_width - 1)
            )),
        ),
        (
            "a deployment-id token one digit long",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-forms/{}",
                deployment_id(minted_width + 1)
            )),
        ),
        (
            "a deployment-id token spelled as a UUID",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-forms/11111111-2222-3333-4444-555555555555"
            )),
        ),
        (
            "a deployment-id token with a character outside the alphabet",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/arn-forms/-{}",
                deployment_id(minted_width - 1)
            )),
        ),
    ];

    for (row, bad_arn) in &malformed {
        for operation in DEPLOYMENT_OPERATIONS {
            let case = format!("{operation}: {row}");
            let (code, message) =
                refusal(&router, operation, deployment_request(operation, bad_arn), &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (INVALID_PARAMETER, INVALID_ARN_MESSAGE),
                "case {case:?}: expected the shape refusal for {bad_arn:?}, got {code} with \
                 {message:?}"
            );
        }
    }

    // The MINTED ARN gets past that rule at all three: the read answers the rollout it
    // names, and the two calls that act on one answer the state they find it in — which is
    // never the shape refusal.
    let case = "the stored rollout, described";
    let described: Vec<String> = list(
        &ok(
            &router,
            "DescribeServiceDeployments",
            deployment_request("DescribeServiceDeployments", &stored),
            case,
        )
        .await,
        "serviceDeployments",
        case,
    )
    .iter()
    .map(|deployment| string(deployment, "serviceDeploymentArn", case))
    .collect();
    assert_eq!(
        described,
        vec![stored.clone()],
        "case {case:?}: expected the stored rollout back, got {described:?}"
    );
    for operation in ["ContinueServiceDeployment", "StopServiceDeployment"] {
        let case = format!("{operation}: the stored rollout");
        let (status, body) = crate::ecs_wire::answered(
            &router,
            operation,
            deployment_request(operation, &stored),
        )
        .await;
        assert_ne!(
            body["message"].as_str(),
            Some(INVALID_ARN_MESSAGE),
            "case {case:?}: a minted ARN is not a shape refusal, got {status} with {body}"
        );
    }
}

/// A WELL-FORMED `serviceDeploymentArn` that names no stored rollout is an identity nothing
/// holds, not a malformed value: the batch read reports it as a failure of its own entry,
/// and each call that acts on one rollout refuses it `ClientException` with the sentence
/// that call words the condition in.
#[tokio::test]
async fn a_well_formed_arn_that_names_no_rollout_is_not_a_syntax_refusal() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    // A second cluster this account HOLDS, so a row can name a rollout under a cluster that
    // exists and holds none.
    cluster_with_capacity(&router, "elsewhere", case).await;
    let definition = register_definition(&router, "absent-arns", case).await;
    let stored = paused_service(&router, "absent-arns", &definition, "PRE_SCALE_UP", case).await;
    let (_, minted_id) = stored
        .rsplit_once('/')
        .expect("a service-deployment ARN ends in its deployment-id token");

    // The rows differ along WHICH part of the identity names nothing: the deployment id
    // (in either spelling of the minted alphabet), the service, the cluster (one that
    // exists), and the Region the ARN was minted in.
    let absent: Vec<(&str, String)> = vec![
        (
            "a deployment id nothing was minted under",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/absent-arns/{}",
                deployment_id(minted_id.len())
            )),
        ),
        (
            "a deployment id of letters, nothing minted",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/absent-arns/{}",
                "z".repeat(minted_id.len())
            )),
        ),
        (
            "a service that was never created",
            deployment_arn(&format!(
                "service-deployment/{CLUSTER}/never-created/{minted_id}"
            )),
        ),
        (
            "another cluster this account holds",
            deployment_arn(&format!(
                "service-deployment/elsewhere/absent-arns/{minted_id}"
            )),
        ),
        ("another Region", stored.replace(SIGNED_REGION, "eu-west-1")),
    ];

    for (row, absent_arn) in &absent {
        // The batch READ answers per entry: the entry that resolved to nothing is a
        // `MISSING` failure naming the ARN the caller sent, and nothing is described.
        let case = format!("DescribeServiceDeployments: {row}");
        let body = ok(
            &router,
            "DescribeServiceDeployments",
            deployment_request("DescribeServiceDeployments", absent_arn),
            &case,
        )
        .await;
        let reported: Vec<(String, String)> = list(&body, "failures", &case)
            .iter()
            .map(|failure| {
                (
                    string(failure, "arn", &case),
                    string(failure, "reason", &case),
                )
            })
            .collect();
        assert_eq!(
            reported,
            vec![(absent_arn.clone(), MISSING.to_string())],
            "case {case:?}: expected one MISSING failure naming {absent_arn}, got {reported:?}"
        );
        assert!(
            list(&body, "serviceDeployments", &case).is_empty(),
            "case {case:?}: expected nothing described, got {body}"
        );

        // The call that ACTS on a paused rollout states the condition in one fixed
        // sentence, which carries none of the request's values.
        let case = format!("ContinueServiceDeployment: {row}");
        let (code, message) = refusal(
            &router,
            "ContinueServiceDeployment",
            deployment_request("ContinueServiceDeployment", absent_arn),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (NOT_FOUND, DOES_NOT_EXIST_MESSAGE),
            "case {case:?}: expected the not-found refusal for {absent_arn}, got {code} with \
             {message:?}"
        );

        // The call that STOPS one words the same condition its own way, naming the ARN the
        // request carried.
        let case = format!("StopServiceDeployment: {row}");
        let (code, message) = refusal(
            &router,
            "StopServiceDeployment",
            deployment_request("StopServiceDeployment", absent_arn),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                NOT_FOUND,
                format!("Service deployment: {absent_arn}, not found").as_str()
            ),
            "case {case:?}: expected the not-found refusal for {absent_arn}, got {code} with \
             {message:?}"
        );

        // A `hookId` names a hook OF one rollout, so the rollout is resolved before the
        // hook id is read at all: the SAME refusal stands whatever the hookId spells.
        let case = format!("ContinueServiceDeployment with a malformed hookId: {row}");
        let (code, message) = refusal(
            &router,
            "ContinueServiceDeployment",
            json!({ "serviceDeploymentArn": absent_arn, "hookId": "not-a-paused-hook" }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (NOT_FOUND, DOES_NOT_EXIST_MESSAGE),
            "case {case:?}: expected the rollout to be resolved before the hookId, got \
             {code} with {message:?}"
        );
    }

    // A well-formed ARN under a cluster this account holds NO record of leaves the batch
    // read nothing to report on at all, which is the condition it answers instead.
    let case = "an ARN under a cluster that was never created";
    let (code, message) = refusal(
        &router,
        "DescribeServiceDeployments",
        deployment_request(
            "DescribeServiceDeployments",
            &deployment_arn(&format!(
                "service-deployment/never-created/absent-arns/{minted_id}"
            )),
        ),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("ClusterNotFoundException", "Cluster not found."),
        "case {case:?}: expected the missing-cluster refusal, got {code} with {message:?}"
    );
}
