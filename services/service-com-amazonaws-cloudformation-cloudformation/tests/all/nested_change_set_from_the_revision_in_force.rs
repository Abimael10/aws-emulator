// SPDX-License-Identifier: CC0-1.0

//! Which template a hierarchy's change set is EXPANDED from — end to end over the awsQuery
//! wire.
//!
//! RULE: with `IncludeNestedStacks` set ("creates a change set for the all nested stacks
//! specified in the template"), the nested stacks a change set expands are the
//! `AWS::CloudFormation::Stack` resources of the revision that change set would DEPLOY — the
//! submitted body where the request submits one and the stack's own where it reuses it
//! (`UsePreviousTemplate`: "whether to reuse the template that's associated with the stack to
//! create the change set") — with each declaration EVALUATED under the parameter values that
//! change set would deploy with. So a change on such a resource carries
//! `ResourceChange$ChangeSetId` ("the change set ID of the nested change set") naming a change
//! set this service really holds, whose own `Changes` are the child's resources, however the
//! revision reached the request and however the declaration addresses the child's template.
//!
//! The rows differ along what the rule generalises over: WHERE the revision came from (the
//! request's own body and the stack's reused one) and HOW the declaration addresses the child
//! template (text the template states, a parameter reference, and a URL built from a parameter
//! and a pseudo parameter).

use serde_json::json;

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::harness::{call, create_stack, member, router_with_engine, xml_blocks, xml_text};
use crate::nested_stacks::{staged_url, STACK_RESOURCE_TYPE};
use crate::template_by_reference::{create_staging_bucket, stage, STAGING_BUCKET};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_action, change_set_status, status,
};
use smithy_server_runtime::MultiServiceRouter;

/// The key each revision of the child template is staged under.
const DEPLOYED_KEY: &str = "revision-in-force/child-deployed.json";
const REVISED_KEY: &str = "revision-in-force/child-revised.json";

/// The display name the deployed child declares, and the one the revised child does: a
/// property change that is no replacement, so the child's own plan reports a `Modify` of the
/// resource rather than its recreation.
const DEPLOYED_DISPLAY_NAME: &str = "the revision in force";
const REVISED_DISPLAY_NAME: &str = "the revision the change set plans";

/// A child template declaring one topic with the display name it is given.
fn child_template(display_name: &str) -> String {
    json!({
        "Resources": {
            "ChildTopic": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"DisplayName": display_name}
            }
        }
    })
    .to_string()
}

/// A parent template whose one nested stack addresses the child by the value it is given,
/// which may be text or an expression.
fn parent_template(parameters: serde_json::Value, template_url: serde_json::Value) -> String {
    json!({
        "Parameters": parameters,
        "Resources": {
            "Child": {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {"TemplateURL": template_url}
            }
        }
    })
    .to_string()
}

/// The `Parameters` members an SDK sends for the values a case states.
fn parameters(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let position = index + 1;
            [
                member(
                    &format!("Parameters.member.{position}.ParameterKey"),
                    key,
                ),
                member(
                    &format!("Parameters.member.{position}.ParameterValue"),
                    value,
                ),
            ]
        })
        .collect()
}

/// The `ResourceChange` block one change set reports for `logical_id`.
fn resource_change(xml: &str, logical_id: &str) -> String {
    xml_blocks(xml, "ResourceChange")
        .into_iter()
        .find(|change| xml_text(change, "LogicalResourceId").as_deref() == Some(logical_id))
        .unwrap_or_default()
}

/// Settle the store until `stack` reports a status that is not in flight, or the deadline
/// passes: a hierarchy settles one level per transition, so the test waits for the CONDITION
/// rather than for a span of the clock.
async fn settled_stack(
    case: &str,
    router: &MultiServiceRouter,
    engine: &::std::sync::Arc<engine::InMemoryEngine>,
    stack: &str,
) -> String {
    for _ in 0..64 {
        settle_the_store(router, engine).await;
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks({stack}) must answer, got {code} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
    }
    panic!("case {case:?}: the stack {stack} never settled");
}

/// One row: the revision the parent stack is deployed with, the values it is deployed with,
/// and the members the change set states — which is where the revision it plans comes from.
struct Row {
    case: &'static str,
    parent: String,
    created_with: Vec<(String, String)>,
    revised_with: Vec<(String, String)>,
}

