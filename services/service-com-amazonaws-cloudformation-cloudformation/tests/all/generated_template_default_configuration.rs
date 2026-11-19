// SPDX-License-Identifier: CC0-1.0

//! The `TemplateConfiguration` a generated template is in force with, end to end over the
//! awsQuery wire — and the scan → generate → IMPORT workflow that configuration is what
//! makes possible.
//!
//! RULE: a generated template's `TemplateConfiguration` is in force on every record whether
//! or not the create stated one — each of the two settings a create leaves out is in force as
//! the generator's own default, `RETAIN` — so `DescribeGeneratedTemplate` reports both
//! settings for every record and the template `GetGeneratedTemplate` renders states a
//! `DeletionPolicy` and an `UpdateReplacePolicy` on every resource it declares. That is the
//! attribute an IMPORT change set requires of every resource it adopts, so the template the
//! generator answers for a resource no stack manages can be used, unchanged, to bring that
//! resource under a stack's management.
//!
//! The rows differ along what the rule generalises over: which of the two settings the create
//! states (neither, one, the other, both), the value it states (both values the modelled
//! enums carry), and where the covered resource comes from (a base stack, the request's own
//! list).

use std::time::Instant;

use service_com_amazonaws_cloudformation_cloudformation::state::generated_template as generated;
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_action, change_set_status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::generated_template_body::{attribute_policy, get_generated_template, parsed_template};
use crate::generated_templates::{
    create_generated_template, deploy, describe_generated_template,
};
use crate::harness::{router_with_engine, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL};
use crate::stack_lifecycle::{call, member, router, sns, xml_text, xml_texts};

/// A base stack declaring one resource, under the logical id `Topic`.
const ONE_RESOURCE_STACK: &str = r#"{"Resources":{"Topic":{"Type":"AWS::SNS::Topic"}}}"#;

/// One row: the `TemplateConfiguration` members the create states, and the configuration the
/// record must then be in force with.
struct ConfigurationCase {
    case: &'static str,
    /// Whether the generation covers a base stack's resource rather than one the request
    /// lists itself.
    from_base_stack: bool,
    /// The `TemplateConfiguration` settings the create states, by member name.
    stated: &'static [(&'static str, &'static str)],
    /// The `(DeletionPolicy, UpdateReplacePolicy)` the record must report, as the modelled
    /// enums spell them.
    expected: (&'static str, &'static str),
}

/// The rows: every combination of which settings the create states, over resources of both
/// origins.
const CONFIGURATION_CASES: &[ConfigurationCase] = &[
    ConfigurationCase {
        case: "no TemplateConfiguration at all, over a base stack's resource",
        from_base_stack: true,
        stated: &[],
        expected: ("RETAIN", "RETAIN"),
    },
    ConfigurationCase {
        case: "no TemplateConfiguration at all, over a resource the request lists itself",
        from_base_stack: false,
        stated: &[],
        expected: ("RETAIN", "RETAIN"),
    },
    ConfigurationCase {
        case: "a DeletionPolicy alone, leaving the other setting at the default",
        from_base_stack: true,
        stated: &[("DeletionPolicy", "DELETE")],
        expected: ("DELETE", "RETAIN"),
    },
    ConfigurationCase {
        case: "an UpdateReplacePolicy alone, leaving the other setting at the default",
        from_base_stack: false,
        stated: &[("UpdateReplacePolicy", "DELETE")],
        expected: ("RETAIN", "DELETE"),
    },
    ConfigurationCase {
        case: "both settings stated as DELETE, which is neither setting's default",
        from_base_stack: true,
        stated: &[("DeletionPolicy", "DELETE"), ("UpdateReplacePolicy", "DELETE")],
        expected: ("DELETE", "DELETE"),
    },
    ConfigurationCase {
        case: "both settings stated as RETAIN, which the default agrees with",
        from_base_stack: false,
        stated: &[("DeletionPolicy", "RETAIN"), ("UpdateReplacePolicy", "RETAIN")],
        expected: ("RETAIN", "RETAIN"),
    },
];

