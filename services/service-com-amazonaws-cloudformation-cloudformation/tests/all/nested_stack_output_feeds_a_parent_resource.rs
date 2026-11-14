// SPDX-License-Identifier: CC0-1.0

//! What a declared `AWS::Lambda::Function`'s own configuration carries in AWS Lambda, for
//! the properties whose values the TEMPLATE computes.
//!
//! RULE: the document a stack creates a resource with is its declared `Properties` with every
//! intrinsic function evaluated, so a property whose value reads another resource of the same
//! template carries that resource's published value — and the owning service's own read
//! reports it back. For an `AWS::Lambda::Function` that read is `GetFunctionConfiguration`,
//! whose `Environment.Variables` map holds exactly the pairs the declaration stated, with the
//! identifiers the stack published substituted in. A client that deploys a function by
//! template and then resolves a table name, a queue URL or an endpoint out of
//! `Environment.Variables` reads the value the template computed rather than a member the
//! create dropped.
//!
//! The rows differ along what the rule generalises over: WHERE in the declaration the
//! reference sits (a map value, a map value joined with text of the template's own, and a
//! scalar property of the resource itself), WHICH intrinsic computes it (`Ref`, `Fn::GetAtt`
//! in both of its spellings, `Fn::Join`, `Fn::Sub`), and how many variables the environment
//! carries beside it (one, several, and one declared with a literal).
//!
//! Every expected value is the one the stack itself publishes through an output of its own,
//! read back over the wire, so no row restates how a resource type is named.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::hotswapped_function_code::lambda;
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, describe_stack, output_value, router, xml_text,
};

/// How long a case looks again for the stack to settle, and how often. Generous: it bounds
/// the spans the emulator owes a stack's resources, so it says nothing about how fast this
/// host runs.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The logical ids every template below declares.
const TABLE: &str = "Items";
const ROLE: &str = "HandlerRole";
const FUNCTION: &str = "Handler";

/// The handler the declared function carries. Never invoked here: what is under test is the
/// configuration the function was created with.
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// The literal one row declares beside the computed variables, which the configuration must
/// report as declared.
const LITERAL_VARIABLE: (&str, &str) = ("STAGE", "prod");

/// The text a joined row puts before the table's name, which is the template's own.
const JOINED_PREFIX: &str = "table-";

/// One row: what the function's declaration carries beside its runtime, and what the
/// configuration must then report, as `(member path, the output the value comes from, the
/// prefix the template adds)`.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Environment.Variables` the declaration states.
    variables: Value,
    /// The `Description` the declaration states, if it states one.
    description: Option<Value>,
    /// What each declared variable must report: the variable's name, the stack output whose
    /// value it carries, and the text the template puts before that value.
    expected: Vec<(&'static str, &'static str, &'static str)>,
    /// What `Description` must report, as the same triple without a name.
    expected_description: Option<(&'static str, &'static str)>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one variable reading the table's Ref",
            stack: "function-environment-ref",
            variables: json!({"TABLE_NAME": {"Ref": TABLE}}),
            description: None,
            expected: vec![("TABLE_NAME", "TableName", "")],
            expected_description: None,
        },
        Row {
            case: "several variables, one of them a literal",
            stack: "function-environment-several",
            variables: json!({
                "TABLE_NAME": {"Ref": TABLE},
                "TABLE_ARN": {"Fn::GetAtt": [TABLE, "Arn"]},
                "ROLE_ARN": {"Fn::GetAtt": format!("{ROLE}.Arn")},
                LITERAL_VARIABLE.0: LITERAL_VARIABLE.1,
            }),
            description: None,
            expected: vec![
                ("TABLE_NAME", "TableName", ""),
                ("TABLE_ARN", "TableArn", ""),
                ("ROLE_ARN", "RoleArn", ""),
            ],
            expected_description: None,
        },
        Row {
            case: "a variable joining the table's name with text of the template's own",
            stack: "function-environment-joined",
            variables: json!({
                "TABLE_NAME": {"Fn::Join": ["", [JOINED_PREFIX, {"Ref": TABLE}]]}
            }),
            description: None,
            expected: vec![("TABLE_NAME", "TableName", JOINED_PREFIX)],
            expected_description: None,
        },
        Row {
            case: "a scalar property computed beside the environment",
            stack: "function-environment-and-description",
            variables: json!({"TABLE_NAME": {"Ref": TABLE}}),
            description: Some(json!({"Fn::Sub": format!("{JOINED_PREFIX}${{{TABLE}}}")})),
            expected: vec![("TABLE_NAME", "TableName", "")],
            expected_description: Some(("TableName", JOINED_PREFIX)),
        },
    ]
}

