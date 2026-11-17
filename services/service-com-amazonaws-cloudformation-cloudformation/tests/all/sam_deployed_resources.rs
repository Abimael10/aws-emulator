// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED SAM stack holds, read back over the awsQuery wire.
//!
//! The rule every row here holds the service to: **a template whose `Transform` section
//! names `AWS::Serverless-2016-10-31` is PROCESSED by that macro before the stack deploys
//! anything, so the resources the stack reports are the concrete ones the macro synthesized
//! (a function's `AWS::Lambda::Function`, the `AWS::IAM::Role` it runs as when it declares
//! none of its own, its `AWS::Lambda::Permission` per API event, an API's
//! `AWS::ApiGateway::RestApi`/`Deployment`/`Stage`, a simple table's
//! `AWS::DynamoDB::Table`) beside the resources the macro has nothing to do with, which
//! pass through under the type and the logical id the template gave them — and never an
//! `AWS::Serverless::*` type, which exists only as the macro's input.**
//!
//! `tests/all/sam_transform.rs` holds the macro to what it EXPANDS a declaration into (the
//! plan, the logical ids, the deployment's content hash) from one implicit-API template.
//! This suite is about the DEPLOYED stack a client then reads: the rows differ along which
//! serverless types the template declares, whether its API is explicit or implicit, whether
//! its function names a role of its own, whether resources the macro leaves alone are
//! declared beside them, and whether the body is spelled as JSON or as YAML. Both readers a
//! client has for a deployed stack are asserted (`DescribeStackResources` and
//! `ListStackResources`), because the defect this pins was visible through them, and so is
//! `GetTemplate`'s pair of stages.
//!
//! What this suite deliberately does NOT re-run, because `sam_transform.rs` owns it: the
//! LOGICAL IDS the macro synthesizes (the API trio's, the role's, the permission's), the
//! deployment logical id's content hash and its stability under an edited definition, what
//! a change set PLANS for a SAM template, and the serverless types this crate expands
//! beyond the three below (`HttpApi`, `LayerVersion`, `StateMachine`) or deliberately
//! leaves as declared. A row here states the resource-type SET its template deploys plus
//! the identities the template itself is entitled to keep, never a synthesized id.
//!
//! The recorded trace this suite was written for is ONE row of the table: an explicit
//! `AWS::Serverless::Api`, a function routed to it and a `AWS::Serverless::SimpleTable`,
//! beside CloudWatch and CloudWatch Logs resources the macro leaves alone.
//!
//! The second half of the rule is the DEPLOYMENT PATH: the macro's output is what the stack
//! deploys whichever operation deployed the template, so a `CREATE` change set over a
//! transform-declaring template executes the EXPANSION — every expanded resource is created
//! in the service that owns it and the stack completes, rather than failing and rolling back.
//! [`an_executed_change_set_deploys_the_expansion_into_the_owning_services`] holds that half,
//! over the `sam deploy` path (`CreateChangeSet` + `ExecuteChangeSet`), and reads each
//! expanded resource back through the wire of the service that owns it.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::dynamodb_table_resources::{describe_table, failure_reasons};
use crate::hotswapped_function_code::lambda;
use crate::iam_identity_resources::xml_policy_document;
use crate::stack_lifecycle::{
    call, create_stack, iam, kv, member, router_with_engine, xml_blocks, xml_text, xml_texts,
};
use crate::sam_policies_shorthand::function_with_policies;
use crate::sam_transform::sam_template;
use crate::stack_resources::{wire_resources, WireResource};
use crate::stack_resource_provisioning::{s3, stage_code_objects};

/// The namespace a SAM resource type lives under: no resource of a DEPLOYED stack may
/// carry it, because the macro consumed every such declaration.
const SERVERLESS_TYPE_PREFIX: &str = "AWS::Serverless::";

/// One row of the deployed-resources table.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// Every resource TYPE the deployed stack must hold — sorted and deduplicated, so a
    /// row states the type SET its template expands to rather than the synthesized logical
    /// ids (which `sam_transform.rs` owns).
    types: Vec<&'static str>,
    /// `(logical id, type)` pairs the deployed stack must still carry under the identity
    /// the TEMPLATE gave them: a resource the macro leaves alone, and the ones SAM keeps
    /// the declared logical id of.
    kept: Vec<(&'static str, &'static str)>,
    /// The execution role this row's template NAMES rather than declares, created in IAM
    /// before the stack deploys — an execution role provisioned outside the template, which
    /// is what a function that "names a role of its own" is given.
    external_role: Option<&'static str>,
}

