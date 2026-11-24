// SPDX-License-Identifier: CC0-1.0

//! `AWS::Bedrock::Blueprint`, `AWS::Bedrock::DataAutomationProject` and
//! `AWS::Bedrock::DataAutomationLibrary` are resource types a template may DECLARE: every
//! operation that submits a template admits a body declaring one, and the catalogue entries
//! behind that say what a resource of each type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes** —
//! the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation does
//! NOT know is refused `Template format error: Unrecognized resource types: [<types>]`, and
//! that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with each declaration — the call to Amazon Bedrock Data
//! Automation's own `CreateBlueprint`, `CreateDataAutomationProject` and
//! `CreateDataAutomationLibrary`, the resource that service holds afterwards, its removal with
//! the stack and the rollback a declaration that service refuses earns — needs that service
//! mounted beside this one, so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_bedrock_data_automation.rs`). This suite
//! asserts admission alone, which is what a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: which of the three types the body
//! declares, how many resources one body declares, which optional properties the declaration
//! carries (a stage, a description, tags, a customer-managed key, a project's output
//! configurations), the form a blueprint's `Schema` is written in (text and a JSON document),
//! the template spelling (JSON and YAML), and whether the declaration stands beside a resource
//! of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, name_property_candidates, published_attribute_names,
    BEDROCK_BLUEPRINT, BEDROCK_DATA_AUTOMATION_LIBRARY, BEDROCK_DATA_AUTOMATION_PROJECT,
    BLUEPRINT_ARN_ATTRIBUTE, LIBRARY_ARN_ATTRIBUTE, PROJECT_ARN_ATTRIBUTE,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which no
/// row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// A blueprint schema, as the text `BlueprintSchema` declares (a string of 1..100000
/// characters).
const SCHEMA_TEXT: &str = r#"{\"class\": \"invoice\"}"#;

/// A customer-managed key a declaration may state, of the form `KmsKeyId`'s own pattern
/// requires (`^[A-Za-z0-9][A-Za-z0-9:_/+=,@.-]+$`).
const KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// One row: a template body declaring one or more of the three types, and the name of the stack
/// it is submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // A named blueprint whose schema is written as text, its ARN read back through `Ref`.
        Row {
            case: "a named blueprint with a schema written as text, Ref read back",
            stack_name: "bda-blueprint-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Blueprint": {{"Type": "{BEDROCK_BLUEPRINT}",
                       "Properties": {{"BlueprintName": "cfn-invoice-blueprint",
                         "Type": "DOCUMENT",
                         "Schema": "{SCHEMA_TEXT}"}}}}}},
                     "Outputs": {{"BlueprintArn": {{"Value": {{"Ref": "Blueprint"}}}}}}}}"#
            ),
        },
        // The schema written as a JSON DOCUMENT, with a customer-managed key, the `LIVE` stage
        // and the type's own tags, and the ARN read through `Fn::GetAtt`.
        Row {
            case: "a blueprint whose schema is a document, over a customer-managed key",
            stack_name: "bda-blueprint-document-schema",
            template: format!(
                r#"{{"Resources": {{"Blueprint": {{"Type": "{BEDROCK_BLUEPRINT}",
                     "Properties": {{"BlueprintName": "cfn-receipt-blueprint",
                       "Type": "IMAGE", "BlueprintStage": "LIVE",
                       "KmsKeyId": "{KEY_ARN}",
                       "KmsEncryptionContext": {{"tenant": "acme"}},
                       "Schema": {{"class": "receipt"}},
                       "Tags": [{{"Key": "team", "Value": "platform"}}]}}}}}},
                     "Outputs": {{"BlueprintArn": {{
                       "Value": {{"Fn::GetAtt": ["Blueprint", "{BLUEPRINT_ARN_ATTRIBUTE}"]}}}}}}}}"#
            ),
        },
        // A project with the output configurations its own API takes, a description and a
        // stage.
        Row {
            case: "a described project with its output configurations",
            stack_name: "bda-project-admitted",
            template: format!(
                r#"{{"Resources": {{"Project": {{"Type": "{BEDROCK_DATA_AUTOMATION_PROJECT}",
                     "Properties": {{"ProjectName": "cfn-invoice-project",
                       "ProjectDescription": "The project this stack deploys.",
                       "ProjectStage": "LIVE",
                       "StandardOutputConfiguration": {{"Document": {{
                         "Extraction": {{"Granularity": {{"Types": ["PAGE"]}},
                           "BoundingBox": {{"State": "ENABLED"}}}},
                         "GenerativeField": {{"State": "ENABLED"}}}}}}}}}}}},
                     "Outputs": {{"ProjectArn": {{
                       "Value": {{"Fn::GetAtt": ["Project", "{PROJECT_ARN_ATTRIBUTE}"]}}}}}}}}"#
            ),
        },
        // A library in the YAML spelling, with the short-form intrinsic and its own
        // description.
        Row {
            case: "a described library, YAML spelling",
            stack_name: "bda-library-admitted",
            template: format!(
                "\
Resources:
  Library:
    Type: {BEDROCK_DATA_AUTOMATION_LIBRARY}
    Properties:
      LibraryName: cfn-entity-library
      LibraryDescription: The library this stack deploys.
      Tags:
        - Key: team
          Value: platform
Outputs:
  LibraryArn:
    Value: !GetAtt Library.{LIBRARY_ARN_ATTRIBUTE}
"
            ),
        },
        // All three in one body, beside a resource of another type: the catalogue admits every
        // type a body declares or none of them.
        Row {
            case: "all three types beside a resource of another type",
            stack_name: "bda-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "Blueprint": {{"Type": "{BEDROCK_BLUEPRINT}",
                       "Properties": {{"BlueprintName": "cfn-mixed-blueprint",
                         "Type": "VIDEO", "Schema": "{SCHEMA_TEXT}"}}}},
                     "Library": {{"Type": "{BEDROCK_DATA_AUTOMATION_LIBRARY}",
                       "Properties": {{"LibraryName": "cfn-mixed-library"}}}},
                     "Project": {{"Type": "{BEDROCK_DATA_AUTOMATION_PROJECT}",
                       "Properties": {{"ProjectName": "cfn-mixed-project",
                         "StandardOutputConfiguration": {{"Audio": {{
                           "GenerativeField": {{"State": "DISABLED"}}}}}},
                         "CustomOutputConfiguration": {{"Blueprints": [{{
                           "BlueprintArn": {{"Fn::GetAtt": ["Blueprint", "{BLUEPRINT_ARN_ATTRIBUTE}"]}}}}]}}}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "bda-mixed-topic"}}}}}}}}"#
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

