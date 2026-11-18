// SPDX-License-Identifier: CC0-1.0

//! A nested stack's output CONFIGURES a resource of its parent: the parent's function is
//! created with the environment variable the child published, and the table that variable
//! names is a table Amazon DynamoDB really holds.
//!
//! The rule under test: **a resource property that reads another resource's `Fn::GetAtt`
//! attribute is sent to the owning service with that attribute's value substituted, wherever
//! in the property's structure the read sits and whichever spelling reads it — including an
//! `AWS::CloudFormation::Stack`'s `Outputs.<key>`, which is published only once the child
//! stack has finished coming up.**
//!
//! Three crates have to agree for that to hold — CloudFormation creates the child stack and
//! then the parent's function, Amazon S3 holds the child template the parent points at, AWS
//! Lambda holds the function whose configuration is read back, and Amazon DynamoDB owns the
//! table the child created — so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: WHICH spelling reads the child's
//! output (the two-part `Fn::GetAtt` array, the dotted short form, and the `${Name.Attribute}`
//! variable of an `Fn::Sub`), HOW MANY variables the map carries beside it, whether the read
//! is the whole value or one part of a composed string, and which of the child's outputs is
//! read (a name a `Ref` published and an ARN an `Fn::GetAtt` published).
//!
//! Nothing here sleeps for a span the emulator owes: every wait polls for the CONDITION with a
//! deadline far longer than the spans this emulator gives these transitions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The terminal status a deployment that worked reaches.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a condition the clock owes is waited for, and how often it is looked at.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The bucket the child templates are staged in.
const STAGING_BUCKET: &str = "nested-output-configures-parent";

/// A handler that does nothing: what the function DOES is not under test here, only the
/// configuration CloudFormation created it with.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the child stack the parent creates, the
/// function it configures and the table the child declares are all served by this one router.
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

/// One CloudFormation call over its awsQuery wire (API version 2010-05-15).
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

/// One AWS Lambda `restJson1` call, as its status and its parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body())
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(response.body()).into_owned()))
    };
    (status, parsed)
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("dynamodb.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("dynamodb"))
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Stage one child template in Amazon S3, which is where a nested stack's `TemplateURL` points.
async fn stage(router: &MultiServiceRouter, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "setup: staging {key:?} must be accepted, got {status} with {answered}"
    );
}

/// The URL a staged child template is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// The CHILD template every row stages: one Amazon DynamoDB table, with its name and its ARN
/// published as outputs — the two kinds of published value a parent reads back.
fn child_template() -> String {
    json!({
        "Resources": {
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [
                        {"AttributeName": "pk", "AttributeType": "S"},
                    ],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            },
        },
        "Outputs": {
            "TableName": {"Value": {"Ref": "Table"}},
            "TableArn": {"Value": {"Fn::GetAtt": ["Table", "Arn"]}},
        },
    })
    .to_string()
}

/// How one row's environment variable reads the child's output.
#[derive(Debug, Clone, Copy)]
enum Spelling {
    /// `{"Fn::GetAtt": ["Child", "Outputs.<key>"]}` — the two-part array form.
    GetAttArray,
    /// `{"Fn::GetAtt": "Child.Outputs.<key>"}` — the dotted short form.
    GetAttDotted,
    /// `{"Fn::Sub": "…${Child.Outputs.<key>}…"}` — the same read spelled inside a string, so
    /// the value is one PART of a composed string rather than the whole of it.
    SubbedIntoText,
}

/// One row: how the parent's function reads the child's output into its environment.
struct Row {
    case: &'static str,
    stack: &'static str,
    key: &'static str,
    /// The child output the environment variable is built from.
    output: &'static str,
    /// The variable name that carries it.
    variable: &'static str,
    /// How it is read.
    spelling: Spelling,
    /// The literal variables the map carries BESIDE it.
    literals: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "the two-part Fn::GetAtt array, the only variable",
        stack: "nested-output-env-array",
        key: "env-array.json",
        output: "TableName",
        variable: "TABLE_NAME",
        spelling: Spelling::GetAttArray,
        literals: &[],
    },
    Row {
        case: "the dotted Fn::GetAtt short form, beside two literal variables",
        stack: "nested-output-env-dotted",
        key: "env-dotted.json",
        output: "TableName",
        variable: "TABLE_NAME",
        spelling: Spelling::GetAttDotted,
        literals: &[("STAGE", "prod"), ("LOG_LEVEL", "info")],
    },
    Row {
        case: "an Fn::Sub variable reading the child's ARN output into composed text",
        stack: "nested-output-env-subbed",
        key: "env-subbed.json",
        output: "TableArn",
        variable: "TABLE_TARGET",
        spelling: Spelling::SubbedIntoText,
        literals: &[("STAGE", "beta")],
    },
];

/// The logical id the parent gives its nested stack, and the one it gives its function.
const CHILD_LOGICAL_ID: &str = "Data";
const FUNCTION_LOGICAL_ID: &str = "Writer";

/// The text an [`Spelling::SubbedIntoText`] row wraps the read in, so the assertion is about a
/// value that was COMPOSED from the child's output rather than copied from it.
const SUB_PREFIX: &str = "target=";

/// What the row's variable must end up carrying, given the value the child published for the
/// output it reads.
fn expected_variable(row: &Row, published: &str) -> String {
    match row.spelling {
        Spelling::GetAttArray | Spelling::GetAttDotted => published.to_string(),
        Spelling::SubbedIntoText => format!("{SUB_PREFIX}{published}"),
    }
}