/// A trust policy that lets AWS Lambda assume the role, which is what the service resolves
/// before it creates a function that runs as it.
fn lambda_trust_policy() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
         "Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
        .to_string()
}

/// A function declaration whose `Api` event names `RestApiId`, which is what makes the API
/// it is served by the EXPLICIT one the template declares.
///
/// The counterpart of `sam_transform.rs`'s `implicit_api_function`, which is the same
/// declaration WITHOUT a `RestApiId` — the member is the whole difference between an
/// explicit API and the one the macro adds, so the two builders state it rather than one
/// taking a flag.
fn routed_function(logical_id: &str, api: &str, path: &str, method: &str) -> String {
    format!(
        r#""{logical_id}":{{"Type":"AWS::Serverless::Function","Properties":{{
             "Handler":"index.handler","Runtime":"python3.12",
             "CodeUri":"s3://code-bucket/{logical_id}.zip",
             "Events":{{"Route":{{"Type":"Api","Properties":{{
                "Path":"{path}","Method":"{method}","RestApiId":{{"Ref":"{api}"}}}}}}}}}}}}"#
    )
}

/// The resources a serverless application is monitored by, which the macro has nothing to
/// do with: they are part of the processed template exactly as the client wrote them.
///
/// `group` names the log group, because the rows below deploy against ONE emulator: a log
/// group is really created in Amazon CloudWatch Logs now
/// (`state::provisioning::logs_resources`), and two stacks cannot both create the group of
/// the same name — the second create is refused, exactly as it is in an account.
fn monitoring_resources(group: &str) -> String {
    format!(
        r#""Failures":{{"Type":"AWS::Logs::LogGroup",
         "Properties":{{"LogGroupName":"{group}","RetentionInDays":7}}}},
       "FailureCount":{{"Type":"AWS::Logs::MetricFilter",
         "Properties":{{"LogGroupName":{{"Ref":"Failures"}},"FilterPattern":"ERROR",
           "MetricTransformations":[{{"MetricName":"Failures","MetricNamespace":"Orders",
             "MetricValue":"1"}}]}}}},
       "FailureAlarm":{{"Type":"AWS::CloudWatch::Alarm",
         "Properties":{{"AlarmName":"orders-failures","ComparisonOperator":"GreaterThanThreshold",
           "EvaluationPeriods":1,"Threshold":1,"MetricName":"Failures","Namespace":"Orders",
           "Period":60,"Statistic":"Sum"}}}}"#
    )
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an explicit Api, a function routed to it and a simple table, beside \
                   resources the macro leaves alone",
            stack: "sam-deployed-explicit-api",
            template: sam_template(&format!(
                r#"{{"OrdersApi":{{"Type":"AWS::Serverless::Api",
                     "Properties":{{"StageName":"live"}}}},
                   "Orders":{{"Type":"AWS::Serverless::SimpleTable",
                     "Properties":{{"PrimaryKey":{{"Name":"orderId","Type":"String"}}}}}},
                   {},{}}}"#,
                routed_function("Place", "OrdersApi", "/orders", "post"),
                monitoring_resources("/aws/lambda/orders")
            )),
            types: vec![
                "AWS::ApiGateway::Deployment",
                "AWS::ApiGateway::RestApi",
                "AWS::ApiGateway::Stage",
                "AWS::CloudWatch::Alarm",
                "AWS::DynamoDB::Table",
                "AWS::IAM::Role",
                "AWS::Lambda::Function",
                "AWS::Lambda::Permission",
                "AWS::Logs::LogGroup",
                "AWS::Logs::MetricFilter",
            ],
            kept: vec![
                // SAM keeps the declared logical id of the resource it expands a
                // declaration into, so these are the ids the template itself states.
                ("OrdersApi", "AWS::ApiGateway::RestApi"),
                ("Orders", "AWS::DynamoDB::Table"),
                ("Place", "AWS::Lambda::Function"),
                // Untouched by the macro.
                ("Failures", "AWS::Logs::LogGroup"),
                ("FailureCount", "AWS::Logs::MetricFilter"),
                ("FailureAlarm", "AWS::CloudWatch::Alarm"),
            ],
            external_role: None,
        },
        Row {
            case: "a simple table alone, the one serverless type that expands to a single \
                   resource",
            stack: "sam-deployed-table",
            template: sam_template(
                r#"{"Sessions":{"Type":"AWS::Serverless::SimpleTable",
                     "Properties":{"PrimaryKey":{"Name":"sessionId","Type":"String"}}}}"#,
            ),
            types: vec!["AWS::DynamoDB::Table"],
            kept: vec![("Sessions", "AWS::DynamoDB::Table")],
            external_role: None,
        },
        Row {
            case: "a function that names a role of its own: no role is synthesized for it",
            stack: "sam-deployed-own-role",
            template: sam_template(
                r#"{"Worker":{"Type":"AWS::Serverless::Function","Properties":{
                     "Handler":"index.handler","Runtime":"python3.12",
                     "CodeUri":"s3://code-bucket/worker.zip",
                     "Role":"arn:aws:iam::000000000000:role/declared-by-the-template"}}}"#,
            ),
            types: vec!["AWS::Lambda::Function"],
            kept: vec![("Worker", "AWS::Lambda::Function")],
            external_role: Some("declared-by-the-template"),
        },
        Row {
            case: "the YAML spelling of a function with an implicit Api event and a table",
            stack: "sam-deployed-yaml",
            template: [
                "Transform: AWS::Serverless-2016-10-31",
                "Resources:",
                "  Greeter:",
                "    Type: AWS::Serverless::Function",
                "    Properties:",
                "      Handler: index.handler",
                "      Runtime: python3.12",
                "      CodeUri: s3://code-bucket/greeter.zip",
                "      Events:",
                "        Hello:",
                "          Type: Api",
                "          Properties:",
                "            Path: /hello",
                "            Method: get",
                "  Visits:",
                "    Type: AWS::Serverless::SimpleTable",
                "",
            ]
            .join("\n"),
            types: vec![
                "AWS::ApiGateway::Deployment",
                "AWS::ApiGateway::RestApi",
                "AWS::ApiGateway::Stage",
                "AWS::DynamoDB::Table",
                "AWS::IAM::Role",
                "AWS::Lambda::Function",
                "AWS::Lambda::Permission",
            ],
            kept: vec![
                ("Greeter", "AWS::Lambda::Function"),
                ("Visits", "AWS::DynamoDB::Table"),
                // The API no declaration names: the one the macro adds for the events that
                // name no `RestApiId`.
                ("ServerlessRestApi", "AWS::ApiGateway::RestApi"),
            ],
            external_role: None,
        },
        Row {
            case: "the macro declared over resources it expands nothing of",
            stack: "sam-deployed-nothing-to-expand",
            template: sam_template(&format!(
                "{{{}}}",
                monitoring_resources("/aws/lambda/orders-unexpanded")
            )),
            types: vec![
                "AWS::CloudWatch::Alarm",
                "AWS::Logs::LogGroup",
                "AWS::Logs::MetricFilter",
            ],
            kept: vec![
                ("Failures", "AWS::Logs::LogGroup"),
                ("FailureCount", "AWS::Logs::MetricFilter"),
                ("FailureAlarm", "AWS::CloudWatch::Alarm"),
            ],
            external_role: None,
        },
    ]
}

