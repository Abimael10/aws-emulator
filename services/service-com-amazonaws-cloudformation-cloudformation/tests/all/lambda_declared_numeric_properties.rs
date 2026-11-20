// SPDX-License-Identifier: CC0-1.0

//! The numeric and boolean properties an `AWS::Lambda::*` resource declares, as they reach AWS
//! Lambda.
//!
//! RULE: a template's scalar is TEXT wherever its author quoted it, wherever the YAML a
//! transform rendered carries it quoted, and wherever it arrived through a `Ref` to a
//! parameter — while the member of the owning service's call it reaches is modelled as an
//! integer or a boolean, so the declaration is read as the TYPES that service's own model
//! states for its members before the call is made. A member the model types as a collection
//! keyed by names the REQUESTER chose (a function's environment variables, a tag set) is read
//! as the kind of value the model types that collection to hold, so a variable whose value
//! reads as a number is still the text it was declared as.
//!
//! What that buys: the stack deploys at all (an unread scalar is refused by the owning
//! service's deserializer, failing the resource and rolling the whole stack back), and the
//! value that reaches the service is judged by that service's OWN constraints — so a number
//! outside a member's published `@range` is refused with that range's clause rather than with a
//! type error.
//!
//! The rows of the table differ along what the rule generalises over: which call the member
//! belongs to (`CreateFunction`, `CreateEventSourceMapping`, `PutFunctionEventInvokeConfig`),
//! the modelled type (integer, boolean, and a boolean in each of its two spellings), the depth
//! the member sits at (a property of its own, a member of a nested structure), the FORM the
//! template states it in (quoted text, a JSON number, a `Ref` to a `String` parameter, a `Ref`
//! to a `Number` parameter), and where the value sits in the member's published range (a value
//! between the bounds, its published minimum, and one past that minimum).
//!
//! Every value is read back through AWS LAMBDA's own wire, not out of CloudFormation's record
//! of the declaration.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_text, REGION,
};
use crate::stack_lifecycle::acknowledging_iam;

/// A router carrying CloudFormation, AWS Lambda, Amazon SQS and IAM on ONE engine — the shape
/// `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `lambda_event_source_mapping_batch_item_failures.rs` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn router_with_a_queue_service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// The published bounds of the three function members this suite states values inside.
///
/// Source: those members' own `@range` in AWS Lambda's model
/// (`smithy_query(query="operation",operation="CreateFunction")` for `MemorySize` 128..32768 and
/// `Timeout` 1..5400, and `smithy_query(query="shape",shape="EphemeralStorage")` for
/// `EphemeralStorage$Size` 512..32768).
const MIN_MEMORY_SIZE_MB: i64 = 128;
const MIN_TIMEOUT_SECONDS: i64 = 1;
const MIN_EPHEMERAL_STORAGE_MB: i64 = 512;

/// The sentence a JSON deserializer answers for a member it was handed text for where it wanted
/// a number — the report that says the declaration never reached the service's own validation.
/// No case here may ever see it.
const TYPE_ERROR: &str = "expected number, got string";

/// Call one AWS Lambda operation over its restJson1 wire, signed for this suite's Region.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed Lambda request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// What `GetFunctionConfiguration` reports for one function.
async fn function_configuration(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
    )
    .await
}

/// The physical id the stack published for one of its logical resources.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource for {logical_id}, got \
         {status} with {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: no PhysicalResourceId for {logical_id} in {body}")
    })
}

/// The execution-role declaration every template here carries, with the actions a poller and a
/// handler of this suite act as the role to take.
fn role(logical_id: &str) -> Value {
    json!({
        logical_id: {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                },
                "Policies": [{
                    "PolicyName": "read-the-source",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": [
                                "sqs:ReceiveMessage",
                                "sqs:DeleteMessage",
                                "sqs:GetQueueAttributes",
                            ],
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }
    })
}