/// One row's template: the table the declaration reads, the function's execution role, and
/// the function itself, with the identifiers the row's assertions read published as outputs.
fn template(row: &Row) -> String {
    let mut properties = json!({
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
        "Code": {"ZipFile": HANDLER_SOURCE},
        "Environment": {"Variables": row.variables.clone()},
    });
    if let Some(description) = &row.description {
        properties["Description"] = description.clone();
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            TABLE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                },
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            FUNCTION: {"Type": "AWS::Lambda::Function", "Properties": properties},
        },
        "Outputs": {
            "TableName": {"Value": {"Ref": TABLE}},
            "TableArn": {"Value": {"Fn::GetAtt": [TABLE, "Arn"]}},
            "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
            "FunctionName": {"Value": {"Ref": FUNCTION}},
        },
    })
    .to_string()
}

/// The `DescribeStacks` body at the first read that reports `stack` out of
/// `CREATE_IN_PROGRESS`.
async fn described_once_settled(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, described) = describe_stack(router, stack).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stack to be readable, got {status} {described}"
        );
        if xml_text(&described, "StackStatus").as_deref() != Some("CREATE_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the create to settle, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

#[tokio::test]
async fn a_declared_functions_computed_configuration_is_what_lambda_reports() {
    let router = router();

    for row in rows() {
        let case = row.case;
        let (status, body) =
            create_stack(&router, row.stack, &template(&row), &acknowledging_iam(&[])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status} {body}"
        );

        let described = described_once_settled(case, &router, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {described}"
        );

        // What the stack itself publishes for the resources the declaration read.
        let published = |key: &str| -> String {
            output_value(&described, key).unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish {key:?}, got {described}")
            })
        };
        let function = published("FunctionName");

        // And what AWS Lambda holds for the function the stack created.
        let answer = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            b"",
            &[],
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetFunctionConfiguration({function}) must answer, got {} {}",
            answer.status, answer.body
        );

        let variables = &answer.body["Environment"]["Variables"];
        assert!(
            variables.is_object(),
            "case {case:?}: the configuration must report the declared environment, got {}",
            answer.body
        );
        for (name, output, prefix) in &row.expected {
            let expected = format!("{prefix}{}", published(output));
            assert_eq!(
                variables[*name].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected {name} to carry {expected:?}, got {}",
                answer.body
            );
        }
        // A variable the declaration stated as a literal is reported as declared, and no
        // variable the declaration did not state is reported at all.
        if row.variables.get(LITERAL_VARIABLE.0).is_some() {
            assert_eq!(
                variables[LITERAL_VARIABLE.0].as_str(),
                Some(LITERAL_VARIABLE.1),
                "case {case:?}: expected the literal {} to be reported as declared, got {}",
                LITERAL_VARIABLE.0,
                answer.body
            );
        }
        let declared_names = row
            .variables
            .as_object()
            .expect("the row declares a variable map")
            .len();
        assert_eq!(
            variables
                .as_object()
                .expect("the configuration reports a map")
                .len(),
            declared_names,
            "case {case:?}: expected exactly the {declared_names} declared variables, got {}",
            answer.body
        );

        match row.expected_description {
            Some((output, prefix)) => {
                let expected = format!("{prefix}{}", published(output));
                assert_eq!(
                    answer.body["Description"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the Description to carry {expected:?}, got {}",
                    answer.body
                );
            }
            // A function declared with no description reports the empty one AWS reports for
            // it, never a value the template did not state.
            None => assert_eq!(
                answer.body["Description"].as_str(),
                Some(""),
                "case {case:?}: expected no declared description, got {}",
                answer.body
            ),
        }
    }
}
