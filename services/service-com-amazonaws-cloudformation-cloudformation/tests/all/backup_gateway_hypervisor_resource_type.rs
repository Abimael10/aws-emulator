// SPDX-License-Identifier: CC0-1.0

//! `AWS::BackupGateway::Hypervisor` is a resource type a template may DECLARE: every operation
//! that submits a template admits a body declaring one, and the catalogue entries behind that
//! say what a resource of the type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes**. It
//! is the mirror of `unrecognized_resource_types.rs`, which pins the other direction: a type
//! CloudFormation does NOT know is refused `Template format error: Unrecognized resource
//! types: [<types>]`, and that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Backup Gateway's own
//! `ImportHypervisorConfiguration`, the ARN that service mints, the hypervisor's removal with
//! the stack and the refusal it answers for a name outside the modelled pattern — needs that
//! service mounted beside this one, so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_backup_gateway_hypervisors.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is
//! attempted.
//!
//! The rows differ along what the rule generalises over: the declared properties (the two
//! members the import requires alone, those plus the credentials, those plus the encryption
//! key and the type's own `Tags`, those plus the log group only `UpdateHypervisor` carries),
//! the form of the host (an IPv4 address and a fully-qualified domain name, the two
//! `Host`'s own documentation names), which published return value the body reads back, the
//! template spelling (JSON and YAML), how many hypervisors one body declares, and whether the
//! declaration stands beside a resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    BACKUP_GATEWAY_HYPERVISOR,
};

use crate::harness::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// Every `Fn::GetAtt` attribute the type's "Return values" section publishes: the ARN Backup
/// Gateway minted, which is also what its `Ref` returns.
const PUBLISHED_ATTRIBUTES: &[&str] = &["HypervisorArn"];