/// The record of `name`, read once its generation has concluded.
///
/// Polled for the CONDITION — a generation concludes on the clock, so the read is repeated
/// until the record reports it rather than timed against the machine's load. `pub(crate)` so
/// a suite that has to wait for a generation can poll for it instead of sleeping past
/// `generated_templates::wait_for_the_generation`'s due instant.
pub(crate) async fn completed_generation(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = describe_generated_template(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeGeneratedTemplate answered {status} with {body}"
        );
        if xml_text(&body, "Status").as_deref() == Some(generated::status::COMPLETE) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the generation never concluded, last read {body}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// The one resource a row's generation covers, as the request lists it.
fn listed_members(topic_arn: &str) -> Vec<(String, String)> {
    vec![
        member("Resources.member.1.ResourceType", "AWS::SNS::Topic"),
        member("Resources.member.1.LogicalResourceId", "ListedTopic"),
        member("Resources.member.1.ResourceIdentifier.entry.1.key", "TopicArn"),
        member(
            "Resources.member.1.ResourceIdentifier.entry.1.value",
            topic_arn,
        ),
    ]
}

/// RULE: both settings of the configuration are in force on every record — the ones the
/// create stated, and the generator's own default for the ones it did not — and the rendered
/// template states both attributes on every resource it declares.
#[tokio::test]
async fn the_configuration_in_force_defaults_where_the_create_states_none() {
    let router = router();

    for (index, row) in CONFIGURATION_CASES.iter().enumerate() {
        let case = row.case;
        let name = format!("configured-{index}");
        let mut members: Vec<(String, String)> = Vec::new();
        if row.from_base_stack {
            let stack = format!("configured-base-{index}");
            deploy(&router, &stack, ONE_RESOURCE_STACK).await;
            members.push(member("StackName", &stack));
        } else {
            members.extend(listed_members(&format!(
                "arn:aws:sns:us-east-1:000000000000:listed-topic-{index}"
            )));
        }
        for (setting, value) in row.stated {
            members.push(member(&format!("TemplateConfiguration.{setting}"), value));
        }

        let (status, body) = create_generated_template(&router, &name, &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateGeneratedTemplate answered {status} with {body}"
        );

        // What the record REPORTS: both settings, whether the create stated them or not.
        let described = completed_generation(case, &router, &name).await;
        let reported = (
            xml_text(&described, "DeletionPolicy"),
            xml_text(&described, "UpdateReplacePolicy"),
        );
        let expected = (
            Some(row.expected.0.to_string()),
            Some(row.expected.1.to_string()),
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the configuration {expected:?} in force, got \
             {reported:?} in {described}"
        );

        // And what the TEMPLATE it renders declares: the same configuration, assigned to
        // every resource of the template in the attribute's own vocabulary.
        let (status, body) = get_generated_template(&router, &name, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetGeneratedTemplate answered {status} with {body}"
        );
        let template_body = xml_text(&body, "TemplateBody")
            .unwrap_or_else(|| panic!("case {case:?}: expected a TemplateBody, got {body}"));
        // Read back with this service's own template reader, so the attributes are asserted
        // on EVERY resource the rendered template declares rather than anywhere in its text.
        let parsed = parsed_template(case, &template_body);
        assert!(
            !parsed.resources.is_empty(),
            "case {case:?}: expected the covered resource declared, got {template_body}"
        );
        for resource in &parsed.resources {
            for (attribute, configured) in [
                ("DeletionPolicy", row.expected.0),
                ("UpdateReplacePolicy", row.expected.1),
            ] {
                let expected = Some(attribute_policy(case, configured));
                let stated = resource
                    .declaration
                    .get(attribute)
                    .and_then(|value| value.as_str());
                assert_eq!(
                    stated, expected,
                    "case {case:?}: expected {attribute} {expected:?} on {:?}, got \
                     {stated:?} in {template_body}",
                    resource.logical_id
                );
            }
        }
    }
}

/// RULE, end to end: because the generated template states a `DeletionPolicy` on every
/// resource even when the create asked for no configuration, the template the generator
/// answers for a resource NO stack manages is accepted, unchanged, by an IMPORT change set —
/// and executing it leaves the stack managing that very resource.
#[tokio::test]
async fn a_generated_template_brings_an_unmanaged_resource_under_a_stack() {
    let (router, engine) = router_with_engine();
    let case = "a topic created out of band, generated and then imported";

    // A resource no stack manages: a topic created through Amazon SNS's own API.
    let (status, body) = sns(&router, "CreateTopic", &[("Name", "unmanaged-topic")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic answered {status} with {body}"
    );
    let topic_arn = xml_text(&body, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TopicArn, got {body}"));

    // The generated template for it, created with NO `TemplateConfiguration` at all.
    let (status, body) =
        create_generated_template(&router, "scanned", &listed_members(&topic_arn)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateGeneratedTemplate answered {status} with {body}"
    );
    completed_generation(case, &router, "scanned").await;
    let (status, body) = get_generated_template(&router, "scanned", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetGeneratedTemplate answered {status} with {body}"
    );
    let template_body = xml_text(&body, "TemplateBody")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TemplateBody, got {body}"));

    // The IMPORT takes that template exactly as the generator answered it.
    let stack = "adopting-stack";
    let (status, body) = create_change_set(
        &router,
        stack,
        "cs",
        &[
            member("ChangeSetType", "IMPORT"),
            member("TemplateBody", &template_body),
            member("ResourcesToImport.member.1.ResourceType", "AWS::SNS::Topic"),
            member("ResourcesToImport.member.1.LogicalResourceId", "ListedTopic"),
            member(
                "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key",
                "TopicArn",
            ),
            member(
                "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value",
                &topic_arn,
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the generated template must be importable, got {status} with {body}"
    );

    settle_the_store(&router, &engine).await;
    let (_, body) = describe_change_set(&router, "cs", Some(stack)).await;
    let planned = (
        xml_text(&body, "Status").unwrap_or_default(),
        xml_texts(&body, "Action"),
        xml_texts(&body, "PhysicalResourceId"),
    );
    let expected = (
        change_set_status::CREATE_COMPLETE.to_string(),
        vec![change_action::IMPORT.to_string()],
        vec![topic_arn.clone()],
    );
    assert_eq!(
        planned, expected,
        "case {case:?}: expected one Import of the existing topic, got {planned:?} in {body}"
    );

    // Executed, the stack really manages the topic that already existed.
    let (status, body) = call(
        &router,
        "ExecuteChangeSet",
        &[member("ChangeSetName", "cs"), member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ExecuteChangeSet answered {status} with {body}"
    );
    settle_the_store(&router, &engine).await;
    let (status, body) = call(
        &router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources answered {status} with {body}"
    );
    let managed = (
        xml_texts(&body, "LogicalResourceId"),
        xml_texts(&body, "PhysicalResourceId"),
    );
    let expected = (
        vec!["ListedTopic".to_string()],
        vec![topic_arn.clone()],
    );
    assert_eq!(
        managed, expected,
        "case {case:?}: expected the stack to manage the imported topic, got {managed:?} in \
         {body}"
    );

    // And the topic it adopted is the one Amazon SNS still holds: the import created
    // nothing, it took over what was there.
    let (status, body) = sns(&router, "ListTopics", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTopics answered {status} with {body}"
    );
    assert_eq!(
        xml_texts(&body, "TopicArn"),
        vec![topic_arn],
        "case {case:?}: expected the one topic the account already had, got {body}"
    );
}
