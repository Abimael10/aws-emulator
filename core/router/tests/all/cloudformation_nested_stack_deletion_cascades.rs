// SPDX-License-Identifier: CC0-1.0

//! TERMINATION PROTECTION over a NESTING HIERARCHY, and the deletion cascade that follows
//! from it, on the assembled emulator.
//!
//! The rule under test: **termination protection belongs to the stack at the TOP of a nesting
//! hierarchy. "For nested stacks, termination protection is set on the root stack and can't be
//! changed directly on the nested stack"
//! (`smithy_query(query="doc",shape="Stack",member="EnableTerminationProtection")`, the same
//! sentence `UpdateTerminationProtection` carries), so a request that asks to change the flag
//! on a stack another stack's `AWS::CloudFormation::Stack` resource created is refused `400
//! ValidationError` and writes nothing, what such a stack reports for
//! `EnableTerminationProtection` is the ROOT stack's value, and deleting an unprotected root
//! therefore CASCADES: every stack below it is deleted, and so are the resources those stacks
//! created in the services that own them.**
//!
//! Three crates have to agree for that to hold — CloudFormation holds the hierarchy, Amazon S3
//! holds the child templates the nested-stack resources point at, and AWS Systems Manager
//! holds the parameter the deepest stack created, which is what says the cascade really
//! reached the bottom — so the suite lives here.
//!
//! The rows differ along what the rule generalises over: the DEPTH of the stack the request
//! names (the first level of nesting and the second), whether it names that stack by NAME or
//! by STACK ID, and whether it asks to ENABLE or to DISABLE protection. The root stack of the
//! same hierarchy is the row that must be accepted, which is what keeps the refusal from being
//! a refusal of every stack.
//!
//! Nothing here sleeps for a span the emulator owes: every wait is a poll for the CONDITION
//! with a deadline far longer than the spans this emulator gives these transitions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a condition the clock owes is waited for, and how often it is looked at.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The bucket the child templates are staged in, which is where a nested stack's
/// `TemplateURL` points.
const STAGING_BUCKET: &str = "nested-protection-templates";

/// The keys the two child templates are staged under.
const CHILD_KEY: &str = "child.json";
const GRANDCHILD_KEY: &str = "grandchild.json";

/// The root stack of the hierarchy every row is about.
const ROOT_STACK: &str = "nested-protection-root";

/// The logical id each level declares the level below it under.
const CHILD_ID: &str = "Child";
const GRANDCHILD_ID: &str = "Kid";

/// The output key every level publishes the parameter name under.
const PARAM_NAME_OUTPUT: &str = "ParamName";

/// The error code every awsQuery refusal of this service carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The error AWS Systems Manager refuses a read of a parameter it does not hold with.
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// The emulator as it is ASSEMBLED, on one engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
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

/// One awsQuery call of the service `signing_name`.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call (API version 2010-05-15).
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One AWS Systems Manager call (`sigv4Name: ssm`, service shape `AmazonSSM`, awsJson1_1).
async fn ssm(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let host = format!("ssm.{REGION}.amazonaws.com");
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .header("authorization", authorization("ssm"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The error code an awsJson answer carries, as a client branches on it.
fn json_error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|code| code.rsplit(['#', '/']).next().unwrap_or(code).to_string())
        .unwrap_or_default()
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The URL a staged child template is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// The deepest template: one Systems Manager parameter, whose generated name it publishes.
///
/// The parameter is what says the cascade reached the bottom of the hierarchy: it lives in the
/// service that OWNS it, so a read of that service decides whether it is still there.
fn grandchild_template() -> String {
    json!({
        "Resources": {
            "Param": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {"Type": "String", "Value": "nested-protection"}
            }
        },
        "Outputs": {PARAM_NAME_OUTPUT: {"Value": {"Ref": "Param"}}}
    })
    .to_string()
}

/// The middle template: one nested stack of its own, whose published name it passes upward.
fn child_template() -> String {
    json!({
        "Resources": {
            GRANDCHILD_ID: {
                "Type": "AWS::CloudFormation::Stack",
                "Properties": {"TemplateURL": staged_url(GRANDCHILD_KEY)}
            }
        },
        "Outputs": {
            PARAM_NAME_OUTPUT: {
                "Value": {
                    "Fn::GetAtt": [GRANDCHILD_ID, format!("Outputs.{PARAM_NAME_OUTPUT}")]
                }
            }
        }
    })
    .to_string()
}

/// The root template: the first level of nesting, and the name the hierarchy published.
fn root_template() -> String {
    json!({
        "Resources": {
            CHILD_ID: {
                "Type": "AWS::CloudFormation::Stack",
                "Properties": {"TemplateURL": staged_url(CHILD_KEY)}
            }
        },
        "Outputs": {
            "ChildParamName": {
                "Value": {"Fn::GetAtt": [CHILD_ID, format!("Outputs.{PARAM_NAME_OUTPUT}")]}
            }
        }
    })
    .to_string()
}

/// How a row's request names the stack it is about.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// `StackName` carrying the stack's own name.
    ByName,
    /// `StackName` carrying the stack's id, which the member also accepts.
    ById,
}

