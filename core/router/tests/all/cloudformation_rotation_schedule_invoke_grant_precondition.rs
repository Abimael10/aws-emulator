// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack's `AWS::SecretsManager::RotationSchedule` does to the secret it
//! names, and what has to be true of the rotation function before it is created at all.
//!
//! Two rules, both read through the owning services rather than through CloudFormation's own
//! bookkeeping:
//!
//! * **A schedule a stack created IS the secret's rotation configuration.** The resource is
//!   `RotateSecret` ("configures and starts the asynchronous process of rotating the secret"),
//!   so once it is created `DescribeSecret` reports the secret's rotation as on, through the
//!   function and the rules the declaration named; and a later deployment that drops the
//!   resource is `CancelRotateSecret` ("turns off automatic rotation"), so the same read then
//!   reports rotation configured and off while the secret itself — a resource of its own —
//!   stays.
//! * **A rotation Secrets Manager could not perform is never reported configured.** That
//!   service invokes the rotation function as its own service principal and needs
//!   `lambda:InvokeFunction` on it ("you also need lambda:InvokeFunction permissions on the
//!   rotation function", `RotateSecret`, that service's Smithy model), judged against the
//!   FUNCTION's own resource policy for the secret being rotated — so a declaration whose
//!   function's policy does not admit that invocation is REFUSED, which takes the resource to
//!   `CREATE_FAILED` with that service's own refusal and the stack to `ROLLBACK_COMPLETE`
//!   rather than publishing a schedule nothing can rotate through.
//!
//! The rows differ along what the second rule generalises over — the ways a function's policy
//! fails to admit the invocation: no grant at all, a grant to another service principal, a
//! grant of another action, and a grant of the invoke to this service but scoped to a
//! DIFFERENT secret. The grant that admits the rotation is a row of the same table, so a
//! template broken in some unrelated way cannot make the refusals look right.
//!
//! Four crates have to agree for either rule to hold (CloudFormation makes the calls, Secrets
//! Manager holds the configuration and reads the function's policy, Lambda holds that policy
//! and the function, IAM answers for the function's execution role), so the suite lives here
//! rather than in any one of them.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::stabilising::CREATE_STABILISATION_WINDOW;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    ABANDON_GRACE, HOST_WAIT_CEILING, INIT_PHASE_LIMIT, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources live
/// in.
const REGION: &str = "us-west-2";

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", the refusal
/// below).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// A service principal that is NOT the one a rotation is invoked as, for the row whose grant
/// admits somebody else.
const ANOTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// The action a rotation needs on the function: "you also need `lambda:InvokeFunction`
/// permissions on the rotation function" (`RotateSecret`, the Secrets Manager Smithy model).
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action of Lambda's that is not the invocation, for the row whose grant admits this
/// service to something else ("Function name – `my-function`. Function ARN – …",
/// `smithy_query(query="operation",operation="AddPermission")`, whose `Action` is the action
/// being granted).
const ANOTHER_ACTION: &str = "lambda:GetFunction";

/// What Secrets Manager answers for a rotation function its own invocation is not admitted on
/// — fixed text, naming the principal whose grant the policy has to carry and nothing of the
/// request.
const INVOKE_REFUSAL: &str = "Secrets Manager cannot invoke the specified Lambda function. \
                              Ensure that the function policy grants access to the principal \
                              secretsmanager.amazonaws.com.";

/// The `HandlerErrorCode` class CloudFormation files a refused resource under when the owning
/// service refused the call for want of a permission.
const HANDLER_ACCESS_DENIED: &str = "HandlerErrorCode: AccessDenied";

/// The resource status of a resource a stack could not create.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The rotation interval every row's schedule declares, in days — inside the range that member
/// publishes (`RotationRulesType$AutomaticallyAfterDays`, @range 1..1000, the Secrets Manager
/// Smithy model).
const ROTATION_INTERVAL_DAYS: u64 = 30;

/// The value the stack's secret is created with.
const INITIAL_VALUE: &str = "{\"username\":\"appuser\",\"password\":\"initial\"}";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is which policies admit a rotation
/// and not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How often this suite looks again for a condition another service owes.
const POLL: Duration = Duration::from_millis(50);

