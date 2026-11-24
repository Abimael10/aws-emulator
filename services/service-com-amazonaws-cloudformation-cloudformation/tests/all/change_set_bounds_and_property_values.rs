// SPDX-License-Identifier: CC0-1.0

//! What a change set is BOUNDED by and what it REPORTS about the values it would deploy,
//! end to end over the awsQuery wire.
//!
//! The rules every case here holds the service to:
//!
//! * **`ResourceTypes` bounds the UPDATE a change set plans**: a plan over a stack that
//!   already stands is refused before anything is written when the template it would deploy
//!   declares a type the stated list does not cover, while a change set that brings its stack
//!   into existence — which deploys nothing until it is executed — is admitted with the list
//!   recorded on it.
//! * **the stack's own tags are the tags of the resources it manages**: a revision stating
//!   them differently changes every resource a service holds tags for, under the `Tags`
//!   attribute rather than under `Properties`, so a tags-only change set is a change set and
//!   not a no-op.
//! * **`IncludePropertyValues` is what carries the values**: each change then reports the
//!   `Properties` document each side of it would deploy (`BeforeContext`/`AfterContext`) and
//!   each detail the target's path, the two values and what the change does to it; a request
//!   that does not ask for them is answered without them.
//! * **an update reports the documented phases of its own progress**: per resource an
//!   opening `CREATE_IN_PROGRESS`, one carrying `Resource creation Initiated` and the
//!   completion, then the stack's `UPDATE_COMPLETE_CLEANUP_IN_PROGRESS` and its terminal
//!   status — every event of the operation carrying that operation's client request token.
//! * **a change set is the only thing that creates one**: a stack deployed through
//!   `CreateStack`/`UpdateStack` carries no change set to read back.
//!
//! The recorded traces these cases were written for are ONE row of each table below: the rows
//! span the actions a plan can carry, the tag edits a revision can state, the three change
//! actions a context is reported for and the deletion policies a removal reports. Every read
//! goes through the real request path (form-encoded body in, XML envelope out), so what is
//! asserted is what a client sees.

use std::sync::Arc;