/// Which stack of the hierarchy a row is about — one of the NESTED ones, since the row is
/// about the refusal; the root is the case the test makes on its own below.
#[derive(Clone, Copy, Debug)]
enum Level {
    /// The first level of nesting.
    Child,
    /// The second level of nesting.
    Grandchild,
}

/// One row: the stack the request names, how it names it, and what it asks for.
#[derive(Debug)]
struct Row {
    case: &'static str,
    level: Level,
    named: Named,
    enable: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the first level of nesting, named by its stack id, asked to enable",
        level: Level::Child,
        named: Named::ById,
        enable: true,
    },
    Row {
        case: "the first level of nesting, named by name, asked to disable",
        level: Level::Child,
        named: Named::ByName,
        enable: false,
    },
    Row {
        case: "the second level of nesting, named by its stack id, asked to enable",
        level: Level::Grandchild,
        named: Named::ById,
        enable: true,
    },
    Row {
        case: "the second level of nesting, named by name, asked to enable",
        level: Level::Grandchild,
        named: Named::ByName,
        enable: true,
    },
];

/// The sentence a refusal must read, which is the RULE with the two ids filled in: the stack
/// the request resolved to, and the stack at the top of its hierarchy.
///
/// Spelled out here rather than imported from the crate, because the sentence IS the
/// requirement: a reworded message must fail this suite.
fn expected_refusal(stack_id: &str, root_id: &str) -> String {
    format!(
        "Cannot change termination protection directly on the nested stack [{stack_id}]. For \
         nested stacks, termination protection is set on the root stack [{root_id}]."
    )
}

/// Stage one template in Amazon S3.
async fn stage(router: &MultiServiceRouter, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "setup: staging {key:?} must be accepted, got {status} with {answered}"
    );
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer that status
/// with the answer it was read from. `None` where this service no longer reports the stack at
/// all, which is what a deletion addressed by NAME leaves behind.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Option<(String, String)> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        if code != 200 {
            return None;
        }
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.is_empty() && !status.ends_with("_IN_PROGRESS") {
            return Some((status, body));
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {status:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The `DescribeStacks` answer for one stack, which must be there.
async fn described(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks({stack}) must answer, got {code} {body}"
    );
    body
}

/// The `PhysicalResourceId` one stack reports for one of its resources — for an
/// `AWS::CloudFormation::Stack` that is the child stack's own id.
async fn child_stack_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResource({stack}, {logical_id}) must answer, got {code} \
         {body}"
    );
    let id = opt_element(&body, "PhysicalResourceId").unwrap_or_default();
    assert!(
        id.starts_with("arn:"),
        "case {case:?}: expected {logical_id} of {stack} to report the nested stack's id, got \
         {id:?}"
    );
    id
}

/// Every resource the stack reports a FAILED status for, with the reason it gave.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    if code != 200 {
        return Vec::new();
    }
    members_of(&body)
        .into_iter()
        .filter(|member| {
            opt_element(member, "ResourceStatus").is_some_and(|status| status.contains("FAILED"))
        })
        .map(|member| {
            format!(
                "{}={}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default()
            )
        })
        .collect()
}

