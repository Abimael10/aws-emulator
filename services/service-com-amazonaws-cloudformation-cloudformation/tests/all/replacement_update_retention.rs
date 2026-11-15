// SPDX-License-Identifier: CC0-1.0

//! What an update that REPLACES a resource reports, and what becomes of the resource it
//! replaced, end to end over the awsQuery wire.
//!
//! RULE under test: **an update that asks a resource for a value one of its create-only
//! properties cannot take REPLACES that resource inside the update, so every event the stack
//! records for the logical id belongs to the UPDATE family — `UPDATE_IN_PROGRESS` announcing
//! that a new physical resource is being created, `UPDATE_IN_PROGRESS` reporting that create
//! initiated, then `UPDATE_COMPLETE` carrying the NEW physical id — and what becomes of the
//! OLD physical resource in the update's cleanup phase is that resource's own
//! `UpdateReplacePolicy`: `Retain` asks nothing of the owning service and records one
//! `DELETE_SKIPPED` against the old physical id, while any other value deletes the old
//! resource and records `DELETE_IN_PROGRESS` then `DELETE_COMPLETE` against it.**
//!
//! Without it a client that follows `DescribeStackEvents` to decide what an update did cannot
//! tell a replacement from a first create, and a resource declared with `UpdateReplacePolicy`
//! `Retain` — whose whole point is to outlive the replacement — is destroyed while the event
//! stream reports a successful delete rather than a skip.
//!
//! The rows differ along what the rule generalises over: the `UpdateReplacePolicy` value
//! (absent, and each of the three the attribute takes, `state::deletion_policy` carrying them
//! with the sentence each is published under) and the resource type whose create-only property
//! the update changes (`AWS::SSM::Parameter`'s `Name`, and `AWS::S3::Bucket`'s `BucketName` on a
//! second type and in a second owning service). Renaming is only HOW each row forces the
//! replacement; what each row turns on is the policy and the type.
//!
//! Every assertion is a client's: the stack is read through `DescribeStackEvents` and
//! `DescribeStackResources`, and the resources through the wire of the service that owns them —
//! AWS Systems Manager's `GetParameter` and Amazon S3's `HEAD /<bucket>` — so a resource
//! reported as retained is one that service really still holds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    deletion_policy, resource_status, status, RESOURCE_CREATION_INITIATED,
    RESOURCE_REPLACEMENT_IN_PROGRESS,
};

use crate::dynamic_references::ssm;
use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};
use crate::stack_resource_provisioning::s3;

/// The logical id the replaced resource is declared under in every revision below.
const LOGICAL_ID: &str = "Store";

/// The value the parameter rows declare, which is what a retained parameter must still answer
/// with after the update that replaced it.
const PARAMETER_VALUE: &str = "the value the replaced parameter keeps";

/// The type whose create-only property a row's update changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `AWS::SSM::Parameter`: `Name` is create-only, and it is also the resource's physical id
    /// (`Ref` of the type "returns the parameter name"), so a declaration naming another one
    /// asks for a different parameter.
    Parameter,
    /// `AWS::S3::Bucket`: `BucketName` is create-only the same way, on a second type owned by a
    /// second service.
    Bucket,
}

impl Kind {
    /// The template type name.
    fn resource_type(self) -> &'static str {
        match self {
            Self::Parameter => "AWS::SSM::Parameter",
            Self::Bucket => "AWS::S3::Bucket",
        }
    }

    /// One revision's template: the resource under the physical name `name`, carrying `policy`
    /// as its `UpdateReplacePolicy` when the row declares one.
    fn template(self, name: &str, policy: Option<&str>) -> String {
        let mut declaration = match self {
            Self::Parameter => json!({
                "Type": self.resource_type(),
                "Properties": {"Type": "String", "Name": name, "Value": PARAMETER_VALUE},
            }),
            Self::Bucket => json!({
                "Type": self.resource_type(),
                "Properties": {"BucketName": name},
            }),
        };
        if let (Some(policy), Some(members)) = (policy, declaration.as_object_mut()) {
            members.insert(
                "UpdateReplacePolicy".to_string(),
                Value::String(policy.to_string()),
            );
        }
        json!({"Resources": {LOGICAL_ID: declaration}}).to_string()
    }
}

/// Whether the service that owns the resource still holds it under `name`, and — for a
/// parameter, which carries one — the value it reports.
///
/// `None` is that service answering that it no longer holds the resource, which is what says a
/// replaced resource was really deleted rather than merely dropped from the stack's records.
async fn still_held(router: &MultiServiceRouter, kind: Kind, name: &str) -> Option<String> {
    match kind {
        Kind::Parameter => {
            let (code, body) = ssm(router, "GetParameter", &json!({"Name": name})).await;
            if code != 200 {
                return None;
            }
            Some(
                body.pointer("/Parameter/Value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Kind::Bucket => {
            let answer = s3(router, "HEAD", &format!("/{name}"), b"").await;
            (answer.status == 200).then(String::new)
        }
    }
}

/// One `StackEvent`, reduced to the three members this suite reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    status: String,
    reason: Option<String>,
    physical_id: Option<String>,
}

/// Every event the stack recorded for `LOGICAL_ID` whose status belongs to the UPDATE or the
/// DELETE family, OLDEST FIRST: the events of the replacing update, with the create's own
/// `CREATE_*` records — which the revision before it made — left out.
async fn replacement_events(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<Event> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    let mut events: Vec<Event> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(LOGICAL_ID))
        .map(|event| Event {
            status: xml_text(event, "ResourceStatus").unwrap_or_default(),
            reason: xml_text(event, "ResourceStatusReason"),
            physical_id: xml_text(event, "PhysicalResourceId"),
        })
        .filter(|event| event.status.starts_with("UPDATE_") || event.status.starts_with("DELETE_"))
        .collect();
    // `DescribeStackEvents` answers "events ... with the most recently added events first".
    events.reverse();
    events
}