/// A KMS key ARN of the form `KmsKeyArn`'s own pattern admits, for the rows that state one.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// A log-group ARN of the form `LogGroupArn`'s own pattern admits, for the row that states one.
const LOG_GROUP_ARN: &str =
    "arn:aws:logs:us-east-1:000000000000:log-group:/aws/backup-gateway/hypervisors:*";

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: the two members the import requires, with the published return
        // value read back through `Outputs`, on a host stated as an IPv4 address.
        Row {
            case: "the host and the name alone, with the return value read back",
            stack_name: "backup-gateway-hypervisor-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Hypervisor": {{"Type": "{BACKUP_GATEWAY_HYPERVISOR}",
                       "Properties": {{"Host": "10.0.0.11", "Name": "a-hypervisor"}}}}}},
                     "Outputs": {{"HypervisorRef": {{"Value": {{"Ref": "Hypervisor"}}}},
                       "HypervisorArn": {{"Value": {{"Fn::GetAtt": ["Hypervisor", "HypervisorArn"]}}}}}}}}"#
            ),
        },
        // The same type carrying the `@sensitive` credentials the import takes, on a host
        // stated as a fully-qualified domain name.
        Row {
            case: "a declaration carrying the hypervisor's credentials and an FQDN host",
            stack_name: "backup-gateway-hypervisor-credentials",
            template: format!(
                r#"{{"Resources": {{"Hypervisor": {{"Type": "{BACKUP_GATEWAY_HYPERVISOR}",
                     "Properties": {{"Host": "vcenter.example.internal",
                       "Name": "hypervisor-with-credentials",
                       "Username": "backup-operator", "Password": "a-pass-phrase"}}}}}}}}"#
            ),
        },
        // The YAML spelling, with the encryption key, the type's own `Tags` and a short-form
        // intrinsic reading the one published attribute.
        Row {
            case: "the YAML spelling, with the encryption key and the type's own Tags",
            stack_name: "backup-gateway-hypervisor-yaml",
            template: format!(
                "\
Resources:
  Hypervisor:
    Type: {BACKUP_GATEWAY_HYPERVISOR}
    Properties:
      Host: 192.168.40.5
      Name: hypervisor-yaml
      KmsKeyArn: {KMS_KEY_ARN}
      Tags:
        - Key: team
          Value: platform
Outputs:
  HypervisorArn:
    Value: !GetAtt Hypervisor.HypervisorArn
"
            ),
        },
        // The log group, which only `UpdateHypervisor` carries: a declaration stating it is
        // still one body the catalogue admits.
        Row {
            case: "a declaration carrying the log group only the update takes",
            stack_name: "backup-gateway-hypervisor-log-group",
            template: format!(
                r#"{{"Resources": {{"Hypervisor": {{"Type": "{BACKUP_GATEWAY_HYPERVISOR}",
                     "Properties": {{"Host": "10.0.0.12", "Name": "hypervisor-logged",
                       "LogGroupArn": "{LOG_GROUP_ARN}"}}}}}}}}"#
            ),
        },
        // Two hypervisors in one body, beside a resource of another type: the catalogue admits
        // every type a body declares or none of them.
        Row {
            case: "two hypervisors beside a resource of another type",
            stack_name: "backup-gateway-hypervisor-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "First": {{"Type": "{BACKUP_GATEWAY_HYPERVISOR}",
                       "Properties": {{"Host": "10.0.0.13", "Name": "first-hypervisor"}}}},
                     "Second": {{"Type": "{BACKUP_GATEWAY_HYPERVISOR}",
                       "Properties": {{"Host": "10.0.0.14", "Name": "second-hypervisor"}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "backup-gateway-hypervisor-topic"}}}}}}}}"#
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
async fn every_operation_that_takes_a_template_admits_a_backup_gateway_hypervisor() {
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
/// spelling of it still is not, and the type reports the identity and the return value its own
/// "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_hypervisor_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&BACKUP_GATEWAY_HYPERVISOR),
        "expected {BACKUP_GATEWAY_HYPERVISOR:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(BACKUP_GATEWAY_HYPERVISOR) && is_recognized(BACKUP_GATEWAY_HYPERVISOR),
        "expected {BACKUP_GATEWAY_HYPERVISOR:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::BackupGateway::Hypervisors",
        "AWS::BackupGateway::Gateway",
        "AWS::Backup::Hypervisor",
        "AWS::BackupGateway::VirtualMachine",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // Backup Gateway mints the ARN a hypervisor is addressed by — the import takes no
    // identifier at all — so the identity in force is the one that service answered rather than
    // a name computed from the template, and that ARN is the member a client names an existing
    // hypervisor by.
    assert!(
        identity_is_service_assigned(BACKUP_GATEWAY_HYPERVISOR),
        "expected the type's identity to be the one Backup Gateway assigns, it is not"
    );
    assert_eq!(
        primary_identifier::properties(BACKUP_GATEWAY_HYPERVISOR),
        vec!["HypervisorArn".to_string()],
        "expected the hypervisor's ARN to be the type's primary identifier, got {:?}",
        primary_identifier::properties(BACKUP_GATEWAY_HYPERVISOR)
    );
    assert_eq!(
        published_attribute_names(BACKUP_GATEWAY_HYPERVISOR),
        PUBLISHED_ATTRIBUTES,
        "expected the type's published return values, got {:?}",
        published_attribute_names(BACKUP_GATEWAY_HYPERVISOR)
    );
    // `KmsKeyArn` is a member of `ImportHypervisorConfiguration` and of no member of
    // `UpdateHypervisor`, so a declaration restating it asks for a different hypervisor; the
    // `Name` is NOT in that position, since that update takes the ARN together with a new name.
    assert_eq!(
        replacement_properties(BACKUP_GATEWAY_HYPERVISOR),
        &["KmsKeyArn"],
        "expected the encryption key to be the type's create-only property, got {:?}",
        replacement_properties(BACKUP_GATEWAY_HYPERVISOR)
    );
}