/// The inline source every function here is declared with: a handler that returns, since what is
/// under test is the configuration the function was created with.
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// One row of the function table: the forms a template states the three numeric members in, and
/// the numbers AWS Lambda must then report for them.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    /// The `MemorySize` the template declares, as the template states it.
    memory_size: Value,
    /// The `Timeout` it declares, as the template states it.
    timeout: Value,
    /// The `EphemeralStorage$Size` it declares, as the template states it.
    storage: Value,
    /// The parameters the stack is created with, as `(key, value)` pairs, and the parameter
    /// declarations the template carries for them.
    parameters: &'static [(&'static str, &'static str, &'static str)],
    /// What AWS Lambda must report for each of the three.
    expected_memory_size: i64,
    expected_timeout: i64,
    expected_storage: i64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "quoted integers, as a rendered template carries them",
            stack: "lambda-numbers-quoted",
            function: "lambda-numbers-quoted-fn",
            memory_size: json!("512"),
            timeout: json!("30"),
            storage: json!("1024"),
            parameters: &[],
            expected_memory_size: 512,
            expected_timeout: 30,
            expected_storage: 1024,
        },
        Row {
            case: "the same three stated as JSON numbers",
            stack: "lambda-numbers-unquoted",
            function: "lambda-numbers-unquoted-fn",
            memory_size: json!(256),
            timeout: json!(15),
            storage: json!(2048),
            parameters: &[],
            expected_memory_size: 256,
            expected_timeout: 15,
            expected_storage: 2048,
        },
        Row {
            case: "through a Ref to a String parameter",
            stack: "lambda-numbers-string-parameter",
            function: "lambda-numbers-string-parameter-fn",
            memory_size: json!({"Ref": "Memory"}),
            timeout: json!({"Ref": "Seconds"}),
            storage: json!("768"),
            parameters: &[("Memory", "String", "1024"), ("Seconds", "String", "7")],
            expected_memory_size: 1024,
            expected_timeout: 7,
            expected_storage: 768,
        },
        Row {
            case: "through a Ref to a Number parameter, at each member's published minimum",
            stack: "lambda-numbers-number-parameter",
            function: "lambda-numbers-number-parameter-fn",
            memory_size: json!({"Ref": "Memory"}),
            timeout: json!({"Ref": "Seconds"}),
            storage: json!({"Ref": "Storage"}),
            parameters: &[
                ("Memory", "Number", "128"),
                ("Seconds", "Number", "1"),
                ("Storage", "Number", "512"),
            ],
            expected_memory_size: MIN_MEMORY_SIZE_MB,
            expected_timeout: MIN_TIMEOUT_SECONDS,
            expected_storage: MIN_EPHEMERAL_STORAGE_MB,
        },
    ]
}

/// The template one function row deploys: the execution role, and a function stating the row's
/// three numeric members beside two environment variables whose values READ as numbers.
fn function_template(row: &Row) -> String {
    let mut resources = role("HandlerRole");
    resources["Handler"] = json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": row.function,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
            "MemorySize": row.memory_size,
            "Timeout": row.timeout,
            "EphemeralStorage": { "Size": row.storage },
            // Two variables whose names are members the catalogue types as numbers and whose
            // values read as numbers: environment variables are text, whatever they spell.
            "Environment": { "Variables": { "MemorySize": "99", "Timeout": "7" } },
            "Code": { "ZipFile": HANDLER_SOURCE },
        },
    });
    let mut declared = json!({ "Resources": resources });
    if !row.parameters.is_empty() {
        let mut parameters = serde_json::Map::new();
        for (name, kind, _) in row.parameters {
            parameters.insert((*name).to_string(), json!({ "Type": kind }));
        }
        declared["Parameters"] = Value::Object(parameters);
    }
    declared.to_string()
}

/// The `Parameters.member.N.*` request members one row's parameter values are sent as, beside
/// the IAM capability a template declaring a role is deployed with.
fn stack_parameters(row: &Row) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (position, (name, _, value)) in row.parameters.iter().enumerate() {
        let index = position + 1;
        members.push(member(
            &format!("Parameters.member.{index}.ParameterKey"),
            name,
        ));
        members.push(member(
            &format!("Parameters.member.{index}.ParameterValue"),
            value,
        ));
    }
    acknowledging_iam(&members)
}

