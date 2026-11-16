// SPDX-License-Identifier: CC0-1.0

//! The provisioning and update path of a stack whose resources are `AWS::SSM::Parameter`s,
//! end to end over the awsQuery wire.
//!
//! The rules under test, each asserted over a TABLE of inputs whose rows differ along what
//! the rule generalises over:
//!
//! * A resource ATTRIBUTE written as an intrinsic function is resolved against the stack's
//!   conditions and parameters before it is applied, exactly as a property is: the processed
//!   template stage carries the value it resolved to and the teardown reads that value.
//! * A property whose value resolves to `{"Ref": "AWS::NoValue"}` is REMOVED from the
//!   template, so the update request does not carry it and the live resource keeps the value
//!   it already has.
//! * The rollback of a failed update gives up a resource that update CREATED as that
//!   resource's own `DeletionPolicy` states, unless the operation asked for
//!   `RetainExceptOnCreate`.
//! * An update action the stack policy in force denies fails that resource and the rollback
//!   then SKIPS it, which is the pair of events that resource records.
//! * An update that leaves old physical resources to take down reports the cleanup phase it
//!   enters before it deletes them.
//! * Every parameter a stack reports carries its EFFECTIVE value, which for a
//!   `CommaDelimitedList` is its space-trimmed members.
//! * A create the owning service refuses because the identifier is already in use is
//!   reported with CloudFormation's own `AlreadyExists` sentence, and the rollback of that
//!   resource reports only the disposal of a record nothing created.

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    deletion_policy, early_validation, resource_status, stack::RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK,
    status,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::settled_stack_status;
use crate::stack_lifecycle::{
    call, create_stack, get_template, member, resource_event_statuses, router, xml_blocks,
    xml_text, REGION,
};
use crate::update_rollback_recovery::{parameter, ssm, template, template_with_an_unsignalled_gate};

/// One event of a stack's log: `(LogicalResourceId, ResourceStatus, ResourceStatusReason,
/// PhysicalResourceId)`.
type Event = (String, String, Option<String>, Option<String>);

/// Every event a stack recorded, OLDEST FIRST — `DescribeStackEvents` reports them "in
/// reverse chronological order", so the log is reversed here to read as the operation ran.
async fn event_log(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<Event> {
    let (code, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} {body}"
    );
    let mut log: Vec<Event> = xml_blocks(&body, "member")
        .iter()
        .filter_map(|entry| {
            Some((
                xml_text(entry, "LogicalResourceId")?,
                xml_text(entry, "ResourceStatus")?,
                xml_text(entry, "ResourceStatusReason"),
                xml_text(entry, "PhysicalResourceId"),
            ))
        })
        .collect();
    log.reverse();
    log
}

