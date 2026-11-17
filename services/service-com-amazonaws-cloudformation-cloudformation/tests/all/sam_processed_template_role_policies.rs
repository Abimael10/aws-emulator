// SPDX-License-Identifier: CC0-1.0

//! What the SAM transform's output carries for the role it generates, read back through
//! `GetTemplate` at the `Processed` stage.
//!
//! The rule every row here holds the service to: **the processed template a
//! `AWS::Serverless-2016-10-31` declaration expands into carries, on the `AWS::IAM::Role` the
//! transform generates for a declaration, one `Policies` entry per `Policies` entry that is
//! not an AWS managed policy — the document a policy-template entry stands for, scoped to the
//! resources that template's parameters name, or the document an entry states outright —
//! named for the declaration and the entry's position, while a string entry is carried in
//! `ManagedPolicyArns` instead; a declaration that states no `Policies` generates a role that
//! carries none.** So `GetTemplate(TemplateStage='Processed')` shows the permissions the
//! template declared on the resource that was generated to hold them, and the role really
//! deployed holds the same document.
//!
//! The rows differ along what the rule generalises over: which published policy template the
//! entry names (one scoped through an Amazon S3 bucket's ARN and one through an AWS Systems
//! Manager parameter's, each with its own parameter name), how many statements that template
//! grants and whether they are resource-scoped, the kind of entry (the template shorthand, a
//! document stated outright, an AWS managed policy ARN) and the position each entry holds, and
//! the declaration that states no `Policies` at all.
//!
//! Every expected document is computed here from what the row declares, never read back out of
//! the answer being checked.

use ::serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::iam_identity_resources::xml_policy_document;
use crate::sam_transform::sam_template;
use crate::stack_resource_provisioning::stage_code_objects;
use crate::stack_lifecycle::{
    call, create_stack, iam, kv, member, router_with_engine, xml_text, xml_unescape,
};

/// The AWS managed policy every Lambda execution role the transform generates carries.
const BASIC_EXECUTION_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// An AWS managed policy ARN a declaration can ask to have ATTACHED rather than embedded.
const READ_ONLY_ARN: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// One row of the table: the `Policies` a function declares, and the `Policies` the role
/// generated for it must carry in the processed template.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The declaration the role is generated for; the role's logical id is that id plus
    /// `Role`.
    owner: &'static str,
    /// The `Policies` the declaration states, as the template spells it.
    declared: String,
    /// The `Policies` member the generated role must carry — `None` where it must carry none.
    expected: Option<Value>,
    /// The managed policies the generated role's `ManagedPolicyArns` must carry.
    managed: Vec<&'static str>,
}

/// The document `SSMParameterReadPolicy` grants over the parameter `name`: listing the
/// account's parameters, which is not addressable per parameter, and reading the one parameter
/// the entry names.
fn parameter_read_document(name: &str) -> Value {
    json!({
        "Statement": [
            {
                "Action": ["ssm:DescribeParameters"],
                "Effect": "Allow",
                "Resource": "*",
            },
            {
                "Action": [
                    "ssm:GetParameters",
                    "ssm:GetParameter",
                    "ssm:GetParameterHistory",
                ],
                "Effect": "Allow",
                "Resource": {"Fn::Sub": [
                    "arn:${AWS::Partition}:ssm:${AWS::Region}:${AWS::AccountId}:parameter/\
                     ${parameterName}",
                    {"parameterName": name},
                ]},
            },
        ],
    })
}

