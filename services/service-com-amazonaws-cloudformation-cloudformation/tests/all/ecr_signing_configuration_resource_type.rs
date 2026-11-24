// SPDX-License-Identifier: CC0-1.0

//! `AWS::ECR::SigningConfiguration` is a resource type a template may DECLARE: every operation
//! that submits a template admits a body declaring one, and the catalogue entries behind that
//! say what a resource of the type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes** —
//! the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation does
//! NOT know is refused `Template format error: Unrecognized resource types: [<types>]`, and
//! that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Amazon ECR's own
//! `PutSigningConfiguration`, the configuration that registry then answers, the refusal a
//! SECOND declaration gets while the registry already holds one, and the configuration's
//! removal with the stack — needs that registry mounted beside this service, so it lives in the
//! router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_ecr_signing_configurations.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: how many rules the declared
//! configuration carries (one, two, the ten its modelled `@length(min: 0, max: 10)` admits),
//! whether a rule carries repository filters at all and how many, the template spelling (JSON
//! and YAML), and whether the declaration stands beside a resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    ECR_SIGNING_CONFIGURATION,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text, ACCOUNT_ID};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The Amazon Web Services Signer profile the declared rules below sign with, in the form
/// `SigningRule$signingProfileArn`'s own pattern admits
/// (`^arn:aws(-[a-z]+)*:signer:[a-z0-9-]+:[0-9]{12}:\/signing-profiles\/[a-zA-Z0-9_]{2,}$`).
fn signing_profile(name: &str) -> String {
    format!("arn:aws:signer:us-east-1:{ACCOUNT_ID}:/signing-profiles/{name}")
}

/// One declared signing rule as a template writes it: the profile and, where the row states
/// them, the repository filters it applies to.
fn rule(profile: &str, filters: &[&str]) -> String {
    if filters.is_empty() {
        return format!(r#"{{"SigningProfileArn": "{profile}"}}"#);
    }
    let filters = filters
        .iter()
        .map(|filter| format!(r#"{{"Filter": "{filter}", "FilterType": "WILDCARD_MATCH"}}"#))
        .collect::<Vec<_>>()
        .join(", ");
    format!(r#"{{"SigningProfileArn": "{profile}", "RepositoryFilters": [{filters}]}}"#)
}

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    // The ten rules the modelled `SigningRuleList` admits at most, each on a profile of its
    // own, for the row that declares a configuration at that bound.
    let ten_rules = (1..=10)
        .map(|index| rule(&signing_profile(&format!("profile_{index}")), &["*"]))
        .collect::<Vec<_>>()
        .join(", ");
    vec![
        // The reported case: one rule matching one repository, with the return value read back
        // through `Outputs`.
        Row {
            case: "one rule with one repository filter, the return value read back",
            stack_name: "ecr-signing-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Signing": {{"Type": "{ECR_SIGNING_CONFIGURATION}",
                       "Properties": {{"Rules": [{}]}}}}}},
                     "Outputs": {{"SigningRef": {{"Value": {{"Ref": "Signing"}}}},
                       "RegistryId": {{"Value": {{"Fn::GetAtt": ["Signing", "RegistryId"]}}}}}}}}"#,
                rule(&signing_profile("release_profile"), &["app/*"]),
            ),
        },
        // A rule carrying NO repository filters, which the member's own optionality admits: the
        // rule then applies to the registry at large.
        Row {
            case: "a rule with no repository filters",
            stack_name: "ecr-signing-unfiltered",
            template: format!(
                r#"{{"Resources": {{"Signing": {{"Type": "{ECR_SIGNING_CONFIGURATION}",
                     "Properties": {{"Rules": [{}]}}}}}}}}"#,
                rule(&signing_profile("every_repository"), &[]),
            ),
        },
        // Two rules, one of them carrying two filters, in the YAML spelling with a short-form
        // intrinsic reading the published return value.
        Row {
            case: "two rules and two filters on one of them, YAML spelling",
            stack_name: "ecr-signing-yaml",
            template: format!(
                "\
Resources:
  Signing:
    Type: {ECR_SIGNING_CONFIGURATION}
    Properties:
      Rules:
        - SigningProfileArn: {first}
          RepositoryFilters:
            - Filter: team/*
              FilterType: WILDCARD_MATCH
            - Filter: shared/base
              FilterType: WILDCARD_MATCH
        - SigningProfileArn: {second}
Outputs:
  RegistryId:
    Value: !GetAtt Signing.RegistryId
",
                first = signing_profile("team_profile"),
                second = signing_profile("fallback_profile"),
            ),
        },
        // A configuration at the modelled bound of ten rules, beside a resource of another
        // type: the catalogue admits every type a body declares or none of them.
        Row {
            case: "ten rules beside a resource of another type",
            stack_name: "ecr-signing-ten-rules",
            template: format!(
                r#"{{"Resources": {{
                     "Signing": {{"Type": "{ECR_SIGNING_CONFIGURATION}",
                       "Properties": {{"Rules": [{ten_rules}]}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "ecr-signing-topic"}}}}}}}}"#
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
async fn every_operation_that_takes_a_template_admits_an_ecr_signing_configuration() {
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
fn the_catalogue_publishes_the_signing_configuration_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&ECR_SIGNING_CONFIGURATION),
        "expected {ECR_SIGNING_CONFIGURATION:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(ECR_SIGNING_CONFIGURATION) && is_recognized(ECR_SIGNING_CONFIGURATION),
        "expected {ECR_SIGNING_CONFIGURATION:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::ECR::SigningConfigurations",
        "AWS::ECR::SignatureConfiguration",
        "AWS::ECR::SigningProfile",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // A registry id is the owning account's id, which this emulator mints: the identity is a
    // value these naming rules compute rather than one Amazon ECR assigns.
    assert!(
        !identity_is_service_assigned(ECR_SIGNING_CONFIGURATION),
        "expected the registry to be the identity CloudFormation computes, it is reported as \
         one the owning service assigns"
    );
    assert_eq!(
        published_attribute_names(ECR_SIGNING_CONFIGURATION),
        &["RegistryId"],
        "expected the registry to be the type's one published return value, got {:?}",
        published_attribute_names(ECR_SIGNING_CONFIGURATION)
    );
    // `PutSigningConfiguration` is documented "Creates or updates the registry's signing
    // configuration", so the type's one property reaches the configuration in place and
    // nothing about it replaces the resource.
    assert!(
        replacement_properties(ECR_SIGNING_CONFIGURATION).is_empty(),
        "expected no create-only property for a configuration its own put updates, got {:?}",
        replacement_properties(ECR_SIGNING_CONFIGURATION)
    );
}
