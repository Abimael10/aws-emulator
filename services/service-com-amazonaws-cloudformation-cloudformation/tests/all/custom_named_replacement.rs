// SPDX-License-Identifier: CC0-1.0

//! The REPLACEMENT an update cannot carry out, end to end over the awsQuery wire with the
//! services that own the resources mounted beside CloudFormation.
//!
//! RULE under test: **an update that requires REPLACING a resource cannot create the
//! replacement under the identity the resource it replaces still holds** — so an update that
//! requires replacing a resource whose physical identity IS the name the template states,
//! under the name that resource already has, is refused rather than attempted, whatever made
//! the replacement necessary. The refused resource reports exactly one `UPDATE_FAILED` event
//! carrying CloudFormation's own two sentences (what cannot be done, and the rename that would
//! let it be done), the stack rolls back to `UPDATE_ROLLBACK_COMPLETE`, and nothing is asked of
//! the owning service — so the live resource is exactly as it was.
//!
//! The rows differ along what the rule generalises over:
//!
//! * WHICH SERVICE owns the resource and which property the type takes its name from — an
//!   Amazon Cognito user-pool group named by `GroupName`, an Amazon S3 bucket named by
//!   `BucketName`, an Amazon CloudWatch Logs stream named by `LogStreamName`;
//! * WHY the replacement is required, which never decides the refusal — the name property
//!   itself written down for the first time, where CloudFormation had generated the name, and
//!   another property the resource cannot take a new value for (the log group a stream lives
//!   in) changed while the name stays;
//! * and the ways a replacement DOES have an identity of its own to be created under, which
//!   are the rows that must go through: a resource whose identity its owning service mints (a
//!   named user pool, replaced for a changed create-only property), one the update RENAMES —
//!   which is the rename the refusal asks for — and an ordinary change the owning service
//!   applies in place to a resource whose name the template leaves to CloudFormation.
//!
//! The second test is the round trip the refusal exists for: the group is read back through
//! Amazon Cognito's own wire before and after the refused update — same `CreationDate`, same
//! configuration, still the only group in the pool — and the ordinary update that follows is
//! applied in place. The third drives the same refusal through `ExecuteChangeSet`, which is
//! held to the plan its change set stored.

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::REGION;
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, resource_event_reason, resource_event_statuses,
    router, settled_stack, xml_text,
};

/// The awsJson version Amazon Cognito user pools declares (`awsJson1_1`), with its signing
/// name and `X-Amz-Target` prefix (`smithy_query(query="service_info")`).
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";
const COGNITO_IDP: (&str, &str) = ("cognito-idp", "AWSCognitoIdentityProviderService");

/// Whether the row's update is refused or carried out.
#[derive(Debug, PartialEq, Eq)]
enum Expected {
    /// CloudFormation cannot perform the replacement: the resource fails and the stack rolls
    /// back.
    Refused,
    /// The update goes through, whether as a replacement under a new name or in place.
    Performed,
}

/// One row: a stack deployed from `created`, then updated with the template `updated` builds
/// from the name the resource is in force under.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The logical id of the resource the update acts on.
    logical_id: &'static str,
    /// The template the stack is created from.
    created: fn(&str) -> String,
    /// The template the update submits, given the NAME the resource holds.
    updated: fn(&str, &str) -> String,
    /// The name the resource is in force under, from the value the stack publishes for it:
    /// the published name itself for a type whose identity IS its name, and the name the
    /// template declared for one whose identity the owning service minted.
    name_in_force: fn(&str, &str) -> String,
    /// The output key the stack publishes that value under.
    published: &'static str,
    expected: Expected,
}

/// The user pool every Cognito row declares the resource in, and the group declared in it.
fn pool_and_group(stack: &str, group: Option<&str>, description: &str) -> String {
    let mut properties = json!({
        "UserPoolId": {"Ref": "Pool"},
        "Description": description,
        "Precedence": 1,
    });
    if let Some(name) = group {
        properties["GroupName"] = Value::String(name.to_string());
    }
    json!({
        "Resources": {
            "Pool": {
                "Type": "AWS::Cognito::UserPool",
                "Properties": {"UserPoolName": format!("{stack}-pool")},
            },
            "Group": {
                "Type": "AWS::Cognito::UserPoolGroup",
                "Properties": properties,
            },
        },
        "Outputs": {
            "GroupRef": {"Value": {"Ref": "Group"}},
            "PoolRef": {"Value": {"Ref": "Pool"}},
        },
    })
    .to_string()
}

/// The pool declared on its own, with the alias attributes a row changes — a create-only
/// property of the type that is NOT the name.
fn pool_with_alias(stack: &str, alias: &str) -> String {
    json!({
        "Resources": {
            "Pool": {
                "Type": "AWS::Cognito::UserPool",
                "Properties": {
                    "UserPoolName": format!("{stack}-pool"),
                    "AliasAttributes": [alias],
                },
            },
        },
        "Outputs": {"PoolRef": {"Value": {"Ref": "Pool"}}},
    })
    .to_string()
}

