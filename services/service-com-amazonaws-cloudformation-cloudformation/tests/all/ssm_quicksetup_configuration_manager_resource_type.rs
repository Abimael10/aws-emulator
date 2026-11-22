// SPDX-License-Identifier: CC0-1.0

//! `AWS::SSMQuickSetup::ConfigurationManager` is a resource type a template may DECLARE, and
//! its own property rules are what a declaration of it is then held to — end to end over the
//! awsQuery wire.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the
//! declaration is judged by that type's own published contract instead** — a value outside the
//! pattern the type publishes for a property fails the pre-deployment stage, which names the
//! property's own path, and the stack rolls back. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation does
//! NOT know is refused `Template format error: Unrecognized resource types: [<types>]`, and
//! that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with an admitted declaration — the call to Quick Setup's own
//! `CreateConfigurationManager`, the ARN that service mints, the manager's removal with the
//! stack — needs that service mounted beside this one, so it lives in the router's own
//! cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_ssm_quicksetup_configuration_managers.rs`).
//! This suite asserts admission and the stage that runs before any provider is invoked, which
//! is what a client sees before any resource is attempted.
//!
//! The admission rows differ along what the rule generalises over: the declared properties (the
//! definitions alone, those plus the name and the description, those plus the type's own
//! `Tags`), which configuration type a definition deploys, the template spelling (JSON and
//! YAML), how many managers one body declares, and whether the declaration stands beside a
//! resource of another type.
//!
//! The name rows differ along the spelling of the one property whose pattern the type
//! publishes: values over its alphabet (letters, digits, the space and the two punctuation
//! characters), the shortest and the longest value it admits, one character past either bound,
//! and characters outside the alphabet — the dot among them, which the owning API's own member
//! admits and this type's schema does not.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::patterned_properties::{
    pattern_failure_reason, PATTERNED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::{
    PROPERTY_VALIDATION, REQUIRED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    SSM_QUICKSETUP_CONFIGURATION_MANAGER,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The logical id every body below declares its manager under.
const MANAGER: &str = "ConfigManager";

/// The `EventType` a validation result is reported under, from the model's own `EventType`
/// enum (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass, and
/// `OperationEvent$ValidationFailureMode` for one whose failure was handled by FAILING the
/// operation.
const FAILED: &str = "FAILED";
const FAIL: &str = "FAIL";

/// The pattern the type publishes for its `Name`, read from the catalogue the stage judges
/// against rather than restated here, so the suite and the rule cannot drift apart.
fn name_pattern() -> &'static str {
    PATTERNED_PROPERTIES
        .iter()
        .find(|row| {
            row.resource_type == SSM_QUICKSETUP_CONFIGURATION_MANAGER && row.property == "Name"
        })
        .map(|row| row.pattern)
        .expect("the type's Name pattern is catalogued")
}

/// The JSON pointer of the judged property in the submitted template.
fn name_path() -> String {
    format!("/Resources/{MANAGER}/Properties/Name")
}

/// One configuration definition, deploying `type_` with the parameters that name where it
/// goes. Source: `ConfigurationDefinitionInput`, whose members a declaration spells with the
/// same names (`smithy_query(query="shape", shape="ConfigurationDefinitionInput")`).
fn definition(type_: &str) -> String {
    format!(
        r#"{{"Type": "{type_}",
            "Parameters": {{"TargetOrganizationalUnits": "ou-1234-abcdefgh",
                            "TargetRegions": "{REGION}"}}}}"#
    )
}