/// Deploy one row and answer the `(logical id, type)` pairs `DescribeStackResources`
/// reports for the settled stack.
async fn deployed_resources(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
) -> Vec<(String, String)> {
    if let Some(role) = row.external_role {
        let (code, body) = iam(
            router,
            "CreateRole",
            &[
                ("RoleName", role),
                ("AssumeRolePolicyDocument", &lambda_trust_policy()),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: expected the execution role the template names to be created in IAM, \
             got {code} {body}",
            row.case
        );
    }
    stage_code_objects(router, row.case, &row.template).await;
    let (code, body) = create_stack(
        router,
        row.stack,
        &row.template,
        &kv(&[
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ]),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: expected CreateStack to be accepted, got {code} {body}",
        row.case
    );
    settle_the_store(router, engine).await;

    let (code, body) = call(router, "DescribeStacks", &[member("StackName", row.stack)]).await;
    assert_eq!(
        code, 200,
        "case {:?}: expected DescribeStacks to answer, got {code} {body}",
        row.case
    );
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {:?}: expected the stack to deploy what the transform synthesized, got {body}",
        row.case
    );

    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: expected DescribeStackResources to answer, got {code} {body}",
        row.case
    );
    wire_resources(&body)
        .into_iter()
        .map(|resource| {
            (
                resource.logical_id.unwrap_or_default(),
                resource.resource_type.unwrap_or_default(),
            )
        })
        .collect()
}

