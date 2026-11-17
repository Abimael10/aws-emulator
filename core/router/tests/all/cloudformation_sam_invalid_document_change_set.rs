// SPDX-License-Identifier: CC0-1.0

//! An INVALID SAM document submitted as a change set, and what CloudFormation answers for it.
//!
//! The rule under test: **a transform either processes a template or FAILS on it, and its
//! failure is not a bad request: `CreateChangeSet` is accepted and the change set settles
//! `FAILED` carrying the macro's own aggregated diagnostic — `Invalid Serverless Application
//! Specification document. Number of errors found: <n>.` and one `Resource with id [<logical
//! id>] is invalid. <what is wrong>` per faulty declaration — with NO changes planned, the
//! shell stack left in `REVIEW_IN_PROGRESS` and nothing materialised in the services the
//! expansion would have created resources in. A declaration whose type is in the Serverless
//! namespace but is no type the macro defines is CloudFormation's own unrecognised-type error
//! instead.**
//!
//! Two crates have to agree for the second half of that to hold — CloudFormation processes the
//! macro and plans the change set, and AWS Lambda is where the function the document declares
//! would have been created — so the suite lives here: a change set that failed must leave the
//! function ABSENT in Lambda's own API, not merely unmentioned by CloudFormation.
//!
//! The rows differ along what the rule generalises over: WHICH fault the document carries (a
//! `Policies` entry that is no policy, a `Zip` function with no runtime and one with no
//! handler, an event naming no event source type, a `SimpleTable` whose stated primary key
//! says nothing about its type), WHERE the fault is (the position of the faulty policy entry),
//! HOW MANY declarations are faulty (one and two, which is what the count reports), whose
//! error it is (the macro's and CloudFormation's) — and the positive control, a document the
//! macro processes, which plans its changes and reaches `CREATE_COMPLETE`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The macro the AWS Serverless Application Model is published as.
const TRANSFORM: &str = "AWS::Serverless-2016-10-31";

/// How long a change set is given to settle, and how often it is looked at. The wait is on the
/// CONDITION — the change set reporting a status that is no longer pending — with a deadline
/// far longer than the span this emulator gives that transition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The status a change set that has not settled yet reports.
const CREATE_PENDING: &str = "CREATE_PENDING";
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";

/// The status the shell stack of a CREATE change set rests in until one is executed.
const REVIEW_IN_PROGRESS: &str = "REVIEW_IN_PROGRESS";

/// The emulator as it is assembled, on one engine.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
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

/// One CloudFormation call over its awsQuery wire.
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
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One AWS Lambda call over its `restJson1` wire, as its status and parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (parts.status.as_u16(), parsed)
}

/// What one row expects of the change set: the macro's own diagnostic, or CloudFormation's.
enum Expected {
    /// The transform failed: the number of faults and the causes, which together are the
    /// whole `StatusReason` the macro's failure carries.
    Faults(usize, &'static [&'static str]),
    /// CloudFormation's own refusal of a type that does not exist.
    Unrecognised(&'static str),
    /// The document is one the macro processes: the change set is executable.
    Processed,
}

/// One row: a stack and change-set name of its own, the template submitted, and what the
/// change set must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: fn() -> String,
    expected: Expected,
}

/// The name the function every row declares would be created under, which is what says
/// whether a failed change set materialised anything.
const FUNCTION_NAME: &str = "sam-invalid-document-probe";

/// A SAM function declaration with the members a row does not vary.
fn function(mut properties: Value) -> Value {
    let members = properties.as_object_mut().expect("an object of properties");
    members
        .entry("InlineCode".to_string())
        .or_insert_with(|| json!("def handler(event, context):\n    return {}\n"));
    members
        .entry("FunctionName".to_string())
        .or_insert_with(|| json!(FUNCTION_NAME));
    json!({"Type": "AWS::Serverless::Function", "Properties": properties})
}

/// A document whose `Policies` entry at `index` names no published policy template.
fn with_bad_policy_at(index: usize) -> Value {
    let mut policies = vec![json!("AWSLambdaBasicExecutionRole"); index];
    policies.push(json!({"NotAPolicyTemplate": {"TableName": "items"}}));
    function(json!({
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Policies": policies,
    }))
}

