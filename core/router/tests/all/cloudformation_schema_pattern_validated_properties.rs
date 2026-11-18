// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with a declared property value that violates the PATTERN
//! its resource type's schema states for that property, end to end over four services'
//! wires.
//!
//! The rule under test: **CloudFormation validates each declared resource against its
//! resource type's schema before it provisions anything, so a property value outside the
//! pattern that schema states for the property fails the operation before the resource is
//! attempted** — the stack rolls back, its own event stream carries one `CREATE_FAILED` event
//! for the STACK (`DetailedStatus` `VALIDATION_FAILED`, carrying the summary over the stage's
//! errors), the operation's events carry one `VALIDATION_ERROR` naming the validation, the
//! value and the pattern at that property's own path, and the resource the declaration is
//! about has no event of any kind because nothing was asked of the service that owns it.
//!
//! Without that stage a client sees only `ROLLBACK_COMPLETE` and can learn nothing further:
//! the documented diagnosis path is the stack's failure reason, then `DescribeEvents` for the
//! property detail.
//!
//! The rows differ along what the rule generalises over: which resource TYPE the declaration
//! is of and which service owns it (AWS CodePipeline, AWS Lambda), which PROPERTY carries the
//! violating value (a role ARN, a resource name), and what KIND of value violates the pattern
//! (text that is no ARN at all, an ARN of another service, a name over none of the characters
//! the alphabet admits, a role NAME where an ARN is stated) — plus the control row, whose
//! every value its pattern admits and whose pipeline is really created in CodePipeline.
//!
//! Three crates have to agree for that to hold (CloudFormation runs the stage; CodePipeline
//! and AWS Lambda are the services that must be left untouched, and the one that really
//! creates the control row's pipeline), so this suite lives here. Assembling the whole router
//! costs stack (see `boot_serve.rs`), so it is built — and the requests driven — on a sized
//! thread; the wire harness and the poll are the test crate's own
//! ([`crate::cfn_codepipeline_wire`]), and the awsQuery form encoder and XML readers are
//! [`crate::query_wire`]'s.

use serde_json::{json, Value};

use crate::cfn_codepipeline_wire::{
    until, Emulator, BOOT_STACK_SIZE, CREATE_COMPLETE, ROLLBACK_COMPLETE,
};
use crate::query_wire::{element, elements, members_of, opt_element};

/// `StackEvent$ResourceStatus` of the event a failed operation records for the stack itself.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// CloudFormation model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE,
/// VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The `EventType` a validation result is reported under, and the name CloudFormation gives
/// the validation that holds a declaration's properties to its type's contract.
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";
/// See [`VALIDATION_ERROR`].
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The pattern the schema of `AWS::CodePipeline::Pipeline` states for `RoleArn`: the IAM role
/// ARN form CodePipeline publishes for `PipelineDeclaration$roleArn`.
const PIPELINE_ROLE_ARN_PATTERN: &str = r"arn:aws(-[\w]+)*:iam::[0-9]{12}:role/.*";

/// ...and the one it states for `Name`, the alphabet CodePipeline publishes for
/// `PipelineDeclaration$name`.
const PIPELINE_NAME_PATTERN: &str = r"[A-Za-z0-9.@\-_]+";

/// ...and the one the schema of `AWS::Lambda::Function` states for `Role`, the execution role
/// ARN form AWS Lambda publishes for `CreateFunction$Role`.
const FUNCTION_ROLE_PATTERN: &str =
    r"^arn:(aws[a-zA-Z-]*)?:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$";

/// How CloudFormation summarises a failed pre-deployment property validation ON THE STACK:
/// how many errors the stage found, and that the detail of each is on the operation's own
/// events. The one variable part is the count.
fn validation_summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How CloudFormation reports ONE property value outside the pattern its type's schema states
/// for the property.
fn pattern_reason(value: &str, pattern: &str) -> String {
    format!("Property value [{value}] does not match pattern: {pattern}")
}

/// What one row expects of the validation: the property that fails, the value it carried and
/// the pattern that value violates.
struct Violation {
    property: &'static str,
    value: &'static str,
    pattern: &'static str,
}

/// One row: a stack to deploy, and the property violation its declaration carries — `None`
/// for the control row, every value of which its pattern admits.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the resource the declaration is about.
    logical_id: &'static str,
    resource_type: &'static str,
    template: fn() -> String,
    /// Whether the template declares an IAM resource, which the deployment acknowledges.
    iam: bool,
    violation: Option<Violation>,
}

/// The artifact bucket every pipeline template below stores into.
const BUCKET_ID: &str = "ArtifactBucket";

/// ...and the role declaration a pipeline assumes, for the rows that declare one.
fn pipeline_role() -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "codepipeline.amazonaws.com" },
                    "Action": "sts:AssumeRole"
                }]
            }
        }
    })
}

