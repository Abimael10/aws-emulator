// SPDX-License-Identifier: CC0-1.0

//! The numeric properties an `AWS::Lambda::Function` takes from a template PARAMETER, over every
//! deployment of the stack that declares them.
//!
//! RULE: a value a stack resolves for a resource property arrives as TEXT — a stack's parameter
//! values are submitted as strings, so a `Ref` to one resolves to the characters of the number
//! its caller gave — while the member of the owning service's call it reaches is modelled as an
//! integer, so a resolved value is read as the TYPE that service's own model states for its
//! member before the call is made, on the CREATE and on every later APPLY alike. The number that
//! then reaches that service is judged by that service's OWN published constraints, so a value
//! outside a member's `@range` is refused with that range's clause — quoting the value and naming
//! the member — rather than with a type error, which would say the declaration never reached the
//! constraint at all.
//!
//! `lambda_declared_numeric_properties.rs` holds that rule over the FORMS one template can state
//! a scalar in, on the create alone. This suite is the other axis: the one form every IaC tool
//! that parameterises a property produces, carried through the stack's whole life — including a
//! property whose value no `CreateFunction` member carries, applied by a Lambda call of its own.
//!
//! The rows differ along what the rule generalises over: the parameter's declared type (`Number`,
//! `String`), the member the resolved value reaches (`MemorySize`, `Timeout`,
//! `EphemeralStorage$Size`, `ReservedConcurrentExecutions`), the depth it sits at (a property of
//! its own, a member of a nested structure), the Lambda call it travels in (`CreateFunction` and
//! `UpdateFunctionConfiguration`, `PutFunctionConcurrency`), where the created value sits in the
//! member's published range (its published minimum, between the bounds), and which way the later
//! apply moves it (up, down).
//!
//! Every number is read back through AWS LAMBDA's own wire rather than out of CloudFormation's
//! record of the declaration, and every expected number is computed from what the row declared.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    assert_sdk_diagnostic, call, create_stack, failed_resource_reason, member, quoted_and_class,
    router, settled_stack_status, stack_event_reasons, xml_text, REGION,
};
use crate::stack_lifecycle::acknowledging_iam;

/// The published minimum of the two members a row states at its bound.
///
/// Source: those members' own `@range` in AWS Lambda's model
/// (`smithy_query(query="shape",shape="MemorySize,EphemeralStorageSize")`: `MemorySize`
/// 128..32768 and `EphemeralStorageSize` 512..32768).
const MIN_MEMORY_SIZE_MB: i64 = 128;
const MIN_EPHEMERAL_STORAGE_MB: i64 = 512;

/// The sentence a JSON deserializer answers for a member it was handed text for where it wanted a
/// number — the report that says a resolved value never reached the owning service's own
/// validation. No case here may ever see it.
const TYPE_ERROR: &str = "expected number, got string";

/// The statuses a row's stack settles on, before and after the apply that revises its parameter.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The logical id every template here declares its function under, and the role it assumes.
const FUNCTION_ID: &str = "Handler";
const ROLE_ID: &str = "HandlerRole";

/// The inline source every function here is declared with: a handler that returns, since what is
/// under test is the configuration the function was created and then updated with.
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// Where AWS Lambda reports the number one row declared.
enum Reported {
    /// A JSON pointer into `GetFunctionConfiguration`'s answer.
    Configuration(&'static str),
    /// `GetFunctionConcurrency`'s `ReservedConcurrentExecutions` — the reserved concurrency is no
    /// member of any function-configuration call, so Lambda reports it through a read of its own.
    Concurrency,
}

/// One row: the parameter a function property is declared as a `Ref` to, the value the create
/// gives it, and the value the later apply revises it to.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    /// The parameter's declared `Type` — `Number` and `String` both arrive as text on the wire.
    parameter_type: &'static str,
    /// Where in the function's `Properties` the `Ref` to the parameter is declared, as the path of
    /// member names leading to it.
    property: &'static [&'static str],
    /// Where AWS Lambda reports it.
    reported: Reported,
    /// The parameter value the create is given, and the one the later apply revises it to.
    created: i64,
    revised: i64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "MemorySize from a Number parameter, raised from the member's published minimum",
            stack: "lambda-parameter-memory",
            function: "lambda-parameter-memory-fn",
            parameter_type: "Number",
            property: &["MemorySize"],
            reported: Reported::Configuration("/MemorySize"),
            created: MIN_MEMORY_SIZE_MB,
            revised: 256,
        },
        Row {
            case: "Timeout from a String parameter, raised",
            stack: "lambda-parameter-timeout",
            function: "lambda-parameter-timeout-fn",
            parameter_type: "String",
            property: &["Timeout"],
            reported: Reported::Configuration("/Timeout"),
            created: 7,
            revised: 30,
        },
        Row {
            case: "EphemeralStorage.Size, a member of a nested structure, raised from its \
                   published minimum",
            stack: "lambda-parameter-storage",
            function: "lambda-parameter-storage-fn",
            parameter_type: "Number",
            property: &["EphemeralStorage", "Size"],
            reported: Reported::Configuration("/EphemeralStorage/Size"),
            created: MIN_EPHEMERAL_STORAGE_MB,
            revised: 1024,
        },
        Row {
            case: "ReservedConcurrentExecutions, which a Lambda call of its own carries, lowered",
            stack: "lambda-parameter-concurrency",
            function: "lambda-parameter-concurrency-fn",
            parameter_type: "Number",
            property: &["ReservedConcurrentExecutions"],
            reported: Reported::Concurrency,
            created: 5,
            revised: 2,
        },
    ]
}

