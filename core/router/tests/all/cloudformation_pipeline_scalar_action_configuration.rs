// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack hands AWS CodePipeline for an action whose declaration spells
//! a value as a bare scalar, end to end over both services' wires.
//!
//! The rule under test: **a value a template spells as a bare scalar for a property the
//! owning API states as TEXT is handed over as that scalar's own text, so the declaration
//! deploys as written** — an action's `Configuration` is a map of string to string
//! (`ActionConfigurationMap`) and its `ActionTypeId` members are strings
//! (`ActionTypeId$category`/`owner`/`provider`/`version`), while a template may write
//! `PollForSourceChanges: false` or `Version: 1` without quotes, which is the spelling
//! CloudFormation accepts and passes on as `"false"` and `"1"`. Handing the JSON spelling
//! through instead has the owning service refuse the create for an ill-typed member, and the
//! stack rolls back reporting a refusal AWS never makes.
//!
//! The rows differ along what the rule generalises over: WHICH kind of scalar the declaration
//! carries (a boolean, an integer) and WHERE it carries it (a configuration value, an action
//! type's version) — beside the control row, which quotes every one of them and must deploy to
//! exactly the same pipeline.
//!
//! Two crates have to agree for that to hold (CloudFormation makes the call, CodePipeline
//! serves it), so this suite lives here. Assembling the whole router costs stack (see
//! `boot_serve.rs`), so it is built — and the requests driven — on a sized thread; the wire
//! harness and the poll are the test crate's own ([`crate::cfn_codepipeline_wire`]), and the
//! awsQuery form encoder and XML readers are [`crate::query_wire`]'s.

use serde_json::{json, Value};

use crate::cfn_codepipeline_wire::{until, Emulator, BOOT_STACK_SIZE, CREATE_COMPLETE};
use crate::query_wire::{element, opt_element};

/// The artifact bucket every template below stores into.
const BUCKET_ID: &str = "ArtifactBucket";

/// One row: a stack, and the spelling its source action's declaration carries.
struct Row {
    case: &'static str,
    stack: &'static str,
    pipeline: &'static str,
    /// The `PollForSourceChanges` the configuration carries, in the row's own spelling.
    poll_for_source_changes: Value,
    /// ...and the `Version` of the action type, in the row's own spelling.
    version: Value,
    /// The `RunOrder` the action carries, in the row's own spelling.
    run_order: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every value quoted, as the model states them",
            stack: "cfn-scalar-config-quoted",
            pipeline: "cfn-scalar-config-quoted-pipeline",
            poll_for_source_changes: json!("false"),
            version: json!("1"),
            run_order: json!(1),
        },
        Row {
            case: "a boolean configuration value and an integer action type version",
            stack: "cfn-scalar-config-bare",
            pipeline: "cfn-scalar-config-bare-pipeline",
            poll_for_source_changes: json!(false),
            version: json!(1),
            run_order: json!(1),
        },
    ]
}

/// The template one row deploys: the artifact bucket, the role the pipeline assumes, and the
/// pipeline whose source action carries the row's spellings.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            BUCKET_ID: { "Type": "AWS::S3::Bucket" },
            "PipelineRole": {
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
            },
            "Pipeline": {
                "Type": "AWS::CodePipeline::Pipeline",
                "Properties": {
                    "Name": row.pipeline,
                    "RoleArn": { "Fn::GetAtt": ["PipelineRole", "Arn"] },
                    "ArtifactStore": { "Type": "S3", "Location": { "Ref": BUCKET_ID } },
                    "Stages": [{
                        "Name": "Source",
                        "Actions": [{
                            "Name": "Fetch",
                            "ActionTypeId": {
                                "Category": "Source",
                                "Owner": "AWS",
                                "Provider": "S3",
                                "Version": row.version,
                            },
                            "Configuration": {
                                "S3Bucket": { "Ref": BUCKET_ID },
                                "S3ObjectKey": "source.zip",
                                "PollForSourceChanges": row.poll_for_source_changes,
                            },
                            "OutputArtifacts": [{ "Name": "SourceArtifact" }],
                            "RunOrder": row.run_order,
                        }]
                    }, {
                        // CodePipeline holds a pipeline to at least two stages, so the
                        // declaration carries a second one: a manual approval.
                        "Name": "Approve",
                        "Actions": [{
                            "Name": "Approval",
                            "ActionTypeId": {
                                "Category": "Approval",
                                "Owner": "AWS",
                                "Provider": "Manual",
                                "Version": "1",
                            },
                            "RunOrder": 1,
                        }]
                    }]
                }
            }
        }
    })
    .to_string()
}

#[test]
fn a_bare_scalar_action_value_is_handed_to_codepipeline_as_its_text() {
    std::thread::Builder::new()
        .name("cfn-codepipeline-scalar-configuration".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();

            for row in rows() {
                let case = row.case;
                let body = template(&row);
                let (status, created) = emulator.cloudformation(
                    "CreateStack",
                    &[
                        ("StackName", row.stack),
                        ("TemplateBody", body.as_str()),
                        ("Capabilities.member.1", "CAPABILITY_IAM"),
                    ],
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateStack must answer 200, got {status} {created}"
                );

                let (status, described) = until(
                    || emulator.cloudformation("DescribeStacks", &[("StackName", row.stack)]),
                    |(_, described)| element(described, "StackStatus") == CREATE_COMPLETE,
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: the stack must be readable, got {status} {described}"
                );
                let settled = element(&described, "StackStatus");
                assert_eq!(
                    settled, CREATE_COMPLETE,
                    "case {case:?}: expected the stack to deploy, got {settled:?} with reason \
                     {:?}",
                    opt_element(&described, "StackStatusReason")
                );

                // The pipeline CodePipeline really holds carries every one of those values as
                // the TEXT the model states, whichever way the template spelled it.
                let (status, pipeline) =
                    emulator.codepipeline("GetPipeline", json!({ "name": row.pipeline }));
                assert_eq!(
                    status, 200,
                    "case {case:?}: the deployed pipeline must be readable, got {status} \
                     {pipeline}"
                );
                let action = &pipeline["pipeline"]["stages"][0]["actions"][0];
                let polled = action["configuration"]["PollForSourceChanges"].clone();
                assert_eq!(
                    polled,
                    json!("false"),
                    "case {case:?}: expected the configuration value as its text, got {polled} \
                     ({pipeline})"
                );
                let version = action["actionTypeId"]["version"].clone();
                assert_eq!(
                    version,
                    json!("1"),
                    "case {case:?}: expected the action type version as its text, got {version} \
                     ({pipeline})"
                );
            }
        })
        .expect("the sized thread spawns")
        .join()
        .expect("the suite's thread runs to completion");
}