#[tokio::test]
async fn a_functions_numeric_properties_reach_lambda_as_the_numbers_it_models() {
    for row in rows() {
        let router = router();
        let (status, created) = create_stack(
            &router,
            row.stack,
            &function_template(&row),
            &stack_parameters(&row),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from CreateStack, got {status} with {created}",
            row.case
        );
        let reached = settled_stack_status(row.case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {:?}: expected the create to complete, got {reached} with {reasons:?}",
            row.case
        );
        for reason in &reasons {
            assert!(
                !reason.contains(TYPE_ERROR),
                "case {:?}: expected no deserializer type error, got {reason:?}",
                row.case
            );
        }

        let (status, held) = function_configuration(&router, row.function).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected AWS Lambda to report the function the stack created, got \
             {status} with {held}",
            row.case
        );
        let reported = (
            held.get("MemorySize").and_then(Value::as_i64),
            held.get("Timeout").and_then(Value::as_i64),
            held.pointer("/EphemeralStorage/Size").and_then(Value::as_i64),
        );
        let expected = (
            Some(row.expected_memory_size),
            Some(row.expected_timeout),
            Some(row.expected_storage),
        );
        assert_eq!(
            reported, expected,
            "case {:?}: expected (MemorySize, Timeout, EphemeralStorage.Size) {expected:?} as \
             numbers, got {reported:?} in {held}",
            row.case
        );
        let variables = held.pointer("/Environment/Variables").cloned();
        assert_eq!(
            variables,
            Some(json!({ "MemorySize": "99", "Timeout": "7" })),
            "case {:?}: expected the declared environment variables to stay text, got \
             {variables:?} in {held}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_declared_number_past_a_members_range_is_refused_by_lambda_itself() {
    // One past the published minimum of `MemorySize`, stated as the text a template carries: the
    // value reaches AWS Lambda as the number it models, so that service judges it against its
    // own `@range` and the resource fails with THAT clause — not with a type error, which would
    // say the declaration never reached the constraint at all.
    let case = "a MemorySize one below the published minimum, as text";
    let stack = "lambda-numbers-below-the-minimum";
    let below = MIN_MEMORY_SIZE_MB - 1;
    let router = router();
    let mut resources = role("HandlerRole");
    resources["Handler"] = json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": "lambda-numbers-below-the-minimum-fn",
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
            "MemorySize": below.to_string(),
            "Code": { "ZipFile": HANDLER_SOURCE },
        },
    });
    let template = json!({ "Resources": resources }).to_string();

    let (status, created) =
        create_stack(&router, stack, &template, &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack).await;
    let reasons = stack_event_reasons(&router, REGION, stack).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the refused create to roll back, got {reached} with {reasons:?}"
    );
    let clause = format!("greater than or equal to {MIN_MEMORY_SIZE_MB}");
    assert!(
        reasons.iter().any(|reason| reason.contains(&clause)),
        "case {case:?}: expected a reason carrying {clause:?}, got {reasons:?}"
    );
    for reason in &reasons {
        assert!(
            !reason.contains(TYPE_ERROR),
            "case {case:?}: expected no deserializer type error, got {reason:?}"
        );
    }
}