/// One template, with the transform declared.
fn template(resources: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Transform": TRANSFORM,
        "Resources": resources,
    })
    .to_string()
}

fn bad_policy_name() -> String {
    template(json!({"Fn": with_bad_policy_at(0)}))
}

fn bad_policy_at_the_second_entry() -> String {
    template(json!({"Fn": with_bad_policy_at(1)}))
}

fn no_handler() -> String {
    template(json!({"Fn": function(json!({"Runtime": "python3.12"}))}))
}

fn no_runtime() -> String {
    template(json!({"Fn": function(json!({"Handler": "index.handler"}))}))
}

fn unknown_event_type() -> String {
    template(json!({"Fn": function(json!({
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Events": {"Ev": {"Type": "NotAnEventType", "Properties": {}}},
    }))}))
}

fn table_without_a_key_type() -> String {
    template(json!({
        "Table": {
            "Type": "AWS::Serverless::SimpleTable",
            "Properties": {"PrimaryKey": {"Name": "id"}},
        },
    }))
}

fn two_faults() -> String {
    template(json!({
        "Fn": function(json!({"Runtime": "python3.12"})),
        "Table": {
            "Type": "AWS::Serverless::SimpleTable",
            "Properties": {"PrimaryKey": {"Name": "id"}},
        },
    }))
}

fn a_type_the_macro_does_not_define() -> String {
    template(json!({"Thing": {"Type": "AWS::Serverless::NotAThing", "Properties": {}}}))
}

fn a_document_the_macro_processes() -> String {
    template(json!({"Fn": function(json!({
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Policies": [
            "AWSLambdaBasicExecutionRole",
            {"DynamoDBCrudPolicy": {"TableName": "items"}},
            {"Statement": [{"Effect": "Allow", "Action": "s3:GetObject", "Resource": "*"}]},
        ],
        "Events": {"Ev": {"Type": "Api", "Properties": {"Path": "/items", "Method": "get"}}},
    }))}))
}

const ROWS: &[Row] = &[
    Row {
        case: "a Policies entry naming no published policy template",
        stack: "sam-bad-policy",
        template: bad_policy_name,
        expected: Expected::Faults(
            1,
            &["Resource with id [Fn] is invalid. Policy at index 0 in the 'Policies' property \
               is not valid"],
        ),
    },
    Row {
        case: "the faulty policy entry at another position",
        stack: "sam-bad-policy-second",
        template: bad_policy_at_the_second_entry,
        expected: Expected::Faults(
            1,
            &["Resource with id [Fn] is invalid. Policy at index 1 in the 'Policies' property \
               is not valid"],
        ),
    },
    Row {
        case: "a Zip function with no handler",
        stack: "sam-no-handler",
        template: no_handler,
        expected: Expected::Faults(
            1,
            &["Resource with id [Fn] is invalid. Runtime and Handler needs to be present when \
               PackageType is of type `Zip`"],
        ),
    },
    Row {
        case: "a Zip function with no runtime",
        stack: "sam-no-runtime",
        template: no_runtime,
        expected: Expected::Faults(
            1,
            &["Resource with id [Fn] is invalid. Runtime and Handler needs to be present when \
               PackageType is of type `Zip`"],
        ),
    },
    Row {
        case: "an event naming no event source type",
        stack: "sam-bad-event",
        template: unknown_event_type,
        expected: Expected::Faults(
            1,
            &["Resource with id [Fn] is invalid. Event with id [Ev] is invalid. Resource dict \
               has missing or invalid value for key Type. Event Type is: NotAnEventType."],
        ),
    },
    Row {
        case: "a simple table whose stated primary key says nothing about its type",
        stack: "sam-bad-table",
        template: table_without_a_key_type,
        expected: Expected::Faults(
            1,
            &["Resource with id [Table] is invalid. Property 'PrimaryKey.Type' is required."],
        ),
    },
    Row {
        case: "two faulty declarations in one document",
        stack: "sam-two-faults",
        template: two_faults,
        expected: Expected::Faults(
            2,
            &[
                "Resource with id [Fn] is invalid. Runtime and Handler needs to be present \
                 when PackageType is of type `Zip`",
                "Resource with id [Table] is invalid. Property 'PrimaryKey.Type' is required.",
            ],
        ),
    },
    Row {
        case: "a type in the Serverless namespace the macro does not define",
        stack: "sam-unknown-type",
        template: a_type_the_macro_does_not_define,
        expected: Expected::Unrecognised("AWS::Serverless::NotAThing"),
    },
    Row {
        case: "the positive control: a document the macro processes",
        stack: "sam-valid-document",
        template: a_document_the_macro_processes,
        expected: Expected::Processed,
    },
];