/// The document `S3ReadPolicy` grants over the bucket `name` — the bucket and the objects in
/// it, which is what every bucket-scoped template is granted over.
fn bucket_read_document(name: &str) -> Value {
    json!({
        "Statement": [{
            "Action": [
                "s3:GetObject",
                "s3:ListBucket",
                "s3:GetBucketLocation",
                "s3:GetObjectVersion",
                "s3:GetLifecycleConfiguration",
            ],
            "Effect": "Allow",
            "Resource": [
                {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${bucketName}", {"bucketName": name}]},
                {"Fn::Sub": ["arn:${AWS::Partition}:s3:::${bucketName}/*", {"bucketName": name}]},
            ],
        }],
    })
}

/// One inline policy of the generated role, as the processed template carries it.
fn inline(name: &str, document: Value) -> Value {
    json!({ "PolicyName": name, "PolicyDocument": document })
}

/// A function declaration whose `Policies` is `policies`, spelled exactly as the row states
/// it, or one that states none at all.
fn function(logical_id: &str, policies: &str) -> String {
    let policies = match policies.is_empty() {
        true => String::new(),
        false => format!(r#","Policies":{policies}"#),
    };
    format!(
        r#"{{"{logical_id}":{{"Type":"AWS::Serverless::Function","Properties":{{
             "Handler":"index.handler","Runtime":"python3.12",
             "CodeUri":"s3://code-bucket/{logical_id}.zip"{policies}}}}}}}"#
    )
}

/// A document an entry states outright, which `Statement` is what makes a value one.
fn stated_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sns:Publish",
            "Resource": "*",
        }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a parameter-read policy template, whose two statements are scoped \
                   differently",
            stack: "samprocessed-parameter-read",
            owner: "Fn",
            declared: r#"[{"SSMParameterReadPolicy":{"ParameterName":"app/config/feature"}}]"#
                .to_string(),
            expected: Some(json!([inline(
                "FnRolePolicy0",
                parameter_read_document("app/config/feature")
            )])),
            managed: vec![BASIC_EXECUTION_ARN],
        },
        Row {
            case: "a bucket-read policy template, granted over two resources",
            stack: "samprocessed-bucket-read",
            owner: "Reader",
            declared: r#"[{"S3ReadPolicy":{"BucketName":"reports-bucket"}}]"#.to_string(),
            expected: Some(json!([inline(
                "ReaderRolePolicy0",
                bucket_read_document("reports-bucket")
            )])),
            managed: vec![BASIC_EXECUTION_ARN],
        },
        Row {
            case: "a managed policy ARN, then a policy template, then a document stated \
                   outright: the attached entry leaves its position behind",
            stack: "samprocessed-mixed",
            owner: "Mixed",
            declared: format!(
                r#"["{READ_ONLY_ARN}",
                    {{"SSMParameterReadPolicy":{{"ParameterName":"shared/secret-name"}}}},
                    {}]"#,
                stated_document()
            ),
            expected: Some(json!([
                inline(
                    "MixedRolePolicy1",
                    parameter_read_document("shared/secret-name")
                ),
                inline("MixedRolePolicy2", stated_document()),
            ])),
            managed: vec![BASIC_EXECUTION_ARN, READ_ONLY_ARN],
        },
        Row {
            case: "the single-value spelling of the property: one template entry, not in a \
                   list",
            stack: "samprocessed-single-value",
            owner: "Lone",
            declared: r#"{"SSMParameterReadPolicy":{"ParameterName":"lone/parameter"}}"#
                .to_string(),
            expected: Some(json!([inline(
                "LoneRolePolicy0",
                parameter_read_document("lone/parameter")
            )])),
            managed: vec![BASIC_EXECUTION_ARN],
        },
        Row {
            case: "a declaration that states no Policies at all",
            stack: "samprocessed-none",
            owner: "Bare",
            declared: String::new(),
            expected: None,
            managed: vec![BASIC_EXECUTION_ARN],
        },
    ]
}

/// The processed template of `stack`, as the document a client parses.
async fn processed_template(router: &MultiServiceRouter, case: &str, stack: &str) -> Value {
    let (code, body) = call(
        router,
        "GetTemplate",
        &[
            member("StackName", stack),
            member("TemplateStage", "Processed"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetTemplate to answer, got {code} {body}"
    );
    let template = xml_text(&body, "TemplateBody")
        .map(|template| xml_unescape(&template))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a processed template body, got {body}")
        });
    ::serde_json::from_str(&template).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a JSON processed template, got {error} on {template}")
    })
}