/// What Lambda allows the ONE invocation a schedule's create makes — the test of the
/// configuration it applied: its wait for capacity, the initialization of the execution
/// environment, the longest the run itself is waited for and the grace past that
/// (`service_com_amazonaws_lambda_awsgirapiservice::state::execution`).
const INVOCATION_CEILING: Duration = PERMIT_WAIT
    .saturating_add(INIT_PHASE_LIMIT)
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(ABANDON_GRACE);

/// How long a stack operation of this suite is waited for. DERIVED rather than picked, because
/// a picked span is a bet on how fast the host runs: CloudFormation itself holds a resource
/// open for its own stabilisation window
/// (`service_com_amazonaws_cloudformation_cloudformation::state::stabilising`), and the one
/// invocation a schedule's create makes may spend Lambda's published bounds on top of it. It
/// bounds the loops only so they cannot hang.
const STACK_CEILING: Duration = CREATE_STABILISATION_WINDOW.saturating_add(INVOCATION_CEILING);

/// See [`POLL`]: the ceiling in whole polls, which is what the loops count.
const POLLS: usize = STACK_CEILING.as_millis() as usize / POLL.as_millis() as usize;

/// The assembled emulator every request of this suite is dispatched into, so the policy Lambda
/// holds for the stack's function is the policy Secrets Manager reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one AWS Secrets Manager operation over its awsJson1_1 wire, requiring the 200 it
/// answers.
async fn describe_secret(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "secretsmanager.DescribeSecret")
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(json!({ "SecretId": secret_id }).to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSecret, got {status} with body {payload}"
    );
    payload
}

/// What `GetSecretValue` answers for the version in use, so a secret that outlived a stack
/// update is read as a client reads it.
async fn current_value(router: &MultiServiceRouter, case: &str, secret_id: &str) -> String {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "secretsmanager.GetSecretValue")
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(
            json!({ "SecretId": secret_id, "VersionStage": "AWSCURRENT" }).to_string(),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    payload["SecretString"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no SecretString in {payload}"))
        .to_string()
}

/// Poll `stack` until CloudFormation first reports `wanted`, and answer the last status it
/// reported with the `DescribeStacks` answer that status was read from.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = (reported, body);
        if last.0 == wanted {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// Every `ResourceStatusReason` the stack has recorded, so a deploy that did not reach the
/// status a rule states reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The reason the stack recorded against `logical_id` for the `CREATE_FAILED` of that resource,
/// read from the one event carrying both — never by zipping the two element lists, which a
/// member omitting either would misalign.
async fn create_failure_of(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Option<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&events).into_iter().find_map(|event| {
        let failed = elements(event, "ResourceStatus")
            .into_iter()
            .next()
            .is_some_and(|status| status == RESOURCE_CREATE_FAILED);
        let named = elements(event, "LogicalResourceId")
            .into_iter()
            .next()
            .is_some_and(|named| named == logical_id);
        (failed && named)
            .then(|| elements(event, "ResourceStatusReason").into_iter().next())
            .flatten()
    })
}

/// The rotation function every row deploys. The rule under test is which POLICIES admit an
/// invocation of it, so the handler does no rotation work of its own: it answers the step it
/// was called with, which is all the one invocation a schedule's create makes — the test of the
/// configuration it applied — needs of it.
const ROTATION_FUNCTION: &str = r#"
def lambda_handler(event, context):
    return {"step": event.get("Step")}
"#;

/// One row of the table: the `AWS::Lambda::Permission` the stack declares on the rotation
/// function, and whether that grant admits a rotation of the stack's own secret.
struct Row {
    case: &'static str,
    /// The properties of the grant the template declares, or `None` for a template that
    /// declares no grant at all.
    grant: Option<fn() -> Value>,
    /// Whether the function's policy, as that grant leaves it, admits Secrets Manager's
    /// invocation of it for the secret the schedule names.
    admits: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the invoke granted to this service, scoped to nothing",
        grant: Some(admitting_grant),
        admits: true,
    },
    Row {
        case: "no grant on the rotation function at all",
        grant: None,
        admits: false,
    },
    Row {
        case: "the invoke granted to another service principal",
        grant: Some(another_principals_grant),
        admits: false,
    },
    Row {
        case: "another action of Lambda's granted to this service",
        grant: Some(another_actions_grant),
        admits: false,
    },
    Row {
        case: "the invoke granted to this service for another secret",
        grant: Some(another_secrets_grant),
        admits: false,
    },
];

