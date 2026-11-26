// SPDX-License-Identifier: CC0-1.0

//! `AWS::GameLift::Build` and `AWS::GameLift::Script` are resource types a template may
//! DECLARE: every operation that submits a template admits a body declaring one, and the
//! catalogue entries behind that say what a resource of either type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes** —
//! the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation
//! does NOT know is refused `Template format error: Unrecognized resource types: [<types>]`,
//! and that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Amazon GameLift Servers' own
//! `CreateBuild`/`CreateScript`, the id that service mints, the resource's removal with the
//! stack and the refusals it answers — needs that service mounted beside this one, so it
//! lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_gamelift_builds_and_scripts.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is
//! attempted.
//!
//! The rows differ along what the rule generalises over: which of the two types the body
//! declares, the declared properties (the name and the storage location alone, those plus the
//! operating system and server SDK version, those plus the type's own `Tags`, a storage
//! location naming an object version), the template spelling (JSON and YAML), how many
//! resources one body declares, and whether the declaration stands beside a resource of
//! another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    GAMELIFT_BUILD, GAMELIFT_SCRIPT,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The Amazon S3 bucket the game code the bodies below ship lives in, and the role that lets
/// Amazon GameLift Servers read it.
const BUCKET: &str = "studio-game-artifacts";
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/GameLiftS3Access";

/// One row: a template body declaring one of the two types, and the name of the stack it is
/// submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own build stack: a build read from an uploaded S3 object, with both
        // published return values read back through `Outputs`.
        Row {
            case: "a build with every published return value read back, JSON spelling",
            stack_name: "gamelift-build-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Build": {{"Type": "{GAMELIFT_BUILD}",
                       "Properties": {{"Name": "demo-build",
                         "OperatingSystem": "AMAZON_LINUX_2023",
                         "ServerSdkVersion": "5.1.2",
                         "StorageLocation": {{"Bucket": "{BUCKET}", "Key": "build.zip",
                           "RoleArn": "{ROLE_ARN}"}}}}}}}},
                     "Outputs": {{"BuildRef": {{"Value": {{"Ref": "Build"}}}},
                       "BuildId": {{"Value": {{"Fn::GetAtt": ["Build", "BuildId"]}}}},
                       "BuildArn": {{"Value": {{"Fn::GetAtt": ["Build", "Arn"]}}}}}}}}"#
            ),
        },
        // A build declaring only the two properties its create needs, with no operating
        // system or server SDK version at all.
        Row {
            case: "a build with the name and the storage location alone",
            stack_name: "gamelift-build-minimal",
            template: format!(
                r#"{{"Resources": {{"Build": {{"Type": "{GAMELIFT_BUILD}",
                     "Properties": {{"Name": "minimal",
                       "OperatingSystem": "WINDOWS_2016",
                       "StorageLocation": {{"Bucket": "{BUCKET}", "Key": "win.zip",
                         "RoleArn": "{ROLE_ARN}"}}}}}}}}}}"#
            ),
        },
        // The script type in YAML with short-form intrinsics, the type's own `Tags`, and a
        // storage location naming an object version.
        Row {
            case: "a script in the YAML spelling, with Tags and a versioned object",
            stack_name: "gamelift-script-yaml",
            template: format!(
                "\
Resources:
  Script:
    Type: {GAMELIFT_SCRIPT}
    Properties:
      Name: demo-script
      NodeJsVersion: 24.x
      Version: \"1.0.0\"
      StorageLocation:
        Bucket: {BUCKET}
        Key: script.zip
        RoleArn: {ROLE_ARN}
        ObjectVersion: 3HL4kqtJlcpXroDTDmjVBH40Nrjfkd
      Tags:
        - Key: team
          Value: liveops
Outputs:
  ScriptId:
    Value: !Ref Script
  ScriptArn:
    Value: !GetAtt Script.Arn
"
            ),
        },
        // Two resources of the two types in one body, beside a resource of another type: the
        // catalogue admits every type a body declares or none of them.
        Row {
            case: "a build and a script beside a resource of another type",
            stack_name: "gamelift-pair-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "Build": {{"Type": "{GAMELIFT_BUILD}",
                       "Properties": {{"Name": "paired-build",
                         "OperatingSystem": "AMAZON_LINUX_2",
                         "StorageLocation": {{"Bucket": "{BUCKET}", "Key": "a.zip",
                           "RoleArn": "{ROLE_ARN}"}}}}}},
                     "Script": {{"Type": "{GAMELIFT_SCRIPT}",
                       "Properties": {{"Name": "paired-script", "NodeJsVersion": "10.x",
                         "StorageLocation": {{"Bucket": "{BUCKET}", "Key": "b.zip",
                           "RoleArn": "{ROLE_ARN}"}}}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "gamelift-topic"}}}}}}}}"#
            ),
        },
    ]
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, status: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got {status} \
         with body {body}"
    );
}