/// The name every row's parameter is declared under.
const PARAMETER: &str = "DeclaredNumber";

/// Call one AWS Lambda operation over its restJson1 wire, signed for this suite's Region.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
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

/// The number AWS Lambda reports for one row's member, read through that service's own wire.
async fn reported_number(router: &MultiServiceRouter, case: &str, row: &Row) -> Option<i64> {
    let (uri, read) = match row.reported {
        Reported::Configuration(pointer) => (
            format!("/2015-03-31/functions/{}/configuration", row.function),
            pointer,
        ),
        Reported::Concurrency => (
            format!("/2019-09-30/functions/{}/concurrency", row.function),
            "/ReservedConcurrentExecutions",
        ),
    };
    let (status, held) = lambda(router, &uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the function the stack deployed, got \
         {status} with {held}"
    );
    held.pointer(read).and_then(Value::as_i64)
}

/// The execution-role declaration every template here carries.
fn role() -> Value {
    json!({
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
        },
    })
}

/// The template one row deploys: the execution role, and a function whose `property` is a `Ref` to
/// the row's parameter — the form every IaC tool produces for a parameterised property.
fn template(function: &str, parameter_type: &str, property: &[&str]) -> String {
    let mut properties = json!({
        "FunctionName": function,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
        "Code": { "ZipFile": HANDLER_SOURCE },
    });
    // The `Ref` goes at the END of the path, every level above it an object of its own, so a
    // member a nested structure holds is declared exactly as its own property is.
    let mut declared = json!({ "Ref": PARAMETER });
    for name in property[1..].iter().rev() {
        let mut holder = Map::new();
        holder.insert((*name).to_string(), declared);
        declared = Value::Object(holder);
    }
    properties[property[0]] = declared;
    json!({
        "Parameters": { PARAMETER: { "Type": parameter_type } },
        "Resources": {
            ROLE_ID: role(),
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": properties,
            },
        },
    })
    .to_string()
}

/// The request members one parameter VALUE is submitted as — always text, whatever the parameter's
/// declared type — beside the capability a template declaring a role is deployed with.
fn parameters(value: i64) -> Vec<(String, String)> {
    acknowledging_iam(&[
        member("Parameters.member.1.ParameterKey", PARAMETER),
        member("Parameters.member.1.ParameterValue", &value.to_string()),
    ])
}

/// The physical id the stack published for its function.
async fn physical_id(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", FUNCTION_ID),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource, got {status} with {body}"
    );
    xml_text(&body, "PhysicalResourceId")
        .unwrap_or_else(|| panic!("case {case:?}: no PhysicalResourceId in {body}"))
}

/// Hold the quoted message of a refusal AWS LAMBDA answered to the shape its SDK words: that
/// service's own sentence, the service as its SDK names it, the status it answered, the id of the
/// call — and the number of attempts the SDK made at it, which is what this provider's shape
/// carries past the envelope.
///
/// The envelope itself is the harness's ([`crate::harness::assert_sdk_diagnostic`]), which holds a
/// diagnostic that closes AT the call's id; the count past it is split off first, exactly as
/// `apigateway_rest_api_refused_create_reason.rs` does for the same shape.
fn assert_counted_sdk_diagnostic(case: &str, quoted: &str, status: u16) {
    const COUNT: &str = " (SDK Attempt Count: ";
    let (envelope, count) = quoted.rsplit_once(COUNT).unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK's attempt count past the envelope, got {quoted:?}")
    });
    let count = count.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the attempt count to close, got {quoted:?}")
    });
    assert!(
        count.parse::<u32>().is_ok_and(|attempts| attempts >= 1),
        "case {case:?}: expected the attempts the SDK made at the call, got {count:?}"
    );
    assert_sdk_diagnostic(case, envelope, "Lambda", status);
}

