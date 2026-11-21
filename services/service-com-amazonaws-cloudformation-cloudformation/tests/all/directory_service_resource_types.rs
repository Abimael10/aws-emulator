// SPDX-License-Identifier: CC0-1.0

//! `AWS::DirectoryService::MicrosoftAD` and `AWS::DirectoryService::SimpleAD` are resource
//! types a template may DECLARE: every operation that submits a template admits a body
//! declaring one, and the catalogue entries behind that say what a resource of either type
//! reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes** —
//! the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation
//! does NOT know is refused `Template format error: Unrecognized resource types: [<types>]`,
//! and that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call into Directory Service's own
//! `CreateMicrosoftAD`/`CreateDirectory`, the `d-…` id that service mints, the directory
//! reaching `Active` and its removal with the stack — needs that service and the owner of the
//! subnets its servers go in mounted beside this one, so it lives in the router's own
//! cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_directory_service_directories.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is
//! attempted.
//!
//! The rows differ along what the rule generalises over: which of the two types the body
//! declares, the declared properties (the required three alone, those plus the optional short
//! name, edition or size, description and the two flags), the template spelling (JSON and
//! YAML), how many directories one body declares, and whether the declaration stands beside a
//! resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    DIRECTORY_SERVICE_MICROSOFT_AD, DIRECTORY_SERVICE_SIMPLE_AD,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// A password the modelled `Password` pattern admits
/// (`^(?=^.{8,64}$)((?=.*\d)(?=.*[A-Z])(?=.*[a-z])|…)`).
const PASSWORD: &str = "Passw0rd!2345";

