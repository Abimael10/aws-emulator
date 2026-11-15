// SPDX-License-Identifier: CC0-1.0

//! `AWS::CustomerProfiles::Domain` is a resource type a template may DECLARE: every operation
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
//! What the stack then DOES with the declaration — the call to that service's own
//! `CreateDomain`, the values it reports for the domain, the data plane the domain serves, the
//! change an update applies and its removal with the stack — is
//! `customerprofiles_domain_provisioning.rs`. This suite asserts admission alone, which is what
//! a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: the declared properties (the required
//! two alone, those plus the encryption key and the dead-letter queue, those plus a matching
//! configuration and the type's own `Tags`), whether the declaration names the domain at all,
//! the expiration window's position in its published range, the template spelling (JSON and
//! YAML), how many domains one body declares, and whether the declaration stands beside a
//! resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, name_property_candidates, published_attribute_names,
    replacement_properties, CUSTOMER_PROFILES_DOMAIN,
};

use crate::harness::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// Every `Fn::GetAtt` attribute the type's "Return values" section publishes, in the sorted
/// order a resource's own attribute map holds them in: the two instants the owning service
/// recorded, the status of its rule-based matching configuration, and the four counts its
/// `DomainStats` keeps (`smithy_query(query="shape", shape="DomainStats")` on that service's
/// own model).
const PUBLISHED_ATTRIBUTES: &[&str] = &[
    "CreatedAt",
    "LastUpdatedAt",
    "RuleBasedMatching.Status",
    "Stats.MeteringProfileCount",
    "Stats.ObjectCount",
    "Stats.ProfileCount",
    "Stats.TotalSize",
];

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: the two `@required` members of the create, with every published
        // return value read back through `Outputs`.
        Row {
            case: "the required members alone, with every return value read back",
            stack_name: "customer-profiles-domain-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Profiles": {{"Type": "{CUSTOMER_PROFILES_DOMAIN}",
                       "Properties": {{"DomainName": "connect-profiles-admitted",
                         "DefaultExpirationDays": 90}}}}}},
                     "Outputs": {{"DomainRef": {{"Value": {{"Ref": "Profiles"}}}},
                       "CreatedAt": {{"Value": {{"Fn::GetAtt": ["Profiles", "CreatedAt"]}}}},
                       "LastUpdatedAt": {{"Value": {{"Fn::GetAtt": ["Profiles", "LastUpdatedAt"]}}}},
                       "ProfileCount": {{"Value": {{"Fn::GetAtt": ["Profiles", "Stats.ProfileCount"]}}}},
                       "ObjectCount": {{"Value": {{"Fn::GetAtt": ["Profiles", "Stats.ObjectCount"]}}}},
                       "TotalSize": {{"Value": {{"Fn::GetAtt": ["Profiles", "Stats.TotalSize"]}}}},
                       "MeteringProfileCount": {{"Value": {{"Fn::GetAtt": ["Profiles", "Stats.MeteringProfileCount"]}}}}}}}}"#
            ),
        },
        // The encryption key and the dead-letter queue beside them, at the LOWEST expiration
        // the published range admits (`expirationDaysInteger`, range 1..1098), and with NO
        // declared name at all — so the domain carries the one CloudFormation generates.
        Row {
            case: "the encryption key and dead-letter queue, the minimum window, no declared name",
            stack_name: "customer-profiles-domain-unnamed",
            template: format!(
                r#"{{"Resources": {{"Profiles": {{"Type": "{CUSTOMER_PROFILES_DOMAIN}",
                     "Properties": {{"DefaultExpirationDays": 1,
                       "DefaultEncryptionKey": "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555",
                       "DeadLetterQueueUrl": "https://sqs.us-east-1.amazonaws.com/000000000000/profiles-dlq"}}}}}}}}"#
            ),
        },
        // The YAML spelling, at the HIGHEST expiration the published range admits, with a
        // matching configuration, the type's own `Tags` and a short-form intrinsic reading one
        // published count.
        Row {
            case: "the YAML spelling, the maximum window, a matching configuration and Tags",
            stack_name: "customer-profiles-domain-yaml",
            template: format!(
                "\
Resources:
  Profiles:
    Type: {CUSTOMER_PROFILES_DOMAIN}
    Properties:
      DomainName: connect-profiles-yaml
      DefaultExpirationDays: 1098
      Matching:
        Enabled: true
      Tags:
        - Key: team
          Value: contact-center
Outputs:
  ProfileCount:
    Value: !GetAtt Profiles.Stats.ProfileCount
"
            ),
        },
        // Two domains in one body, beside a resource of another type: the catalogue admits
        // every type a body declares or none of them.
        Row {
            case: "two domains beside a resource of another type",
            stack_name: "customer-profiles-domain-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "First": {{"Type": "{CUSTOMER_PROFILES_DOMAIN}",
                       "Properties": {{"DomainName": "connect-profiles-mixed-first",
                         "DefaultExpirationDays": 30}}}},
                     "Second": {{"Type": "{CUSTOMER_PROFILES_DOMAIN}",
                       "Properties": {{"DomainName": "connect-profiles-mixed-second",
                         "DefaultExpirationDays": 365,
                         "RuleBasedMatching": {{"Enabled": false}}}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "customer-profiles-domain-topic"}}}}}}}}"#
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
async fn every_operation_that_takes_a_template_admits_a_customer_profiles_domain() {
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
/// spelling of it still is not, and the type reports the identity and the return values its
/// own "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_customer_profiles_domain_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&CUSTOMER_PROFILES_DOMAIN),
        "expected {CUSTOMER_PROFILES_DOMAIN:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(CUSTOMER_PROFILES_DOMAIN) && is_recognized(CUSTOMER_PROFILES_DOMAIN),
        "expected {CUSTOMER_PROFILES_DOMAIN:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::CustomerProfiles::Domains",
        "AWS::CustomerProfiles::Profile",
        "AWS::Customerprofiles::Domain",
        "AWS::CustomerProfile::Domain",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // "Ref returns the DomainName", which is the name the template states in the type's own
    // `DomainName` property — so the identity is not one the owning service assigns, and the
    // property the name is read from is a candidate of the type.
    assert!(
        !identity_is_service_assigned(CUSTOMER_PROFILES_DOMAIN),
        "expected the type's identity to be the declared name, it is service-assigned"
    );
    assert!(
        name_property_candidates(CUSTOMER_PROFILES_DOMAIN).contains(&"DomainName".to_string()),
        "expected the type to take its name from its own DomainName property, got {:?}",
        name_property_candidates(CUSTOMER_PROFILES_DOMAIN)
    );
    assert_eq!(
        published_attribute_names(CUSTOMER_PROFILES_DOMAIN),
        PUBLISHED_ATTRIBUTES,
        "expected the type's published return values, got {:?}",
        published_attribute_names(CUSTOMER_PROFILES_DOMAIN)
    );
    // `UpdateDomain` takes every property of the type but the domain's NAME, which is the
    // handle it is addressed by — and a declaration naming another domain is another resource
    // by the naming rules alone — so the type has no create-only property beside its name.
    assert_eq!(
        replacement_properties(CUSTOMER_PROFILES_DOMAIN),
        &[] as &[&str],
        "expected the type to have no create-only property beside its name, got {:?}",
        replacement_properties(CUSTOMER_PROFILES_DOMAIN)
    );
}
