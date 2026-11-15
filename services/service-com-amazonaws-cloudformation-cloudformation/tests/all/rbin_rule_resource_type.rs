// SPDX-License-Identifier: CC0-1.0

//! `AWS::Rbin::Rule` is a resource type a template may DECLARE: every operation that submits a
//! template admits a body declaring one, and the catalogue entries behind that say what a
//! resource of the type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes**. It
//! is the mirror of `unrecognized_resource_types.rs`, which pins the other direction: a type
//! CloudFormation does NOT know is refused `Template format error: Unrecognized resource types:
//! [<types>]`, and that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Recycle Bin's own `CreateRule`,
//! the identity that service mints, the rule's removal with the stack and the refusal it
//! answers for a period a resource type does not support — needs that service mounted beside
//! this one, so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_rbin_rules.rs`). This suite asserts
//! admission alone, which is what a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: the declared properties (the two
//! `@required` members of the create alone, those plus a resource-tag selection and the type's
//! own `Tags`, those plus a description, and a Region-level declaration carrying exclusion tags
//! and a lock configuration), which resource type the rule retains, which published return
//! value the body reads back, the template spelling (JSON and YAML), how many rules one body
//! declares, and whether the declaration stands beside a resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties, RBIN_RULE,
};

use crate::harness::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which no
/// row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// Every `Fn::GetAtt` attribute the type's "Return values" section publishes, in the sorted
/// order a resource's own attribute map holds them in: the rule's ARN, the identifier Recycle
/// Bin minted for it and the lock state it keeps.
const PUBLISHED_ATTRIBUTES: &[&str] = &["Arn", "Identifier", "LockState"];

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: a tag-level rule with the two `@required` members of the create,
        // the type's own `Tags`, and every published return value read back through `Outputs`.
        Row {
            case: "a tag-level snapshot rule with every return value read back",
            stack_name: "rbin-rule-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Rule": {{"Type": "{RBIN_RULE}",
                       "Properties": {{"ResourceType": "EBS_SNAPSHOT",
                         "RetentionPeriod": {{"RetentionPeriodValue": 1,
                           "RetentionPeriodUnit": "DAYS"}},
                         "ResourceTags": [{{"ResourceTagKey": "retain",
                           "ResourceTagValue": "yes"}}],
                         "Tags": [{{"Key": "sim2real-rbin", "Value": "admitted"}}]}}}}}},
                     "Outputs": {{"RuleRef": {{"Value": {{"Ref": "Rule"}}}},
                       "RuleArn": {{"Value": {{"Fn::GetAtt": ["Rule", "Arn"]}}}},
                       "RuleIdentifier": {{"Value": {{"Fn::GetAtt": ["Rule", "Identifier"]}}}},
                       "RuleLockState": {{"Value": {{"Fn::GetAtt": ["Rule", "LockState"]}}}}}}}}"#
            ),
        },
        // The required members alone, on a second resource type, with nothing read back.
        Row {
            case: "the required members alone, retaining EBS volumes",
            stack_name: "rbin-rule-required-only",
            template: format!(
                r#"{{"Resources": {{"Rule": {{"Type": "{RBIN_RULE}",
                     "Properties": {{"ResourceType": "EBS_VOLUME",
                       "RetentionPeriod": {{"RetentionPeriodValue": 7,
                         "RetentionPeriodUnit": "DAYS"}}}}}}}}}}"#
            ),
        },
        // The YAML spelling, on the third resource type, with a description and a short-form
        // intrinsic reading the ARN.
        Row {
            case: "the YAML spelling, retaining EBS-backed AMIs, with a description",
            stack_name: "rbin-rule-yaml",
            template: format!(
                "\
Resources:
  Rule:
    Type: {RBIN_RULE}
    Properties:
      ResourceType: EC2_IMAGE
      RetentionPeriod:
        RetentionPeriodValue: 365
        RetentionPeriodUnit: DAYS
      Description: the images this Region keeps
Outputs:
  RuleArn:
    Value: !GetAtt Rule.Arn
  RuleLockState:
    Value: !GetAtt Rule.LockState
"
            ),
        },
        // Two rules in one body, beside a resource of another type: one Region-level rule
        // carrying exclusion tags and a lock configuration, one tag-level rule. The catalogue
        // admits every type a body declares or none of them.
        Row {
            case: "two rules beside a resource of another type",
            stack_name: "rbin-rule-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "Locked": {{"Type": "{RBIN_RULE}",
                       "Properties": {{"ResourceType": "EBS_SNAPSHOT",
                         "RetentionPeriod": {{"RetentionPeriodValue": 90,
                           "RetentionPeriodUnit": "DAYS"}},
                         "ExcludeResourceTags": [{{"ResourceTagKey": "skip",
                           "ResourceTagValue": "yes"}}],
                         "LockConfiguration": {{"UnlockDelay": {{"UnlockDelayValue": 7,
                           "UnlockDelayUnit": "DAYS"}}}}}}}},
                     "Tagged": {{"Type": "{RBIN_RULE}",
                       "Properties": {{"ResourceType": "EC2_IMAGE",
                         "RetentionPeriod": {{"RetentionPeriodValue": 30,
                           "RetentionPeriodUnit": "DAYS"}},
                         "ResourceTags": [{{"ResourceTagKey": "retain",
                           "ResourceTagValue": "images"}}]}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "rbin-rule-topic"}}}}}}}}"#
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

/// Every operation that submits a template for deployment admits a body declaring the type, and
/// so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_an_rbin_rule() {
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
/// spelling of it still is not, and the type reports the identity and the return values its own
/// "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_rbin_rule_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&RBIN_RULE),
        "expected {RBIN_RULE:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(RBIN_RULE) && is_recognized(RBIN_RULE),
        "expected {RBIN_RULE:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::Rbin::Rules",
        "AWS::Rbin::RetentionRule",
        "AWS::RBin::Rule",
        "AWS::RecycleBin::Rule",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // Recycle Bin mints the identifier a rule is addressed by and the ARN built around it — the
    // create takes neither — so the identity in force is the one that service answered rather
    // than a name computed from the template, and the ARN is the member a client names an
    // existing rule by.
    assert!(
        identity_is_service_assigned(RBIN_RULE),
        "expected the type's identity to be the one Recycle Bin assigns, it is not"
    );
    assert_eq!(
        primary_identifier::properties(RBIN_RULE),
        vec!["Arn".to_string()],
        "expected the rule's ARN to be the type's primary identifier, got {:?}",
        primary_identifier::properties(RBIN_RULE)
    );
    assert_eq!(
        published_attribute_names(RBIN_RULE),
        PUBLISHED_ATTRIBUTES,
        "expected the type's published return values, got {:?}",
        published_attribute_names(RBIN_RULE)
    );
    // `UpdateRule`'s `ResourceType` member is documented "currently not supported. You can't
    // update a retention rule's resource type after creation", so a declaration naming another
    // type asks for a different rule.
    assert_eq!(
        replacement_properties(RBIN_RULE),
        &["ResourceType"],
        "expected the retained resource type to be the type's create-only property, got {:?}",
        replacement_properties(RBIN_RULE)
    );
}