/// The grant that admits the rotation: this service's own principal, on the invocation, for
/// every secret.
fn admitting_grant() -> Value {
    json!({
        "Action": INVOKE_ACTION,
        "FunctionName": { "Fn::GetAtt": ["RotationFunction", "Arn"] },
        "Principal": SERVICE_PRINCIPAL
    })
}

/// The same grant, made to a service principal that is not the one a rotation is invoked as.
fn another_principals_grant() -> Value {
    let mut grant = admitting_grant();
    grant["Principal"] = json!(ANOTHER_PRINCIPAL);
    grant
}

/// A grant to this service's principal of an action that is not the invocation.
fn another_actions_grant() -> Value {
    let mut grant = admitting_grant();
    grant["Action"] = json!(ANOTHER_ACTION);
    grant
}

/// The invocation granted to this service's principal, but scoped by source to a DIFFERENT
/// secret of the same stack — so the grant names one secret whose rotation may use the
/// function, and the schedule names the other.
fn another_secrets_grant() -> Value {
    let mut grant = admitting_grant();
    grant["SourceArn"] = json!({ "Ref": "OtherSecret" });
    grant
}

/// The policy that lets AWS Lambda assume the rotation function's execution role.
fn assume_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// The rotation rules every row's schedule declares.
fn rotation_rules() -> Value {
    json!({ "AutomaticallyAfterDays": ROTATION_INTERVAL_DAYS })
}

/// The template one row deploys: the secret the rotation is for, a second secret the scoped
/// grant can name, the rotation function with its execution role, the grant the row declares,
/// and the schedule that binds the function to the first secret.
///
/// Every row declares the same estate apart from that grant, which is what the rule
/// generalises over.
fn template_for(row: &Row, stack: &str) -> String {
    let mut resources = json!({
        "RotationRole": {
            "Type": "AWS::IAM::Role",
            "Properties": { "AssumeRolePolicyDocument": assume_role_policy() }
        },
        "RotationFunction": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": ROTATION_FUNCTION }
            }
        },
        "AppSecret": {
            "Type": "AWS::SecretsManager::Secret",
            "Properties": {
                "Name": format!("{stack}-credentials"),
                "SecretString": INITIAL_VALUE
            }
        },
        "OtherSecret": {
            "Type": "AWS::SecretsManager::Secret",
            "Properties": {
                "Name": format!("{stack}-other-credentials"),
                "SecretString": INITIAL_VALUE
            }
        },
        "Schedule": {
            "Type": "AWS::SecretsManager::RotationSchedule",
            "Properties": {
                "SecretId": { "Ref": "AppSecret" },
                "RotationLambdaARN": { "Fn::GetAtt": ["RotationFunction", "Arn"] },
                "RotationRules": rotation_rules(),
                // The rotation itself is the sibling suites' subject; what this one is about is
                // the configuration, which `RotateSecret` writes either way.
                "RotateImmediatelyOnUpdate": false
            }
        }
    });
    if let Some(grant) = row.grant {
        resources["InvokePermission"] =
            json!({ "Type": "AWS::Lambda::Permission", "Properties": grant() });
        // The grant has to be in force before the schedule is configured, which is what
        // `DependsOn` states.
        resources["Schedule"]["DependsOn"] = json!("InvokePermission");
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a secret whose rotation a stack configures through a function it deploys",
        "Resources": resources,
        "Outputs": {
            "SecretArn": { "Value": { "Ref": "AppSecret" } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["RotationFunction", "Arn"] } }
        }
    })
    .to_string()
}

