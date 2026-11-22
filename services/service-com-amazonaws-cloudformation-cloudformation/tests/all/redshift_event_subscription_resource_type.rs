// SPDX-License-Identifier: CC0-1.0

//! `AWS::Redshift::EventSubscription` is a resource type a template may DECLARE: every
//! operation that submits a template admits a body declaring one, and the catalogue entries
//! behind that say what a resource of the type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! is identified by the property it takes its name from** — the half of the rule this crate
//! owns on its own. It is the mirror of `unrecognized_resource_types.rs`, which pins the other
//! direction: a type CloudFormation does NOT know is refused `Template format error:
//! Unrecognized resource types: [<types>]`, and that refusal must never be the answer for a
//! type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Amazon Redshift's own
//! `CreateEventSubscription`, the subscription that service holds afterwards, the change a
//! re-deploy applies to it and its removal with the stack — needs those services mounted
//! beside this one, so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_redshift_event_subscriptions.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: whether the declaration names the
//! subscription itself or leaves the name to CloudFormation, how it states the topic (a literal
//! ARN, a `Ref` to a topic of the same stack), which of the optional scoping properties it
//! carries (none, a source type with source ids, categories, a severity, the enabled flag), the
//! template spelling (JSON and YAML), how many subscriptions one body declares, and whether the
//! declaration stands beside a resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    name_property_candidates, read_only_properties,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The type under test, spelled as the "AWS resource and property types reference" spells it.
const SUBSCRIPTION: &str = "AWS::Redshift::EventSubscription";

/// The property the type takes its name from, which is the member
/// `CreateEventSubscription` creates a subscription under
/// (`smithy_query(query="operation", operation="CreateEventSubscription")`).
const NAME_PROPERTY: &str = "SubscriptionName";