/// One row: a template body declaring one of the two types, and the name of the stack it is
/// submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own body: the three properties `CreateMicrosoftAD` requires, with the
        // type's two published return values read back through `Outputs`.
        Row {
            case: "a Managed Microsoft AD with the required properties alone",
            stack_name: "ds-microsoft-ad-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Directory": {{"Type": "{DIRECTORY_SERVICE_MICROSOFT_AD}",
                       "Properties": {{"Name": "corp.example.com", "Password": "{PASSWORD}",
                         "VpcSettings": {{"VpcId": "vpc-00000000000000001",
                           "SubnetIds": ["subnet-00000000000000001",
                                         "subnet-00000000000000002"]}}}}}}}},
                     "Outputs": {{"DirectoryRef": {{"Value": {{"Ref": "Directory"}}}},
                       "Alias": {{"Value": {{"Fn::GetAtt": ["Directory", "Alias"]}}}}}}}}"#
            ),
        },
        // The same type carrying every optional property it publishes beside those three.
        Row {
            case: "a Managed Microsoft AD carrying the short name, edition and both flags",
            stack_name: "ds-microsoft-ad-full",
            template: format!(
                r#"{{"Resources": {{"Directory": {{"Type": "{DIRECTORY_SERVICE_MICROSOFT_AD}",
                     "Properties": {{"Name": "example.corp.example.com",
                       "ShortName": "EXAMPLE", "Password": "{PASSWORD}",
                       "Edition": "Standard", "CreateAlias": true, "EnableSso": false,
                       "VpcSettings": {{"VpcId": "vpc-00000000000000001",
                         "SubnetIds": ["subnet-00000000000000001",
                                       "subnet-00000000000000002"]}}}}}}}}}}"#
            ),
        },
        // The OTHER type of the namespace, in the YAML spelling with a short-form intrinsic,
        // carrying the `Size` and `Description` a Simple AD publishes where a Managed
        // Microsoft AD publishes an `Edition`.
        Row {
            case: "a Simple AD with its size and description, YAML spelling",
            stack_name: "ds-simple-ad-yaml",
            template: format!(
                "\
Resources:
  Directory:
    Type: {DIRECTORY_SERVICE_SIMPLE_AD}
    Properties:
      Name: simple.example.com
      Password: \"{PASSWORD}\"
      Size: Small
      Description: a simple directory
      VpcSettings:
        VpcId: vpc-00000000000000001
        SubnetIds:
          - subnet-00000000000000001
          - subnet-00000000000000002
Outputs:
  DirectoryId:
    Value: !GetAtt Directory.DirectoryId
  Alias:
    Value: !GetAtt Directory.Alias
"
            ),
        },
        // One of each type in one body, beside a resource of another type: the catalogue
        // admits every type a body declares or none of them.
        Row {
            case: "one directory of each type beside a resource of another type",
            stack_name: "ds-directories-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "Managed": {{"Type": "{DIRECTORY_SERVICE_MICROSOFT_AD}",
                       "Properties": {{"Name": "first.example.com", "Password": "{PASSWORD}",
                         "VpcSettings": {{"VpcId": "vpc-00000000000000001",
                           "SubnetIds": ["subnet-00000000000000001",
                                         "subnet-00000000000000002"]}}}}}},
                     "Simple": {{"Type": "{DIRECTORY_SERVICE_SIMPLE_AD}",
                       "Properties": {{"Name": "second.example.com", "Password": "{PASSWORD}",
                         "Size": "Large",
                         "VpcSettings": {{"VpcId": "vpc-00000000000000001",
                           "SubnetIds": ["subnet-00000000000000001",
                                         "subnet-00000000000000002"]}}}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "ds-directory-topic"}}}}}}}}"#
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
async fn every_operation_that_takes_a_template_admits_a_directory() {
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
fn the_catalogue_publishes_both_directory_types_and_what_they_return() {
    for resource_type in [DIRECTORY_SERVICE_MICROSOFT_AD, DIRECTORY_SERVICE_SIMPLE_AD] {
        assert!(
            AWS_RESOURCE_TYPES.contains(&resource_type),
            "expected {resource_type:?} in the published catalogue, it is absent"
        );
        assert!(
            is_published(resource_type) && is_recognized(resource_type),
            "expected {resource_type:?} to be a type a template may declare, it is not"
        );
        // Directory Service mints the `d-…` id a directory is addressed by, so the identifier
        // in force is the one that service answered rather than a name computed from the
        // template.
        assert!(
            identity_is_service_assigned(resource_type),
            "case {resource_type:?}: expected the identity to be the one Directory Service \
             assigns, it is not"
        );
        // The single sign-on flag is the one property either type does NOT replace the
        // directory for: `EnableSso`/`DisableSso` turn it over in place.
        assert!(
            !replacement_properties(resource_type).contains(&"EnableSso"),
            "case {resource_type:?}: expected the single sign-on flag to be applied in place, \
             got {:?}",
            replacement_properties(resource_type)
        );
        for create_only in ["Password", "ShortName", "VpcSettings", "CreateAlias"] {
            assert!(
                replacement_properties(resource_type).contains(&create_only),
                "case {resource_type:?}: expected {create_only:?} to be create-only, got {:?}",
                replacement_properties(resource_type)
            );
        }
    }
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::DirectoryService::MicrosoftAd",
        "AWS::DirectoryService::ManagedAD",
        "AWS::DirectoryService::ADConnector",
        "AWS::DirectoryService::Directory",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // "Fn::GetAtt: Alias, DnsIpAddresses" for a Managed Microsoft AD, with a Simple AD
    // publishing `DirectoryId` beside them; the addresses are a LIST, which these naming
    // rules stand in for no value of.
    assert_eq!(
        published_attribute_names(DIRECTORY_SERVICE_MICROSOFT_AD),
        &["Alias"],
        "expected the alias to be the return value these naming rules compute, got {:?}",
        published_attribute_names(DIRECTORY_SERVICE_MICROSOFT_AD)
    );
    assert_eq!(
        published_attribute_names(DIRECTORY_SERVICE_SIMPLE_AD),
        &["Alias", "DirectoryId"],
        "expected the alias and the directory id, got {:?}",
        published_attribute_names(DIRECTORY_SERVICE_SIMPLE_AD)
    );
}