/// Create the stack `stack` from `template`, acknowledging the role it declares.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// RULE: a schedule a stack created is the secret's own rotation configuration, so the owning
/// service reports the secret's rotation as on through the declared function and rules; and a
/// later deployment that drops the resource turns that rotation off while the secret stays.
#[tokio::test]
async fn a_stack_declared_schedule_turns_the_secrets_rotation_on_and_off() {
    for row in ROWS.iter().filter(|row| row.admits) {
        let case = row.case;
        let router = build_router();
        let stack = "rotationgrantadmitted";
        let template = template_for(row, stack);
        create_stack(&router, case, stack, &template).await;

        let (reached, body) = wait_for_stack_status(&router, stack, "CREATE_COMPLETE").await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete, got {reached:?} with {body} and \
             resource failures {:?}",
            failure_reasons(&router, stack).await
        );
        let outputs = outputs_of(&body);
        let secret = outputs
            .get("SecretArn")
            .unwrap_or_else(|| panic!("case {case:?}: no SecretArn output in {body}"))
            .clone();
        let function = outputs
            .get("FunctionArn")
            .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn output in {body}"))
            .clone();

        // The rotation state a client reads is the one the stack put in force.
        let described = describe_secret(&router, case, &secret).await;
        assert_eq!(
            described["RotationEnabled"].as_bool(),
            Some(true),
            "case {case:?}: expected the secret to report rotation enabled, got {described}"
        );
        assert_eq!(
            described["RotationLambdaARN"].as_str(),
            Some(function.as_str()),
            "case {case:?}: expected the declared rotation function {function:?}, got {described}"
        );
        assert_eq!(
            described["RotationRules"]["AutomaticallyAfterDays"],
            json!(ROTATION_INTERVAL_DAYS),
            "case {case:?}: expected the declared rotation interval of \
             {ROTATION_INTERVAL_DAYS} days, got {described}"
        );

        // The same template without the schedule: the resource's removal turns the rotation
        // off, and everything else the stack declares stays.
        let mut without: Value = serde_json::from_str(&template).expect("the template is JSON");
        without["Resources"]
            .as_object_mut()
            .expect("the template declares resources")
            .remove("Schedule");
        let (status, answered) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &without.to_string()),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {answered}"
        );
        let (reached, body) = wait_for_stack_status(&router, stack, "UPDATE_COMPLETE").await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {reached:?} with {body} and \
             resource failures {:?}",
            failure_reasons(&router, stack).await
        );

        let described = describe_secret(&router, case, &secret).await;
        assert_eq!(
            described["RotationEnabled"].as_bool(),
            Some(false),
            "case {case:?}: expected the removed schedule to leave rotation configured and off, \
             got {described}"
        );
        let value = current_value(&router, case, &secret).await;
        assert_eq!(
            value, INITIAL_VALUE,
            "case {case:?}: expected the secret to outlive the update with its declared value, \
             got {value:?}"
        );
    }
}

/// RULE: a rotation Secrets Manager could not perform is never reported configured — whatever
/// way the rotation function's policy fails to admit that service's invocation of it for the
/// secret, the schedule is `CREATE_FAILED` with that service's own refusal and the stack rolls
/// back.
///
/// The rows run in sequence inside ONE test: each deploys a stack of its own, and nothing about
/// the rule needs them concurrent.
#[tokio::test]
async fn a_schedule_whose_function_admits_no_invocation_rolls_the_stack_back() {
    for (index, row) in ROWS.iter().filter(|row| !row.admits).enumerate() {
        let case = row.case;
        let router = build_router();
        let stack = format!("rotationgrantrefused{index}");
        let template = template_for(row, &stack);
        create_stack(&router, case, &stack, &template).await;

        let (reached, body) = wait_for_stack_status(&router, &stack, "ROLLBACK_COMPLETE").await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the refused schedule to roll the stack back, got \
             {reached:?} with {body} and resource failures {:?}",
            failure_reasons(&router, &stack).await
        );

        // It is the SCHEDULE that was refused, and for the invocation the policy did not admit:
        // the function the grant was declared on is created either way, so a stack that failed
        // somewhere else would not satisfy this.
        let reason = create_failure_of(&router, &stack, "Schedule").await;
        let reported = reason.clone().unwrap_or_default();
        assert!(
            reported.contains(INVOKE_REFUSAL),
            "case {case:?}: expected the {RESOURCE_CREATE_FAILED} reason on the schedule to \
             carry {INVOKE_REFUSAL:?}, got {reason:?} among the stack's reasons {:?}",
            failure_reasons(&router, &stack).await
        );
        assert!(
            reported.contains(HANDLER_ACCESS_DENIED),
            "case {case:?}: expected the refusal to be reported as {HANDLER_ACCESS_DENIED:?}, \
             got {reported:?}"
        );
        let function_failure = create_failure_of(&router, &stack, "RotationFunction").await;
        assert!(
            function_failure.is_none(),
            "case {case:?}: expected the rotation function itself to be created, got the \
             failure {function_failure:?}"
        );
    }
}
