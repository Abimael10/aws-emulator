// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON ECS refused reports on its `CREATE_FAILED` event, end to end over
//! the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception it raised for
//! it — the plugin's wording of the class the refusal falls in, around the owning service's own
//! sentence followed by that SDK's diagnostic of the call — so the status and the error CODE
//! the owning service answered are readable out of the resource's `ResourceStatusReason`.**
//! Amazon ECS's SDK words that diagnostic as `(Service: AmazonECS; Status Code: <status>; Error
//! Code: <code>; Request ID: <id>; Proxy: null)`, and the task definition's provider re-raises
//! a refusal of the REQUEST as the plugin's invalid-request exception, naming the call it was
//! making: `Invalid request provided: Create TaskDefinition: <the SDK exception>`. Which
//! refusal the registration earned does not change either part.
//!
//! Why it matters: tooling and operators parse the service, the status and the error code out
//! of `ResourceStatusReason` to decide whether a rollback was a client error worth
//! re-templating or a throttle or 5xx worth retrying, and log matchers key on the owning
//! service's own wording. A reason carrying only free text leaves no service, no status and no
//! code to branch on, so every failed registration reads the same.
//!
//! The rows differ along what the rule generalises over: WHICH refusal the registration earned
//! — a task `cpu` Fargate publishes no size for, a `cpu`/`memory` pair outside the published
//! catalog, a network mode Fargate does not run, a task size member left undeclared, and an
//! ephemeral-storage size outside the range the member admits — and so along the sentence the
//! service worded, while the envelope and the class are the same for every one of them. The
//! `Proxy` field is the same for every row because every one of these calls is made directly,
//! with no proxy between.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::harness::{
    create_stack, envelope_parts, failed_resource_reason, is_a_request_id, router,
    settled_stack_status, REGION,
};

/// The SDK's own name for Amazon ECS, as the exception its SDK raises states it.
const ECS_SERVICE: &str = "AmazonECS";

/// The modelled error a registration this service refuses answers, and the status it carries
/// (`ClientException`, `@httpError(400)`).
const CLIENT_EXCEPTION: &str = "ClientException";
const REFUSED_STATUS: u16 = 400;

/// What the diagnostic states for a call made with no proxy configured, which every call this
/// emulator makes in process is.
const DIRECT_CALL_PROXY: &str = "Proxy: null";

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, which is also the class
/// whose exception the provider words.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The wording the resource-provider plugin puts on that class, and the call this type's
/// provider names inside it.
const INVALID_REQUEST_PROVIDED: &str = "Invalid request provided: ";
const CREATE_TASK_DEFINITION: &str = "Create TaskDefinition: ";

/// The statuses a failed resource and the stack it failed settle in.
const CREATE_FAILED: &str = "CREATE_FAILED";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// One row: the stack to deploy, the declaration Amazon ECS refuses, and the sentence that
/// service worded for it.
struct Row {
    case: &'static str,
    stack: &'static str,
    family: &'static str,
    /// The `AWS::ECS::TaskDefinition` properties, as YAML lines indented six spaces.
    properties: &'static str,
    /// The owning service's own sentence, which the quoted message must carry verbatim.
    sentence: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a task cpu Fargate publishes no task size for",
        stack: "ecs-td-reason-invalid-cpu",
        family: "ecs-td-reason-invalid-cpu-family",
        properties: "      Cpu: '100'\n      Memory: '512'\n",
        sentence: "Invalid 'cpu' setting for task. For more information, see the \
                   Troubleshooting section of the Amazon ECS Developer Guide.",
    },
    Row {
        case: "a pair outside the published catalog",
        stack: "ecs-td-reason-unpaired-size",
        family: "ecs-td-reason-unpaired-size-family",
        properties: "      Cpu: '256'\n      Memory: '4096'\n",
        sentence: "No Fargate configuration exists for given values: 256 CPU, 4096 memory. \
                   See the Amazon ECS documentation for the valid values.",
    },
    Row {
        case: "a network mode Fargate does not run",
        stack: "ecs-td-reason-network-mode",
        family: "ecs-td-reason-network-mode-family",
        properties: "      Cpu: '256'\n      Memory: '512'\n      NetworkMode: bridge\n",
        sentence: "Fargate only supports network mode \u{2018}awsvpc\u{2019}.",
    },
    Row {
        case: "a task size member left undeclared",
        stack: "ecs-td-reason-no-memory",
        family: "ecs-td-reason-no-memory-family",
        properties: "      Cpu: '256'\n",
        sentence: "Fargate requires that 'memory' be defined at the task level.",
    },
    Row {
        case: "an ephemeral storage size outside the range the member admits",
        stack: "ecs-td-reason-ephemeral",
        family: "ecs-td-reason-ephemeral-family",
        properties: "      Cpu: '256'\n      Memory: '512'\n      EphemeralStorage:\n        \
                     SizeInGiB: '9'\n",
        sentence: "EphemeralStorage size should be at least 21",
    },
];