/// One admission row: a template body declaring the type, and the name of the stack it is
/// submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: a manager with a name, a description and one host-management
        // definition, with both return values read back through `Outputs`.
        Row {
            case: "a named manager with one definition, with its return values read back",
            stack_name: "quicksetup-manager-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"{MANAGER}": {{"Type": "{SSM_QUICKSETUP_CONFIGURATION_MANAGER}",
                       "Properties": {{"Name": "host-mgmt", "Description": "managed hosts",
                         "ConfigurationDefinitions": [{}]}}}}}},
                     "Outputs": {{"ManagerArnRef": {{"Value": {{"Ref": "{MANAGER}"}}}},
                       "ManagerArn": {{"Value": {{"Fn::GetAtt": ["{MANAGER}", "ManagerArn"]}}}}}}}}"#,
                definition("AWSQuickSetupType-SSMHostMgmt")
            ),
        },
        // The same type carrying the definitions ALONE, which is every property the type
        // requires, and deploying another of the configuration types Quick Setup offers.
        Row {
            case: "the required definitions alone, deploying a patch policy",
            stack_name: "quicksetup-manager-minimal",
            template: format!(
                r#"{{"Resources": {{"{MANAGER}": {{"Type": "{SSM_QUICKSETUP_CONFIGURATION_MANAGER}",
                     "Properties": {{"ConfigurationDefinitions": [{}]}}}}}}}}"#,
                definition("AWSQuickSetupType-PatchPolicy")
            ),
        },
        // The YAML spelling, with the type's own `Tags` — a MAP rather than the list most
        // types publish — and a short-form intrinsic.
        Row {
            case: "the YAML spelling, with the type's own Tags",
            stack_name: "quicksetup-manager-yaml",
            template: format!(
                "\
Resources:
  {MANAGER}:
    Type: {SSM_QUICKSETUP_CONFIGURATION_MANAGER}
    Properties:
      Name: yaml spelled manager
      ConfigurationDefinitions:
        - Type: AWSQuickSetupType-SSM
          Parameters:
            TargetOrganizationalUnits: ou-1234-abcdefgh
            TargetRegions: {REGION}
      Tags:
        team: platform
Outputs:
  ManagerArn:
    Value: !GetAtt {MANAGER}.ManagerArn
"
            ),
        },
        // Two managers in one body, beside a resource of another type: the catalogue admits
        // every type a body declares or none of them.
        Row {
            case: "two managers beside a resource of another type",
            stack_name: "quicksetup-manager-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "{MANAGER}": {{"Type": "{SSM_QUICKSETUP_CONFIGURATION_MANAGER}",
                       "Properties": {{"Name": "first", "ConfigurationDefinitions": [{first}]}}}},
                     "SecondManager": {{"Type": "{SSM_QUICKSETUP_CONFIGURATION_MANAGER}",
                       "Properties": {{"Name": "second", "ConfigurationDefinitions": [{second}]}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "quicksetup-manager-topic"}}}}}}}}"#,
                first = definition("AWSQuickSetupType-SSMHostMgmt"),
                second = definition("AWSQuickSetupType-Distributor"),
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

/// Every operation that submits a template for deployment admits a body declaring the type,
/// and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_a_configuration_manager() {
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

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it still is not, and the type reports the identity, the return value and the
/// create-only property its own contract publishes.
#[test]
fn the_catalogue_publishes_the_configuration_manager_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&SSM_QUICKSETUP_CONFIGURATION_MANAGER),
        "expected {SSM_QUICKSETUP_CONFIGURATION_MANAGER:?} in the published catalogue, it is \
         absent"
    );
    assert!(
        is_published(SSM_QUICKSETUP_CONFIGURATION_MANAGER)
            && is_recognized(SSM_QUICKSETUP_CONFIGURATION_MANAGER),
        "expected {SSM_QUICKSETUP_CONFIGURATION_MANAGER:?} to be a type a template may \
         declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::SSMQuickSetup::ConfigurationManagers",
        "AWS::SSMQuickSetup::Configuration",
        "AWS::SSM::ConfigurationManager",
        "AWS::QuickSetup::ConfigurationManager",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // Quick Setup mints the ARN a manager is addressed by, so the identifier in force is the
    // one that service answered rather than a name computed from the template.
    assert!(
        identity_is_service_assigned(SSM_QUICKSETUP_CONFIGURATION_MANAGER),
        "expected the type's identity to be the one Quick Setup assigns, it is not"
    );
    assert_eq!(
        published_attribute_names(SSM_QUICKSETUP_CONFIGURATION_MANAGER),
        &["ManagerArn"],
        "expected the ARN to be the return value these naming rules compute, got {:?}",
        published_attribute_names(SSM_QUICKSETUP_CONFIGURATION_MANAGER)
    );
    // `UpdateConfigurationManager` takes the manager's name and description and no definition
    // at all, so a declaration restating the definitions asks for a different manager.
    assert_eq!(
        replacement_properties(SSM_QUICKSETUP_CONFIGURATION_MANAGER),
        &["ConfigurationDefinitions"],
        "expected the definitions to be the type's create-only property, got {:?}",
        replacement_properties(SSM_QUICKSETUP_CONFIGURATION_MANAGER)
    );
    // ...and a manager IS the deployment of its definitions, so a declaration carrying none is
    // one the stage below refuses.
    let required = REQUIRED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == SSM_QUICKSETUP_CONFIGURATION_MANAGER)
        .map(|row| row.required)
        .unwrap_or_default();
    assert_eq!(
        required,
        &["ConfigurationDefinitions"],
        "expected the definitions to be the type's required property, got {required:?}"
    );
}