/// Every operation that submits a template for deployment admits a body declaring either
/// type, and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_the_gamelift_types() {
    for row in rows() {
        let router = router();
        let (status, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_admitted(row.case, status, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {:?}: expected CreateStack to answer a stack id, got {body}",
            row.case
        );

        let (status, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &row.template)],
        )
        .await;
        assert_admitted(row.case, status, &body);

        let (status, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{}-change-set", row.stack_name)),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &row.template),
            ],
        )
        .await;
        assert_admitted(row.case, status, &body);
    }
}

/// The catalogue entries the rule rests on: both types are published and recognised, a
/// near-miss spelling of either still is not, and each type reports the identity and the
/// return values its own "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_gamelift_types_and_what_they_return() {
    for resource_type in [GAMELIFT_BUILD, GAMELIFT_SCRIPT] {
        assert!(
            AWS_RESOURCE_TYPES.contains(&resource_type),
            "expected {resource_type:?} in the published catalogue, it is absent"
        );
        assert!(
            is_published(resource_type) && is_recognized(resource_type),
            "expected {resource_type:?} to be a type a template may declare, it is not"
        );
        // Amazon GameLift Servers mints the id a build or a script is addressed by, so the
        // identifier in force is the one that service answered rather than a name computed
        // from the template.
        assert!(
            identity_is_service_assigned(resource_type),
            "expected {resource_type:?} to take the identity that service assigns, it does not"
        );
    }
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::GameLift::Builds",
        "AWS::GameLift::BuildVersion",
        "AWS::GameLift::Scripts",
        "AWS::GameLift::RealtimeScript",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // Source: each type's own "Return values" section — a build's `BuildId` and the `Arn`
    // built around it, and a script's `Id` and `Arn`.
    assert_eq!(
        published_attribute_names(GAMELIFT_BUILD),
        &["Arn", "BuildId"],
        "expected the build's minted id and ARN to be its return values, got {:?}",
        published_attribute_names(GAMELIFT_BUILD)
    );
    assert_eq!(
        published_attribute_names(GAMELIFT_SCRIPT),
        &["Arn", "Id"],
        "expected the script's minted id and ARN to be its return values, got {:?}",
        published_attribute_names(GAMELIFT_SCRIPT)
    );
    // `UpdateBuild` takes a build's name and version and nothing else, and a script's runtime
    // "cannot be updated later", so those are the create-only properties.
    assert_eq!(
        replacement_properties(GAMELIFT_BUILD),
        &["OperatingSystem", "ServerSdkVersion", "StorageLocation"],
        "expected the build's create-only properties, got {:?}",
        replacement_properties(GAMELIFT_BUILD)
    );
    assert_eq!(
        replacement_properties(GAMELIFT_SCRIPT),
        &["NodeJsVersion"],
        "expected the script's runtime to be its create-only property, got {:?}",
        replacement_properties(GAMELIFT_SCRIPT)
    );
}