/// The PARENT template: the function's execution role, the nested stack, and the function whose
/// environment reads the child's output.
fn parent_template(row: &Row) -> String {
    let read = match row.spelling {
        Spelling::GetAttArray => {
            json!({"Fn::GetAtt": [CHILD_LOGICAL_ID, format!("Outputs.{}", row.output)]})
        }
        Spelling::GetAttDotted => {
            json!({"Fn::GetAtt": format!("{CHILD_LOGICAL_ID}.Outputs.{}", row.output)})
        }
        Spelling::SubbedIntoText => json!({
            "Fn::Sub": format!(
                "{SUB_PREFIX}${{{CHILD_LOGICAL_ID}.Outputs.{}}}",
                row.output
            )
        }),
    };
    let mut variables = serde_json::Map::new();
    variables.insert(row.variable.to_string(), read);
    for (name, value) in row.literals {
        variables.insert((*name).to_string(), json!(value));
    }
    json!({
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            CHILD_LOGICAL_ID: {
                "Type": "AWS::CloudFormation::Stack",
                "Properties": {"TemplateURL": staged_url(row.key)},
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": HANDLER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 30,
                    "Environment": {"Variables": variables},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": FUNCTION_LOGICAL_ID}},
            // The same read, in the parent's OWN outputs: an output is evaluated once the
            // records are settled, so this says whether the child's attribute was published at
            // all — separating that from whether the function's property was resolved with it.
            "ReadBack": {
                "Value": {"Fn::GetAtt": [CHILD_LOGICAL_ID, format!("Outputs.{}", row.output)]},
            },
        },
    })
    .to_string()
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer that status
/// with the answer it was read from.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DescribeStacks to answer for {stack:?}, got {code} {body}"
        );
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.is_empty() && !status.ends_with("_IN_PROGRESS") {
            return (status, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {status:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource the stack reports a FAILED status for, with the reason it gave — quoted when
/// a deployment did not complete.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    if code != 200 {
        return Vec::new();
    }
    crate::query_wire::members_of(&body)
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

#[tokio::test]
async fn a_parent_function_is_configured_with_what_its_nested_stack_published() {
    let router = build_router();
    let (code, body) = s3(&router, "PUT", &format!("/{STAGING_BUCKET}"), "").await;
    assert!(
        (200..300).contains(&code),
        "setup: the staging bucket must be created, got {code} with {body}"
    );

    for row in ROWS {
        let case = row.case;
        stage(&router, row.key, &child_template()).await;
        let (code, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &parent_template(row)),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} with {body}"
        );

        let (status, described) = settled(&router, case, row.stack).await;
        assert_eq!(
            status,
            CREATE_COMPLETE,
            "case {case:?}: expected the parent to reach {CREATE_COMPLETE}, got {status:?} with \
             {described}; failures: {:?}",
            reported_failures(&router, row.stack).await
        );

        // What the CHILD published, read from the child stack itself — the value the parent's
        // function had to be configured with, taken from the stack that published it rather
        // than from the configuration under test.
        let (code, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the parent's resources to be reported, got {code} {resources}"
        );
        let physical_ids = physical_ids_of(&resources);
        let child_stack = physical_ids
            .get(CHILD_LOGICAL_ID)
            .cloned()
            .unwrap_or_default();
        assert!(
            !child_stack.is_empty(),
            "case {case:?}: expected the nested stack resource to carry its child's id, got \
             {physical_ids:?}"
        );
        let (code, child_body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", &child_stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the child stack {child_stack:?} to be described, got {code} \
             {child_body}"
        );
        let published = outputs_of(&child_body);
        let child_value = published.get(row.output).cloned().unwrap_or_default();
        assert!(
            !child_value.is_empty(),
            "case {case:?}: expected the child to publish {:?}, got {published:?}",
            row.output
        );

        // The parent's OWN output over the same read: the child's attribute IS published, so
        // what the function was configured with is the only question left.
        let read_back = outputs_of(&described).get("ReadBack").cloned();
        assert_eq!(
            read_back.as_deref(),
            Some(child_value.as_str()),
            "case {case:?}: expected the parent's own output to report the child's {:?}, got \
             {read_back:?} in {described}",
            row.output
        );

        // The function AWS Lambda holds, read through its own API: the environment it was
        // created with is what the parent's declaration asked for, with the child's published
        // value substituted.
        let function = outputs_of(&described)
            .get("FunctionName")
            .cloned()
            .unwrap_or_default();
        assert!(
            !function.is_empty(),
            "case {case:?}: expected the parent to publish its function's name, got {described}"
        );
        let (code, configuration) = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected AWS Lambda to report {function:?}, got {code} {configuration}"
        );
        let reported: BTreeMap<String, String> = configuration
            .get("Environment")
            .and_then(|environment| environment.get("Variables"))
            .and_then(Value::as_object)
            .map(|variables| {
                variables
                    .iter()
                    .map(|(name, value)| {
                        (name.clone(), value.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut expected: BTreeMap<String, String> = row
            .literals
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        expected.insert(
            row.variable.to_string(),
            expected_variable(row, &child_value),
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the function's environment to be {expected:?}, got \
             {reported:?} in {configuration}"
        );

        // ...and the table that variable names is one Amazon DynamoDB really holds, so the
        // value the parent was configured with addresses a resource rather than reading as one.
        let table = published.get("TableName").cloned().unwrap_or_default();
        let (code, table_body) =
            dynamodb(&router, "DescribeTable", &json!({"TableName": table})).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected Amazon DynamoDB to hold the child's table {table:?}, got \
             {code} {table_body}"
        );
    }
}