/// The `(logical id, type)` pairs `ListStackResources` reports — the other reader a client
/// has for a deployed stack's resources, and the one the recorded trace read.
async fn listed_resources(router: &MultiServiceRouter, row: &Row) -> Vec<(String, String)> {
    let (code, body) = call(
        router,
        "ListStackResources",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: expected ListStackResources to answer, got {code} {body}",
        row.case
    );
    xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(entry, "ResourceType").unwrap_or_default(),
            )
        })
        .collect()
}

/// The sorted, deduplicated resource types of a `(logical id, type)` list.
fn type_set(resources: &[(String, String)]) -> Vec<String> {
    let mut types: Vec<String> = resources
        .iter()
        .map(|(_, resource_type)| resource_type.clone())
        .collect();
    types.sort();
    types.dedup();
    types
}

/// The resource types a template BODY declares, read out of its `Resources` map.
///
/// Read as a document rather than searched for as text, so a type that appears inside a
/// generated OpenAPI definition or an `Fn::Sub` string cannot be mistaken for a resource
/// the template declares.
fn declared_types(case: &str, body: &str) -> Vec<String> {
    let document: ::serde_json::Value = ::serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: expected a JSON template, got {error}"));
    let resources = document
        .get("Resources")
        .and_then(::serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: expected a Resources map, got {document}"));
    let mut types: Vec<String> = resources
        .values()
        .filter_map(|declaration| {
            declaration
                .get("Type")
                .and_then(::serde_json::Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    types.sort();
    types.dedup();
    types
}

#[tokio::test]
async fn a_deployed_sam_stack_holds_the_synthesized_resources_and_no_serverless_type() {
    let (router, engine) = router_with_engine();
    for row in rows() {
        let deployed = deployed_resources(&router, &engine, &row).await;

        let expected: Vec<String> = row.types.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            type_set(&deployed),
            expected,
            "case {:?}: expected exactly the types the transform leaves behind, got \
             {deployed:?}",
            row.case
        );

        let untransformed: Vec<&(String, String)> = deployed
            .iter()
            .filter(|(_, resource_type)| resource_type.starts_with(SERVERLESS_TYPE_PREFIX))
            .collect();
        assert!(
            untransformed.is_empty(),
            "case {:?}: expected no {SERVERLESS_TYPE_PREFIX}* type in the deployed stack, \
             got {untransformed:?}",
            row.case
        );

        for (logical_id, resource_type) in &row.kept {
            let found = deployed
                .iter()
                .find(|(deployed_id, _)| deployed_id == logical_id);
            assert_eq!(
                found.map(|(_, deployed_type)| deployed_type.as_str()),
                Some(*resource_type),
                "case {:?}: expected {logical_id} to be deployed as a {resource_type}, got \
                 {found:?} in {deployed:?}",
                row.case
            );
        }

        // Both readers report one deployed stack, so they report the same resources.
        let mut listed = listed_resources(&router, &row).await;
        let mut described = deployed.clone();
        listed.sort();
        described.sort();
        assert_eq!(
            listed, described,
            "case {:?}: expected ListStackResources and DescribeStackResources to report \
             the same resources, got {listed:?} and {described:?}",
            row.case
        );
    }
}

/// `GetTemplate` answers the SUBMITTED body for the `Original` stage and the macro's output
/// for `Processed` (the default): "To get the user-submitted template, specify Original. To
/// get the template after CloudFormation has processed all transforms, specify Processed …
/// By default, CloudFormation specifies Processed."
#[tokio::test]
async fn get_template_answers_the_submitted_body_and_the_processed_one() {
    let (router, engine) = router_with_engine();
    for row in rows() {
        let _ = deployed_resources(&router, &engine, &row).await;

        let (code, body) = call(
            &router,
            "GetTemplate",
            &[
                member("StackName", row.stack),
                member("TemplateStage", "Original"),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: expected GetTemplate to answer, got {code} {body}",
            row.case
        );
        assert_eq!(
            xml_text(&body, "TemplateBody")
                .map(|template| crate::stack_lifecycle::xml_unescape(&template)),
            Some(row.template.clone()),
            "case {:?}: expected the submitted body for the Original stage, got {body}",
            row.case
        );

        let (code, body) = call(&router, "GetTemplate", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected GetTemplate to answer, got {code} {body}",
            row.case
        );
        let processed = xml_text(&body, "TemplateBody")
            .map(|template| crate::stack_lifecycle::xml_unescape(&template))
            .unwrap_or_default();
        // The types the PROCESSED body declares are read out of its `Resources` map, so
        // this says what the template declares rather than what its text happens to
        // mention. The set equality below is what rules a macro input type out — no row's
        // expected set carries one — so it is asserted once, here.
        let expected: Vec<String> = row.types.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            declared_types(row.case, &processed),
            expected,
            "case {:?}: expected the processed template to declare exactly the types the \
             transform leaves behind, and no {SERVERLESS_TYPE_PREFIX}* one, got {processed}",
            row.case
        );
    }
}

/// An AWS managed policy ARN a `Policies` entry can name, which is ATTACHED to the generated
/// role and embeds nothing — so a row carrying one has fewer inline policies than entries.
const MANAGED_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// One row of the change-set table: a SAM template deployed the way `sam deploy` deploys one
/// — a `CREATE` change set over the template, acknowledged for IAM and for the transform,
/// then `ExecuteChangeSet`.
struct Executed {
    case: &'static str,
    stack: &'static str,
    change_set: &'static str,
    template: String,
    /// Each function declaration the row reads: its logical id, and how many INLINE policies
    /// its `Policies` asks for on the role the transform generates for it (`<id>Role`).
    functions: Vec<(&'static str, usize)>,
    /// The logical ids of the tables the row declares, which Amazon DynamoDB must hold.
    tables: Vec<&'static str>,
    /// The logical ids of the buckets the row declares, which Amazon S3 must hold.
    buckets: Vec<&'static str>,
}

/// The rows: the class is any template whose `Transform` the request acknowledges, deployed
/// through a change set. They differ along what the rule generalises over — which serverless
/// and plain types the template declares beside the function, how the function's `Policies`
/// are spelled (a list of templates, the single-value form, a managed ARN beside a template)
/// and how the parameters they carry are stated (a `Ref` to a resource of the same template
/// and a literal name), how MANY functions the template declares, whether the function's
/// events make the transform add an API of its own, and whether the body is JSON or YAML.
///
/// The first row is the recorded trace's own shape: one function whose shorthand policies
/// name the table and the bucket declared beside it.
fn executed_rows() -> Vec<Executed> {
    vec![
        Executed {
            case: "a function whose shorthand policies name the table and the bucket \
                   declared beside it",
            stack: "sam-executed-shorthand",
            change_set: "sam-executed-shorthand-set",
            template: sam_template(&format!(
                r#"{{"Items":{{"Type":"AWS::Serverless::SimpleTable",
                     "Properties":{{"PrimaryKey":{{"Name":"itemId","Type":"String"}}}}}},
                   "Reports":{{"Type":"AWS::S3::Bucket"}},
                   {}}}"#,
                function_with_policies(
                    "Probe",
                    r#"[{"DynamoDBCrudPolicy":{"TableName":{"Ref":"Items"}}},
                        {"S3ReadPolicy":{"BucketName":{"Ref":"Reports"}}}]"#
                )
            )),
            functions: vec![("Probe", 2)],
            tables: vec!["Items"],
            buckets: vec!["Reports"],
        },
        Executed {
            case: "the single-value spelling of Policies over a literally named bucket, \
                   beside a plain DynamoDB table",
            stack: "sam-executed-literal",
            change_set: "sam-executed-literal-set",
            template: sam_template(&format!(
                r#"{{"Carts":{{"Type":"AWS::DynamoDB::Table","Properties":{{
                     "TableName":"sam-executed-carts",
                     "KeySchema":[{{"AttributeName":"cartId","KeyType":"HASH"}}],
                     "AttributeDefinitions":[{{"AttributeName":"cartId",
                       "AttributeType":"S"}}],
                     "BillingMode":"PAY_PER_REQUEST"}}}},
                   "Assets":{{"Type":"AWS::S3::Bucket",
                     "Properties":{{"BucketName":"sam-executed-assets"}}}},
                   {}}}"#,
                function_with_policies(
                    "Writer",
                    r#"{"S3CrudPolicy":{"BucketName":"sam-executed-assets"}}"#
                )
            )),
            functions: vec![("Writer", 1)],
            tables: vec!["Carts"],
            buckets: vec!["Assets"],
        },
        Executed {
            case: "two functions sharing one table, one of them naming a managed policy \
                   beside its template entry",
            stack: "sam-executed-two-functions",
            change_set: "sam-executed-two-functions-set",
            template: sam_template(&format!(
                r#"{{"Ledger":{{"Type":"AWS::Serverless::SimpleTable"}},{},{}}}"#,
                function_with_policies(
                    "Reader",
                    r#"[{"DynamoDBReadPolicy":{"TableName":{"Ref":"Ledger"}}}]"#
                ),
                function_with_policies(
                    "Appender",
                    &format!(
                        r#"["{MANAGED_POLICY_ARN}",
                            {{"DynamoDBWritePolicy":{{"TableName":{{"Ref":"Ledger"}}}}}}]"#
                    )
                )
            )),
            // The managed ARN is attached rather than embedded, so the second function's
            // role carries ONE inline policy for its two entries.
            functions: vec![("Reader", 1), ("Appender", 1)],
            tables: vec!["Ledger"],
            buckets: vec![],
        },
        Executed {
            case: "the YAML spelling, whose function's Api event makes the transform add an \
                   API of its own",
            stack: "sam-executed-yaml",
            change_set: "sam-executed-yaml-set",
            template: [
                "Transform: AWS::Serverless-2016-10-31",
                "Resources:",
                "  Visits:",
                "    Type: AWS::Serverless::SimpleTable",
                "  Greeter:",
                "    Type: AWS::Serverless::Function",
                "    Properties:",
                "      Handler: index.handler",
                "      Runtime: python3.12",
                "      CodeUri: s3://code-bucket/greeter.zip",
                "      Policies:",
                "        - DynamoDBReadPolicy:",
                "            TableName: !Ref Visits",
                "      Events:",
                "        Hello:",
                "          Type: Api",
                "          Properties:",
                "            Path: /hello",
                "            Method: get",
                "",
            ]
            .join("\n"),
            functions: vec![("Greeter", 1)],
            tables: vec!["Visits"],
            buckets: vec![],
        },
    ]
}

