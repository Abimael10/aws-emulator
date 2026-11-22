// SPDX-License-Identifier: CC0-1.0

//! A whole order-intake application — a REST API whose method invokes a function, the grant
//! that admits it, the function's log group and the alarm that watches it — deployed in ONE
//! stack, end to end.
//!
//! RULE: a template CloudFormation accepts is deployed by calling the service that OWNS each
//! declared resource with a request that service's own model admits, so no resource of a
//! well-formed template fails on the owning service's deserialization or on its published
//! constraints and the stack reaches `CREATE_COMPLETE`. Two halves of that one rule are what
//! this suite spans:
//!
//! * an identifier CloudFormation GENERATES for a resource a template leaves unnamed
//!   (`<stack name>-<logical id>-<unique id>`) is FITTED to the maximum length the owning
//!   service publishes for the member it is sent as before the call is made — for an
//!   `AWS::Lambda::Permission` that member is `AddPermission.StatementId`, `@length(min: 1,
//!   max: 100)` with pattern `^([a-zA-Z0-9-_]+)$` (AWS Lambda's own Smithy model), and the
//!   type publishes no statement-id property, so the statement is always added under the
//!   generated identifier;
//! * a scalar a template carries is TEXT wherever its author quoted it and wherever it
//!   arrived through a `Ref` to a parameter, while the member of the owning call it reaches is
//!   modelled as a number, so the declaration is read as the TYPE that service's model states
//!   before the call is made rather than being handed on as a string.
//!
//! What that buys is the issue's own observable: the application EXISTS. A stack that fails
//! either half rolls back, and then nothing downstream of the deploy — the API serving its
//! stage, the grant that admits the invocation, the function's configuration, the log group,
//! the alarm's cadence — is there to be exercised at all.
//!
//! The rows differ along what the rule generalises over: the LENGTH of what is fitted (a
//! short stack name with a short logical id, well inside Lambda's bound; a CDK stack name
//! with the long logical id a construct generates for an API-method permission, past it; and
//! a long stack name with a 150-character logical id, far past it), and the FORM the numeric
//! properties are stated in (JSON numbers, quoted text, and a `Ref` to a parameter of each
//! declared type). The numbers reach THREE owning services — AWS Lambda, Amazon CloudWatch
//! and Amazon CloudWatch Logs — and every one of them is read back through that service's own
//! wire, not out of CloudFormation's record of the declaration.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_text, REGION,
};
use crate::logs_group_children_resources::logs;
use crate::stack_lifecycle::acknowledging_iam;

/// The longest `AddPermission.StatementId` AWS Lambda accepts: `StatementId`,
/// `@length(min: 1, max: 100)` with pattern `^([a-zA-Z0-9-_]+)$` (that service's own Smithy
/// model).
const STATEMENT_ID_MAX_CHARACTERS: usize = 100;

/// How many characters the unique segment of a generated identifier carries
/// (`<stack name>-<logical id>-<unique id>`, 12 hexadecimal characters), and the two hyphens
/// that join the three segments.
const UNIQUE_SEGMENT_CHARACTERS: usize = 12;
const SEPARATORS: usize = 2;

/// The sentence a JSON deserializer answers for a member it was handed text for where it
/// wanted a number — the report that says a declaration never reached the owning service's own
/// validation. No case here may ever see it.
const TYPE_ERROR: &str = "expected number, got string";

/// The clause a `@length` refusal carries, which is what an unfitted identifier would come
/// back as. No case here may ever see it either.
const LENGTH_CLAUSE: &str = "failed to satisfy constraint";

/// The stage the API is served at, and the path its method is declared under.
const STAGE: &str = "prod";
const PATH_PART: &str = "orders";

/// The handler the function is declared with: what is under test is the configuration it was
/// created with, so the source only has to be a function.
const HANDLER_SOURCE: &str = "def handler(event, context):\n    return {}\n";

