// SPDX-License-Identifier: CC0-1.0

//! The outputs of a stack whose resources live in THREE other services — Amazon DynamoDB,
//! AWS Lambda and Amazon CloudWatch Logs — read back over the awsQuery wire.
//!
//! RULE: once a stack's deployment has reached a terminal `*_COMPLETE` status,
//! `DescribeStacks` reports every output the template declares with BOTH members of the
//! modelled `Output` shape populated — `OutputKey` and the `OutputValue` the declared `Value`
//! resolves to against the stack's parameters, its pseudo parameters and the identities the
//! services that own its resources minted — so the identifier a client reads out of an output
//! is one the owning service answers its own read for.
//!
//! That is the whole purpose of a stack output: a deployment hands the next step the physical
//! ids of what it created through them. An entry carrying a key and no value makes the
//! stack's contract unreadable — the standard `{o["OutputKey"]: o["OutputValue"]}` read
//! raises on it — and CloudFormation reports nothing to explain it.
//!
//! The axis this suite owns is the RESOLUTION of each published identifier in the service
//! that holds the resource — `stack_outputs.rs` and `stack_output_resolution.rs` already span
//! the intrinsic functions and the template spellings an output's `Value` can be written in,
//! so nothing here re-checks those on their own account.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES each of
//! the three resources or leaves CloudFormation to name it (so one row's expected values are
//! the declared names and another's are ids only the outputs can reveal), the runtime family
//! the function is declared with, the log group's retention (including a row declaring none),
//! and the template spelling the row writes all of that in. Each row publishes its outputs
//! through every form a value is spelled in: a `Ref` to a resource, a `Fn::GetAtt` of a
//! published attribute, a `Ref` to a pseudo parameter, an `Fn::Sub` and an `Fn::Join`
//! composing two resources' ids, and a plain literal carrying a description and an export
//! name.
//!
//! Every case drives the real request path for all four services through one router — the
//! CloudFormation calls over the awsQuery wire (the harness `stack_lifecycle.rs` owns), and
//! the DynamoDB, AWS Lambda and CloudWatch Logs calls as a client's SDK makes them — so what
//! is asserted is what a client that deploys the template and then resolves each published
//! identifier observes. The function-shaped halves of that — the poll for a settled stack, the
//! `GetFunction` call, the execution role's declaration and the inline source per runtime —
//! are the ones `stack_declared_function_identity.rs` owns, reused rather than copied.

use serde_json::{json, Value};

use crate::dynamodb_table_resources::describe_table;
use crate::logs_and_iot_resources::described_group;
use crate::stack_declared_function_identity::{
    get_function, inline_source, settled_stack, trust_policy, MANAGED_POLICY_ARN, STATE_ACTIVE,
};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, output_value, router, ACCOUNT_ID, REGION,
};
use crate::stack_outputs::{output_for, wire_outputs};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The output keys every row's template declares, and the forms they are declared in.
const TABLE_NAME: &str = "TableName";
const TABLE_ARN: &str = "TableArn";
const FUNCTION_NAME: &str = "FunctionName";
const FUNCTION_ARN: &str = "FunctionArn";
const GROUP_NAME: &str = "LogGroupName";
const GROUP_ARN: &str = "LogGroupArn";
const ROLE_ARN: &str = "RoleArn";
const STACK_REGION: &str = "StackRegion";
const REPORTED_STACK_NAME: &str = "ReportedStackName";
/// An `Fn::Sub` over two resources' `Ref`s, and an `Fn::Join` over the same two.
const WIRING: &str = "Wiring";
const JOINED: &str = "Joined";
/// A literal, which also carries the two optional members of `Output`.
const FIXED: &str = "Fixed";

/// The value the literal output declares, and the description and export name beside it.
const FIXED_VALUE: &str = "processing-pipeline";
const FIXED_DESCRIPTION: &str = "the pipeline this stack deploys";

/// Every output key a row's stack must report.
const DECLARED_OUTPUTS: &[&str] = &[
    TABLE_NAME,
    TABLE_ARN,
    FUNCTION_NAME,
    FUNCTION_ARN,
    GROUP_NAME,
    GROUP_ARN,
    ROLE_ARN,
    STACK_REGION,
    REPORTED_STACK_NAME,
    WIRING,
    JOINED,
    FIXED,
];