/// Every operation that submits a template for deployment admits a body declaring the types,
/// and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_the_data_automation_types() {
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

/// The catalogue entries the rule rests on: each type is published and recognised, a near-miss
/// spelling of one still is not, and each type reports the identity and the return value its own
/// "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_data_automation_types_and_what_they_return() {
    // (the type, the property its name is declared in, the attribute its ARN is published as)
    let declared: [(&str, &str, &str); 3] = [
        (BEDROCK_BLUEPRINT, "BlueprintName", BLUEPRINT_ARN_ATTRIBUTE),
        (
            BEDROCK_DATA_AUTOMATION_PROJECT,
            "ProjectName",
            PROJECT_ARN_ATTRIBUTE,
        ),
        (
            BEDROCK_DATA_AUTOMATION_LIBRARY,
            "LibraryName",
            LIBRARY_ARN_ATTRIBUTE,
        ),
    ];
    for (resource_type, name_property, arn_attribute) in declared {
        assert!(
            AWS_RESOURCE_TYPES.contains(&resource_type),
            "expected {resource_type:?} in the published catalogue, it is absent"
        );
        assert!(
            is_published(resource_type) && is_recognized(resource_type),
            "expected {resource_type:?} to be a type a template may declare, it is not"
        );
        // "Ref returns the ARN": Amazon Bedrock Data Automation mints the identifier inside it,
        // so the identity in force is the one that service assigned rather than a name these
        // rules could compute.
        assert!(
            identity_is_service_assigned(resource_type),
            "expected {resource_type:?} to carry its owning service's identity, it does not"
        );
        // The NAME a declaration states is still CloudFormation's, and it is the first property
        // the naming rules read for the type.
        assert_eq!(
            name_property_candidates(resource_type).first().map(String::as_str),
            Some(name_property),
            "expected {resource_type:?} to take its name from {name_property:?}, got {:?}",
            name_property_candidates(resource_type)
        );
        assert_eq!(
            published_attribute_names(resource_type),
            [arn_attribute],
            "expected {resource_type:?} to publish {arn_attribute:?}, got {:?}",
            published_attribute_names(resource_type)
        );
    }
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::Bedrock::Blueprints",
        "AWS::Bedrock::DataAutomation",
        "AWS::Bedrock::DataAutomationProjects",
        "AWS::Bedrock::Library",
        "AWS::BedrockDataAutomation::Blueprint",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
}