/// One row: the stack the application is declared in, how long the permission's logical id is,
/// the forms its numeric properties are stated in, and the numbers each owning service must
/// then report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id the `AWS::Lambda::Permission` is declared under, whose length is what
    /// pushes the generated statement id past Lambda's bound.
    permission_logical_id: String,
    /// The names the row's own resources are declared under, so two rows of one table never
    /// read each other's records.
    function: &'static str,
    api_name: &'static str,
    log_group: &'static str,
    alarm: &'static str,
    /// `AWS::Lambda::Function`'s `MemorySize` and `Timeout`, as the template states them.
    memory_size: Value,
    timeout: Value,
    /// `AWS::Logs::LogGroup`'s `RetentionInDays`, as the template states it.
    retention: Value,
    /// `AWS::CloudWatch::Alarm`'s `Period`, `EvaluationPeriods` and `Threshold`, as the
    /// template states them.
    period: Value,
    evaluation_periods: Value,
    threshold: Value,
    /// The parameters the stack is created with, as `(name, declared type, value)`.
    parameters: &'static [(&'static str, &'static str, &'static str)],
    /// What the owning services must report, as the numbers their models state.
    expected_memory_size: i64,
    expected_timeout: i64,
    expected_retention: i64,
    expected_period: i64,
    expected_evaluation_periods: i64,
    expected_threshold: f64,
}

/// The logical id a CDK construct generates for the test permission of an API method, which is
/// the one the recorded deployment carries: a path, a verb, the construct's own name and the
/// hashes of the scopes it was minted under.
const CDK_PERMISSION_LOGICAL_ID: &str =
    "ApiordersPOSTApiPermissionTestOrderServiceStackApiDF2F37BDPOSTordersC0D5751F";

/// A logical id of `characters` characters starting with `prefix`, padded with the
/// alphanumerics a logical id is built from ("the logical ID must be alphanumeric (A-Za-z0-9)
/// and unique within the template").
fn logical_id(prefix: &str, characters: usize) -> String {
    let padding = "OrderServiceStackApiPermissionTestIntakeFunctionResource".repeat(4);
    let held = prefix.chars().count();
    assert!(
        characters >= held,
        "a row must ask for at least the {held} characters of {prefix:?}, got {characters}"
    );
    let mut id = prefix.to_string();
    id.push_str(&padding[..characters - held]);
    id
}

/// The rows, from one inside Lambda's bound to one far past it.
///
/// The declared numbers are inside every member's own published range: `MemorySize` 128..32768
/// and `Timeout` 1..5400 (`smithy_query(query="operation",operation="CreateFunction")`),
/// `retentionInDays` one of the values `PutRetentionPolicy` publishes, and an alarm's `Period`
/// a multiple of 60 (`smithy_query(query="operation",operation="PutMetricAlarm")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "short names, with every number stated as a JSON number",
            stack: "order-intake-plain",
            permission_logical_id: "IntakePermission".to_string(),
            function: "order-intake-plain-fn",
            api_name: "order-intake-plain-api",
            log_group: "/orders/intake-plain",
            alarm: "order-intake-plain-errors",
            memory_size: json!(256),
            timeout: json!(10),
            retention: json!(7),
            period: json!(60),
            evaluation_periods: json!(1),
            threshold: json!(1),
            parameters: &[],
            expected_memory_size: 256,
            expected_timeout: 10,
            expected_retention: 7,
            expected_period: 60,
            expected_evaluation_periods: 1,
            expected_threshold: 1.0,
        },
        Row {
            case: "a CDK stack name with the long logical id a construct generates for an API \
                   method's permission, every number quoted",
            stack: "OrderServiceStack-cdk-assetfree",
            permission_logical_id: CDK_PERMISSION_LOGICAL_ID.to_string(),
            function: "order-intake-cdk-fn",
            api_name: "order-intake-cdk-api",
            log_group: "/orders/intake-cdk",
            alarm: "order-intake-cdk-errors",
            memory_size: json!("512"),
            timeout: json!("30"),
            retention: json!("14"),
            period: json!("300"),
            evaluation_periods: json!("2"),
            threshold: json!("0.5"),
            parameters: &[],
            expected_memory_size: 512,
            expected_timeout: 30,
            expected_retention: 14,
            expected_period: 300,
            expected_evaluation_periods: 2,
            expected_threshold: 0.5,
        },
        Row {
            case: "a long stack name with a 150-character logical id, every number arriving \
                   through a Ref to a parameter",
            stack: "platform-production-order-intake-service-deployment-stack",
            permission_logical_id: logical_id("ApiPermissionTest", 150),
            function: "order-intake-parameterised-fn",
            api_name: "order-intake-parameterised-api",
            log_group: "/orders/intake-parameterised",
            alarm: "order-intake-parameterised-errors",
            memory_size: json!({"Ref": "Memory"}),
            timeout: json!({"Ref": "Seconds"}),
            retention: json!({"Ref": "Retention"}),
            period: json!({"Ref": "Period"}),
            evaluation_periods: json!({"Ref": "Evaluations"}),
            threshold: json!({"Ref": "Budget"}),
            parameters: &[
                ("Memory", "Number", "1024"),
                ("Seconds", "String", "7"),
                ("Retention", "String", "30"),
                ("Period", "Number", "900"),
                ("Evaluations", "String", "3"),
                ("Budget", "Number", "2.5"),
            ],
            expected_memory_size: 1024,
            expected_timeout: 7,
            expected_retention: 30,
            expected_period: 900,
            expected_evaluation_periods: 3,
            expected_threshold: 2.5,
        },
    ]
}