/// A bucket, named or left for CloudFormation to name.
fn bucket(name: Option<&str>) -> String {
    let properties = match name {
        Some(name) => json!({"BucketName": name}),
        None => json!({}),
    };
    json!({
        "Resources": {"Bucket": {"Type": "AWS::S3::Bucket", "Properties": properties}},
        "Outputs": {"BucketRef": {"Value": {"Ref": "Bucket"}}},
    })
    .to_string()
}

/// A topic under the name the template states.
fn topic(name: &str) -> String {
    json!({
        "Resources": {
            "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": name}},
        },
        "Outputs": {"TopicRef": {"Value": {"Ref": "Topic"}}},
    })
    .to_string()
}

/// Two log groups and one NAMED stream in the group `on_group` names — the scope a row moves
/// the stream to, which is another property whose change recreates it.
fn stream_in(stack: &str, on_group: &str) -> String {
    json!({
        "Resources": {
            "First": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": format!("{stack}-first")},
            },
            "Second": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": format!("{stack}-second")},
            },
            "Stream": {
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    "LogStreamName": format!("{stack}-stream"),
                    "LogGroupName": {"Ref": on_group},
                },
            },
        },
        "Outputs": {"StreamRef": {"Value": {"Ref": "Stream"}}},
    })
    .to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The reported case: the name CloudFormation generated, declared back to it.
            case: "a user-pool group whose generated GroupName the update declares",
            stack: "custom-named-group",
            logical_id: "Group",
            created: |stack| pool_and_group(stack, None, "d1"),
            updated: |stack, name| pool_and_group(stack, Some(name), "d2"),
            name_in_force: |_, published| published.to_string(),
            published: "GroupRef",
            expected: Expected::Refused,
        },
        Row {
            // The same rule on another service, whose type takes its name from another
            // property.
            case: "a bucket whose generated BucketName the update declares",
            stack: "custom-named-bucket",
            logical_id: "Bucket",
            created: |_| bucket(None),
            updated: |_, name| bucket(Some(name)),
            name_in_force: |_, published| published.to_string(),
            published: "BucketRef",
            expected: Expected::Refused,
        },
        Row {
            // A resource whose identity its OWNING SERVICE assigns: the pool id is what
            // identifies it, so the replacement its changed create-only alias attributes
            // require is created under an id of its own and the declared name collides with
            // nothing.
            case: "a named user pool whose create-only alias attributes the update changes",
            stack: "custom-named-pool",
            logical_id: "Pool",
            created: |stack| pool_with_alias(stack, "email"),
            updated: |stack, _| pool_with_alias(stack, "preferred_username"),
            name_in_force: |stack, _| format!("{stack}-pool"),
            published: "PoolRef",
            expected: Expected::Performed,
        },
        Row {
            // The replacement is required by the SCOPE the resource lives in rather than by
            // its name — and the replacement would still have to be created under the name
            // the stream already holds, so it is refused all the same: WHY a replacement is
            // required never decides this.
            case: "a named log stream the update moves to another log group",
            stack: "custom-named-stream",
            logical_id: "Stream",
            created: |stack| stream_in(stack, "First"),
            updated: |stack, _| stream_in(stack, "Second"),
            name_in_force: |_, published| published.to_string(),
            published: "StreamRef",
            expected: Expected::Refused,
        },
        Row {
            // The rename the refusal asks for: the replacement has a name of its own, so it
            // is carried out.
            case: "a named topic the update renames",
            stack: "custom-named-topic",
            logical_id: "Topic",
            created: |stack| topic(&format!("{stack}-topic")),
            updated: |stack, _| topic(&format!("{stack}-renamed")),
            name_in_force: |stack, _| format!("{stack}-topic"),
            published: "TopicRef",
            expected: Expected::Performed,
        },
        Row {
            // The ordinary change: nothing names the resource, so nothing is replaced.
            case: "a user-pool group the template does not name, changed in place",
            stack: "generated-named-group",
            logical_id: "Group",
            created: |stack| pool_and_group(stack, None, "d1"),
            updated: |stack, _| pool_and_group(stack, None, "d2"),
            name_in_force: |_, published| published.to_string(),
            published: "GroupRef",
            expected: Expected::Performed,
        },
    ]
}

/// The one sentence pair a refused replacement reports, around the name of the resource that
/// has to be renamed.
fn refusal_reason(name: &str) -> String {
    format!(
        "CloudFormation cannot update a stack when a custom-named resource requires replacing. \
         Rename {name} and update the stack again."
    )
}