/// The `(LogicalResourceId, PhysicalResourceId)` pairs the stack records right now.
async fn recorded_resources(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<(String, String)> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter_map(|record| {
            Some((
                xml_text(record, "LogicalResourceId")?,
                xml_text(record, "PhysicalResourceId").unwrap_or_default(),
            ))
        })
        .collect()
}

/// One row: the type whose create-only property changes, the `UpdateReplacePolicy` the
/// declaration carries, and the two physical names the two revisions ask for.
struct Row {
    case: &'static str,
    stack: &'static str,
    kind: Kind,
    /// `None` is a declaration that states no `UpdateReplacePolicy` at all.
    policy: Option<&'static str>,
    first: &'static str,
    second: &'static str,
    /// Whether the OLD physical resource must survive the replacement — true exactly for
    /// `Retain`.
    retains_the_old: bool,
}

/// The rows: every value `UpdateReplacePolicy` takes, on the type the attribute is asserted
/// about, plus a second type in a second owning service.
///
/// The policy values are `Delete`, `Retain` and `Snapshot`, as `state::deletion_policy` carries
/// them; a declaration stating none gets `Delete`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a parameter replaced with no UpdateReplacePolicy declared",
            stack: "replacement-retention-parameter-default",
            kind: Kind::Parameter,
            policy: None,
            first: "/replacement-retention/default/first",
            second: "/replacement-retention/default/second",
            retains_the_old: false,
        },
        Row {
            case: "a parameter replaced under UpdateReplacePolicy Delete",
            stack: "replacement-retention-parameter-delete",
            kind: Kind::Parameter,
            policy: Some(deletion_policy::DELETE),
            first: "/replacement-retention/delete/first",
            second: "/replacement-retention/delete/second",
            retains_the_old: false,
        },
        Row {
            // "For resources that support snapshots, CloudFormation creates a snapshot for the
            // resource before deleting it" — Systems Manager takes no snapshot of a parameter,
            // so there is none to take and the old parameter is deleted.
            case: "a parameter replaced under UpdateReplacePolicy Snapshot, a type its service \
                   takes no snapshot of",
            stack: "replacement-retention-parameter-snapshot",
            kind: Kind::Parameter,
            policy: Some(deletion_policy::SNAPSHOT),
            first: "/replacement-retention/snapshot/first",
            second: "/replacement-retention/snapshot/second",
            retains_the_old: false,
        },
        Row {
            case: "a parameter replaced under UpdateReplacePolicy Retain",
            stack: "replacement-retention-parameter-retain",
            kind: Kind::Parameter,
            policy: Some(deletion_policy::RETAIN),
            first: "/replacement-retention/retain/first",
            second: "/replacement-retention/retain/second",
            retains_the_old: true,
        },
        Row {
            case: "a bucket replaced under UpdateReplacePolicy Retain",
            stack: "replacement-retention-bucket-retain",
            kind: Kind::Bucket,
            policy: Some(deletion_policy::RETAIN),
            first: "replacement-retention-bucket-retain-one",
            second: "replacement-retention-bucket-retain-two",
            retains_the_old: true,
        },
        Row {
            case: "a bucket replaced with no UpdateReplacePolicy declared",
            stack: "replacement-retention-bucket-default",
            kind: Kind::Bucket,
            policy: None,
            first: "replacement-retention-bucket-default-one",
            second: "replacement-retention-bucket-default-two",
            retains_the_old: false,
        },
    ]
}