/// Ask `UpdateTerminationProtection` for one stack.
async fn protect(
    router: &MultiServiceRouter,
    stack: &str,
    enable: bool,
) -> (u16, String) {
    cloudformation(
        router,
        "UpdateTerminationProtection",
        &[
            ("StackName", stack),
            (
                "EnableTerminationProtection",
                if enable { "true" } else { "false" },
            ),
        ],
    )
    .await
}

/// What `DescribeStacks` reports for one stack's `EnableTerminationProtection`.
async fn reported_protection(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = described(router, case, stack).await;
    opt_element(&body, "EnableTerminationProtection").unwrap_or_default()
}

#[tokio::test]
async fn termination_protection_is_the_root_stacks_and_an_unprotected_root_cascades() {
    let router = build_router();
    let (code, body) = s3(&router, "PUT", &format!("/{STAGING_BUCKET}"), "").await;
    assert!(
        (200..300).contains(&code),
        "setup: the staging bucket must be created, got {code} with {body}"
    );
    stage(&router, GRANDCHILD_KEY, &grandchild_template()).await;
    stage(&router, CHILD_KEY, &child_template()).await;

    let (code, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", ROOT_STACK), ("TemplateBody", &root_template())],
    )
    .await;
    assert_eq!(
        code, 200,
        "setup: CreateStack must be accepted, got {code} {body}"
    );
    let (status, body) = settled(&router, "the hierarchy", ROOT_STACK)
        .await
        .expect("the root stack just created");
    assert_eq!(
        status, "CREATE_COMPLETE",
        "setup: the root stack must come up, got {status} {body}; failures {:?}",
        reported_failures(&router, ROOT_STACK).await
    );

    // The hierarchy, read the way a client follows it: each level's nested-stack resource
    // reports the stack below it by that stack's own id.
    let root_id = opt_element(&body, "StackId").unwrap_or_default();
    let child_id = child_stack_id(&router, "the hierarchy", ROOT_STACK, CHILD_ID).await;
    let grandchild_id = child_stack_id(&router, "the hierarchy", &child_id, GRANDCHILD_ID).await;
    let child_name = opt_element(&described(&router, "the hierarchy", &child_id).await, "StackName")
        .unwrap_or_default();
    let grandchild_body = described(&router, "the hierarchy", &grandchild_id).await;
    let grandchild_name = opt_element(&grandchild_body, "StackName").unwrap_or_default();
    // The parameter the deepest stack created, read from the stack that created it: what says
    // the cascade reached the bottom is that Systems Manager no longer holds it.
    let parameter_name = outputs_of(&grandchild_body)
        .get(PARAM_NAME_OUTPUT)
        .cloned()
        .unwrap_or_else(|| {
            panic!("setup: the deepest stack must publish its parameter name, got {grandchild_body}")
        });

    for row in ROWS {
        let case = row.case;
        let (stack_id, stack_name) = match row.level {
            Level::Child => (child_id.clone(), child_name.clone()),
            Level::Grandchild => (grandchild_id.clone(), grandchild_name.clone()),
        };
        let named = match row.named {
            Named::ById => stack_id.clone(),
            Named::ByName => stack_name.clone(),
        };
        let (code, body) = protect(&router, &named, row.enable).await;
        assert_eq!(
            code, 400,
            "case {case:?}: the change must be refused, got {code} {body}"
        );
        // Read through the DECODED reader, which is what a client sees: the serializer
        // escapes an answer's text and a client decodes it back.
        let answered = (
            elements(&body, "Code").into_iter().next().unwrap_or_default(),
            elements(&body, "Message")
                .into_iter()
                .next()
                .unwrap_or_default(),
        );
        let expected = expected_refusal(&stack_id, &root_id);
        assert_eq!(
            answered,
            (VALIDATION_ERROR.to_string(), expected.clone()),
            "case {case:?}: expected the refusal to read {expected:?}, got {answered:?}"
        );
        // That the refusal WROTE nothing is not observable through this operation's own read —
        // what a nested stack reports is the root's value either way — so it is asserted where
        // it can be: on the record itself, by the owning crate's unit table
        // (`state::stack_nesting`), and by the cascade at the end of this test, which a flag
        // written onto either child would have refused.
    }

    // The ROOT is where the flag lives, so the same call on it is carried out — and every
    // stack of the hierarchy then reports the value it was set to.
    let (code, body) = protect(&router, ROOT_STACK, true).await;
    assert_eq!(
        code, 200,
        "case \"the root stack, asked to enable\": the change must be carried out, got {code} \
         {body}"
    );
    for (level, stack) in [
        ("the root stack", root_id.as_str()),
        ("the first level of nesting", child_id.as_str()),
        ("the second level of nesting", grandchild_id.as_str()),
    ] {
        let reported = reported_protection(&router, level, stack).await;
        assert_eq!(
            reported, "true",
            "case \"a protected hierarchy\": expected {level} to report the root's protection, \
             got {reported:?}"
        );
    }
    // ...and the protection is in force: "if a user attempts to delete a stack with
    // termination protection enabled, the operation fails and the stack remains unchanged".
    let (code, body) = cloudformation(&router, "DeleteStack", &[("StackName", ROOT_STACK)]).await;
    assert_eq!(
        code, 400,
        "case \"a protected hierarchy\": the root's deletion must be refused, got {code} {body}"
    );
    let refused = (
        elements(&body, "Code").into_iter().next().unwrap_or_default(),
        elements(&body, "Message")
            .into_iter()
            .next()
            .unwrap_or_default(),
    );
    assert_eq!(
        refused,
        (
            VALIDATION_ERROR.to_string(),
            format!(
                "Stack [{ROOT_STACK}] cannot be deleted while TerminationProtection is enabled"
            )
        ),
        "case \"a protected hierarchy\": expected the documented refusal, got {refused:?}"
    );

    let (code, body) = protect(&router, ROOT_STACK, false).await;
    assert_eq!(
        code, 200,
        "case \"the root stack, asked to disable\": the change must be carried out, got {code} \
         {body}"
    );
    for (level, stack) in [
        ("the root stack", root_id.as_str()),
        ("the first level of nesting", child_id.as_str()),
        ("the second level of nesting", grandchild_id.as_str()),
    ] {
        let reported = reported_protection(&router, level, stack).await;
        assert_eq!(
            reported, "false",
            "case \"an unprotected hierarchy\": expected {level} to report the root's cleared \
             protection, got {reported:?}"
        );
    }

    // The CASCADE: with nothing of the hierarchy protected, the root's deletion deletes every
    // stack below it, and the resources those stacks created.
    let case = "an unprotected hierarchy";
    let (code, body) = cloudformation(&router, "DeleteStack", &[("StackName", ROOT_STACK)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DeleteStack must be accepted, got {code} {body}"
    );
    let settled_root = settled(&router, case, &root_id).await;
    let (status, body) = settled_root.expect("a deleted stack is still read by its id");
    assert_eq!(
        status, "DELETE_COMPLETE",
        "case {case:?}: expected the root to be deleted, got {status} {body}; failures {:?}",
        reported_failures(&router, &root_id).await
    );
    // A stack whose deletion completed is no longer addressable by NAME.
    let (code, refused) =
        cloudformation(&router, "DescribeStacks", &[("StackName", ROOT_STACK)]).await;
    assert_eq!(
        code, 400,
        "case {case:?}: the deleted root must not be found by name, got {code} {refused}"
    );
    for (level, stack) in [
        ("the first level of nesting", child_id.as_str()),
        ("the second level of nesting", grandchild_id.as_str()),
    ] {
        let (status, body) = settled(&router, case, stack)
            .await
            .expect("a deleted stack is still read by its id");
        assert_eq!(
            status, "DELETE_COMPLETE",
            "case {case:?}: expected {level} to be deleted with the root, got {status} {body}; \
             failures {:?}",
            reported_failures(&router, stack).await
        );
    }
    let (code, read) = ssm(
        &router,
        "GetParameter",
        &json!({"Name": parameter_name.clone()}),
    )
    .await;
    assert_eq!(
        (code, json_error_code(&read).as_str()),
        (400, PARAMETER_NOT_FOUND),
        "case {case:?}: expected the parameter {parameter_name:?} the deepest stack created to \
         be gone, got {code} with {read}"
    );
}