#[tokio::test]
async fn a_triggers_and_an_invoke_configs_declared_scalars_reach_lambda_as_well() {
    // The same rule over the other two calls this family makes: `CreateEventSourceMapping`'s own
    // integer and boolean members, and `PutFunctionEventInvokeConfig`'s two integers, every one
    // of them stated as the text a template carries.
    let case = "a trigger and an invoke configuration stated entirely as text";
    let stack = "lambda-numbers-trigger";
    let function = "lambda-numbers-trigger-fn";
    let router = router_with_a_queue_service();
    let mut resources = role("ConsumerRole");
    resources["Source"] = json!({
        "Type": "AWS::SQS::Queue",
        "Properties": { "QueueName": "lambda-numbers-source" },
    });
    resources["Paused"] = json!({
        "Type": "AWS::SQS::Queue",
        "Properties": { "QueueName": "lambda-numbers-paused" },
    });
    resources["Consumer"] = json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": function,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
            "Code": { "ZipFile": HANDLER_SOURCE },
        },
    });
    resources["Trigger"] = json!({
        "Type": "AWS::Lambda::EventSourceMapping",
        "Properties": {
            "FunctionName": { "Ref": "Consumer" },
            "EventSourceArn": { "Fn::GetAtt": ["Source", "Arn"] },
            "BatchSize": "5",
            "Enabled": "true",
            "ScalingConfig": { "MaximumConcurrency": "5" },
        },
    });
    resources["AsyncConfig"] = json!({
        "Type": "AWS::Lambda::EventInvokeConfig",
        "Properties": {
            "FunctionName": { "Ref": "Consumer" },
            "Qualifier": "$LATEST",
            "MaximumRetryAttempts": "1",
            "MaximumEventAgeInSeconds": "120",
        },
    });
    // The boolean in the OTHER direction, over an event source of its own: "When false, Lambda
    // pauses polling and invocation", so a declaration read as the boolean it models settles
    // this mapping in the state that sentence names rather than in the one a mapping that
    // declares nothing settles in.
    resources["PausedTrigger"] = json!({
        "Type": "AWS::Lambda::EventSourceMapping",
        "Properties": {
            "FunctionName": { "Ref": "Consumer" },
            "EventSourceArn": { "Fn::GetAtt": ["Paused", "Arn"] },
            "BatchSize": "1",
            "Enabled": "false",
        },
    });
    let template = json!({ "Resources": resources }).to_string();

    let (status, created) =
        create_stack(&router, stack, &template, &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack).await;
    let reasons = stack_event_reasons(&router, REGION, stack).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with {reasons:?}"
    );
    for reason in &reasons {
        assert!(
            !reason.contains(TYPE_ERROR),
            "case {case:?}: expected no deserializer type error, got {reason:?}"
        );
    }

    let uuid = physical_id(&router, case, stack, "Trigger").await;
    let (status, mapping) = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/event-source-mappings/{uuid}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the mapping the stack created, got \
         {status} with {mapping}"
    );
    let reported = (
        mapping.get("BatchSize").and_then(Value::as_i64),
        mapping
            .pointer("/ScalingConfig/MaximumConcurrency")
            .and_then(Value::as_i64),
    );
    assert_eq!(
        reported,
        (Some(5), Some(5)),
        "case {case:?}: expected (BatchSize, MaximumConcurrency) (5, 5) as numbers, got \
         {reported:?} in {mapping}"
    );
    // `Enabled` is the boolean that decides the state a mapping settles in — "When true, the
    // event source mapping is active. When false, Lambda pauses polling and invocation" — so the
    // two mappings, declaring the two spellings as text, settle in the two states that sentence
    // names, and neither is merely whatever a mapping defaults to.
    let state = mapping
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        state, "Enabled",
        "case {case:?}: expected the declared Enabled to reach Lambda as true, got {mapping}"
    );
    let paused_uuid = physical_id(&router, case, stack, "PausedTrigger").await;
    let (status, paused) = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/event-source-mappings/{paused_uuid}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the paused mapping, got {status} with \
         {paused}"
    );
    let paused_state = paused
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        (
            paused_state,
            paused.get("BatchSize").and_then(Value::as_i64)
        ),
        ("Disabled".to_string(), Some(1)),
        "case {case:?}: expected the declared Enabled of \"false\" to pause the mapping and its \
         BatchSize to be the number 1, got {paused}"
    );

    let (status, config) = lambda(
        &router,
        "GET",
        &format!("/2019-09-25/functions/{function}/event-invoke-config?Qualifier=$LATEST"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the invoke configuration, got {status} \
         with {config}"
    );
    let reported = (
        config.get("MaximumRetryAttempts").and_then(Value::as_i64),
        config
            .get("MaximumEventAgeInSeconds")
            .and_then(Value::as_i64),
    );
    assert_eq!(
        reported,
        (Some(1), Some(120)),
        "case {case:?}: expected (MaximumRetryAttempts, MaximumEventAgeInSeconds) (1, 120) as \
         numbers, got {reported:?} in {config}"
    );
}