/// How one row spells its template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// JSON, long-form intrinsics.
    Json,
    /// YAML, short-form intrinsics (`!Ref`, `!GetAtt`, `!Sub`, `!Join`).
    Yaml,
}

/// One row of the table.
struct Case<'a> {
    case: &'a str,
    stack: &'a str,
    spelling: Spelling,
    /// The `TableName` the declaration states, or `None` to leave the naming to
    /// CloudFormation.
    declared_table: Option<&'a str>,
    /// The `FunctionName` the declaration states, or `None` to leave the naming to
    /// CloudFormation.
    declared_function: Option<&'a str>,
    /// The `LogGroupName` the declaration states, or `None` to leave the naming to
    /// CloudFormation.
    declared_group: Option<&'a str>,
    /// The `RetentionInDays` the group is declared with, where the row declares one.
    retention: Option<i64>,
    runtime: &'a str,
    handler: &'a str,
}

fn cases() -> Vec<Case<'static>> {
    vec![
        Case {
            // The issue's own shape: a table, the function that processes it and the log
            // group that function writes to, every one of them NAMED by the template, with
            // the outputs a client reads the deployment's contract out of.
            case: "a named table, function and log group in one JSON template",
            stack: "pipeline-named",
            spelling: Spelling::Json,
            declared_table: Some("pipeline-events"),
            declared_function: Some("pipeline-processor"),
            declared_group: Some("/aws/lambda/pipeline-processor"),
            retention: Some(14),
            runtime: "python3.12",
            handler: "index.handler",
        },
        Case {
            // Nothing named: CloudFormation decides all three physical ids, and the outputs
            // are the only way a client learns them.
            case: "a table, function and log group CloudFormation names, in YAML",
            stack: "pipeline-generated",
            spelling: Spelling::Yaml,
            declared_table: None,
            declared_function: None,
            declared_group: None,
            retention: None,
            runtime: "nodejs20.x",
            handler: "index.handler",
        },
        Case {
            // Mixed: the table is named and the function is not, and the group is declared
            // at a name of its own rather than at the function's.
            case: "a named table beside a function CloudFormation names",
            stack: "pipeline-mixed",
            spelling: Spelling::Json,
            declared_table: Some("pipeline-ledger"),
            declared_function: None,
            declared_group: Some("/pipeline/mixed"),
            retention: Some(1),
            runtime: "ruby3.3",
            handler: "index.handler",
        },
    ]
}