/// Deploy one row through a `CREATE` change set and answer the resources the settled stack
/// reports.
///
/// Every assertion on the way is part of the rule: the change set must PLAN (reach
/// `CREATE_COMPLETE`), executing it must be accepted, and the stack must then complete rather
/// than fail and roll back — which is why the status assertion reports the resource failure
/// reasons the stack recorded.
async fn execute_a_change_set(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Executed,
) -> Vec<WireResource> {
    let case = row.case;
    stage_code_objects(router, case, &row.template).await;
    let (code, body) = create_change_set(
        router,
        row.stack,
        row.change_set,
        &[
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &row.template),
            member("Capabilities.member.1", "CAPABILITY_IAM"),
            member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateChangeSet to be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;

    let (code, body) = describe_change_set(router, row.change_set, Some(row.stack)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeChangeSet to answer, got {code} {body}"
    );
    let planned = xml_text(&body, "Status");
    assert_eq!(
        planned.as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the change set to plan the expansion, got {planned:?} \
         because {:?}",
        xml_text(&body, "StatusReason")
    );

    let (code, body) = execute_change_set(router, row.change_set, Some(row.stack)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ExecuteChangeSet to be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;

    let (code, described) = call(router, "DescribeStacks", &[member("StackName", row.stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {code} {described}"
    );
    let deployed = xml_text(&described, "StackStatus");
    assert_eq!(
        deployed.as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the executed change set to create every resource the \
         transform expanded, got {deployed:?} because {:?}",
        failure_reasons(router, row.stack).await
    );

    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResources to answer, got {code} {body}"
    );
    wire_resources(&body)
}

/// The physical resource id the settled stack reports for one logical id.
fn physical_of(case: &str, deployed: &[WireResource], logical_id: &str) -> String {
    let found = deployed
        .iter()
        .find(|resource| resource.logical_id.as_deref() == Some(logical_id));
    let resource = found.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the deployed stack to carry {logical_id}, got {:?}",
            deployed
                .iter()
                .map(|resource| resource.logical_id.clone())
                .collect::<Vec<_>>()
        )
    });
    resource.physical_id.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a physical id for {logical_id}, got none")
    })
}