/// The stages a declared pipeline carries: one S3 source, one manual approval.
fn pipeline_stages() -> Value {
    json!([
        {
            "Name": "Source",
            "Actions": [{
                "Name": "Fetch",
                "ActionTypeId": {
                    "Category": "Source", "Owner": "AWS", "Provider": "S3", "Version": "1"
                },
                "Configuration": {
                    "S3Bucket": { "Ref": BUCKET_ID },
                    "S3ObjectKey": "source.zip",
                    "PollForSourceChanges": "false"
                },
                "OutputArtifacts": [{ "Name": "SourceArtifact" }],
                "RunOrder": 1
            }]
        },
        {
            "Name": "Approve",
            "Actions": [{
                "Name": "Approval",
                "ActionTypeId": {
                    "Category": "Approval", "Owner": "AWS", "Provider": "Manual",
                    "Version": "1"
                },
                "RunOrder": 1
            }]
        }
    ])
}

/// A template whose pipeline carries `properties` beside the artifact bucket it stores into,
/// and the role declaration where `with_role` asks for one.
fn pipeline_template(properties: Value, with_role: bool) -> String {
    let mut resources = json!({
        BUCKET_ID: { "Type": "AWS::S3::Bucket" },
        "Pipeline": { "Type": "AWS::CodePipeline::Pipeline", "Properties": properties },
    });
    if with_role {
        resources["PipelineRole"] = pipeline_role();
    }
    json!({ "Resources": resources }).to_string()
}

/// The pipeline name the rows that declare one ask for.
const DECLARED_PIPELINE: &str = "cfn-pattern-pipeline";

/// ...and the function name the AWS Lambda row asks for.
const DECLARED_FUNCTION: &str = "cfn-pattern-function";

/// The value the recorded client deployed: text that is no ARN at all.
const NOT_AN_ARN: &str = "not-an-arn";

/// An ARN of a service other than IAM, which is no role ARN either.
const ANOTHER_SERVICES_ARN: &str = "arn:aws:codepipeline:us-east-1:000000000000:role/pipeline";

/// A pipeline name over none of the characters its alphabet admits.
const NO_ADMITTED_CHARACTER: &str = "!!!";

/// A role NAME where the property states an ARN.
const A_ROLE_NAME: &str = "cfn-pattern-exec-role";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a pipeline whose role ARN is no ARN at all",
            stack: "cfn-pattern-no-arn",
            logical_id: "Pipeline",
            resource_type: "AWS::CodePipeline::Pipeline",
            template: || {
                pipeline_template(
                    json!({
                        "Name": DECLARED_PIPELINE,
                        "RoleArn": NOT_AN_ARN,
                        "ArtifactStore": { "Type": "S3", "Location": { "Ref": BUCKET_ID } },
                        "Stages": pipeline_stages(),
                    }),
                    false,
                )
            },
            iam: false,
            violation: Some(Violation {
                property: "RoleArn",
                value: NOT_AN_ARN,
                pattern: PIPELINE_ROLE_ARN_PATTERN,
            }),
        },
        Row {
            case: "a pipeline whose role ARN names a service other than IAM",
            stack: "cfn-pattern-other-service-arn",
            logical_id: "Pipeline",
            resource_type: "AWS::CodePipeline::Pipeline",
            template: || {
                pipeline_template(
                    json!({
                        "Name": DECLARED_PIPELINE,
                        "RoleArn": ANOTHER_SERVICES_ARN,
                        "ArtifactStore": { "Type": "S3", "Location": { "Ref": BUCKET_ID } },
                        "Stages": pipeline_stages(),
                    }),
                    false,
                )
            },
            iam: false,
            violation: Some(Violation {
                property: "RoleArn",
                value: ANOTHER_SERVICES_ARN,
                pattern: PIPELINE_ROLE_ARN_PATTERN,
            }),
        },
        Row {
            case: "a pipeline whose name is over no character its alphabet admits",
            stack: "cfn-pattern-name",
            logical_id: "Pipeline",
            resource_type: "AWS::CodePipeline::Pipeline",
            template: || {
                pipeline_template(
                    json!({
                        "Name": NO_ADMITTED_CHARACTER,
                        "RoleArn": { "Fn::GetAtt": ["PipelineRole", "Arn"] },
                        "ArtifactStore": { "Type": "S3", "Location": { "Ref": BUCKET_ID } },
                        "Stages": pipeline_stages(),
                    }),
                    true,
                )
            },
            iam: true,
            violation: Some(Violation {
                property: "Name",
                value: NO_ADMITTED_CHARACTER,
                pattern: PIPELINE_NAME_PATTERN,
            }),
        },
        Row {
            case: "a function whose execution role is a role name rather than an ARN",
            stack: "cfn-pattern-function-role",
            logical_id: "Function",
            resource_type: "AWS::Lambda::Function",
            template: || {
                json!({
                    "Resources": {
                        "Function": {
                            "Type": "AWS::Lambda::Function",
                            "Properties": {
                                "FunctionName": DECLARED_FUNCTION,
                                "Runtime": "python3.12",
                                "Handler": "index.handler",
                                "Role": A_ROLE_NAME,
                                "Code": { "ZipFile": "def handler(event, context):\n    return 1\n" },
                            },
                        },
                    },
                })
                .to_string()
            },
            iam: false,
            violation: Some(Violation {
                property: "Role",
                value: A_ROLE_NAME,
                pattern: FUNCTION_ROLE_PATTERN,
            }),
        },
        Row {
            case: "a pipeline every value of which its pattern admits",
            stack: "cfn-pattern-control",
            logical_id: "Pipeline",
            resource_type: "AWS::CodePipeline::Pipeline",
            template: || {
                pipeline_template(
                    json!({
                        "Name": DECLARED_PIPELINE,
                        "RoleArn": { "Fn::GetAtt": ["PipelineRole", "Arn"] },
                        "ArtifactStore": { "Type": "S3", "Location": { "Ref": BUCKET_ID } },
                        "Stages": pipeline_stages(),
                    }),
                    true,
                )
            },
            iam: true,
            violation: None,
        },
    ]
}