#[tokio::test]
async fn the_processed_template_carries_the_generated_roles_expanded_policies() {
    let (router, engine) = router_with_engine();
    for row in rows() {
        let case = row.case;
        let template = sam_template(&function(row.owner, &row.declared));
        stage_code_objects(&router, case, &template).await;
        let (code, body) = create_stack(
            &router,
            row.stack,
            &template,
            &kv(&[
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
            ]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;
        let (code, body) = call(
            &router,
            "DescribeStacks",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to deploy what the transform generated, got \
             {code} {body}"
        );

        // 1. The generated role's own resource in the PROCESSED template carries the
        // expansion of what the declaration asked for.
        let processed = processed_template(&router, case, row.stack).await;
        let role = format!("{}Role", row.owner);
        let generated = processed
            .get("Resources")
            .and_then(|resources| resources.get(&role))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            generated.get("Type").and_then(Value::as_str),
            Some("AWS::IAM::Role"),
            "case {case:?}: expected the processed template to declare {role:?} as a role, got \
             {processed}"
        );
        let properties = generated.get("Properties").cloned().unwrap_or(Value::Null);
        assert_eq!(
            properties.get("Policies"),
            row.expected.as_ref(),
            "case {case:?}: expected {role:?} to carry the Policies the declaration expands \
             into, got {properties}"
        );
        let expected_managed: Vec<Value> =
            row.managed.iter().map(|arn| json!(arn)).collect();
        assert_eq!(
            properties.get("ManagedPolicyArns"),
            Some(&Value::Array(expected_managed.clone())),
            "case {case:?}: expected {role:?} to attach exactly {expected_managed:?}, got \
             {properties}"
        );

        // 2. ...and the role the stack really deployed holds that same document, which is the
        // evidence the expanded policy was installed rather than only written down.
        let (code, body) = call(
            &router,
            "DescribeStackResources",
            &[
                member("StackName", row.stack),
                member("LogicalResourceId", &role),
            ],
        )
        .await;
        let deployed = xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
            panic!("case {case:?}: expected a physical id for {role:?}, got {code} {body}")
        });
        for entry in row.expected.as_ref().and_then(Value::as_array).unwrap_or(&Vec::new()) {
            let name = entry["PolicyName"].as_str().unwrap_or_default();
            let (code, body) = iam(
                &router,
                "GetRolePolicy",
                &[("RoleName", &deployed), ("PolicyName", name)],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected GetRolePolicy({deployed:?}, {name:?}) to answer, got \
                 {code} {body}"
            );
            let held = xml_policy_document(&body, "PolicyDocument").unwrap_or_else(|| {
                panic!("case {case:?}: expected a document for {name:?}, got {body}")
            });
            // The deployed document is the expansion with CloudFormation's substitutions
            // carried out, so what is asserted of it is that every action the entry grants and
            // every resource it is granted over is in it.
            for granted in granted_texts(&entry["PolicyDocument"]) {
                assert!(
                    held.contains(granted.as_str()),
                    "case {case:?} policy {name:?}: expected the deployed document to grant \
                     {granted:?}, got {held:?}"
                );
            }
        }
    }
}

/// Every action one expanded document grants, and every literal ARN segment it is granted
/// over — the texts the DEPLOYED document must carry once CloudFormation has substituted the
/// partition, the Region and the account into the ARNs.
fn granted_texts(document: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    let mut walk = vec![document.clone()];
    while let Some(value) = walk.pop() {
        match value {
            Value::Object(members) => {
                for (name, member) in members {
                    // A substitution's pattern names what CloudFormation fills in, so the
                    // pattern itself is not text the deployed document carries; its VALUES
                    // are.
                    match (name.as_str(), &member) {
                        ("Fn::Sub", Value::Array(arguments)) => {
                            walk.extend(arguments.iter().skip(1).cloned());
                        }
                        _ => walk.push(member),
                    }
                }
            }
            Value::Array(items) => walk.extend(items),
            Value::String(text) if text != "*" && !text.contains("${") => texts.push(text),
            _ => {}
        }
    }
    texts
}