/// RULE (the deployment-path half): the macro's output is what the stack deploys whichever
/// operation deployed the template, so executing a `CREATE` change set over a
/// transform-declaring template creates every EXPANDED resource in the service that owns it
/// and completes the stack — the generated execution role with the inline policies the
/// declaration's `Policies` asked for, the table, the bucket and the function — rather than
/// failing one of them and rolling the stack back.
#[tokio::test]
async fn an_executed_change_set_deploys_the_expansion_into_the_owning_services() {
    let (router, engine) = router_with_engine();
    for row in executed_rows() {
        let case = row.case;
        let deployed = execute_a_change_set(&router, &engine, &row).await;

        // Nothing of the expansion is left failed, unidentified, or still a macro input
        // type.
        for resource in &deployed {
            let logical_id = resource.logical_id.clone().unwrap_or_default();
            assert_eq!(
                resource.status.as_deref(),
                Some("CREATE_COMPLETE"),
                "case {case:?}: expected {logical_id} to be created, got {resource:?}"
            );
            assert!(
                resource
                    .physical_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty()),
                "case {case:?}: expected a physical id for {logical_id}, got {resource:?}"
            );
            let resource_type = resource.resource_type.clone().unwrap_or_default();
            assert!(
                !resource_type.starts_with(SERVERLESS_TYPE_PREFIX),
                "case {case:?}: expected no {SERVERLESS_TYPE_PREFIX}* type in the deployed \
                 stack, got {logical_id} as a {resource_type}"
            );
        }

        for (function, inline) in &row.functions {
            // AWS Lambda holds the function the declaration expanded to, under the name the
            // stack published for it.
            let name = physical_of(case, &deployed, function);
            let answer = lambda(
                &router,
                "GET",
                &format!("/2015-03-31/functions/{name}/configuration"),
                b"",
                &[],
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected AWS Lambda to hold the function {name:?} the stack \
                 published, got {} {:?}",
                answer.status, answer.body
            );

            // And IAM holds the execution role the transform generated for it, carrying one
            // inline policy per embedded `Policies` entry.
            let role = physical_of(case, &deployed, &format!("{function}Role"));
            let (code, body) = iam(&router, "GetRole", &[("RoleName", &role)]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected IAM to hold the generated role {role:?}, got {code} \
                 {body}"
            );
            assert_eq!(
                xml_text(&body, "RoleName").as_deref(),
                Some(role.as_str()),
                "case {case:?}: expected the role read back to be the published one, got \
                 {body}"
            );
            let (code, body) = iam(&router, "ListRolePolicies", &[("RoleName", &role)]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected ListRolePolicies({role:?}) to answer, got {code} \
                 {body}"
            );
            let names = xml_texts(&body, "member");
            assert_eq!(
                names.len(),
                *inline,
                "case {case:?}: expected {inline} inline policies on {role:?}, got {names:?}"
            );
        }

        for table in &row.tables {
            let name = physical_of(case, &deployed, table);
            let held = describe_table(&router, &name).await;
            assert_eq!(
                held.as_ref()
                    .and_then(|table| table.get("TableStatus"))
                    .and_then(Value::as_str),
                Some("ACTIVE"),
                "case {case:?}: expected Amazon DynamoDB to hold the table {name:?} the \
                 executed change set created, got {held:?}"
            );
        }

        for bucket in &row.buckets {
            let name = physical_of(case, &deployed, bucket);
            let answer = s3(&router, "HEAD", &format!("/{name}"), b"").await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected Amazon S3 to hold the bucket {name:?} the executed \
                 change set created, got {} {}",
                answer.status, answer.body
            );
        }
    }
}