use service_com_amazonaws_cloudformation_cloudformation::state::change_plan::{
    attribute_change_type, policy_action, replacement,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_action, change_set_status, NO_CHANGES_REASON,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::harness::{
    call, create_stack, member, refusal, router_with_engine, settled_stack_status, xml_blocks,
    xml_text, xml_texts, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};

/// A resource type a service of this emulator really holds — and really tags: an
/// `AWS::SSM::Parameter` is created in AWS Systems Manager, which holds its tags.
const TAGGED_TYPE: &str = "AWS::SSM::Parameter";

/// A resource type CloudFormation serves itself, which no service holds tags for: a
/// `WaitConditionHandle` is "a resource that has no properties" and nothing tags it.
const UNTAGGED_TYPE: &str = "AWS::CloudFormation::WaitConditionHandle";

/// A declaration of one `AWS::SSM::Parameter` carrying `value`.
fn parameter(value: &str) -> String {
    format!(r#"{{"Type":"{TAGGED_TYPE}","Properties":{{"Type":"String","Value":"{value}"}}}}"#)
}

/// A template declaring each `(logical id, declaration)` pair.
fn template(resources: &[(&str, String)]) -> String {
    let declared: Vec<String> = resources
        .iter()
        .map(|(logical_id, declaration)| format!(r#""{logical_id}":{declaration}"#))
        .collect();
    format!(r#"{{"Resources":{{{}}}}}"#, declared.join(","))
}

/// The `ResourceTypes.member.N` request members one stated list is sent as.
fn resource_types(allowed: &[&str]) -> Vec<(String, String)> {
    allowed
        .iter()
        .enumerate()
        .map(|(position, entry)| member(&format!("ResourceTypes.member.{}", position + 1), entry))
        .collect()
}

/// The `Tags.member.N.Key`/`Value` request members one tag list is sent as.
fn tag_members(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (position, (key, value)) in tags.iter().enumerate() {
        members.push(member(&format!("Tags.member.{}.Key", position + 1), key));
        members.push(member(&format!("Tags.member.{}.Value", position + 1), value));
    }
    members
}

/// The change set as it reads once its creation has settled, with the property values asked
/// for or not.
async fn described(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    stack: &str,
    change_set: &str,
    with_property_values: bool,
) -> String {
    settle_the_store(router, engine).await;
    let mut members = vec![
        member("ChangeSetName", change_set),
        member("StackName", stack),
    ];
    if with_property_values {
        members.push(member("IncludePropertyValues", "true"));
    }
    let (code, body) = call(router, "DescribeChangeSet", &members).await;
    assert_eq!(
        code, 200,
        "the change set of {stack:?} must be describable, got {code} {body}"
    );
    body
}

/// `(Attribute, Path, BeforeValue, AfterValue, AttributeChangeType, ChangeSource)` as one
/// `ResourceTargetDefinition` carries them — the members these cases read off a detail.
type ReportedTarget = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// One `ResourceChange` as the response carries it, with the members these cases read.
#[derive(Debug, PartialEq, Eq)]
struct Reported {
    action: Option<String>,
    logical_resource_id: Option<String>,
    resource_type: Option<String>,
    replacement: Option<String>,
    policy_action: Option<String>,
    scope: Vec<String>,
    before_context: Option<String>,
    after_context: Option<String>,
    /// One entry per detail, in response order.
    targets: Vec<ReportedTarget>,
}

/// Every `ResourceChange` a `DescribeChangeSet` response carries, in response order.
fn reported(xml: &str) -> Vec<Reported> {
    xml_blocks(xml, "ResourceChange")
        .iter()
        .map(|change| Reported {
            action: xml_text(change, "Action"),
            logical_resource_id: xml_text(change, "LogicalResourceId"),
            resource_type: xml_text(change, "ResourceType"),
            replacement: xml_text(change, "Replacement"),
            policy_action: xml_text(change, "PolicyAction"),
            scope: xml_blocks(change, "Scope")
                .first()
                .map(|scope| xml_texts(scope, "member"))
                .unwrap_or_default(),
            before_context: xml_text(change, "BeforeContext"),
            after_context: xml_text(change, "AfterContext"),
            targets: xml_blocks(change, "Details")
                .first()
                .map(|details| {
                    xml_blocks(details, "member")
                        .iter()
                        .map(|entry| {
                            (
                                xml_text(entry, "Attribute"),
                                xml_text(entry, "Path"),
                                xml_text(entry, "BeforeValue"),
                                xml_text(entry, "AfterValue"),
                                xml_text(entry, "AttributeChangeType"),
                                xml_text(entry, "ChangeSource"),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// The change of `logical_id` the response reports, or a panic naming what it reported
/// instead.
fn change_of(changes: &[Reported], case: &str, logical_id: &str) -> Reported {
    let found = changes
        .iter()
        .find(|change| change.logical_resource_id.as_deref() == Some(logical_id));
    match found {
        Some(change) => Reported {
            action: change.action.clone(),
            logical_resource_id: change.logical_resource_id.clone(),
            resource_type: change.resource_type.clone(),
            replacement: change.replacement.clone(),
            policy_action: change.policy_action.clone(),
            scope: change.scope.clone(),
            before_context: change.before_context.clone(),
            after_context: change.after_context.clone(),
            targets: change.targets.clone(),
        },
        None => panic!("case {case:?}: expected a change for {logical_id:?}, got {changes:?}"),
    }
}

/// The `(Status, StatusReason)` the described change set carries.
fn status_of(body: &str) -> (Option<String>, Option<String>) {
    (
        xml_text(body, "Status"),
        xml_text(body, "StatusReason"),
    )
}

/// Poll `DescribeChangeSet` until the change set's own creation has settled, and answer the
/// `Status` it settled on.
///
/// `CreateChangeSet` is answered while CloudFormation is still computing the plan, so what a
/// caller waits for is the CONDITION — a status that is not `CREATE_PENDING` — under the
/// harness's own generous deadline, never for a span of time.
async fn settled_change_set_status(
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
) -> String {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = describe_change_set(router, change_set, Some(stack)).await;
        assert_eq!(
            code, 200,
            "the change set {change_set:?} of {stack:?} must be describable, got {code} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if reported != change_set_status::CREATE_PENDING {
            return reported;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the plan of {change_set:?} to be computed, it is still {reported:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_allow_list_bounds_the_template_a_change_set_would_deploy() {
    let (router, engine) = router_with_engine();
    // The stack the UPDATE rows plan a change to: one resource of a type a service holds,
    // and one of a type CloudFormation serves itself.
    let deployed = template(&[
        ("Held", parameter("v1")),
        ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
    ]);
    // (case, whether the row CREATES its stack, the template the change set submits, the
    // list it states, the type the refusal must name — `None` for a row the list admits)
    //
    // What the list bounds is the UPDATE the plan would carry out: over a stack that already
    // stands, a body declaring a type the list does not cover is refused before anything is
    // written, and a body whose every declared type it covers is admitted. A change set of a
    // type that brings its stack into existence plans no update of a stack, so it is admitted
    // whatever its template declares — `change_set_resource_types_bound.rs` holds that side of
    // the rule over every change-set type.
    type BoundRow<'a> = (&'a str, bool, String, Vec<&'a str>, Option<&'a str>);
    let rows: Vec<BoundRow<'_>> = vec![
        (
            "a plan that adds a resource of a type the list does not cover",
            false,
            template(&[
                ("Held", parameter("v1")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Added", parameter("new")),
            ]),
            vec![UNTAGGED_TYPE],
            Some(TAGGED_TYPE),
        ),
        (
            "a plan that modifies a resource of a type the list does not cover",
            false,
            template(&[
                ("Held", parameter("v2")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
            ]),
            vec![UNTAGGED_TYPE],
            Some(TAGGED_TYPE),
        ),
        (
            "a submitted template declaring only covered types",
            false,
            template(&[
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Another", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
            ]),
            vec![UNTAGGED_TYPE],
            None,
        ),
        (
            "a service wildcard that covers one declared type and not the other",
            false,
            template(&[
                ("Held", parameter("v1")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Added", parameter("new")),
            ]),
            vec!["AWS::SSM::*"],
            Some(UNTAGGED_TYPE),
        ),
        (
            "a list naming both declared types outright",
            false,
            template(&[
                ("Held", parameter("v1")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Added", parameter("new")),
            ]),
            vec![TAGGED_TYPE, UNTAGGED_TYPE],
            None,
        ),
        (
            "every published type",
            false,
            template(&[
                ("Held", parameter("v1")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Added", parameter("new")),
            ]),
            vec!["AWS::*"],
            None,
        ),
        (
            "no list at all, which bounds nothing",
            false,
            template(&[
                ("Held", parameter("v1")),
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Added", parameter("new")),
            ]),
            Vec::new(),
            None,
        ),
        (
            "a change set that would create its stack, over a type the list does not cover",
            true,
            template(&[
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Param", parameter("v1")),
            ]),
            vec![UNTAGGED_TYPE],
            None,
        ),
        (
            "a change set that would create its stack, over covered types",
            true,
            template(&[
                ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
                ("Param", parameter("v1")),
            ]),
            vec![TAGGED_TYPE, UNTAGGED_TYPE],
            None,
        ),
    ];

    for (index, (case, creates, submitted, allowed, refused)) in rows.into_iter().enumerate() {
        let stack = format!("csbound-{index}");
        let mut extra = vec![member("TemplateBody", &submitted)];
        if creates {
            extra.push(member("ChangeSetType", "CREATE"));
        } else {
            let (code, body) = create_stack(&router, &stack, &deployed, &[]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the stack the plan changes must exist, got {code} {body}"
            );
            settle_the_store(&router, &engine).await;
        }
        extra.extend(resource_types(&allowed));
        let (code, body) = create_change_set(&router, &stack, "plan", &extra).await;
        match refused {
            Some(resource_type) => {
                assert_eq!(
                    code, 400,
                    "case {case:?}: the call must be refused, got {code} {body}"
                );
                let answered = refusal(&body);
                let expected = (
                    "ValidationError".to_string(),
                    format!(
                        "Resource type {resource_type} is not allowed by parameter \
                         ResourceTypes [{}]",
                        allowed.join(", ")
                    ),
                );
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                );
                // Nothing was written: no plan under that name, and no stack a creating
                // change set would have brought into existence.
                let (code, body) = describe_change_set(&router, "plan", Some(&stack)).await;
                assert_eq!(
                    code,
                    if creates { 400 } else { 404 },
                    "case {case:?}: a refused call leaves no plan, got {code} {body}"
                );
            }
            None => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the list admits the template, got {code} {body}"
                );
                let reported = described(&router, &engine, &stack, "plan", false).await;
                let status = status_of(&reported);
                assert_eq!(
                    status.0.as_deref(),
                    Some(change_set_status::CREATE_COMPLETE),
                    "case {case:?}: the admitted plan must be created, got {status:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_tags_only_revision_changes_every_resource_a_service_holds_tags_for() {
    let (router, engine) = router_with_engine();
    let deployed = template(&[
        ("Param", parameter("v1")),
        ("Handle", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
    ]);
    // (case, the tags the change set states, whether the revision changes the tag set)
    type TagRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, bool);
    let rows: Vec<TagRow<'_>> = vec![
        ("a tag value stated differently", vec![("env", "prod")], true),
        (
            "a key the stack does not carry",
            vec![("env", "dev"), ("owner", "platform")],
            true,
        ),
        ("every tag removed", Vec::new(), true),
        ("the very same tags", vec![("env", "dev")], false),
    ];

    for (index, (case, tags, changes_them)) in rows.into_iter().enumerate() {
        let stack = format!("cstags-{index}");
        let (code, body) = create_stack(
            &router,
            &stack,
            &deployed,
            &tag_members(&[("env", "dev")]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the tagged stack must exist, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        let mut extra = vec![member("TemplateBody", &deployed)];
        extra.extend(tag_members(&tags));
        // An empty tag list is a statement that the stack carries none, which the wire
        // cannot spell as a member: the request states it with the empty list marker the
        // query protocol uses for a list of no entries.
        if tags.is_empty() {
            extra.push(member("Tags", ""));
        }
        let (code, body) = create_change_set(&router, &stack, "tagged", &extra).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set must be created, got {code} {body}"
        );
        let described = described(&router, &engine, &stack, "tagged", false).await;
        let status = status_of(&described);
        if !changes_them {
            assert_eq!(
                status,
                (
                    Some(change_set_status::FAILED.to_string()),
                    Some(NO_CHANGES_REASON.to_string())
                ),
                "case {case:?}: a revision that restates the same tags changes nothing, got \
                 {described}"
            );
            continue;
        }
        assert_eq!(
            status.0.as_deref(),
            Some(change_set_status::CREATE_COMPLETE),
            "case {case:?}: a tag change is a change, got {described}"
        );
        let changes = reported(&described);
        let logical_ids: Vec<Option<String>> = changes
            .iter()
            .map(|change| change.logical_resource_id.clone())
            .collect();
        assert_eq!(
            logical_ids,
            vec![Some("Param".to_string())],
            "case {case:?}: only the resource a service holds tags for changes, got {changes:?}"
        );
        let change = change_of(&changes, case, "Param");
        assert_eq!(
            (
                change.action.as_deref(),
                change.scope.clone(),
                change.replacement.as_deref()
            ),
            (
                Some(change_action::MODIFY),
                vec!["Tags".to_string()],
                Some(replacement::FALSE)
            ),
            "case {case:?}: the tag change modifies the resource under the Tags scope, got \
             {change:?}"
        );
        // One detail, naming the attribute that triggers the update: re-tagging never
        // recreates the resource, both sets are read, and nothing in the template states it,
        // so no `ChangeSource` is reported.
        let detail_shape: Vec<(Option<String>, Option<String>, Option<String>)> = change
            .targets
            .iter()
            .map(|(attribute, _, _, _, _, change_source)| {
                (attribute.clone(), None, change_source.clone())
            })
            .collect();
        assert_eq!(
            detail_shape,
            vec![(Some("Tags".to_string()), None, None)],
            "case {case:?}: expected one Tags detail with no ChangeSource, got {change:?}"
        );
    }
}

#[tokio::test]
async fn the_property_values_a_change_would_deploy_are_reported_when_asked_for() {
    let (router, engine) = router_with_engine();
    // (case, the `DeletionPolicy` the removed resource declares, the action its removal
    // reports on the physical resource). Source: the `DeletionPolicy` resource attribute,
    // whose values are `Delete` (the default), `Retain`, `RetainExceptOnCreate` and
    // `Snapshot`.
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        ("no deletion policy at all", None, policy_action::DELETE),
        ("a retained resource", Some("Retain"), policy_action::RETAIN),
        (
            "a resource retained only on a failed create",
            Some("RetainExceptOnCreate"),
            policy_action::RETAIN,
        ),
        (
            "a snapshotted resource",
            Some("Snapshot"),
            policy_action::SNAPSHOT,
        ),
    ];

    for (index, (case, deletion_policy, expected_action)) in rows.into_iter().enumerate() {
        let stack = format!("csvalues-{index}");
        let gone = match deletion_policy {
            Some(policy) => format!(
                r#"{{"Type":"{TAGGED_TYPE}","DeletionPolicy":"{policy}","Properties":{{"Type":"String","Value":"away"}}}}"#
            ),
            None => parameter("away"),
        };
        let deployed = template(&[("Keep", parameter("k1")), ("Gone", gone)]);
        let (code, body) = create_stack(&router, &stack, &deployed, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the stack the plan changes must exist, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        // One change of each action: `Keep` is modified, `Gone` removed and `Added` added.
        let submitted = template(&[("Keep", parameter("k2")), ("Added", parameter("fresh"))]);
        let (code, body) = create_change_set(
            &router,
            &stack,
            "values",
            &[member("TemplateBody", &submitted)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set must be created, got {code} {body}"
        );

        let reported_body = described(&router, &engine, &stack, "values", true).await;
        let changes = reported(&reported_body);

        let modified = change_of(&changes, case, "Keep");
        assert_eq!(
            modified.action.as_deref(),
            Some(change_action::MODIFY),
            "case {case:?}: Keep is modified, got {modified:?}"
        );
        // Both contexts: "an encoded JSON string that contains the context of the resource
        // before the change is executed", and the one after it.
        let before: ::serde_json::Value = ::serde_json::from_str(
            modified.before_context.as_deref().unwrap_or_default(),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: a JSON BeforeContext, got {modified:?}"));
        let after: ::serde_json::Value = ::serde_json::from_str(
            modified.after_context.as_deref().unwrap_or_default(),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: a JSON AfterContext, got {modified:?}"));
        assert_eq!(
            before["Properties"]["Value"].as_str(),
            Some("k1"),
            "case {case:?}: the before context carries the value in force, got {before}"
        );
        assert_eq!(
            after["Properties"]["Value"].as_str(),
            Some("k2"),
            "case {case:?}: the after context carries the value submitted, got {after}"
        );
        assert_eq!(
            modified.targets,
            vec![(
                Some("Properties".to_string()),
                Some("/Properties/Value".to_string()),
                Some("k1".to_string()),
                Some("k2".to_string()),
                Some(attribute_change_type::MODIFY.to_string()),
                Some("DirectModification".to_string()),
            )],
            "case {case:?}: the detail reports the target's path and its two values, got \
             {modified:?}"
        );

        let added = change_of(&changes, case, "Added");
        assert_eq!(
            (added.action.as_deref(), added.before_context.clone()),
            (Some(change_action::ADD), None),
            "case {case:?}: an addition has no before context, got {added:?}"
        );
        let after: ::serde_json::Value =
            ::serde_json::from_str(added.after_context.as_deref().unwrap_or_default())
                .unwrap_or_else(|_| panic!("case {case:?}: a JSON AfterContext, got {added:?}"));
        assert_eq!(
            after["Properties"]["Value"].as_str(),
            Some("fresh"),
            "case {case:?}: the addition's context is what it would deploy, got {after}"
        );

        let removed = change_of(&changes, case, "Gone");
        assert_eq!(
            (
                removed.action.as_deref(),
                removed.after_context.clone(),
                removed.policy_action.as_deref()
            ),
            (Some(change_action::REMOVE), None, Some(expected_action)),
            "case {case:?}: a removal reports its deletion policy and no after context, got \
             {removed:?}"
        );
        let before: ::serde_json::Value =
            ::serde_json::from_str(removed.before_context.as_deref().unwrap_or_default())
                .unwrap_or_else(|_| panic!("case {case:?}: a JSON BeforeContext, got {removed:?}"));
        assert_eq!(
            before["Properties"]["Value"].as_str(),
            Some("away"),
            "case {case:?}: the removal's context is what it holds now, got {before}"
        );

        // ...and a request that does not ask for the property values is answered without
        // them: the flag is what carries the detailed changes in the values.
        let plain = described(&router, &engine, &stack, "values", false).await;
        for change in reported(&plain) {
            assert_eq!(
                (
                    change.before_context.clone(),
                    change.after_context.clone(),
                    change
                        .targets
                        .iter()
                        .filter_map(|(_, path, _, _, _, _)| path.clone())
                        .collect::<Vec<String>>()
                ),
                (None, None, Vec::new()),
                "case {case:?}: without the flag no property values are reported, got \
                 {change:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_executed_change_set_reports_the_documented_phases_of_the_update() {
    let (router, engine) = router_with_engine();
    let stack = "csphases";
    let (code, body) = create_stack(&router, stack, &template(&[("First", parameter("v1"))]), &[])
        .await;
    assert_eq!(code, 200, "the stack to update must exist, got {code} {body}");
    // The event stream this case asserts is the WALK the operation made, whose instants the
    // service lays out inside the operation's own span: the stack is polled until it reports
    // a status that is not an operation in flight, rather than having its due instants pulled
    // into the past, so the order the log is read in is the order the walk really made.
    let settled = settled_stack_status("the stack to update", &router, REGION, stack).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "the stack to update must be created"
    );

    let submitted = template(&[
        ("First", parameter("v1")),
        ("One", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
        ("Two", format!(r#"{{"Type":"{UNTAGGED_TYPE}"}}"#)),
    ]);
    let (code, body) = create_change_set(
        &router,
        stack,
        "phases",
        &[member("TemplateBody", &submitted)],
    )
    .await;
    assert_eq!(code, 200, "the change set must be created, got {code} {body}");
    let created = settled_change_set_status(&router, stack, "phases").await;
    assert_eq!(
        created,
        change_set_status::CREATE_COMPLETE,
        "the plan must be computed before it is executed"
    );

    const TOKEN: &str = "csphases-execution";
    let (code, body) = call(
        &router,
        "ExecuteChangeSet",
        &[
            member("ChangeSetName", "phases"),
            member("StackName", stack),
            member("ClientRequestToken", TOKEN),
        ],
    )
    .await;
    assert_eq!(code, 200, "the execution must be accepted, got {code} {body}");
    let settled = settled_stack_status("an executed change set", &router, REGION, stack).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "the execution must carry its change through"
    );
    let _ = engine;

    let (code, body) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(code, 200, "the events must be readable, got {code} {body}");
    // The events of THIS operation, oldest first: every event a stack operation causes
    // carries the token the call supplied.
    let events: Vec<(String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ClientRequestToken").as_deref() == Some(TOKEN))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ResourceStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .rev()
        .collect();
    let sequence: Vec<(&str, &str)> = events
        .iter()
        .map(|(logical_id, status, _)| (logical_id.as_str(), status.as_str()))
        .collect();
    let expected: Vec<(&str, &str)> = vec![
        (stack, "UPDATE_IN_PROGRESS"),
        ("One", "CREATE_IN_PROGRESS"),
        ("Two", "CREATE_IN_PROGRESS"),
        ("One", "CREATE_IN_PROGRESS"),
        ("Two", "CREATE_IN_PROGRESS"),
        ("One", "CREATE_COMPLETE"),
        ("Two", "CREATE_COMPLETE"),
        (stack, "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS"),
        (stack, "UPDATE_COMPLETE"),
    ];
    assert_eq!(
        sequence, expected,
        "case \"an executed change set\": expected {expected:?}, got {events:?}"
    );
    // The second in-progress event of each resource is the one that reports the create
    // reached the service that owns it.
    let initiated: Vec<(String, String)> = events
        .iter()
        .filter(|(_, _, reason)| reason == "Resource creation Initiated")
        .map(|(logical_id, status, _)| (logical_id.clone(), status.clone()))
        .collect();
    assert_eq!(
        initiated,
        vec![
            ("One".to_string(), "CREATE_IN_PROGRESS".to_string()),
            ("Two".to_string(), "CREATE_IN_PROGRESS".to_string()),
        ],
        "case \"an executed change set\": one initiated event per resource, got {events:?}"
    );
}

#[tokio::test]
async fn a_direct_update_reports_the_cleanup_phase_too() {
    let (router, _engine) = router_with_engine();
    let stack = "csdirect";
    let (code, body) = create_stack(&router, stack, &template(&[("First", parameter("v1"))]), &[])
        .await;
    assert_eq!(code, 200, "the stack to update must exist, got {code} {body}");
    let settled = settled_stack_status("a direct update", &router, REGION, stack).await;
    assert_eq!(settled, "CREATE_COMPLETE", "the stack must be created");

    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template(&[("First", parameter("v2"))])),
        ],
    )
    .await;
    assert_eq!(code, 200, "the update must be accepted, got {code} {body}");
    let settled = settled_stack_status("a direct update", &router, REGION, stack).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "the update must carry its change through"
    );

    let (code, body) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(code, 200, "the events must be readable, got {code} {body}");
    let stack_phases: Vec<String> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(stack))
        .filter_map(|event| xml_text(event, "ResourceStatus"))
        .rev()
        .collect();
    assert_eq!(
        stack_phases,
        vec![
            "CREATE_IN_PROGRESS".to_string(),
            "CREATE_COMPLETE".to_string(),
            "UPDATE_IN_PROGRESS".to_string(),
            "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS".to_string(),
            "UPDATE_COMPLETE".to_string(),
        ],
        "case \"a direct update\": the stack reports every documented phase, got {body}"
    );

    // ...and a deploy that used no change set has none to read back: a change set is
    // created by `CreateChangeSet` and by nothing else.
    for named in [stack, "cdk-deploy-change-set"] {
        let (code, body) = describe_change_set(&router, named, Some(stack)).await;
        assert_eq!(
            code, 404,
            "case \"a direct deploy\": {named:?} names no change set, got {code} {body}"
        );
        let answered = refusal(&body);
        assert_eq!(
            answered.0, "ChangeSetNotFound",
            "case \"a direct deploy\": expected ChangeSetNotFound, got {answered:?}"
        );
    }
    let (code, body) = call(&router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(code, 200, "the change sets must be listable, got {code} {body}");
    assert!(
        xml_texts(&body, "ChangeSetName").is_empty(),
        "case \"a direct deploy\": the stack lists no change set, got {body}"
    );
}

#[tokio::test]
async fn an_execution_cannot_restate_the_failure_disposition_the_change_set_carries() {
    let (router, engine) = router_with_engine();
    // (case, the `ChangeSetType` the change set is created with, the `OnStackFailure` it
    // carries). Source: the `OnStackFailure` member, whose values are `DELETE` — "only valid
    // when the ChangeSetType parameter is set to CREATE" — `DO_NOTHING` and `ROLLBACK`.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a create that deletes its stack on failure", "CREATE", "DELETE"),
        ("an update that does nothing on failure", "UPDATE", "DO_NOTHING"),
        ("an update that rolls back on failure", "UPDATE", "ROLLBACK"),
    ];

    for (index, (case, change_set_type, on_stack_failure)) in rows.into_iter().enumerate() {
        let stack = format!("csfail-{index}");
        let body_text = template(&[("Param", parameter("v1"))]);
        if change_set_type == "UPDATE" {
            let (code, body) = create_stack(&router, &stack, &body_text, &[]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the stack to update must exist, got {code} {body}"
            );
            settle_the_store(&router, &engine).await;
        }
        let submitted = template(&[("Param", parameter("v2"))]);
        let (code, body) = create_change_set(
            &router,
            &stack,
            "disposition",
            &[
                member("ChangeSetType", change_set_type),
                member(
                    "TemplateBody",
                    if change_set_type == "UPDATE" {
                        &submitted
                    } else {
                        &body_text
                    },
                ),
                member("OnStackFailure", on_stack_failure),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set must be created, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        let (code, body) = call(
            &router,
            "ExecuteChangeSet",
            &[
                member("ChangeSetName", "disposition"),
                member("StackName", &stack),
                member("DisableRollback", "true"),
            ],
        )
        .await;
        assert_eq!(
            code, 400,
            "case {case:?}: stating both dispositions must be refused, got {code} {body}"
        );
        let answered = refusal(&body);
        let expected = (
            "ValidationError".to_string(),
            "Either DisableRollback or OnStackFailure can be specified, but not both. \
             Validation error as OnStackFailure parameter was specified during \
             CreateChangeSet."
                .to_string(),
        );
        assert_eq!(
            answered, expected,
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
        // Nothing was applied: the change set is still there to execute.
        let described = described(&router, &engine, &stack, "disposition", false).await;
        assert_eq!(
            xml_text(&described, "ExecutionStatus").as_deref(),
            Some("AVAILABLE"),
            "case {case:?}: the refused execution left the plan available, got {described}"
        );
        let _ = execute_change_set(&router, "disposition", Some(&stack)).await;
    }
}