/// The template one row deploys: the execution role, the intake function, its log group, the
/// REST API with the `POST /orders` method that invokes the function, the permission that
/// admits API Gateway — declared under the row's logical id and stating NO statement id of its
/// own — the deployment and stage that serve the method, and the alarm over the function's
/// errors.
fn template(row: &Row) -> String {
    let mut resources = json!({
        "HandlerRole": {
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
        "IntakeFunction": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": row.function,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["HandlerRole", "Arn"]},
                "MemorySize": row.memory_size,
                "Timeout": row.timeout,
                "Code": {"ZipFile": HANDLER_SOURCE},
            },
        },
        "IntakeLogs": {
            "Type": "AWS::Logs::LogGroup",
            "Properties": {
                "LogGroupName": row.log_group,
                "RetentionInDays": row.retention,
            },
        },
        "Api": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": row.api_name,
                "EndpointConfiguration": {"Types": ["REGIONAL"]},
            },
        },
        "Orders": {
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": {"Ref": "Api"},
                "ParentId": {"Fn::GetAtt": ["Api", "RootResourceId"]},
                "PathPart": PATH_PART,
            },
        },
        "PostOrders": {
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": {"Ref": "Api"},
                "ResourceId": {"Ref": "Orders"},
                "HttpMethod": "POST",
                "AuthorizationType": "NONE",
                "MethodResponses": [{"StatusCode": "200"}],
                "Integration": {
                    "Type": "AWS_PROXY",
                    "IntegrationHttpMethod": "POST",
                    "Uri": {"Fn::Sub":
                        "arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/\
                         ${IntakeFunction.Arn}/invocations"},
                },
            },
        },
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["PostOrders"],
            "Properties": {"RestApiId": {"Ref": "Api"}},
        },
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": {"Ref": "Api"},
                "DeploymentId": {"Ref": "Deployment"},
                "StageName": STAGE,
            },
        },
        "ErrorAlarm": {
            "Type": "AWS::CloudWatch::Alarm",
            "Properties": {
                "AlarmName": row.alarm,
                "Namespace": "AWS/Lambda",
                "MetricName": "Errors",
                "Statistic": "Sum",
                "Dimensions": [{"Name": "FunctionName", "Value": row.function}],
                "Period": row.period,
                "EvaluationPeriods": row.evaluation_periods,
                "Threshold": row.threshold,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            },
        },
    });
    // The permission is inserted by NAME, because the logical id it is declared under is the
    // one thing the rows differ on for the fitting half of the rule.
    resources
        .as_object_mut()
        .expect("the resources are an object")
        .insert(
            row.permission_logical_id.clone(),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": {"Fn::GetAtt": ["IntakeFunction", "Arn"]},
                    "Principal": "apigateway.amazonaws.com",
                    "SourceArn": {"Fn::Sub":
                        format!(
                            "arn:aws:execute-api:${{AWS::Region}}:${{AWS::AccountId}}:\
                             ${{Api}}/*/POST/{PATH_PART}"
                        )},
                },
            }),
        );
    let mut declared = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": {"ApiId": {"Value": {"Ref": "Api"}}},
    });
    if !row.parameters.is_empty() {
        let mut parameters = serde_json::Map::new();
        for (name, kind, _) in row.parameters {
            parameters.insert((*name).to_string(), json!({"Type": kind}));
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

/// The physical id the stack published for one of its logical resources.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeStackResource to answer for {logical}, got {status} \
         with {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: the stack published no PhysicalResourceId for {logical} in {body}")
    })
}

/// The `OutputValue` the stack reports for `key`.
async fn output(router: &MultiServiceRouter, case: &str, stack: &str, key: &str) -> String {
    let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeStacks to answer, got {status} with {body}"
    );
    crate::stack_lifecycle::output_value(&body, key).unwrap_or_else(|| {
        panic!("case {case:?}: the stack published no {key} output in {body}")
    })
}