/// RULE (the same half, over an UPDATE change set): a change set over a stack that already
/// exists executes the macro's output too, so a template whose serverless declaration has
/// grown a policy entry and a bucket updates the generated role and creates the new resource,
/// and the stack completes.
#[tokio::test]
async fn an_executed_update_change_set_applies_the_expansion_it_planned() {
    let case = "a deployed SAM stack whose function gains a policy entry and a bucket";
    let (router, engine) = router_with_engine();
    let row = Executed {
        case,
        stack: "sam-executed-update",
        change_set: "sam-executed-update-create",
        template: sam_template(&format!(
            r#"{{"Orders":{{"Type":"AWS::Serverless::SimpleTable"}},{}}}"#,
            function_with_policies(
                "Handler",
                r#"[{"DynamoDBReadPolicy":{"TableName":{"Ref":"Orders"}}}]"#
            )
        )),
        functions: vec![("Handler", 1)],
        tables: vec!["Orders"],
        buckets: vec![],
    };
    let _ = execute_a_change_set(&router, &engine, &row).await;

    // The same application, one declaration richer: a bucket, and the entry that grants the
    // function access to it.
    let grown = sam_template(&format!(
        r#"{{"Orders":{{"Type":"AWS::Serverless::SimpleTable"}},
           "Archive":{{"Type":"AWS::S3::Bucket"}},
           {}}}"#,
        function_with_policies(
            "Handler",
            r#"[{"DynamoDBReadPolicy":{"TableName":{"Ref":"Orders"}}},
                {"S3WritePolicy":{"BucketName":{"Ref":"Archive"}}}]"#
        )
    ));
    stage_code_objects(&router, case, &grown).await;
    let change_set = "sam-executed-update-grow";
    let (code, body) = create_change_set(
        &router,
        row.stack,
        change_set,
        &[
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &grown),
            member("Capabilities.member.1", "CAPABILITY_IAM"),
            member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateChangeSet to be accepted, got {code} {body}"
    );
    settle_the_store(&router, &engine).await;
    let (code, body) = execute_change_set(&router, change_set, Some(row.stack)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ExecuteChangeSet to be accepted, got {code} {body}"
    );
    settle_the_store(&router, &engine).await;

    let (code, described) = call(
        &router,
        "DescribeStacks",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStacks to answer, got {code} {described}"
    );
    let updated = xml_text(&described, "StackStatus");
    assert_eq!(
        updated.as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the executed change set to apply the expansion it planned, \
         got {updated:?} because {:?}",
        failure_reasons(&router, row.stack).await
    );

    let (code, body) = call(
        &router,
        "DescribeStackResources",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResources to answer, got {code} {body}"
    );
    let deployed = wire_resources(&body);

    // The bucket the grown template declares exists in Amazon S3 now.
    let bucket = physical_of(case, &deployed, "Archive");
    let answer = s3(&router, "HEAD", &format!("/{bucket}"), b"").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected Amazon S3 to hold the bucket the update created, got {} {}",
        answer.status, answer.body
    );

    // And the generated role carries one inline policy per entry the grown declaration
    // states, so the update really re-expanded the shorthand.
    let role = physical_of(case, &deployed, "HandlerRole");
    let (code, body) = iam(&router, "ListRolePolicies", &[("RoleName", &role)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ListRolePolicies({role:?}) to answer, got {code} {body}"
    );
    let names = xml_texts(&body, "member");
    assert_eq!(
        names.len(),
        2,
        "case {case:?}: expected the role to carry one inline policy per entry of the grown \
         declaration, got {names:?}"
    );
    let (code, body) = iam(
        &router,
        "GetRolePolicy",
        &[("RoleName", &role), ("PolicyName", "HandlerRolePolicy1")],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the entry the update added to be embedded as \
         HandlerRolePolicy1, got {code} {body}"
    );
    let document = xml_policy_document(&body, "PolicyDocument").unwrap_or_else(|| {
        panic!("case {case:?}: expected a document for HandlerRolePolicy1, got {body}")
    });
    assert!(
        document.contains("s3:PutObject"),
        "case {case:?}: expected the added entry's document to grant what its template \
         grants, got {document:?}"
    );
}