/// RULE: see the module header — a replacement reports the UPDATE family, and the old physical
/// resource's fate is its `UpdateReplacePolicy`.
#[tokio::test]
async fn a_replacement_reports_an_update_and_honours_the_update_replace_policy() {
    for row in rows() {
        let Row {
            case,
            stack,
            kind,
            policy,
            first,
            second,
            retains_the_old,
        } = row;
        let router = router();

        // The revision that CREATES the resource, under the first physical name.
        let (code, body) = create_stack(&router, stack, &kind.template(first, policy), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the create must be accepted, got {code} {body}"
        );
        // A DELETED stack is no longer reported under its NAME, so the read that waits out the
        // deletion below addresses it by the id the create minted.
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: expected CreateStack to report a StackId, got {body}"
        );
        let created = settled_stack_status(case, &router, REGION, stack).await;
        assert_eq!(
            created,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {created:?}"
        );
        let before = still_held(&router, kind, first).await;
        assert!(
            before.is_some(),
            "case {case:?}: expected {} to hold {first:?} after the create",
            kind.resource_type()
        );

        // ...and the revision that asks for another physical name, which is a value the
        // resource that exists cannot take: the update REPLACES it.
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &kind.template(second, policy)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the update must be accepted, got {code} {body}"
        );
        let updated = settled_stack_status(case, &router, REGION, stack).await;
        assert_eq!(
            updated,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the replacing update to complete, got {updated:?}"
        );

        // The replacement is reported as the UPDATE's own: announced, initiated, then complete
        // under the NEW physical id — and no event of the resource's creating half belongs to
        // the CREATE family.
        let events = replacement_events(case, &router, stack).await;
        let announced = vec![
            Event {
                status: resource_status::UPDATE_IN_PROGRESS.to_string(),
                reason: Some(RESOURCE_REPLACEMENT_IN_PROGRESS.to_string()),
                physical_id: Some(String::new()),
            },
            Event {
                status: resource_status::UPDATE_IN_PROGRESS.to_string(),
                reason: Some(RESOURCE_CREATION_INITIATED.to_string()),
                physical_id: Some(second.to_string()),
            },
            Event {
                status: resource_status::UPDATE_COMPLETE.to_string(),
                reason: None,
                physical_id: Some(second.to_string()),
            },
        ];
        assert_eq!(
            events.get(..announced.len()).unwrap_or_default(),
            announced.as_slice(),
            "case {case:?}: expected the replacement to be announced, initiated and completed \
             as {announced:?}, got {events:?}"
        );

        // What the cleanup phase did with the OLD physical resource, as the event stream
        // reports it: the statuses the policy owes, in order, and nothing else...
        let cleanup: Vec<Event> = events[announced.len()..].to_vec();
        let owed: Vec<&str> = if retains_the_old {
            vec![resource_status::DELETE_SKIPPED]
        } else {
            vec![
                resource_status::DELETE_IN_PROGRESS,
                resource_status::DELETE_COMPLETE,
            ]
        };
        assert_eq!(
            cleanup
                .iter()
                .map(|event| event.status.as_str())
                .collect::<Vec<&str>>(),
            owed,
            "case {case:?}: expected the cleanup of the replaced resource to report {owed:?} \
             and nothing else, got {cleanup:?}"
        );
        // ...and the event that CONCLUDES it names the OLD physical resource, carrying no
        // reason: nothing failed, so there is nothing to explain.
        let concluded = cleanup.last().expect("the cleanup reports a conclusion");
        assert_eq!(
            (concluded.physical_id.as_deref(), concluded.reason.as_deref()),
            (Some(first), None),
            "case {case:?}: expected {} to name {first:?} with no reason, got {concluded:?}",
            concluded.status
        );

        // ...and as the service that OWNS the resource answers: the replacement is there, and
        // the resource it replaced survived the update exactly where the policy retains it,
        // still carrying what it held.
        assert!(
            still_held(&router, kind, second).await.is_some(),
            "case {case:?}: expected {} to hold the replacement {second:?}",
            kind.resource_type()
        );
        let old = still_held(&router, kind, first).await;
        if retains_the_old {
            assert_eq!(
                old, before,
                "case {case:?}: expected {first:?} to be left exactly as the create left it, \
                 got {old:?}"
            );
        } else {
            assert_eq!(
                old, None,
                "case {case:?}: expected {first:?} to be deleted by the cleanup, got {old:?}"
            );
        }

        // The stack records exactly one resource under the logical id, and it is the
        // replacement: the record of the one it replaced is gone either way.
        let recorded = recorded_resources(case, &router, stack).await;
        let under_the_logical_id: Vec<&(String, String)> = recorded
            .iter()
            .filter(|(logical, _)| logical == LOGICAL_ID)
            .collect();
        assert_eq!(
            under_the_logical_id.len(),
            1,
            "case {case:?}: expected exactly one record under {LOGICAL_ID:?}, got {recorded:?}"
        );
        assert_eq!(
            under_the_logical_id[0].1, second,
            "case {case:?}: expected the record to name the replacement, got {:?}",
            under_the_logical_id[0].1
        );

        // And a retained resource outlives the stack as well: the stack released it, so its
        // own deletion owes no call about it, while the replacement it manages is deleted.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the deletion must be accepted, got {code} {body}"
        );
        let deleted = settled_stack_status(case, &router, REGION, &stack_id).await;
        assert_eq!(
            deleted,
            status::DELETE_COMPLETE,
            "case {case:?}: expected the deletion to complete, got {deleted:?}"
        );
        assert_eq!(
            still_held(&router, kind, second).await,
            None,
            "case {case:?}: expected the stack's own deletion to delete {second:?}"
        );
        assert_eq!(
            still_held(&router, kind, first).await.is_some(),
            retains_the_old,
            "case {case:?}: expected {first:?} to {} the stack's deletion",
            if retains_the_old {
                "outlive"
            } else {
                "have been gone before"
            }
        );
    }
}