/// Every reason the stack recorded, asserted to carry no deserializer type error: a row that sees
/// one is a row whose resolved value never reached AWS Lambda's own validation.
async fn assert_no_type_error(router: &MultiServiceRouter, case: &str, stack: &str) {
    for reason in stack_event_reasons(router, REGION, stack).await {
        assert!(
            !reason.contains(TYPE_ERROR),
            "case {case:?}: expected no deserializer type error, got {reason:?}"
        );
    }
}

#[tokio::test]
async fn a_parameterised_number_reaches_lambda_on_the_create_and_on_every_later_apply() {
    for row in rows() {
        let case = row.case;
        let body = template(row.function, row.parameter_type, row.property);
        let router = router();

        let (status, created) =
            create_stack(&router, row.stack, &body, &parameters(row.created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            reached,
            CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {reached} with {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        assert_no_type_error(&router, case, row.stack).await;
        let held = reported_number(&router, case, &row).await;
        assert_eq!(
            held,
            Some(row.created),
            "case {case:?}: expected AWS Lambda to hold the resolved parameter as the number \
             {}, got {held:?}",
            row.created
        );
        let before = physical_id(&router, case, row.stack).await;

        // The same template, deployed again with the parameter revised: the resolved value is
        // text again, and the member it reaches is the same modelled integer.
        let mut revising = vec![
            member("StackName", row.stack),
            member("TemplateBody", &body),
        ];
        revising.extend(parameters(row.revised));
        let (status, updated) = call(&router, "UpdateStack", &revising).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            reached,
            UPDATE_COMPLETE,
            "case {case:?}: expected the apply to complete, got {reached} with {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        assert_no_type_error(&router, case, row.stack).await;
        let held = reported_number(&router, case, &row).await;
        assert_eq!(
            held,
            Some(row.revised),
            "case {case:?}: expected AWS Lambda to hold the revised parameter as the number {}, \
             got {held:?}",
            row.revised
        );

        // ...applied to the function the stack already had, rather than to a new one: the handle
        // the stack publishes is the one it published before the apply.
        let after = physical_id(&router, case, row.stack).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the function to keep its physical id across the apply, got \
             {after:?}"
        );
    }
}

#[tokio::test]
async fn a_parameterised_number_outside_a_members_range_is_refused_by_lambda_itself() {
    // One below the published minimum of `MemorySize`, resolved from a parameter: the value
    // reaches AWS Lambda as the number it models, so that service judges it against its own
    // `@range` and the resource fails with THAT clause, in the envelope that service words a
    // refused value in.
    let case = "a parameterised MemorySize one below the member's published minimum";
    let stack = "lambda-parameter-below-the-minimum";
    let below = MIN_MEMORY_SIZE_MB - 1;
    let router = router();
    let body = template(
        "lambda-parameter-below-the-minimum-fn",
        "Number",
        &["MemorySize"],
    );

    let (status, created) = create_stack(&router, stack, &body, &parameters(below)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        reached,
        "ROLLBACK_COMPLETE",
        "case {case:?}: expected the refused create to roll back, got {reached} with {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );

    let reason = failed_resource_reason(case, &router, REGION, stack, FUNCTION_ID, "CREATE_FAILED")
        .await;
    assert!(
        !reason.contains(TYPE_ERROR),
        "case {case:?}: expected no deserializer type error, got {reason:?}"
    );
    let (quoted, class) = quoted_and_class(case, &reason);
    // The value the request carried, the member it was carried in and the bound the member
    // publishes, in the one envelope this wire states a refused value in.
    let expected = format!(
        "1 validation error detected: Value '{below}' at 'memorySize' failed to satisfy \
         constraint: Member must have value greater than or equal to {MIN_MEMORY_SIZE_MB}"
    );
    assert!(
        quoted.starts_with(&expected),
        "case {case:?}: expected the refusal to open with {expected:?}, got {quoted:?}"
    );
    assert_counted_sdk_diagnostic(case, &quoted, 400);
    assert_eq!(
        class, "InvalidRequest",
        "case {case:?}: expected the refused declaration to be classified as the request's own \
         fault, got {class:?} from {reason:?}"
    );
}