/// The `(status, reason)` pairs one resource's events carry, oldest first.
async fn resource_log(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<(String, Option<String>)> {
    event_log(router, case, stack)
        .await
        .into_iter()
        .filter(|(reported, _, _, _)| reported == logical_id)
        .map(|(_, status, reason, _)| (status, reason))
        .collect()
}

/// The same, over the events of the UPDATE alone: the ones recorded from the stack's own
/// `UPDATE_IN_PROGRESS` on, so what a resource the create also acted on reports for THIS
/// operation is read on its own.
async fn updated_resource_log(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<(String, Option<String>)> {
    event_log(router, case, stack)
        .await
        .into_iter()
        .skip_while(|(_, reported, _, _)| reported != status::UPDATE_IN_PROGRESS)
        .filter(|(reported, _, _, _)| reported == logical_id)
        .map(|(_, status, reason, _)| (status, reason))
        .collect()
}

/// The `StackId` a `CreateStack` answered with, which is how a DELETED stack's events and
/// template are still addressable.
fn stack_id_of(case: &str, body: &str) -> String {
    xml_text(body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateStack must answer a StackId, got {body}"))
}

// ---------------------------------------------------------------------------
// A resource attribute written as an intrinsic function
// ---------------------------------------------------------------------------

/// One row: the value the stack's `Mode` parameter carries, whether the template declares the
/// language-extensions transform, and the policy each resource's `Fn::If` then resolves to.
struct AttributeRow {
    case: &'static str,
    stack: &'static str,
    mode: &'static str,
    transform: bool,
    /// What `Kept`'s `{"Fn::If": ["Keep", "Retain", "Delete"]}` resolves to.
    kept: &'static str,
    /// What `Dropped`'s `{"Fn::If": ["Keep", "Delete", "Retain"]}` resolves to.
    dropped: &'static str,
}

/// The class the rule covers: either branch of the condition, with and without the transform
/// — "declaring the transform changes neither acceptance nor the resolved policy".
fn attribute_rows() -> Vec<AttributeRow> {
    vec![
        AttributeRow {
            case: "the condition holds, with no transform declared",
            stack: "ifpolicy-keep-plain",
            mode: "keep",
            transform: false,
            kept: deletion_policy::RETAIN,
            dropped: deletion_policy::DELETE,
        },
        AttributeRow {
            case: "the condition holds, with the language-extensions transform",
            stack: "ifpolicy-keep-transform",
            mode: "keep",
            transform: true,
            kept: deletion_policy::RETAIN,
            dropped: deletion_policy::DELETE,
        },
        AttributeRow {
            case: "the condition does not hold, with no transform declared",
            stack: "ifpolicy-drop-plain",
            mode: "drop",
            transform: false,
            kept: deletion_policy::DELETE,
            dropped: deletion_policy::RETAIN,
        },
        AttributeRow {
            case: "the condition does not hold, with the language-extensions transform",
            stack: "ifpolicy-drop-transform",
            mode: "drop",
            transform: true,
            kept: deletion_policy::DELETE,
            dropped: deletion_policy::RETAIN,
        },
    ]
}

/// The template an attribute row deploys: two parameters whose `DeletionPolicy` is written as
/// an `Fn::If` over a condition the stack's own parameter decides.
fn attribute_template(row: &AttributeRow, kept: &str, dropped: &str) -> String {
    let mut document = json!({
        "Parameters": {"Mode": {"Type": "String", "Default": row.mode}},
        "Conditions": {"Keep": {"Fn::Equals": [{"Ref": "Mode"}, "keep"]}},
        "Resources": {
            "Kept": {
                "Type": "AWS::SSM::Parameter",
                "DeletionPolicy": {"Fn::If": ["Keep", "Retain", "Delete"]},
                "Properties": {"Name": kept, "Type": "String", "Value": "v"},
            },
            "Dropped": {
                "Type": "AWS::SSM::Parameter",
                "DeletionPolicy": {"Fn::If": ["Keep", "Delete", "Retain"]},
                "Properties": {"Name": dropped, "Type": "String", "Value": "v"},
            },
        },
    });
    if row.transform {
        document
            .as_object_mut()
            .expect("a template is a JSON object")
            .insert("Transform".to_string(), json!("AWS::LanguageExtensions"));
    }
    document.to_string()
}

/// The `DeletionPolicy` the PROCESSED stage carries for each resource of `stack`.
async fn processed_policies(router: &MultiServiceRouter, case: &str, stack: &str) -> Value {
    let (code, body) = get_template(router, stack, Some("Processed")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetTemplate(Processed) must answer 200, got {code} {body}"
    );
    let served = xml_text(&body, "TemplateBody").unwrap_or_default();
    let document: Value = serde_json::from_str(&served).unwrap_or_else(|error| {
        panic!("case {case:?}: the processed stage must be a document, got {served:?}: {error}")
    });
    json!({
        "Kept": document.pointer("/Resources/Kept/DeletionPolicy").cloned(),
        "Dropped": document.pointer("/Resources/Dropped/DeletionPolicy").cloned(),
    })
}

/// Whether Parameter Store still holds `name`, with the value it answered.
async fn parameter_still_there(router: &MultiServiceRouter, name: &str) -> (bool, String) {
    let (code, body) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    (code == 200, body.to_string())
}

#[tokio::test]
async fn a_deletion_policy_written_as_an_intrinsic_resolves_and_is_honoured_on_delete() {
    for row in attribute_rows() {
        let router = router();
        let kept_name = format!("/{}/kept", row.stack);
        let dropped_name = format!("/{}/dropped", row.stack);
        let body = attribute_template(&row, &kept_name, &dropped_name);

        let (code, answered) = create_stack(
            &router,
            row.stack,
            &body,
            &if row.transform {
                // "Each macro relies on an underlying Lambda service function for processing
                // stack templates", which is what a template declaring a transform has to be
                // acknowledged for.
                vec![member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND")]
            } else {
                Vec::new()
            },
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let stack_id = stack_id_of(row.case, &answered);
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );

        // The processed stage is the document CloudFormation deploys, so the attribute
        // carries the value it resolved to rather than the expression it was written as.
        let policies = processed_policies(&router, row.case, row.stack).await;
        let expected = json!({"Kept": row.kept, "Dropped": row.dropped});
        assert_eq!(
            policies, expected,
            "case {:?}: expected the processed template to carry {expected}, got {policies}",
            row.case
        );

        let (code, answered) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: DeleteStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, &stack_id).await;
        assert_eq!(
            settled,
            status::DELETE_COMPLETE,
            "case {:?}: expected the deletion to complete, got {settled:?}",
            row.case
        );

        for (logical_id, name, policy) in [
            ("Kept", &kept_name, row.kept),
            ("Dropped", &dropped_name, row.dropped),
        ] {
            let retained = policy == deletion_policy::RETAIN;
            let (there, answered) = parameter_still_there(&router, name).await;
            assert_eq!(
                there, retained,
                "case {:?}: expected {logical_id} (resolved to {policy}) to survive = \
                 {retained}, got {answered}",
                row.case
            );
            let statuses =
                resource_event_statuses(&router, row.case, &stack_id, logical_id).await;
            let expected = if retained {
                resource_status::DELETE_SKIPPED
            } else {
                resource_status::DELETE_COMPLETE
            };
            assert_eq!(
                statuses.first().map(String::as_str),
                Some(expected),
                "case {:?}: expected {logical_id} (resolved to {policy}) to end at \
                 {expected}, got {statuses:?}",
                row.case
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A property that resolves to AWS::NoValue
// ---------------------------------------------------------------------------

/// The `Description` and the `AllowedPattern` the create gives the parameter — the two
/// optional properties `AWS::SSM::Parameter` passes straight through to `PutParameter`.
const DESCRIPTION: &str = "set at create time";
const PATTERN: &str = "^[a-z]+$";

/// One row: how the update spells the property it drops.
struct NoValueRow {
    case: &'static str,
    stack: &'static str,
    /// The value the update declares for both optional properties.
    dropped: Value,
}

/// The class the rule covers: every spelling that RESOLVES to the pseudo parameter.
fn no_value_rows() -> Vec<NoValueRow> {
    vec![
        NoValueRow {
            case: "the pseudo parameter itself",
            stack: "novalue-direct",
            dropped: json!({"Ref": "AWS::NoValue"}),
        },
        NoValueRow {
            case: "a condition that chooses the pseudo parameter",
            stack: "novalue-condition",
            dropped: json!({"Fn::If": ["Never", "stated", {"Ref": "AWS::NoValue"}]}),
        },
        NoValueRow {
            case: "a condition nested inside another",
            stack: "novalue-nested",
            dropped: json!({
                "Fn::If": ["Never", "stated", {"Fn::If": ["Never", "stated", {"Ref": "AWS::NoValue"}]}]
            }),
        },
    ]
}

/// The template the no-value rows deploy: one parameter carrying both optional properties,
/// with `description` and `pattern` for them.
fn no_value_template(name: &str, value: &str, description: &Value, pattern: &Value) -> String {
    json!({
        "Conditions": {"Never": {"Fn::Equals": ["a", "b"]}},
        "Resources": {"Param": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {
                "Name": name,
                "Type": "String",
                "Value": value,
                "Description": description.clone(),
                "AllowedPattern": pattern.clone(),
            },
        }},
    })
    .to_string()
}

/// The `Description` and `AllowedPattern` Parameter Store reports for `name`.
async fn described_parameter(router: &MultiServiceRouter, name: &str) -> (Option<String>, Option<String>) {
    let (code, body) = ssm(router, "DescribeParameters", &json!({})).await;
    assert_eq!(code, 200, "DescribeParameters must answer 200, got {code} {body}");
    let described = body
        .get("Parameters")
        .and_then(Value::as_array)
        .and_then(|parameters| {
            parameters
                .iter()
                .find(|entry| entry.get("Name").and_then(Value::as_str) == Some(name))
                .cloned()
        })
        .unwrap_or(Value::Null);
    (
        described
            .get("Description")
            .and_then(Value::as_str)
            .map(str::to_string),
        described
            .get("AllowedPattern")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

#[tokio::test]
async fn a_property_resolving_to_no_value_leaves_the_live_resource_as_it_was() {
    for row in no_value_rows() {
        let router = router();
        let name = format!("/{}/p", row.stack);
        let created = no_value_template(&name, "before", &json!(DESCRIPTION), &json!(PATTERN));

        let (code, answered) = create_stack(&router, row.stack, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );
        let described = described_parameter(&router, &name).await;
        assert_eq!(
            described,
            (Some(DESCRIPTION.to_string()), Some(PATTERN.to_string())),
            "case {:?}: expected the create to set both properties, got {described:?}",
            row.case
        );

        // The update drops both of them and changes the value, so the owning service is
        // really asked to change the resource.
        let updated = no_value_template(&name, "after", &row.dropped, &row.dropped);
        let (code, answered) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &updated),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: UpdateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::UPDATE_COMPLETE,
            "case {:?}: expected the update to complete, got {settled:?}",
            row.case
        );

        let (code, body) = ssm(&router, "GetParameter", &json!({"Name": name})).await;
        assert_eq!(code, 200, "case {:?}: expected the parameter to still be there, got {body}", row.case);
        assert_eq!(
            body.pointer("/Parameter/Value").and_then(Value::as_str),
            Some("after"),
            "case {:?}: expected the update to write the new value, got {body}",
            row.case
        );
        let described = described_parameter(&router, &name).await;
        assert_eq!(
            described,
            (Some(DESCRIPTION.to_string()), Some(PATTERN.to_string())),
            "case {:?}: expected the live parameter to keep the properties the update \
             omitted, got {described:?}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// The rollback of a failed update, and what it does with what that update created
// ---------------------------------------------------------------------------

/// One row: whether the failed update was asked for `RetainExceptOnCreate`, and what the
/// rollback then leaves of the `DeletionPolicy: Retain` resource it had created.
struct RevertRow {
    case: &'static str,
    stack: &'static str,
    /// `RetainExceptOnCreate` as an EARLIER failed update of the same stack stated it, which
    /// is no part of what this one runs under.
    earlier: Option<&'static str>,
    retain_except_on_create: Option<&'static str>,
    /// Whether the physical resource survives the rollback.
    kept: bool,
    /// The status the resource's last event carries.
    last: &'static str,
}

/// The class the rule covers: the member absent, stated as the documented default, asked
/// for, and absent on an update of a stack whose EARLIER operation asked for it — the member
/// is the operation's, so each operation runs under what it states.
fn revert_rows() -> Vec<RevertRow> {
    vec![
        RevertRow {
            case: "no RetainExceptOnCreate member at all",
            stack: "autorevert-default",
            earlier: None,
            retain_except_on_create: None,
            kept: true,
            last: resource_status::DELETE_SKIPPED,
        },
        RevertRow {
            case: "the documented RetainExceptOnCreate default stated outright",
            stack: "autorevert-false",
            earlier: None,
            retain_except_on_create: Some("false"),
            kept: true,
            last: resource_status::DELETE_SKIPPED,
        },
        RevertRow {
            case: "RetainExceptOnCreate asked for",
            stack: "autorevert-true",
            earlier: None,
            retain_except_on_create: Some("true"),
            kept: false,
            last: resource_status::DELETE_COMPLETE,
        },
        RevertRow {
            case: "no member at all, after an update that asked for it",
            stack: "autorevert-again",
            earlier: Some("true"),
            retain_except_on_create: None,
            kept: true,
            last: resource_status::DELETE_SKIPPED,
        },
    ]
}

#[tokio::test]
async fn the_rollback_of_a_failed_update_honours_the_deletion_policy_of_what_it_created() {
    for row in revert_rows() {
        let router = router();
        let stable_name = format!("/{}/stable", row.stack);
        let added_name = format!("/{}/added", row.stack);
        let created = template(json!({"Stable": parameter(&stable_name, "stable")}));
        let mut added = parameter(&added_name, "added");
        added
            .as_object_mut()
            .expect("a declaration is an object")
            .insert(
                "DeletionPolicy".to_string(),
                json!(deletion_policy::RETAIN),
            );
        let updated = template_with_an_unsignalled_gate(json!({
            "Stable": parameter(&stable_name, "stable"),
            "Added": added,
        }));

        let (code, answered) = create_stack(&router, row.stack, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );

        let mut members = vec![
            member("StackName", row.stack),
            member("TemplateBody", &updated),
        ];
        // An EARLIER failed update of the same stack, under the flag it stated: what it
        // asked for is no part of what the update below runs under.
        if let Some(asked) = row.earlier {
            let mut earlier = members.clone();
            earlier.push(member("RetainExceptOnCreate", asked));
            let (code, answered) = call(&router, "UpdateStack", &earlier).await;
            assert_eq!(
                code, 200,
                "case {:?}: the earlier UpdateStack must be accepted, got {code} {answered}",
                row.case
            );
            let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
            assert_eq!(
                settled,
                status::UPDATE_ROLLBACK_COMPLETE,
                "case {:?}: expected the earlier update to roll back, got {settled:?}",
                row.case
            );
            let (there, answered) = parameter_still_there(&router, &added_name).await;
            assert!(
                !there,
                "case {:?}: expected the earlier rollback to delete what it created, got \
                 {answered}",
                row.case
            );
        }
        if let Some(asked) = row.retain_except_on_create {
            members.push(member("RetainExceptOnCreate", asked));
        }
        let (code, answered) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            code, 200,
            "case {:?}: UpdateStack must be accepted, got {code} {answered}",
            row.case
        );
        // The wait condition nothing signals runs out, which fails the update and rolls it
        // back with no call of the client's.
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::UPDATE_ROLLBACK_COMPLETE,
            "case {:?}: expected the failed update to roll back, got {settled:?}",
            row.case
        );

        let (there, answered) = parameter_still_there(&router, &added_name).await;
        assert_eq!(
            there, row.kept,
            "case {:?}: expected the retained resource the update added to survive = {}, got \
             {answered}",
            row.case, row.kept
        );
        let statuses = resource_event_statuses(&router, row.case, row.stack, "Added").await;
        assert_eq!(
            statuses.first().map(String::as_str),
            Some(row.last),
            "case {:?}: expected the added resource to end at {}, got {statuses:?}",
            row.case,
            row.last
        );
    }
}

// ---------------------------------------------------------------------------
// An update action the stack policy denies
// ---------------------------------------------------------------------------

/// One row: the policy document in force, and the statement its denial names.
struct PolicyRow {
    case: &'static str,
    stack: &'static str,
    policy: Value,
    /// The position the denial names, counted from one as a reader of the document counts.
    statement: usize,
    /// The patterns that statement forbids, as the reason spells them.
    denies: &'static str,
    protects: &'static str,
}

/// The class the rule covers: the denying statement at either position of a document that
/// has more than one, so the numbering is asserted rather than fitted.
fn policy_rows() -> Vec<PolicyRow> {
    vec![
        PolicyRow {
            case: "a document whose only statement denies",
            stack: "denied-first",
            policy: json!({"Statement": [
                {"Effect": "Deny", "Action": "Update:Modify", "Principal": "*",
                 "Resource": "LogicalResourceId/Protected"},
            ]}),
            statement: 1,
            denies: "Update:Modify",
            protects: "LogicalResourceId/Protected",
        },
        PolicyRow {
            case: "a document whose second statement denies",
            stack: "denied-second",
            policy: json!({"Statement": [
                {"Effect": "Allow", "Action": "Update:*", "Principal": "*", "Resource": "*"},
                {"Effect": "Deny", "Action": "Update:*", "Principal": "*",
                 "Resource": "LogicalResourceId/Protected"},
            ]}),
            statement: 2,
            denies: "Update:*",
            protects: "LogicalResourceId/Protected",
        },
    ]
}

#[tokio::test]
async fn a_denied_update_fails_the_resource_and_the_rollback_skips_it() {
    for row in policy_rows() {
        let router = router();
        let name = format!("/{}/p", row.stack);
        let created = template(json!({"Protected": parameter(&name, "before")}));
        let updated = template(json!({"Protected": parameter(&name, "after")}));

        let (code, answered) = create_stack(
            &router,
            row.stack,
            &created,
            &[member("StackPolicyBody", &row.policy.to_string())],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );

        let (code, answered) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &updated),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: UpdateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::UPDATE_ROLLBACK_COMPLETE,
            "case {:?}: expected the denied update to roll back, got {settled:?}",
            row.case
        );

        let expected = vec![
            (
                resource_status::UPDATE_FAILED.to_string(),
                Some(format!(
                    "Action denied by stack policy: Statement [#{}] does not allow [{}] for \
                     resource [{}];",
                    row.statement, row.denies, row.protects
                )),
            ),
            (
                resource_status::UPDATE_COMPLETE.to_string(),
                Some(RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK.to_string()),
            ),
        ];
        let reported = updated_resource_log(&router, row.case, row.stack, "Protected").await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the denied resource's events to be {expected:?}, got \
             {reported:?}",
            row.case
        );

        // Nothing was deployed, so the live value is the one the create wrote.
        let (code, body) = ssm(&router, "GetParameter", &json!({"Name": name})).await;
        assert_eq!(code, 200, "case {:?}: expected the parameter to be there, got {body}", row.case);
        assert_eq!(
            body.pointer("/Parameter/Value").and_then(Value::as_str),
            Some("before"),
            "case {:?}: expected the denied update to leave the value alone, got {body}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// A replacing update and its cleanup phase
// ---------------------------------------------------------------------------

/// One row: the create-only property the update changes, so the resource is REPLACED.
struct ReplacementRow {
    case: &'static str,
    stack: &'static str,
    before: &'static str,
    after: &'static str,
}

/// The class the rule covers: any update that replaces a resource, whichever name the
/// declaration moves between.
fn replacement_rows() -> Vec<ReplacementRow> {
    vec![
        ReplacementRow {
            case: "a renamed parameter",
            stack: "replaced-renamed",
            before: "/replaced-renamed/before",
            after: "/replaced-renamed/after",
        },
        ReplacementRow {
            case: "a parameter renamed into another hierarchy",
            stack: "replaced-moved",
            before: "/replaced-moved/one/p",
            after: "/replaced-moved/two/p",
        },
    ]
}

#[tokio::test]
async fn a_replacing_update_reports_its_cleanup_phase_and_deletes_the_old_resource() {
    for row in replacement_rows() {
        let router = router();
        let created = template(json!({"Param": parameter(row.before, "v")}));
        let updated = template(json!({"Param": parameter(row.after, "v")}));

        let (code, answered) = create_stack(&router, row.stack, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );

        let (code, answered) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &updated),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: UpdateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::UPDATE_COMPLETE,
            "case {:?}: expected the replacing update to complete, got {settled:?}",
            row.case
        );

        let log: Vec<(String, String, Option<String>)> =
            event_log(&router, row.case, row.stack)
                .await
                .into_iter()
                // The create's own events are not what this row is about: the update opens
                // with the stack's own `UPDATE_IN_PROGRESS`.
                .skip_while(|(_, reported, _, _)| reported != status::UPDATE_IN_PROGRESS)
                .map(|(logical_id, reported, reason, _)| (logical_id, reported, reason))
                .collect();
        let expected: Vec<(String, String, Option<String>)> = vec![
            (
                row.stack.to_string(),
                status::UPDATE_IN_PROGRESS.to_string(),
                Some("User Initiated".to_string()),
            ),
            (
                "Param".to_string(),
                resource_status::UPDATE_IN_PROGRESS.to_string(),
                Some(
                    "Requested update requires the creation of a new physical resource; hence \
                     creating one."
                        .to_string(),
                ),
            ),
            (
                "Param".to_string(),
                resource_status::UPDATE_IN_PROGRESS.to_string(),
                Some("Resource creation Initiated".to_string()),
            ),
            (
                "Param".to_string(),
                resource_status::UPDATE_COMPLETE.to_string(),
                None,
            ),
            (
                row.stack.to_string(),
                status::UPDATE_COMPLETE_CLEANUP_IN_PROGRESS.to_string(),
                None,
            ),
            (
                "Param".to_string(),
                resource_status::DELETE_IN_PROGRESS.to_string(),
                None,
            ),
            (
                "Param".to_string(),
                resource_status::DELETE_COMPLETE.to_string(),
                None,
            ),
            (
                row.stack.to_string(),
                status::UPDATE_COMPLETE.to_string(),
                None,
            ),
        ];
        assert_eq!(
            log, expected,
            "case {:?}: expected the replacing update's events to be {expected:?}, got {log:?}",
            row.case
        );

        // The identifiers the phases of the replacement report: the NEW physical resource
        // once it is created, and the OLD one as the cleanup takes it down.
        let identified: Vec<(String, Option<String>)> = event_log(&router, row.case, row.stack)
            .await
            .into_iter()
            .filter(|(logical_id, _, _, _)| logical_id == "Param")
            .map(|(_, reported, _, physical)| (reported, physical))
            .filter(|(reported, _)| {
                [
                    resource_status::UPDATE_COMPLETE,
                    resource_status::DELETE_IN_PROGRESS,
                    resource_status::DELETE_COMPLETE,
                ]
                .contains(&reported.as_str())
            })
            .collect();
        let expected: Vec<(String, Option<String>)> = vec![
            (
                resource_status::UPDATE_COMPLETE.to_string(),
                Some(row.after.to_string()),
            ),
            (
                resource_status::DELETE_IN_PROGRESS.to_string(),
                Some(row.before.to_string()),
            ),
            (
                resource_status::DELETE_COMPLETE.to_string(),
                Some(row.before.to_string()),
            ),
        ];
        assert_eq!(
            identified, expected,
            "case {:?}: expected each phase to name {expected:?}, got {identified:?}",
            row.case
        );

        let (there, answered) = parameter_still_there(&router, row.before).await;
        assert!(
            !there,
            "case {:?}: expected the cleanup to delete the old physical resource, got \
             {answered}",
            row.case
        );
        let (there, answered) = parameter_still_there(&router, row.after).await;
        assert!(
            there,
            "case {:?}: expected the new physical resource to be there, got {answered}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// The effective value a stack reports for each parameter it declares
// ---------------------------------------------------------------------------

/// One row: a `CommaDelimitedList` value as the request spells it, and the EFFECTIVE value
/// the stack reports for it.
///
/// Source: the `CommaDelimitedList` parameter type — "an array of literal strings that are
/// separated by commas […] each member string is space trimmed".
struct ListRow {
    case: &'static str,
    stack: &'static str,
    supplied: &'static str,
    reported: &'static str,
    /// `Fn::Join`ing the list back together, which is what an output built from it carries.
    joined: &'static str,
}

fn list_rows() -> Vec<ListRow> {
    vec![
        ListRow {
            case: "members already trimmed",
            stack: "cdl-trimmed",
            supplied: "x,y,z",
            reported: "x,y,z",
            joined: "x|y|z",
        },
        ListRow {
            case: "space around the commas",
            stack: "cdl-spaced",
            supplied: "x, y ,z",
            reported: "x,y,z",
            joined: "x|y|z",
        },
        ListRow {
            case: "space around the whole value and one member",
            stack: "cdl-padded",
            supplied: " one ,  two,three ",
            reported: "one,two,three",
            joined: "one|two|three",
        },
        ListRow {
            case: "one member only",
            stack: "cdl-single",
            supplied: "  only  ",
            reported: "only",
            joined: "only",
        },
    ]
}

/// The template the list rows deploy: a `CommaDelimitedList` parameter, with outputs built
/// over it with `Fn::Select` and `Fn::Join`.
fn list_template() -> String {
    json!({
        "Parameters": {"Extra": {"Type": "CommaDelimitedList"}},
        "Resources": {"Param": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {
                "Type": "String",
                "Value": {"Fn::Select": ["0", {"Ref": "Extra"}]},
            },
        }},
        "Outputs": {
            "First": {"Value": {"Fn::Select": ["0", {"Ref": "Extra"}]}},
            "Joined": {"Value": {"Fn::Join": ["|", {"Ref": "Extra"}]}},
        },
    })
    .to_string()
}

/// Every `(ParameterKey, ParameterValue)` a `DescribeStacks` body reports.
fn reported_parameters(body: &str) -> Vec<(String, Option<String>)> {
    xml_blocks(body, "member")
        .iter()
        .filter_map(|entry| {
            Some((
                xml_text(entry, "ParameterKey")?,
                xml_text(entry, "ParameterValue"),
            ))
        })
        .collect()
}

/// Every `(OutputKey, OutputValue)` a `DescribeStacks` body reports.
fn reported_outputs(body: &str) -> Vec<(String, Option<String>)> {
    xml_blocks(body, "member")
        .iter()
        .filter_map(|entry| Some((xml_text(entry, "OutputKey")?, xml_text(entry, "OutputValue"))))
        .collect()
}

#[tokio::test]
async fn a_comma_delimited_list_parameter_is_reported_and_resolved_space_trimmed() {
    for row in list_rows() {
        let router = router();
        let (code, answered) = create_stack(
            &router,
            row.stack,
            &list_template(),
            &[
                member("Parameters.member.1.ParameterKey", "Extra"),
                member("Parameters.member.1.ParameterValue", row.supplied),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {settled:?}",
            row.case
        );

        let (code, body) = call(&router, "DescribeStacks", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: DescribeStacks must answer 200, got {code} {body}",
            row.case
        );
        let parameters = reported_parameters(&body);
        let expected = vec![("Extra".to_string(), Some(row.reported.to_string()))];
        assert_eq!(
            parameters, expected,
            "case {:?}: expected the stack to report {expected:?}, got {parameters:?}",
            row.case
        );
        let outputs = reported_outputs(&body);
        let first = row
            .reported
            .split(',')
            .next()
            .expect("a list has a first member")
            .to_string();
        let expected = vec![
            ("First".to_string(), Some(first)),
            ("Joined".to_string(), Some(row.joined.to_string())),
        ];
        assert_eq!(
            outputs, expected,
            "case {:?}: expected the stack to report {expected:?}, got {outputs:?}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// A create whose identifier the owning service already holds
// ---------------------------------------------------------------------------

/// One row: whether the create turned the pre-deployment validations off, and the events the
/// conflicting resource then records.
struct ConflictRow {
    case: &'static str,
    stack: &'static str,
    disable_validation: bool,
    /// The resource statuses the conflicting logical id records, oldest first.
    statuses: Vec<&'static str>,
}

/// The class the rule covers: the validations on (the documented default) and off.
fn conflict_rows() -> Vec<ConflictRow> {
    vec![
        ConflictRow {
            case: "with the pre-deployment validations in force",
            stack: "conflict-validated",
            disable_validation: false,
            statuses: Vec::new(),
        },
        ConflictRow {
            case: "with the pre-deployment validations disabled",
            stack: "conflict-unvalidated",
            disable_validation: true,
            statuses: vec![
                resource_status::CREATE_IN_PROGRESS,
                resource_status::CREATE_FAILED,
                resource_status::DELETE_COMPLETE,
            ],
        },
    ]
}

#[tokio::test]
async fn a_create_under_a_taken_identifier_is_refused_in_cloudformations_own_words() {
    for row in conflict_rows() {
        let router = router();
        let name = format!("/{}/taken", row.stack);
        // The parameter a client created OUT OF BAND, which no record of CloudFormation's
        // own knows about.
        let (code, body) = ssm(
            &router,
            "PutParameter",
            &json!({"Name": name, "Type": "String", "Value": "out of band"}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: expected the out-of-band put to succeed, got {code} {body}",
            row.case
        );

        let body = template(json!({"Param": parameter(&name, "from the stack")}));
        let mut members = vec![];
        if row.disable_validation {
            members.push(member("DisableValidation", "true"));
        }
        let (code, answered) = create_stack(&router, row.stack, &body, &members).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {answered}",
            row.case
        );
        let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {:?}: expected the conflicting create to roll back, got {settled:?}",
            row.case
        );

        let statuses: Vec<String> = resource_log(&router, row.case, row.stack, "Param")
            .await
            .into_iter()
            .map(|(status, _)| status)
            .collect();
        assert_eq!(
            statuses,
            row.statuses,
            "case {:?}: expected the conflicting resource's statuses to be {:?}, got \
             {statuses:?}",
            row.case,
            row.statuses
        );

        if row.disable_validation {
            let reason = resource_log(&router, row.case, row.stack, "Param")
                .await
                .into_iter()
                .find(|(status, _)| status == resource_status::CREATE_FAILED)
                .and_then(|(_, reason)| reason)
                .unwrap_or_default();
            let sentence = early_validation::already_exists("AWS::SSM::Parameter", &name);
            assert!(
                reason.contains(&format!("Resource handler returned message: \"{sentence}\""))
                    && reason.contains("HandlerErrorCode: AlreadyExists"),
                "case {:?}: expected the handler's own AlreadyExists report for \
                 {sentence:?}, got {reason:?}",
                row.case
            );
        }

        // Neither create may touch the resource the client already had.
        let (code, body) = ssm(&router, "GetParameter", &json!({"Name": name})).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected the out-of-band parameter to survive, got {code} {body}",
            row.case
        );
        assert_eq!(
            body.pointer("/Parameter/Value").and_then(Value::as_str),
            Some("out of band"),
            "case {:?}: expected the out-of-band value to be untouched, got {body}",
            row.case
        );
    }
}