fn rows() -> Vec<Row> {
    // The declaration's `TemplateURL` stated three ways: as the text of the deployed and the
    // revised object, as a reference to a parameter carrying it, and as a URL built from a
    // parameter and `AWS::Region` — the form `cdk` and `aws cloudformation package` emit.
    let built_url = json!({
        "Fn::Sub": format!("https://s3.${{AWS::Region}}.amazonaws.com/{STAGING_BUCKET}/${{ChildKey}}")
    });
    let url_parameter = json!({"ChildTemplateUrl": {"Type": "String"}});
    let key_parameter = json!({"ChildKey": {"Type": "String"}});
    vec![
        Row {
            case: "a submitted revision stating the child's URL as text",
            parent: parent_template(json!({}), json!(staged_url(DEPLOYED_KEY))),
            created_with: Vec::new(),
            revised_with: vec![member(
                "TemplateBody",
                &parent_template(json!({}), json!(staged_url(REVISED_KEY))),
            )],
        },
        Row {
            case: "a submitted revision addressing the child by a parameter",
            parent: parent_template(url_parameter.clone(), json!({"Ref": "ChildTemplateUrl"})),
            created_with: parameters(&[("ChildTemplateUrl", &staged_url(DEPLOYED_KEY))]),
            revised_with: [
                vec![member(
                    "TemplateBody",
                    &parent_template(
                        url_parameter.clone(),
                        json!({"Ref": "ChildTemplateUrl"}),
                    ),
                )],
                parameters(&[("ChildTemplateUrl", &staged_url(REVISED_KEY))]),
            ]
            .concat(),
        },
        Row {
            case: "the stack's own revision reused, the child addressed by a parameter",
            parent: parent_template(url_parameter.clone(), json!({"Ref": "ChildTemplateUrl"})),
            created_with: parameters(&[("ChildTemplateUrl", &staged_url(DEPLOYED_KEY))]),
            revised_with: [
                vec![member("UsePreviousTemplate", "true")],
                parameters(&[("ChildTemplateUrl", &staged_url(REVISED_KEY))]),
            ]
            .concat(),
        },
        Row {
            case: "the stack's own revision reused, the child's URL built from a parameter",
            parent: parent_template(key_parameter.clone(), built_url.clone()),
            created_with: parameters(&[("ChildKey", DEPLOYED_KEY)]),
            revised_with: [
                vec![member("UsePreviousTemplate", "true")],
                parameters(&[("ChildKey", REVISED_KEY)]),
            ]
            .concat(),
        },
    ]
}

#[tokio::test]
async fn a_hierarchy_is_expanded_from_the_revision_and_the_values_the_change_set_would_deploy() {
    let (router, engine) = router_with_engine();
    create_staging_bucket(&router).await;
    stage(&router, DEPLOYED_KEY, &child_template(DEPLOYED_DISPLAY_NAME)).await;
    stage(&router, REVISED_KEY, &child_template(REVISED_DISPLAY_NAME)).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let stack = format!("revision-in-force-{index}");
        let change_set = format!("revision-in-force-{index}-set");
        let (code, body) = create_stack(&router, &stack, &row.parent, &row.created_with).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the parent stack must be created first, got {code} {body}"
        );
        let settled = settled_stack(case, &router, &engine, &stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: the parent and its child must deploy, got {settled}"
        );

        let members = [
            vec![member("IncludeNestedStacks", "true")],
            row.revised_with.clone(),
        ]
        .concat();
        let (code, body) = create_change_set(&router, &stack, &change_set, &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateChangeSet must create the change set, got {code} {body}"
        );
        let parent_change_set_id = xml_text(&body, "Id").unwrap_or_default();
        settle_the_store(&router, &engine).await;
        let (code, parent) = describe_change_set(&router, &change_set, Some(&stack)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeChangeSet must answer, got {code} {parent}"
        );
        assert_eq!(
            xml_text(&parent, "Status").as_deref(),
            Some(change_set_status::CREATE_COMPLETE),
            "case {case:?}: the parent change set must complete, got {parent}"
        );

        // The parent's change on the nested stack carries the id of the child's own change
        // set, whichever way the revision and the declaration reached this request.
        let child_change = resource_change(&parent, "Child");
        assert_eq!(
            xml_text(&child_change, "ResourceType").as_deref(),
            Some(STACK_RESOURCE_TYPE),
            "case {case:?}: expected a change on the nested-stack resource, got {parent}"
        );
        let child_stack_id = xml_text(&child_change, "PhysicalResourceId").unwrap_or_default();
        let nested_id = xml_text(&child_change, "ChangeSetId").unwrap_or_default();
        assert!(
            !nested_id.is_empty(),
            "case {case:?}: expected the child's change to name a nested change set, got \
             {parent}"
        );

        // ...and that id names a change set this service really holds, reporting the child's
        // OWN plan: the one topic the child template declares, modified for the property the
        // revised body states differently, with no replacement.
        let (code, nested) = describe_change_set(&router, &nested_id, None).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the nested change set must be describable by its id, got {code} \
             {nested}"
        );
        assert_eq!(
            xml_text(&nested, "StackId").as_deref(),
            Some(child_stack_id.as_str()),
            "case {case:?}: expected the nested change set on the child stack, got {nested}"
        );
        assert_eq!(
            xml_text(&nested, "ParentChangeSetId").as_deref(),
            Some(parent_change_set_id.as_str()),
            "case {case:?}: expected the parent change set named, got {nested}"
        );
        assert_eq!(
            xml_text(&nested, "RootChangeSetId").as_deref(),
            Some(parent_change_set_id.as_str()),
            "case {case:?}: expected the root of the hierarchy named, got {nested}"
        );
        assert_eq!(
            xml_text(&nested, "Status").as_deref(),
            Some(change_set_status::CREATE_COMPLETE),
            "case {case:?}: the nested change set must complete, got {nested}"
        );
        let topic_change = resource_change(&nested, "ChildTopic");
        assert_eq!(
            xml_text(&topic_change, "Action").as_deref(),
            Some(change_action::MODIFY),
            "case {case:?}: expected the child's own topic modified, got {nested}"
        );
        assert_eq!(
            xml_text(&topic_change, "ResourceType").as_deref(),
            Some("AWS::SNS::Topic"),
            "case {case:?}: expected the child's own resource type, got {nested}"
        );
        assert_eq!(
            xml_text(&topic_change, "Replacement").as_deref(),
            Some("False"),
            "case {case:?}: a display-name change replaces nothing, got {nested}"
        );
    }
}