/// One `DescribeStackEvents` row, as the fields this suite reads off it.
struct StackEvent {
    logical_id: String,
    resource_status: String,
    detailed_status: Option<String>,
    reason: Option<String>,
}

/// Every event of one stack, newest first as the service reports them.
fn stack_events(emulator: &Emulator, stack: &str) -> (u16, String, Vec<StackEvent>) {
    let (status, body) = emulator.cloudformation("DescribeStackEvents", &[("StackName", stack)]);
    let events = members_of(&body)
        .into_iter()
        .filter_map(|event| {
            Some(StackEvent {
                logical_id: elements(event, "LogicalResourceId").into_iter().next()?,
                resource_status: elements(event, "ResourceStatus").into_iter().next()?,
                detailed_status: elements(event, "DetailedStatus").into_iter().next(),
                reason: elements(event, "ResourceStatusReason").into_iter().next(),
            })
        })
        .collect();
    (status, body, events)
}

/// Every validation result the operation's own events carry, as the fields the detail is made
/// of.
fn validation_results(emulator: &Emulator, stack: &str) -> (u16, String, Vec<Vec<String>>) {
    let (status, body) = emulator.cloudformation("DescribeEvents", &[("StackName", stack)]);
    let results = members_of(&body)
        .into_iter()
        .filter(|event| !elements(event, "ValidationName").is_empty())
        .map(|event| {
            [
                "LogicalResourceId",
                "ResourceType",
                "EventType",
                "ValidationName",
                "ValidationStatus",
                "ValidationFailureMode",
                "ValidationStatusReason",
                "ValidationPath",
            ]
            .iter()
            .map(|field| {
                elements(event, field)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            })
            .collect()
        })
        .collect();
    (status, body, results)
}