/// The image every row's container runs.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The row's template: a Fargate revision with one container, carrying the row's own
/// properties. `NetworkMode` is declared by the row that is about it, so the others take the
/// mode Fargate requires.
fn template(row: &Row) -> String {
    let network_mode = match row.properties.contains("NetworkMode") {
        true => String::new(),
        false => "      NetworkMode: awsvpc\n".to_string(),
    };
    format!(
        "Resources:\n  TaskDef:\n    Type: AWS::ECS::TaskDefinition\n    Properties:\n      \
         Family: {family}\n      RequiresCompatibilities:\n        - FARGATE\n{network_mode}\
         {properties}      ContainerDefinitions:\n        - Name: app\n          Image: \
         {IMAGE}\n          Essential: 'true'\n",
        family = row.family,
        properties = row.properties,
    )
}

/// The owning service's own sentence and the diagnostic its SDK appended, split apart.
fn sentence_and_diagnostic(case: &str, quoted: &str) -> (String, String) {
    const OPENS: &str = " (Service: ";
    let (sentence, diagnostic) = quoted.rsplit_once(OPENS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic after the service's own sentence, got \
             {quoted:?}"
        )
    });
    let envelope = diagnostic.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the diagnostic to close, got {quoted:?}")
    });
    (sentence.to_string(), envelope.to_string())
}

/// Hold one diagnostic to the shape Amazon ECS's SDK words: the service, the status, the error
/// code, the id of the call and the proxy it was made through — and nothing else. Answers the
/// request id it stated, so a caller can hold THAT to its own form.
fn assert_diagnostic(case: &str, envelope: &str) -> String {
    let fields: Vec<&str> = envelope.split("; ").collect();
    assert_eq!(
        fields.len(),
        5,
        "case {case:?}: expected the service, the status, the error code, the call's id and the \
         proxy and nothing else, got {envelope:?}"
    );
    assert_eq!(
        fields[0], ECS_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_STATUS}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    assert_eq!(
        fields[2],
        format!("Error Code: {CLIENT_EXCEPTION}"),
        "case {case:?}: expected the error code the owning service answered, got {:?}",
        fields[2]
    );
    let request_id = fields[3]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[3]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
    );
    assert_eq!(
        fields[4], DIRECT_CALL_PROXY,
        "case {case:?}: expected the proxy of a direct call, got {:?}",
        fields[4]
    );
    request_id.to_string()
}

/// Deploy one row's stack and answer the quoted provider message, the token of the resource
/// request and the class the failure was filed under.
async fn refusal_of(
    router: &MultiServiceRouter,
    row: &Row,
    stack: &str,
) -> (String, String, String) {
    let (status, body) = create_stack(router, stack, &template(row), &[]).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from CreateStack, got {status} with {body}",
        row.case
    );
    let reason =
        failed_resource_reason(row.case, router, REGION, stack, "TaskDef", CREATE_FAILED).await;
    envelope_parts(row.case, &reason)
}

/// The revisions Amazon ECS holds under `family`, which a refused registration leaves empty.
async fn registered_revisions(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
) -> Vec<String> {
    let (code, listed) = ecs(
        router,
        "ListTaskDefinitions",
        &json!({"familyPrefix": family}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from ListTaskDefinitions, got {code} with {listed}"
    );
    listed["taskDefinitionArns"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_task_definition_ecs_refused_carries_that_service_s_status_and_error_code() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let (quoted, token, class) = refusal_of(&router, row, row.stack).await;

        // The provider's own wording of the class, around the exception its SDK raised.
        let exception = quoted
            .strip_prefix(INVALID_REQUEST_PROVIDED)
            .and_then(|rest| rest.strip_prefix(CREATE_TASK_DEFINITION))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the quoted message to open with \
                     {INVALID_REQUEST_PROVIDED:?}{CREATE_TASK_DEFINITION:?}, got {quoted:?}"
                )
            });
        let (sentence, envelope) = sentence_and_diagnostic(case, exception);
        assert_eq!(
            sentence, row.sentence,
            "case {case:?}: expected the owning service's own sentence, got {sentence:?}"
        );
        assert_diagnostic(case, &envelope);
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?}"
        );
        assert!(
            is_a_request_id(&token),
            "case {case:?}: expected the resource request's own token, got {token:?}"
        );

        // The registration that was refused registered nothing, and the stack says so.
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll back, got {settled:?}"
        );
        let held = registered_revisions(&router, case, row.family).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected no revision registered for {}, got {held:?}",
            row.family
        );
    }
}

#[tokio::test]
async fn the_diagnostic_states_each_refused_call_s_own_request_id() {
    // The id in the diagnostic identifies the CALL, so two deployments refused the same way
    // state two different ids: an id frozen into the wording would identify nothing.
    let case = "two stacks declaring the same task size Fargate offers nothing for";
    let row = &ROWS[0];
    let router = router();

    let mut ids = Vec::new();
    for stack in ["ecs-td-reason-ids-first", "ecs-td-reason-ids-second"] {
        let (quoted, _, _) = refusal_of(&router, row, stack).await;
        let (_, envelope) = sentence_and_diagnostic(case, &quoted);
        ids.push(assert_diagnostic(case, &envelope));
    }
    assert_ne!(
        ids[0], ids[1],
        "case {case:?}: expected each refused call's own Request ID, got {ids:?}"
    );
}