/// One name row: the value the declaration states for the manager's `Name`, and whether the
/// pattern the type publishes admits it.
struct NameRow {
    case: &'static str,
    stack_name: &'static str,
    name: String,
    admitted: bool,
}

/// The longest `Name` the type's published pattern admits, and one character past that bound.
/// Source: the `{1,50}` bound of the pattern [`name_pattern`] carries.
const NAME_MAX_CHARS: usize = 50;

fn name_rows() -> Vec<NameRow> {
    vec![
        NameRow {
            case: "letters, digits and the two punctuation characters the alphabet admits",
            stack_name: "quicksetup-name-punctuation",
            name: "host-mgmt_2026".to_string(),
            admitted: true,
        },
        NameRow {
            case: "a name carrying the space the alphabet admits",
            stack_name: "quicksetup-name-spaces",
            name: "host management fleet".to_string(),
            admitted: true,
        },
        NameRow {
            case: "the shortest name the pattern admits",
            stack_name: "quicksetup-name-shortest",
            name: "a".to_string(),
            admitted: true,
        },
        NameRow {
            case: "the longest name the pattern admits",
            stack_name: "quicksetup-name-longest",
            name: "n".repeat(NAME_MAX_CHARS),
            admitted: true,
        },
        NameRow {
            case: "one character past the pattern's upper bound",
            stack_name: "quicksetup-name-too-long",
            name: "n".repeat(NAME_MAX_CHARS + 1),
            admitted: false,
        },
        NameRow {
            case: "one character short of the pattern's lower bound",
            stack_name: "quicksetup-name-empty",
            name: String::new(),
            admitted: false,
        },
        NameRow {
            case: "a character outside the alphabet",
            stack_name: "quicksetup-name-punctuated",
            name: "bad!name".to_string(),
            admitted: false,
        },
        NameRow {
            // The owning API's own `Name` admits the dot; the form this TYPE publishes for the
            // property does not, and what a declaration is held to is the type's.
            case: "a dot, which the owning API admits and this type's schema does not",
            stack_name: "quicksetup-name-dotted",
            name: "fleet.operations".to_string(),
            admitted: false,
        },
        NameRow {
            case: "a path separator, which the alphabet does not carry",
            stack_name: "quicksetup-name-slash",
            name: "hosts/prod".to_string(),
            admitted: false,
        },
    ]
}

/// The validation RESULTS the operation's own events report for `stack`: one tuple per event
/// carrying a validation, in the order they are reported.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// A value outside the pattern the type publishes for its `Name` fails the stage that runs
/// before any provider is invoked — naming the resource, the value, the pattern and the
/// property's own path — while a value the pattern admits passes it, whatever the stack then
/// does about the service that owns the resource.
#[tokio::test]
async fn a_name_outside_the_types_pattern_fails_property_validation() {
    for row in name_rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        let template = format!(
            r#"{{"Resources": {{"{MANAGER}": {{"Type": "{SSM_QUICKSETUP_CONFIGURATION_MANAGER}",
                 "Properties": {{"Name": {name},
                   "ConfigurationDefinitions": [{definition}]}}}}}}}}"#,
            name = ::serde_json::Value::String(row.name.clone()),
            definition = definition("AWSQuickSetupType-SSMHostMgmt"),
        );

        let (code, created) = create_stack(&router, row.stack_name, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the body declares a type AWS \
             publishes — got {code} {created}"
        );

        let settled = settled_stack_status(case, &router, REGION, row.stack_name).await;
        let results = validation_results(&router, case, row.stack_name).await;
        if row.admitted {
            assert!(
                results.is_empty(),
                "case {case:?}: expected a name the pattern admits to pass the stage, got \
                 {results:?}"
            );
            continue;
        }
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected a name outside the pattern to roll the stack back, got \
             {settled:?}"
        );
        assert_eq!(
            results,
            vec![(
                MANAGER.to_string(),
                PROPERTY_VALIDATION.to_string(),
                FAILED.to_string(),
                FAIL.to_string(),
                pattern_failure_reason(&row.name, name_pattern()),
                name_path(),
            )],
            "case {case:?}: expected the one validation result the refused name owes, got \
             {results:?}"
        );
    }
}