/// One awsJson call to Amazon Cognito user pools over the same router, signed the way a
/// client's SDK signs it: the HTTP status and the parsed answer.
async fn cognito_idp(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let (signing_name, target_prefix) = COGNITO_IDP;
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", AWS_JSON_1_1)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(http_request).await;
    let status_code = response.status().as_u16();
    let body = response.into_body().to_vec();
    let document: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status_code, document)
}

/// Deploy a template and assert the create completed, answering the settled
/// `DescribeStacks` body the stack's outputs are read from.
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (http_status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {http_status} {body}"
    );
    let described = settled_stack(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the create to complete, got {described}"
    );
    described
}

/// `UpdateStack` with a template body, asserted to be ACCEPTED: the verdict is reached on the
/// stack's own timeline, so a refused update is answered 200 all the same.
async fn update_stack(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) {
    let (http_status, answered) = call(
        router,
        "UpdateStack",
        &[member("StackName", stack), member("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {http_status} {answered}"
    );
}

/// The physical id the stack reports for one of its resources.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (http_status, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStackResource({logical_id}) to succeed, got \
         {http_status} {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

#[tokio::test]
async fn an_update_that_would_replace_a_custom_named_resource_is_refused_and_rolled_back() {
    let router = router();

    for row in rows() {
        let case = row.case;
        let described = deployed(&router, case, row.stack, &(row.created)(row.stack)).await;
        let published = output_value(&described, row.published).unwrap_or_default();
        assert!(
            !published.is_empty(),
            "case {case:?}: expected the stack to publish {}, got {described}",
            row.published
        );
        let name = (row.name_in_force)(row.stack, &published);
        let before = physical_id(&router, case, row.stack, row.logical_id).await;

        update_stack(&router, case, row.stack, &(row.updated)(row.stack, &name)).await;
        let settled = settled_stack(&router, case, row.stack).await;
        let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
        let after = physical_id(&router, case, row.stack, row.logical_id).await;
        let failures = resource_event_statuses(&router, case, row.stack, row.logical_id)
            .await
            .into_iter()
            .filter(|recorded| recorded == "UPDATE_FAILED")
            .count();

        match row.expected {
            Expected::Refused => {
                assert_eq!(
                    reported,
                    status::UPDATE_ROLLBACK_COMPLETE,
                    "case {case:?}: expected the refused update to roll the stack back, got \
                     {reported:?}"
                );
                assert_eq!(
                    failures, 1,
                    "case {case:?}: expected exactly one UPDATE_FAILED event for \
                     {}, got {failures}",
                    row.logical_id
                );
                let recorded =
                    resource_event_reason(&router, row.stack, row.logical_id, "UPDATE_FAILED")
                        .await;
                assert_eq!(
                    recorded,
                    refusal_reason(&name),
                    "case {case:?}: expected the refusal to name the resource to rename, got \
                     {recorded:?}"
                );
                assert_eq!(
                    after, before,
                    "case {case:?}: expected the resource to keep the identity it had, got \
                     {after:?}"
                );
            }
            Expected::Performed => {
                assert_eq!(
                    reported,
                    status::UPDATE_COMPLETE,
                    "case {case:?}: expected the update to be carried out, got {reported:?}; \
                     the resource reported {:?}",
                    resource_event_reason(&router, row.stack, row.logical_id, "UPDATE_FAILED")
                        .await
                );
                assert_eq!(
                    failures, 0,
                    "case {case:?}: expected no UPDATE_FAILED event for {}, got {failures}",
                    row.logical_id
                );
            }
        }
    }
}

/// Poll `DescribeChangeSet` until the change set reports a status its computation is no
/// longer in flight in, and answer that status.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (http_status, body) = call(
            router,
            "DescribeChangeSet",
            &[
                member("StackName", stack),
                member("ChangeSetName", change_set),
            ],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected DescribeChangeSet to succeed, got {http_status} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if !reported.is_empty()
            && !reported.ends_with("_PENDING")
            && !reported.ends_with("_IN_PROGRESS")
        {
            return reported;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the change set to settle, saw {seen:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Executing a change set is held to the same rule, and from the plan the change set stored:
/// the resource the plan would replace reports the refusal and the stack rolls back.
#[tokio::test]
async fn executing_a_change_set_that_would_replace_a_custom_named_resource_is_refused() {
    let router = router();
    let case = "an UPDATE change set declaring the group's generated name";
    let stack = "custom-named-group-change-set";
    let change_set = "declare-the-generated-name";

    let described = deployed(&router, case, stack, &pool_and_group(stack, None, "d1")).await;
    let group_name = output_value(&described, "GroupRef").unwrap_or_default();
    let pool = output_value(&described, "PoolRef").unwrap_or_default();

    let (http_status, body) = call(
        &router,
        "CreateChangeSet",
        &[
            member("StackName", stack),
            member("ChangeSetName", change_set),
            member(
                "TemplateBody",
                &pool_and_group(stack, Some(&group_name), "d2"),
            ),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateChangeSet to succeed, got {http_status} {body}"
    );
    let reported = settled_change_set(&router, case, stack, change_set).await;
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the change set to be computed, got {reported:?}"
    );

    let (http_status, body) = call(
        &router,
        "ExecuteChangeSet",
        &[
            member("StackName", stack),
            member("ChangeSetName", change_set),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected ExecuteChangeSet to be accepted, got {http_status} {body}"
    );
    let settled = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused execution to roll the stack back, got {settled}"
    );
    let recorded = resource_event_reason(&router, stack, "Group", "UPDATE_FAILED").await;
    assert_eq!(
        recorded,
        refusal_reason(&group_name),
        "case {case:?}: expected the refusal to name the resource to rename, got {recorded:?}"
    );
    // Nothing of the change set was applied, so the live group is the one the stack deployed.
    let held = groups_in(&router, case, &pool).await;
    assert_eq!(
        held,
        vec![group_name.clone()],
        "case {case:?}: expected the pool to hold exactly the one group, got {held:?}"
    );
    let live = group_in(&router, case, &pool, &group_name).await;
    assert_eq!(
        live["Description"].as_str(),
        Some("d1"),
        "case {case:?}: expected the live group to keep the configuration in force, got {live}"
    );
}

async fn group_in(router: &MultiServiceRouter, case: &str, pool: &str, name: &str) -> Value {
    let (http_status, answer) = cognito_idp(
        router,
        "GetGroup",
        &json!({"UserPoolId": pool, "GroupName": name}),
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected GetGroup({pool}, {name}) to be served, got {http_status} \
         {answer}"
    );
    answer["Group"].clone()
}

/// The names of every group Amazon Cognito holds for a pool.
async fn groups_in(router: &MultiServiceRouter, case: &str, pool: &str) -> Vec<String> {
    let (http_status, answer) =
        cognito_idp(router, "ListGroups", &json!({"UserPoolId": pool})).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected ListGroups({pool}) to be served, got {http_status} {answer}"
    );
    answer["Groups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group["GroupName"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_refused_replacement_leaves_the_live_group_untouched_and_a_later_update_applies() {
    let router = router();
    let case = "the refused replacement of a generated-name group";
    let stack = "custom-named-group-round-trip";

    let described = deployed(&router, case, stack, &pool_and_group(stack, None, "d1")).await;
    let group_name = output_value(&described, "GroupRef").unwrap_or_default();
    let pool = output_value(&described, "PoolRef").unwrap_or_default();
    let before = group_in(&router, case, &pool, &group_name).await;
    assert_eq!(
        before["Description"].as_str(),
        Some("d1"),
        "case {case:?}: expected the deployed group to carry the declared description, got \
         {before}"
    );

    // The update the refusal is about: the generated name declared back, and the description
    // changed beside it.
    update_stack(
        &router,
        case,
        stack,
        &pool_and_group(stack, Some(&group_name), "d2"),
    )
    .await;
    let settled = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused update to roll the stack back, got {settled}; the \
         group reported {:?}",
        resource_event_reason(&router, stack, "Group", "UPDATE_FAILED").await
    );

    // Nothing was asked of Amazon Cognito: the live group is the one that was there, with the
    // instant it was created at and the configuration it was created with, and it is still the
    // only group in the pool.
    let after = group_in(&router, case, &pool, &group_name).await;
    assert_eq!(
        after["CreationDate"], before["CreationDate"],
        "case {case:?}: expected the live group to keep its CreationDate, got {after}"
    );
    assert_eq!(
        after["Description"].as_str(),
        Some("d1"),
        "case {case:?}: expected the live group to keep the configuration in force, got {after}"
    );
    let held = groups_in(&router, case, &pool).await;
    assert_eq!(
        held,
        vec![group_name.clone()],
        "case {case:?}: expected the pool to hold exactly the one group, got {held:?}"
    );

    // And the ordinary update that follows — the same change with no name declared — is
    // applied in place.
    update_stack(&router, case, stack, &pool_and_group(stack, None, "d2")).await;
    let settled = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "case {case:?}: expected the ordinary update to complete, got {settled}; the group \
         reported {:?}",
        resource_event_reason(&router, stack, "Group", "UPDATE_FAILED").await
    );
    let changed = group_in(&router, case, &pool, &group_name).await;
    assert_eq!(
        changed["Description"].as_str(),
        Some("d2"),
        "case {case:?}: expected the in-place update to change the live description, got \
         {changed}"
    );
    assert_eq!(
        changed["CreationDate"], before["CreationDate"],
        "case {case:?}: expected the in-place update to keep the group's CreationDate, got \
         {changed}"
    );
}