/// The whole `StatusReason` the macro's failure carries for `causes`.
fn transform_failure(count: usize, causes: &[&str]) -> String {
    format!(
        "Transform {TRANSFORM} failed with: Invalid Serverless Application Specification \
         document. Number of errors found: {count}. {}",
        causes.join(" ")
    )
}

/// Poll one change set until it reports a status that is no longer pending, and answer that
/// status with the answer it was read from.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("StackName", stack), ("ChangeSetName", change_set)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DescribeChangeSet to answer, got {code} with {body}"
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        if status != CREATE_PENDING && status != CREATE_IN_PROGRESS {
            return (status, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the change set to settle, last was {status:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The logical ids the change set plans a change for.
fn planned_ids(body: &str) -> Vec<String> {
    members_of(body)
        .into_iter()
        .filter_map(|member| opt_element(member, "LogicalResourceId"))
        .collect()
}

#[tokio::test]
async fn an_invalid_sam_document_fails_its_change_set_and_materialises_nothing() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let change_set = format!("{}-plan", row.stack);
        let (code, body) = cloudformation(
            &router,
            "CreateChangeSet",
            &[
                ("StackName", row.stack),
                ("ChangeSetName", &change_set),
                ("ChangeSetType", "CREATE"),
                ("TemplateBody", &(row.template)()),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: a transform failure is not a bad request — expected \
             CreateChangeSet to be accepted, got {code} with {body}"
        );

        let (status, described) = settled(&router, case, row.stack, &change_set).await;
        // Read through the reader that undoes the serializer's escaping: the diagnostic
        // quotes a property name, which XML carries escaped.
        let reason = elements(&described, "StatusReason")
            .into_iter()
            .next()
            .unwrap_or_default();
        match &row.expected {
            Expected::Processed => {
                assert_eq!(
                    status, "CREATE_COMPLETE",
                    "case {case:?}: expected the change set to be executable, got {status:?} \
                     with reason {reason:?}"
                );
                assert!(
                    planned_ids(&described).contains(&"Fn".to_string()),
                    "case {case:?}: expected the plan to carry the function the macro \
                     expanded, got {:?}",
                    planned_ids(&described)
                );
                continue;
            }
            Expected::Faults(count, causes) => {
                let expected = transform_failure(*count, causes);
                assert_eq!(
                    status, "FAILED",
                    "case {case:?}: expected the change set to fail, got {status:?} with \
                     reason {reason:?}"
                );
                assert_eq!(
                    reason, expected,
                    "case {case:?}: expected the reason {expected:?}, got {reason:?}"
                );
            }
            Expected::Unrecognised(resource_type) => {
                let expected =
                    format!("Template format error: Unrecognized resource types: [{resource_type}]");
                assert_eq!(
                    status, "FAILED",
                    "case {case:?}: expected the change set to fail, got {status:?} with \
                     reason {reason:?}"
                );
                assert_eq!(
                    reason, expected,
                    "case {case:?}: expected the reason {expected:?}, got {reason:?}"
                );
            }
        }

        // A change set the transform failed on offers no plan at all...
        assert!(
            planned_ids(&described).is_empty(),
            "case {case:?}: expected no change to be planned, got {:?}",
            planned_ids(&described)
        );
        // ...its shell stack is left in review with nothing deployed...
        let (code, stack_body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the shell stack to be described, got {code} with \
             {stack_body}"
        );
        assert_eq!(
            opt_element(&stack_body, "StackStatus").as_deref(),
            Some(REVIEW_IN_PROGRESS),
            "case {case:?}: expected the shell stack to stay in review, got {stack_body}"
        );
        // ...and nothing the expansion would have created exists in the service that would
        // hold it: the function is still absent in AWS Lambda's own API.
        let (code, answered) = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION_NAME}"),
        )
        .await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected AWS Lambda to hold no such function, got {code} with \
             {answered}"
        );
    }
}