/// The JSON template a row deploys.
fn json_template(case: &Case<'_>) -> String {
    let mut table = json!({
        "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if let Some(name) = case.declared_table {
        table["TableName"] = json!(name);
    }
    let mut group = json!({});
    if let Some(name) = case.declared_group {
        group["LogGroupName"] = json!(name);
    }
    if let Some(days) = case.retention {
        group["RetentionInDays"] = json!(days);
    }
    let mut function = json!({
        "Runtime": case.runtime,
        "Handler": case.handler,
        "Role": {"Fn::GetAtt": ["Role", "Arn"]},
        "Code": {"ZipFile": inline_source(case.runtime)},
        "Environment": {"Variables": {"TABLE": {"Ref": "Table"}}},
    });
    if let Some(name) = case.declared_function {
        function["FunctionName"] = json!(name);
    }
    // The outputs are inserted under the SAME constants the assertions read, so a key cannot
    // drift between the template and what is checked.
    let declared: Vec<(&str, Value)> = vec![
        (TABLE_NAME, json!({"Value": {"Ref": "Table"}})),
        (TABLE_ARN, json!({"Value": {"Fn::GetAtt": ["Table", "Arn"]}})),
        (FUNCTION_NAME, json!({"Value": {"Ref": "Processor"}})),
        (
            FUNCTION_ARN,
            json!({"Value": {"Fn::GetAtt": ["Processor", "Arn"]}}),
        ),
        (GROUP_NAME, json!({"Value": {"Ref": "Group"}})),
        (GROUP_ARN, json!({"Value": {"Fn::GetAtt": ["Group", "Arn"]}})),
        (ROLE_ARN, json!({"Value": {"Fn::GetAtt": ["Role", "Arn"]}})),
        (STACK_REGION, json!({"Value": {"Ref": "AWS::Region"}})),
        (
            REPORTED_STACK_NAME,
            json!({"Value": {"Ref": "AWS::StackName"}}),
        ),
        (
            WIRING,
            json!({"Value": {"Fn::Sub": "${Table}|${Processor}|${Group}"}}),
        ),
        (
            JOINED,
            json!({"Value": {"Fn::Join": ["/", [{"Ref": "Table"}, {"Ref": "Processor"}]]}}),
        ),
        (
            FIXED,
            json!({
                "Value": FIXED_VALUE,
                "Description": FIXED_DESCRIPTION,
                "Export": {"Name": {"Fn::Sub": "${AWS::StackName}-pipeline"}},
            }),
        ),
    ];
    let outputs: serde_json::Map<String, Value> = declared
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    json!({
        "Resources": {
            "Table": {"Type": "AWS::DynamoDB::Table", "Properties": table},
            "Role": {"Type": "AWS::IAM::Role", "Properties": {
                "AssumeRolePolicyDocument": trust_policy(),
                "ManagedPolicyArns": [MANAGED_POLICY_ARN],
            }},
            "Group": {"Type": "AWS::Logs::LogGroup", "Properties": group},
            "Processor": {"Type": "AWS::Lambda::Function", "Properties": function,
                          "DependsOn": "Group"},
        },
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The YAML template a row deploys, in the short-form intrinsics a hand-written template
/// uses.
fn yaml_template(case: &Case<'_>) -> String {
    let mut group = String::from("  Group:\n    Type: AWS::Logs::LogGroup\n");
    let mut properties = String::new();
    if let Some(name) = case.declared_group {
        properties.push_str(&format!("      LogGroupName: {name}\n"));
    }
    if let Some(days) = case.retention {
        properties.push_str(&format!("      RetentionInDays: {days}\n"));
    }
    if !properties.is_empty() {
        group.push_str("    Properties:\n");
        group.push_str(&properties);
    }
    let table_name = case
        .declared_table
        .map(|name| format!("      TableName: {name}\n"))
        .unwrap_or_default();
    let function_name = case
        .declared_function
        .map(|name| format!("      FunctionName: {name}\n"))
        .unwrap_or_default();
    let source = inline_source(case.runtime);
    format!(
        r#"Resources:
  Table:
    Type: AWS::DynamoDB::Table
    Properties:
{table_name}      BillingMode: PAY_PER_REQUEST
      KeySchema:
        - AttributeName: id
          KeyType: HASH
      AttributeDefinitions:
        - AttributeName: id
          AttributeType: S
  Role:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal:
              Service: lambda.amazonaws.com
            Action: sts:AssumeRole
      ManagedPolicyArns:
        - {MANAGED_POLICY_ARN}
{group}  Processor:
    Type: AWS::Lambda::Function
    DependsOn: Group
    Properties:
{function_name}      Runtime: {runtime}
      Handler: {handler}
      Role: !GetAtt Role.Arn
      Code:
        ZipFile: {source:?}
      Environment:
        Variables:
          TABLE: !Ref Table
Outputs:
  {TABLE_NAME}:
    Value: !Ref Table
  {TABLE_ARN}:
    Value: !GetAtt Table.Arn
  {FUNCTION_NAME}:
    Value: !Ref Processor
  {FUNCTION_ARN}:
    Value: !GetAtt Processor.Arn
  {GROUP_NAME}:
    Value: !Ref Group
  {GROUP_ARN}:
    Value: !GetAtt Group.Arn
  {ROLE_ARN}:
    Value: !GetAtt Role.Arn
  {STACK_REGION}:
    Value: !Ref AWS::Region
  {REPORTED_STACK_NAME}:
    Value: !Ref AWS::StackName
  {WIRING}:
    Value: !Sub "${{Table}}|${{Processor}}|${{Group}}"
  {JOINED}:
    Value: !Join ["/", [!Ref Table, !Ref Processor]]
  {FIXED}:
    Value: {FIXED_VALUE}
    Description: {FIXED_DESCRIPTION}
    Export:
      Name: !Sub "${{AWS::StackName}}-pipeline"
"#,
        runtime = case.runtime,
        handler = case.handler,
    )
}

fn template(case: &Case<'_>) -> String {
    match case.spelling {
        Spelling::Json => json_template(case),
        Spelling::Yaml => yaml_template(case),
    }
}

/// The `OutputValue` the stack reports for `key`, or a panic naming the case: a declared
/// output of a settled stack that carries no value is the defect this suite is about.
fn required(case: &str, described: &str, key: &str) -> String {
    output_value(described, key).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the output {key:?} of a settled stack must carry an OutputValue; \
             the reported outputs are {:?}",
            wire_outputs(described)
        )
    })
}

#[tokio::test]
async fn every_output_of_a_settled_cross_service_stack_resolves_to_a_live_identifier() {
    for case in cases() {
        let router = router();
        let name = case.case;
        let body = template(&case);
        let (code, created) = create_stack(&router, case.stack, &body, &acknowledging_iam(&[])).await;
        assert_eq!(
            code, 200,
            "case {name:?}: CreateStack must answer 200, got {code}: {created}"
        );

        let (reported_status, described) = settled_stack(&router, name, case.stack).await;
        assert_eq!(
            reported_status,
            status::CREATE_COMPLETE,
            "case {name:?}: the stack must reach CREATE_COMPLETE before its outputs are read, \
             got {reported_status:?} with {described}"
        );

        // The rule, over the WHOLE response: every declared output is reported, and every
        // reported output carries a non-empty `OutputValue`.
        let outputs = wire_outputs(&described);
        assert_eq!(
            outputs.len(),
            DECLARED_OUTPUTS.len(),
            "case {name:?}: expected one Output per declared output ({} of them), got {outputs:?}",
            DECLARED_OUTPUTS.len()
        );
        for declared in DECLARED_OUTPUTS {
            let Some(output) = output_for(&outputs, declared) else {
                panic!("case {name:?}: no Output carries OutputKey {declared:?}, got {outputs:?}");
            };
            assert!(
                output.value.as_deref().is_some_and(|value| !value.is_empty()),
                "case {name:?}: the Output {declared:?} carries no OutputValue, so the standard \
                 {{o[\"OutputKey\"]: o[\"OutputValue\"]}} read of this stack raises; got \
                 {output:?}"
            );
        }

        // Amazon DynamoDB holds the table the stack published, under that name, and the ARN
        // the stack published for it is the one that service reports.
        let table_name = required(name, &described, TABLE_NAME);
        if let Some(declared) = case.declared_table {
            assert_eq!(
                table_name, declared,
                "case {name:?}: Ref of the table must answer the declared TableName, got \
                 {table_name:?}"
            );
        }
        let Some(table) = describe_table(&router, &table_name).await else {
            panic!(
                "case {name:?}: Amazon DynamoDB must hold the table {table_name:?} the stack \
                 published, got none"
            );
        };
        let table_arn = required(name, &described, TABLE_ARN);
        assert_eq!(
            Some(table_arn.as_str()),
            table.get("TableArn").and_then(Value::as_str),
            "case {name:?}: the published table ARN must be the one DynamoDB reports, got \
             {table_arn:?} against {:?}",
            table.get("TableArn")
        );
        assert_eq!(
            table_arn,
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{table_name}"),
            "case {name:?}: the published table ARN must be the documented table ARN of \
             {table_name:?}, got {table_arn:?}"
        );

        // AWS Lambda holds the function the stack published, and reports it ready with the
        // execution role the stack published.
        let function_name = required(name, &described, FUNCTION_NAME);
        if let Some(declared) = case.declared_function {
            assert_eq!(
                function_name, declared,
                "case {name:?}: Ref of the function must answer the declared FunctionName, got \
                 {function_name:?}"
            );
        }
        let function_arn = required(name, &described, FUNCTION_ARN);
        assert_eq!(
            function_arn,
            format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{function_name}"),
            "case {name:?}: the published function ARN must be the function ARN of \
             {function_name:?}, got {function_arn:?}"
        );
        let (code, answered) = get_function(&router, &function_name).await;
        assert_eq!(
            code, 200,
            "case {name:?}: GetFunction({function_name:?}) must answer 200 for the function the \
             stack published, got {code}: {answered}"
        );
        let configuration = answered.get("Configuration").cloned().unwrap_or(Value::Null);
        assert_eq!(
            configuration.get("FunctionArn").and_then(Value::as_str),
            Some(function_arn.as_str()),
            "case {name:?}: AWS Lambda must report the ARN the stack published, got \
             {configuration}"
        );
        assert_eq!(
            configuration.get("State").and_then(Value::as_str),
            Some(STATE_ACTIVE),
            "case {name:?}: AWS Lambda must report the function ready once the stack is \
             complete, got {configuration}"
        );
        let role_arn = required(name, &described, ROLE_ARN);
        assert_eq!(
            configuration.get("Role").and_then(Value::as_str),
            Some(role_arn.as_str()),
            "case {name:?}: AWS Lambda must report the execution role the stack published, got \
             {configuration}"
        );
        assert!(
            role_arn.starts_with(&format!("arn:aws:iam::{ACCOUNT_ID}:role/")),
            "case {name:?}: the published role ARN must be an IAM role ARN, got {role_arn:?}"
        );

        // Amazon CloudWatch Logs holds the group the stack published, with the retention the
        // declaration asked for, and the ARN the stack published is the one that service
        // reports — which ends in `:*`, as the type's own return value documents.
        let group_name = required(name, &described, GROUP_NAME);
        if let Some(declared) = case.declared_group {
            assert_eq!(
                group_name, declared,
                "case {name:?}: Ref of the log group must answer the declared LogGroupName, got \
                 {group_name:?}"
            );
        }
        let Some(group) = described_group(&router, &group_name).await else {
            panic!(
                "case {name:?}: Amazon CloudWatch Logs must hold the group {group_name:?} the \
                 stack published, got none"
            );
        };
        let group_arn = required(name, &described, GROUP_ARN);
        assert_eq!(
            group_arn,
            format!("arn:aws:logs:{REGION}:{ACCOUNT_ID}:log-group:{group_name}:*"),
            "case {name:?}: the published log-group ARN must be the documented form for \
             {group_name:?}, got {group_arn:?}"
        );
        assert_eq!(
            group.get("arn").and_then(Value::as_str),
            Some(group_arn.as_str()),
            "case {name:?}: the published log-group ARN must be the one CloudWatch Logs reports, \
             got {group_arn:?} against {:?}",
            group.get("arn")
        );
        if let Some(days) = case.retention {
            assert_eq!(
                group.get("retentionInDays").and_then(Value::as_i64),
                Some(days),
                "case {name:?}: CloudWatch Logs must hold the declared retention, got {group}"
            );
        }

        // The pseudo parameters, and the two composed outputs: each is the composition of the
        // ids the stack published, which is what says the expression was EVALUATED rather
        // than copied.
        assert_eq!(
            required(name, &described, STACK_REGION),
            REGION,
            "case {name:?}: the Region output must be the Region the stack lives in"
        );
        assert_eq!(
            required(name, &described, REPORTED_STACK_NAME),
            case.stack,
            "case {name:?}: the StackName output must be the stack's own name"
        );
        assert_eq!(
            required(name, &described, WIRING),
            format!("{table_name}|{function_name}|{group_name}"),
            "case {name:?}: the Fn::Sub output must compose the three published ids"
        );
        assert_eq!(
            required(name, &described, JOINED),
            format!("{table_name}/{function_name}"),
            "case {name:?}: the Fn::Join output must join the two published ids"
        );

        // A literal output carries its value verbatim, and the two optional members of
        // `Output` beside it.
        let fixed = output_for(&outputs, FIXED).expect("the literal output is reported");
        assert_eq!(
            fixed.value.as_deref(),
            Some(FIXED_VALUE),
            "case {name:?}: the literal output must carry its declared value, got {fixed:?}"
        );
        assert_eq!(
            fixed.description.as_deref(),
            Some(FIXED_DESCRIPTION),
            "case {name:?}: the literal output must carry its declared Description, got {fixed:?}"
        );
        assert_eq!(
            fixed.export_name.as_deref(),
            Some(format!("{}-pipeline", case.stack).as_str()),
            "case {name:?}: the literal output must carry the export name its Fn::Sub resolves \
             to, got {fixed:?}"
        );
    }
}