/// An Amazon SNS topic ARN a declaration may state literally, in the form Amazon SNS mints.
const TOPIC_ARN: &str = "arn:aws:sns:us-east-1:000000000000:redshift-events";

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own body: a named subscription whose topic is a `Ref` to the
        // `AWS::SNS::Topic` beside it, with the name read back through `Ref`.
        Row {
            case: "a named subscription over a topic of the same stack, with Ref read back",
            stack_name: "redshift-subscription-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{
                       "Topic": {{"Type": "AWS::SNS::Topic",
                         "Properties": {{"TopicName": "redshift-events-admitted"}}}},
                       "Subscription": {{"Type": "{SUBSCRIPTION}",
                         "Properties": {{"{NAME_PROPERTY}": "cfn-subscription-admitted",
                           "SnsTopicArn": {{"Ref": "Topic"}},
                           "SourceType": "cluster",
                           "Severity": "ERROR",
                           "Enabled": true}}}}}},
                     "Outputs": {{
                       "SubscriptionName": {{"Value": {{"Ref": "Subscription"}}}}}}}}"#
            ),
        },
        // The topic stated as a literal ARN and the name left to CloudFormation: the type
        // names itself nowhere, so the name is one CloudFormation generates.
        Row {
            case: "an unnamed subscription over a literal topic ARN",
            stack_name: "redshift-subscription-generated-name",
            template: format!(
                r#"{{"Resources": {{"Subscription": {{"Type": "{SUBSCRIPTION}",
                     "Properties": {{"SnsTopicArn": "{TOPIC_ARN}"}}}}}},
                     "Outputs": {{
                       "SubscriptionName": {{"Value": {{"Ref": "Subscription"}}}}}}}}"#
            ),
        },
        // A declaration scoped to named SOURCES and CATEGORIES, which are the type's two LIST
        // properties.
        Row {
            case: "a subscription scoped to source ids and event categories",
            stack_name: "redshift-subscription-scoped",
            template: format!(
                r#"{{"Resources": {{"Subscription": {{"Type": "{SUBSCRIPTION}",
                     "Properties": {{"{NAME_PROPERTY}": "cfn-subscription-scoped",
                       "SnsTopicArn": "{TOPIC_ARN}",
                       "SourceType": "cluster-parameter-group",
                       "SourceIds": ["a-group", "another-group"],
                       "EventCategories": ["configuration", "management"]}}}}}}}}"#
            ),
        },
        // The YAML spelling, the subscription created INACTIVE, and the type's own `Tags`.
        Row {
            case: "the YAML spelling, an inactive subscription and declared tags",
            stack_name: "redshift-subscription-yaml",
            template: format!(
                "\
Resources:
  Subscription:
    Type: {SUBSCRIPTION}
    Properties:
      {NAME_PROPERTY}: cfn-subscription-yaml
      SnsTopicArn: {TOPIC_ARN}
      Enabled: false
      Tags:
        - Key: team
          Value: analytics
Outputs:
  SubscriptionName:
    Value: !Ref Subscription
"
            ),
        },
        // Two subscriptions in one body, beside a resource of another type: the catalogue
        // admits every type a body declares or none of them.
        Row {
            case: "two subscriptions beside a resource of another type",
            stack_name: "redshift-subscription-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "First": {{"Type": "{SUBSCRIPTION}",
                       "Properties": {{"{NAME_PROPERTY}": "cfn-subscription-first",
                         "SnsTopicArn": "{TOPIC_ARN}", "Severity": "INFO"}}}},
                     "Second": {{"Type": "{SUBSCRIPTION}",
                       "Properties": {{"{NAME_PROPERTY}": "cfn-subscription-second",
                         "SnsTopicArn": "{TOPIC_ARN}"}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "redshift-subscription-topic"}}}}}}}}"#
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
async fn every_operation_that_takes_a_template_admits_an_event_subscription() {
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
/// spelling of it still is not, the type is identified by the property it takes its name from,
/// and the values Amazon Redshift ANSWERS for a subscription are the ones no declaration
/// supplies.
#[test]
fn the_catalogue_publishes_the_event_subscription_type_and_how_it_is_identified() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&SUBSCRIPTION),
        "expected {SUBSCRIPTION:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(SUBSCRIPTION) && is_recognized(SUBSCRIPTION),
        "expected {SUBSCRIPTION:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does. Each is a spelling the resource
    // reference does NOT publish: a plural of this type, a dropped word, and the name of the
    // API shape rather than of the type.
    for misspelled in [
        "AWS::Redshift::EventSubscriptions",
        "AWS::Redshift::Subscription",
        "AWS::Redshift::EventSubscriptionsList",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // "Ref returns the name of the Amazon Redshift event notification subscription": the name a
    // declaration states IS the resource's identity, which is the property a client would name
    // an existing subscription by.
    assert_eq!(
        primary_identifier::properties(SUBSCRIPTION),
        vec![NAME_PROPERTY.to_string()],
        "expected the subscription to be identified by its name property, got {:?}",
        primary_identifier::properties(SUBSCRIPTION)
    );
    assert_eq!(
        name_property_candidates(SUBSCRIPTION)
            .first()
            .map(String::as_str),
        Some(NAME_PROPERTY),
        "expected the name to be read from {NAME_PROPERTY:?}, got {:?}",
        name_property_candidates(SUBSCRIPTION)
    );
    // The six values that service computes for a subscription are read-only: no member of
    // `CreateEventSubscription` or `ModifyEventSubscription` accepts one, so a declaration
    // carrying it is deployed WITHOUT it.
    assert_eq!(
        read_only_properties(SUBSCRIPTION),
        [
            "CustSubscriptionId",
            "CustomerAwsId",
            "EventCategoriesList",
            "SourceIdsList",
            "Status",
            "SubscriptionCreationTime",
        ],
        "expected the values Amazon Redshift answers for a subscription to be read-only, got \
         {:?}",
        read_only_properties(SUBSCRIPTION)
    );
}