/// The `Sid`s the function's resource-based policy holds, with the statement each carries.
async fn policy_statements(router: &MultiServiceRouter, case: &str, function: &str) -> Vec<Value> {
    let (status, answered) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/policy"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the function's policy, got {status} with \
         {answered}"
    );
    let document: Value = answered
        .get("Policy")
        .and_then(Value::as_str)
        .and_then(|policy| serde_json::from_str(policy).ok())
        .unwrap_or(Value::Null);
    document
        .get("Statement")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The whole rule over one row: the stack completes, and every value the declaration carried
/// is held by the service that owns it.
async fn the_application_is_deployed(row: &Row) {
    let case = row.case;
    let router = router();

    let (status, created) =
        create_stack(&router, row.stack, &template(row), &stack_parameters(row)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );

    // The issue's own observable: the stack COMPLETES. Before the fix the generated statement
    // id was past AWS Lambda's bound and a quoted number was refused by its deserializer, and
    // either one rolled the whole application back.
    let reached = settled_stack_status(case, &router, REGION, row.stack).await;
    let reasons = stack_event_reasons(&router, REGION, row.stack).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with {reasons:?}"
    );
    for reason in &reasons {
        assert!(
            !reason.contains(TYPE_ERROR) && !reason.contains(LENGTH_CLAUSE),
            "case {case:?}: expected no deserializer or constraint refusal, got {reason:?}"
        );
    }

    // THE FITTED IDENTIFIER. The type publishes no statement-id property, so the physical id
    // the stack reports IS the statement id `AddPermission` was called with.
    let statement_id = physical_id(&router, case, row.stack, &row.permission_logical_id).await;
    let characters = statement_id.chars().count();
    assert!(
        characters <= STATEMENT_ID_MAX_CHARACTERS,
        "case {case:?}: expected a statement id within the {STATEMENT_ID_MAX_CHARACTERS} \
         characters AWS Lambda accepts, got {characters} ({statement_id:?})"
    );
    assert!(
        statement_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !statement_id.contains("--")
            && statement_id.starts_with(|c: char| c.is_ascii_alphanumeric())
            && statement_id.ends_with(|c: char| c.is_ascii_alphanumeric()),
        "case {case:?}: expected a statement id of the characters Lambda's own pattern admits, \
         got {statement_id:?}"
    );
    // The FORM: three segments, the unique one kept whole — which is what keeps two shortened
    // identifiers of one stack apart — and the other two prefixes of the stack name and the
    // logical id they were shortened from (a logical id carries no hyphen, so the two hyphens
    // that matter are the last two).
    let (head, unique) = statement_id.rsplit_once('-').unwrap_or_else(|| {
        panic!("case {case:?}: {statement_id:?} must carry a unique segment")
    });
    let (stack_segment, logical_segment) = head.rsplit_once('-').unwrap_or_else(|| {
        panic!("case {case:?}: {statement_id:?} must carry three segments")
    });
    assert!(
        unique.chars().count() == UNIQUE_SEGMENT_CHARACTERS
            && unique.chars().all(|c| c.is_ascii_hexdigit()),
        "case {case:?}: expected a {UNIQUE_SEGMENT_CHARACTERS}-character hexadecimal unique \
         segment, got {unique:?} in {statement_id:?}"
    );
    assert!(
        row.stack.starts_with(stack_segment.trim_end_matches('-')),
        "case {case:?}: expected the first segment to be a prefix of the stack name, got \
         {stack_segment:?} in {statement_id:?}"
    );
    assert!(
        !logical_segment.is_empty() && row.permission_logical_id.starts_with(logical_segment),
        "case {case:?}: expected the second segment to be a prefix of the logical id, got \
         {logical_segment:?} in {statement_id:?}"
    );
    // Nothing is fitted away where the whole `<stack>-<logical id>-<unique id>` is already
    // inside the bound.
    let unfitted = row.stack.chars().count()
        + row.permission_logical_id.chars().count()
        + UNIQUE_SEGMENT_CHARACTERS
        + SEPARATORS;
    if unfitted <= STATEMENT_ID_MAX_CHARACTERS {
        assert_eq!(
            statement_id,
            format!("{}-{}-{unique}", row.stack, row.permission_logical_id),
            "case {case:?}: an identifier inside the bound must be the whole \
             <stack name>-<logical id>-<unique id>"
        );
    }

    // And AWS LAMBDA really holds the grant under it: the statement the stack declared, for
    // the principal it declared, on the function the same stack created.
    let statements = policy_statements(&router, case, row.function).await;
    let granted = statements
        .iter()
        .find(|statement| statement.get("Sid").and_then(Value::as_str) == Some(&statement_id))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the function's policy to hold the statement \
                 {statement_id:?}, got {statements:?}"
            )
        });
    assert_eq!(
        (
            granted.get("Effect").and_then(Value::as_str),
            granted.get("Action").and_then(Value::as_str),
            granted
                .pointer("/Principal/Service")
                .and_then(Value::as_str),
        ),
        (
            Some("Allow"),
            Some("lambda:InvokeFunction"),
            Some("apigateway.amazonaws.com"),
        ),
        "case {case:?}: expected the declared grant, got {granted}"
    );

    // THE NUMBERS, each read back through the wire of the service that owns the call they
    // reached: AWS Lambda's function configuration.
    let (status, held) = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/functions/{}/configuration", row.function),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the function the stack created, got \
         {status} with {held}"
    );
    assert_eq!(
        (
            held.get("MemorySize").and_then(Value::as_i64),
            held.get("Timeout").and_then(Value::as_i64),
        ),
        (
            Some(row.expected_memory_size),
            Some(row.expected_timeout),
        ),
        "case {case:?}: expected (MemorySize, Timeout) ({}, {}) as numbers, got {held}",
        row.expected_memory_size,
        row.expected_timeout
    );

    // Amazon CloudWatch Logs' own account of the log group's retention.
    let (status, groups) = logs(
        &router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": row.log_group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeLogGroups to answer, got {status} with {groups}"
    );
    let group = groups
        .get("logGroups")
        .and_then(Value::as_array)
        .and_then(|held| {
            held.iter()
                .find(|entry| {
                    entry.get("logGroupName").and_then(Value::as_str) == Some(row.log_group)
                })
                .cloned()
        })
        .unwrap_or(Value::Null);
    assert_eq!(
        group.get("retentionInDays").and_then(Value::as_i64),
        Some(row.expected_retention),
        "case {case:?}: expected CloudWatch Logs to report the retention as the number {}, got \
         {group}",
        row.expected_retention
    );

    // And Amazon CloudWatch's own account of the alarm's cadence, count and threshold — the
    // alarm ECHOES the period the template declared.
    let (status, described) = cloudwatch(
        &router,
        "DescribeAlarms",
        &json!({"AlarmNames": [row.alarm], "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeAlarms to answer, got {status} with {described}"
    );
    let alarms: Vec<Value> = described
        .get("MetricAlarms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        alarms.len(),
        1,
        "case {case:?}: expected CloudWatch to hold exactly the alarm the stack declared, got \
         {alarms:?}"
    );
    let alarm = alarms.into_iter().next().unwrap_or(Value::Null);
    assert_eq!(
        (
            alarm.get("Period").and_then(Value::as_i64),
            alarm.get("EvaluationPeriods").and_then(Value::as_i64),
            alarm.get("Threshold").and_then(Value::as_f64),
        ),
        (
            Some(row.expected_period),
            Some(row.expected_evaluation_periods),
            Some(row.expected_threshold),
        ),
        "case {case:?}: expected (Period, EvaluationPeriods, Threshold) ({}, {}, {}) as \
         numbers, got {alarm}",
        row.expected_period,
        row.expected_evaluation_periods,
        row.expected_threshold
    );

    // The leg downstream of the deploy the issue could never reach: Amazon API Gateway really
    // serves the stage the stack deployed, under the api the stack published.
    let api_id = output(&router, case, row.stack, "ApiId").await;
    let (status, stage) = apigateway(&router, "GET", &format!("/restapis/{api_id}/stages/{STAGE}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected API Gateway to report the deployed stage, got {status} with \
         {stage}"
    );
    assert_eq!(
        stage.get("stageName").and_then(Value::as_str),
        Some(STAGE),
        "case {case:?}: expected the stage the stack declared, got {stage}"
    );
}

#[tokio::test]
async fn an_order_intake_application_deploys_whatever_its_names_and_number_forms() {
    for row in rows() {
        the_application_is_deployed(&row).await;
    }
}