#[test]
fn a_property_value_outside_its_schemas_pattern_fails_before_anything_is_provisioned() {
    std::thread::Builder::new()
        .name("cfn-schema-pattern-validation".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();

            for row in rows() {
                let case = row.case;
                let template = (row.template)();
                let mut members = vec![
                    ("StackName", row.stack),
                    ("TemplateBody", template.as_str()),
                ];
                if row.iam {
                    members.push(("Capabilities.member.1", "CAPABILITY_IAM"));
                }
                let (status, created) = emulator.cloudformation("CreateStack", &members);
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateStack must answer 200 — the request is valid and the \
                     stack exists — got {status} {created}"
                );

                let expected_status = match row.violation {
                    Some(_) => ROLLBACK_COMPLETE,
                    None => CREATE_COMPLETE,
                };
                let (status, described) = until(
                    || emulator.cloudformation("DescribeStacks", &[("StackName", row.stack)]),
                    |(_, described)| element(described, "StackStatus") == expected_status,
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: the stack must be readable, got {status} {described}"
                );
                let settled = element(&described, "StackStatus");
                assert_eq!(
                    settled, expected_status,
                    "case {case:?}: expected the stack to settle {expected_status}, got \
                     {settled:?} with reason {:?}",
                    opt_element(&described, "StackStatusReason")
                );

                // What the STACK reports for the failure: its own CREATE_FAILED event, with
                // the detailed status the model gives a validation failure and the summary
                // that points a client at the per-property detail below.
                let (status, body, events) = stack_events(&emulator, row.stack);
                assert_eq!(
                    status, 200,
                    "case {case:?}: the stack's events must be readable, got {status} {body}"
                );
                let failed: Vec<(Option<String>, Option<String>)> = events
                    .iter()
                    .filter(|event| {
                        event.logical_id == row.stack && event.resource_status == CREATE_FAILED
                    })
                    .map(|event| (event.detailed_status.clone(), event.reason.clone()))
                    .collect();
                let expected_failed: Vec<(Option<String>, Option<String>)> = row
                    .violation
                    .as_ref()
                    .map(|_| {
                        vec![(
                            Some(VALIDATION_FAILED.to_string()),
                            Some(validation_summary(1)),
                        )]
                    })
                    .unwrap_or_default();
                assert_eq!(
                    failed, expected_failed,
                    "case {case:?}: expected exactly the stack failure event \
                     {expected_failed:?}, got {body}"
                );

                // ...and the DETAIL of it, on the operation's own events: which validation
                // failed, on which resource, the value and the pattern it does not match, at
                // the path of the property that was judged.
                let (status, body, results) = validation_results(&emulator, row.stack);
                assert_eq!(
                    status, 200,
                    "case {case:?}: the operation's events must be readable, got {status} {body}"
                );
                let expected_results: Vec<Vec<String>> = row
                    .violation
                    .as_ref()
                    .map(|violation| {
                        vec![vec![
                            row.logical_id.to_string(),
                            row.resource_type.to_string(),
                            VALIDATION_ERROR.to_string(),
                            PROPERTY_VALIDATION.to_string(),
                            "FAILED".to_string(),
                            "FAIL".to_string(),
                            pattern_reason(violation.value, violation.pattern),
                            format!(
                                "/Resources/{}/Properties/{}",
                                row.logical_id, violation.property
                            ),
                        ]]
                    })
                    .unwrap_or_default();
                assert_eq!(
                    results, expected_results,
                    "case {case:?}: expected the validation results {expected_results:?}, got \
                     {results:?}"
                );

                // Nothing was attempted: the stack's event stream is about the STACK alone,
                // so no resource the template declared reports an event of its own. The
                // control row IS deployed, so its resources report their transitions.
                let about_a_resource: Vec<String> = events
                    .iter()
                    .filter(|event| event.logical_id != row.stack)
                    .map(|event| event.logical_id.clone())
                    .collect();
                assert_eq!(
                    about_a_resource.is_empty(),
                    row.violation.is_some(),
                    "case {case:?}: a refused declaration must leave the resources unattempted \
                     and a deployed one must report them, got {about_a_resource:?}"
                );

                // ...and the service that OWNS the declared type is the proof: a refused
                // declaration left nothing there at all, while the control row's pipeline is
                // really held by CodePipeline, assuming the role the stack created for it.
                match row.resource_type {
                    "AWS::Lambda::Function" => {
                        let (status, body) = emulator.get_function(DECLARED_FUNCTION);
                        assert_ne!(
                            status, 200,
                            "case {case:?}: a refused declaration must leave no function in AWS \
                             Lambda, got {status} {body}"
                        );
                    }
                    _ => {
                        let (status, body) = emulator
                            .codepipeline("GetPipeline", json!({ "name": DECLARED_PIPELINE }));
                        match row.violation {
                            // The refused declarations left no pipeline behind at all.
                            Some(_) => assert_ne!(
                                status, 200,
                                "case {case:?}: a refused declaration must leave no pipeline in \
                                 CodePipeline, got {status} {body}"
                            ),
                            // ...and the control row's pipeline is really there, assuming the
                            // role the stack created for it.
                            None => {
                                assert_eq!(
                                    status, 200,
                                    "case {case:?}: the deployed pipeline must be readable in \
                                     CodePipeline, got {status} {body}"
                                );
                                let role = body["pipeline"]["roleArn"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                let (code, resources) = emulator.cloudformation(
                                    "DescribeStackResources",
                                    &[("StackName", row.stack)],
                                );
                                assert_eq!(
                                    code, 200,
                                    "case {case:?}: the stack's resources must be readable, got \
                                     {code} {resources}"
                                );
                                let role_name = members_of(&resources)
                                    .into_iter()
                                    .find(|member| {
                                        elements(member, "LogicalResourceId")
                                            .first()
                                            .map(String::as_str)
                                            == Some("PipelineRole")
                                    })
                                    .and_then(|member| {
                                        elements(member, "PhysicalResourceId").into_iter().next()
                                    })
                                    .unwrap_or_default();
                                assert!(
                                    role.ends_with(&format!(":role/{role_name}"))
                                        && !role_name.is_empty(),
                                    "case {case:?}: expected the pipeline to assume the role the \
                                     stack created ({role_name:?}), got {role:?}"
                                );
                            }
                        }
                    }
                }
            }
        })
        .expect("the sized thread spawns")
        .join()
        .expect("the suite's thread runs to completion");
}
