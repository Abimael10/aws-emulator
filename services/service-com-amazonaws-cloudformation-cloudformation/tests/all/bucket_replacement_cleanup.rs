// SPDX-License-Identifier: CC0-1.0

//! WHEN the OLD physical resource of a replacement is taken down, and what becomes of it when
//! the owning service REFUSES that delete, end to end over the awsQuery wire.
//!
//! RULE under test: **a resource an update REPLACED is taken down in the update's CLEANUP
//! phase, which runs after the replacement is already in force — "the stack update was
//! successful, but CloudFormation is cleaning up the old resources"
//! (`UPDATE_COMPLETE_CLEANUP_IN_PROGRESS`) — so a delete the owning service refuses there is
//! reported on that resource and leaves the update COMPLETE: the stack records the resource
//! the replacement minted and stops recording the one it replaced, which is left exactly as
//! it is in the service that owns it.**
//!
//! The other half of the same rule, which the second test below pins: an update that does NOT
//! go through never reaches that phase, so it takes NOTHING down. A cancelled update reports
//! no delete of the physical resource that was in force before it, the owning service still
//! holds that resource, and the stack's record — with every output and export resolved from it
//! — still names it. The delete such a rollback does report is of the REPLACEMENT the update
//! had created, which is the resource that should not outlive it.
//!
//! The replacement this suite drives is the one every `AWS::S3::Bucket` rename is — the bucket
//! name is create-only, so a declaration naming another bucket asks for a different resource —
//! and the refusal is the one Amazon S3 publishes for a bucket that still holds objects ("the
//! bucket you tried to delete is not empty"). The rows differ along what the rule generalises
//! over: whether the replaced bucket can be deleted at all (an empty one is, so its record goes
//! and the bucket is gone) and whether it cannot (one holding an object, whose delete is
//! refused), and whether the stack carries a second resource the cleanup must leave alone; the
//! cancelled rows differ along WHAT the update would have taken down (the old half of a
//! replacement, a resource the revision drops) and, as the other side, the same revision that
//! goes through.
//!
//! Every assertion is a client's: the stack is read through `DescribeStacks`,
//! `DescribeStackResources` and `DescribeStackEvents`, and the buckets through Amazon S3's own
//! wire.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, member, router, xml_blocks};
use crate::stack_lifecycle::{xml_text, xml_texts};
use crate::stack_resource_provisioning::s3;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// How long a poll waits for a stack operation to settle before the suite calls it stuck, and
/// how often it looks. The deadline is far longer than the spans this emulator gives a stack's
/// transitions, so a loaded host cannot fail a row; what is asserted is the CONDITION (a status
/// that is not an operation in flight), never the time it took.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The logical id the bucket is declared under in every revision below.
const LOGICAL_ID: &str = "Store";

/// The template one revision deploys: the bucket under `name`, beside the second resource a
/// revision keeps unchanged (another bucket, under its own fixed name).
fn template(name: &str, keeps_a_sibling: bool) -> String {
    let mut resources = json!({
        LOGICAL_ID: {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": name}}
    });
    if keeps_a_sibling {
        if let Some(members) = resources.as_object_mut() {
            members.insert(
                SIBLING_LOGICAL_ID.to_string(),
                json!({
                    "Type": "AWS::S3::Bucket",
                    "Properties": {"BucketName": "bucket-replacement-cleanup-sibling-kept"}
                }),
            );
        }
    }
    json!({"Resources": resources}).to_string()
}

/// The logical id of the resource a row's update leaves exactly as it is.
const SIBLING_LOGICAL_ID: &str = "Archive";

/// Poll `DescribeStacks` the way a client's waiter does and answer the `StackStatus` the stack
/// SETTLED on — the first status that is not an operation in flight.
async fn settled_status(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks must answer 200, got {code} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to settle, saw {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every `(LogicalResourceId, PhysicalResourceId)` the stack records.
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

/// The `(ResourceStatus, ResourceStatusReason)` of every event the stack recorded for
/// `logical_id`, newest first — what says how the cleanup of the replaced resource ended.
async fn events_for(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Vec<(String, String)> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .map(|event| {
            (
                xml_text(event, "ResourceStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// `HEAD /<bucket>` — does Amazon S3 still hold this bucket?
async fn holds_bucket(router: &MultiServiceRouter, bucket: &str) -> bool {
    s3(router, "HEAD", &format!("/{bucket}"), b"").await.status == 200
}

/// One row: whether the replaced bucket holds an object (so Amazon S3 refuses its delete), and
/// whether the stack declares a second resource beside it.
struct Row {
    case: &'static str,
    stack: &'static str,
    first: &'static str,
    second: &'static str,
    /// The object to store in the first bucket, which is what makes its delete refused.
    holds_an_object: bool,
    keeps_a_sibling: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the replaced bucket is empty, so its delete goes through",
            stack: "bucket-replacement-cleanup-empty",
            first: "bucket-replacement-cleanup-empty-one",
            second: "bucket-replacement-cleanup-empty-two",
            holds_an_object: false,
            keeps_a_sibling: false,
        },
        Row {
            case: "the replaced bucket still holds an object, so its delete is refused",
            stack: "bucket-replacement-cleanup-held",
            first: "bucket-replacement-cleanup-held-one",
            second: "bucket-replacement-cleanup-held-two",
            holds_an_object: true,
            keeps_a_sibling: false,
        },
        Row {
            case: "a refused cleanup beside a resource the update keeps",
            stack: "bucket-replacement-cleanup-sibling",
            first: "bucket-replacement-cleanup-sibling-one",
            second: "bucket-replacement-cleanup-sibling-two",
            holds_an_object: true,
            keeps_a_sibling: true,
        },
    ]
}

/// The key the row's object is stored at.
const HELD_KEY: &str = "held/object.bin";

/// RULE: see the module header — the cleanup of a replaced resource cannot fail the update
/// that already landed.
#[tokio::test]
async fn a_refused_cleanup_of_a_replaced_bucket_leaves_the_update_complete() {
    for row in rows() {
        let router = router();
        let (code, body) = create_stack(
            &router,
            row.stack,
            &template(row.first, row.keeps_a_sibling),
            &[],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: the create must be accepted, got {code} {body}",
            row.case
        );
        let created = settled_status(row.case, &router, row.stack).await;
        assert_eq!(
            created,
            status::CREATE_COMPLETE,
            "case {:?}: expected the create to complete, got {created:?}",
            row.case
        );

        if row.holds_an_object {
            let stored = s3(
                &router,
                "PUT",
                &format!("/{}/{HELD_KEY}", row.first),
                b"bytes that outlive the stack's record",
            )
            .await;
            assert_eq!(
                stored.status, 200,
                "case {:?}: the object must be stored first, got {} {}",
                row.case, stored.status, stored.body
            );
        }

        // The RENAME: the bucket name is create-only, so this declaration asks for a different
        // bucket and the update replaces the resource.
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(row.second, row.keeps_a_sibling),
                ),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: the update must be accepted, got {code} {body}",
            row.case
        );
        let updated = settled_status(row.case, &router, row.stack).await;
        assert_eq!(
            updated,
            status::UPDATE_COMPLETE,
            "case {:?}: expected the update to complete whatever the cleanup did, got \
             {updated:?}",
            row.case
        );

        // The stack records the bucket the replacement minted, once.
        let recorded = recorded_resources(row.case, &router, row.stack).await;
        let under_the_logical_id: Vec<&(String, String)> = recorded
            .iter()
            .filter(|(logical, _)| logical == LOGICAL_ID)
            .collect();
        assert_eq!(
            under_the_logical_id.len(),
            1,
            "case {:?}: expected exactly one record under {LOGICAL_ID:?}, got {recorded:?}",
            row.case
        );
        assert_eq!(
            under_the_logical_id[0].1, row.second,
            "case {:?}: expected the record to name the replacement, got {:?}",
            row.case, under_the_logical_id[0].1
        );
        if row.keeps_a_sibling {
            assert!(
                recorded
                    .iter()
                    .any(|(logical, _)| logical == SIBLING_LOGICAL_ID),
                "case {:?}: expected the untouched resource to be recorded still, got \
                 {recorded:?}",
                row.case
            );
        }

        // The replacement exists, and the replaced bucket is gone exactly where its delete
        // went through.
        assert!(
            holds_bucket(&router, row.second).await,
            "case {:?}: expected Amazon S3 to hold the replacement {:?}",
            row.case,
            row.second
        );
        assert_eq!(
            holds_bucket(&router, row.first).await,
            row.holds_an_object,
            "case {:?}: expected the replaced bucket {:?} to be {}",
            row.case,
            row.first,
            if row.holds_an_object {
                "left where it is"
            } else {
                "deleted"
            }
        );

        let events = events_for(row.case, &router, row.stack, LOGICAL_ID).await;
        if row.holds_an_object {
            // The refusal is REPORTED on the resource, carrying the message Amazon S3
            // answered, and the bytes the bucket holds are still readable through its own
            // wire: nothing of the orphaned bucket was taken away.
            let failed = events
                .iter()
                .find(|(state, _)| state == resource_status::DELETE_FAILED);
            let Some((_, reason)) = failed else {
                panic!(
                    "case {:?}: expected a {} event for the replaced bucket, got {events:?}",
                    row.case,
                    resource_status::DELETE_FAILED
                )
            };
            assert!(
                reason.contains("not empty"),
                "case {:?}: expected the reason to carry the refusal Amazon S3 answered, got \
                 {reason:?}",
                row.case
            );
            let read = s3(
                &router,
                "GET",
                &format!("/{}/{HELD_KEY}", row.first),
                b"",
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {:?}: expected the orphaned bucket's object to be readable, got {} {}",
                row.case, read.status, read.body
            );
        } else {
            assert!(
                events
                    .iter()
                    .any(|(state, _)| state == resource_status::DELETE_COMPLETE),
                "case {:?}: expected the replaced bucket's delete to be reported complete, got \
                 {events:?}",
                row.case
            );
            assert!(
                !events
                    .iter()
                    .any(|(state, _)| state == resource_status::DELETE_FAILED),
                "case {:?}: expected no delete failure, got {events:?}",
                row.case
            );
        }

        // And the stack itself reports no failure reason for an update that completed.
        let (code, body) = call(
            &router,
            "DescribeStacks",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: DescribeStacks must answer 200, got {code} {body}",
            row.case
        );
        assert!(
            xml_texts(&body, "StackStatusReason").is_empty(),
            "case {:?}: expected no stack status reason, got {:?}",
            row.case,
            xml_texts(&body, "StackStatusReason")
        );
    }
}

// ---------------------------------------------------------------------------
// The other half of the rule: an update that does not go through takes nothing down
// ---------------------------------------------------------------------------

/// The export the rows below publish the bucket in force under, and the stack that imports it
/// — which is what CANCELS a revision that would publish a different value.
const PINNED_EXPORT: &str = "bucket-replacement-pinned";
const IMPORTER: &str = "bucket-replacement-importer";

/// One cancelled row: the revision it asks for, and what the cancelled update would have taken
/// down.
struct CanceledRow {
    case: &'static str,
    stack: &'static str,
    /// The bucket in force before the revision, and the one the revision names.
    first: &'static str,
    second: &'static str,
    /// Whether the revision still declares the sibling bucket the create made.
    keeps_a_sibling: bool,
    /// What the revision appends to the exported value — the only way a revision that leaves
    /// the exporting resource alone can ask to publish a different value.
    suffix: &'static str,
    /// Whether the importing stack is deployed, which is what cancels the revision.
    pinned: bool,
}

fn canceled_rows() -> Vec<CanceledRow> {
    vec![
        CanceledRow {
            case: "a cancelled update that would have replaced the exporting bucket",
            stack: "bucket-replacement-canceled",
            first: "bucket-replacement-canceled-one",
            second: "bucket-replacement-canceled-two",
            keeps_a_sibling: true,
            suffix: "",
            pinned: true,
        },
        CanceledRow {
            case: "a cancelled update that would also have dropped another resource",
            stack: "bucket-replacement-canceled-sibling",
            first: "bucket-replacement-canceled-sibling-one",
            second: "bucket-replacement-canceled-sibling-two",
            keeps_a_sibling: false,
            suffix: "",
            pinned: true,
        },
        CanceledRow {
            case: "a cancelled update that leaves the exporting bucket alone and drops another",
            stack: "bucket-replacement-canceled-dropped",
            first: "bucket-replacement-canceled-dropped-one",
            second: "bucket-replacement-canceled-dropped-one",
            keeps_a_sibling: false,
            suffix: "-revised",
            pinned: true,
        },
        CanceledRow {
            case: "the same replacing revision with nothing importing the export, which \
                   completes",
            stack: "bucket-replacement-released",
            first: "bucket-replacement-released-one",
            second: "bucket-replacement-released-two",
            keeps_a_sibling: true,
            suffix: "",
            pinned: false,
        },
    ]
}

/// The sibling bucket every cancelled row's CREATE declares, which the first two revisions drop.
const PINNED_SIBLING: &str = "bucket-replacement-canceled-sibling-kept";

/// One revision of a cancelled row's template: the bucket under `name`, the sibling where the
/// revision still declares one, and the output that EXPORTS the bucket in force.
fn pinned_template(name: &str, keeps_a_sibling: bool, suffix: &str) -> String {
    let mut resources = json!({
        LOGICAL_ID: {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": name}}
    });
    if keeps_a_sibling {
        if let Some(members) = resources.as_object_mut() {
            members.insert(
                SIBLING_LOGICAL_ID.to_string(),
                json!({
                    "Type": "AWS::S3::Bucket",
                    "Properties": {"BucketName": PINNED_SIBLING}
                }),
            );
        }
    }
    json!({
        "Resources": resources,
        "Outputs": {
            "Exported": {
                "Value": {"Fn::Join": ["", [{"Ref": LOGICAL_ID}, suffix]]},
                "Export": {"Name": PINNED_EXPORT}
            }
        }
    })
    .to_string()
}

/// The importing stack's template: it reads the export into an output of its own, which is what
/// makes it an importer of that export.
fn pinned_importer_template() -> String {
    json!({
        "Resources": {"Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"}},
        "Outputs": {"Echoed": {"Value": {"Fn::ImportValue": PINNED_EXPORT}}}
    })
    .to_string()
}

/// The `StackStatusReason` CloudFormation records on an update it cancelled because the update
/// would have published a different value under an export another live stack imports.
///
/// Spelled out here rather than read off the emulator's own formatter: fixed text around the
/// export name and the importing stack's name, ending with its period.
fn update_canceled_reason(export_name: &str, importing_stack: &str) -> String {
    format!(
        "Update canceled. Cannot update export {export_name} as it is in use by \
         {importing_stack}."
    )
}

/// The physical resources the stack reports a DELETE transition of, for `logical_id`: what says
/// WHICH physical resource an operation took down under that logical id.
async fn deleted_physical_ids(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Vec<String> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .filter(|event| {
            xml_text(event, "ResourceStatus")
                .unwrap_or_default()
                .starts_with("DELETE")
        })
        .map(|event| xml_text(event, "PhysicalResourceId").unwrap_or_default())
        .collect()
}

/// The `ResourceStatusReason`s of the stack's OWN events — those whose `LogicalResourceId` is
/// the stack itself, which is the stack-level audit trail.
async fn stack_level_reasons(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<String> {
    events_for(case, router, stack, stack)
        .await
        .into_iter()
        .map(|(_, reason)| reason)
        .collect()
}

/// The value `ListExports` publishes under [`PINNED_EXPORT`], or `None` where this Region
/// publishes no such export.
async fn published_value(case: &str, router: &MultiServiceRouter) -> Option<String> {
    let (code, body) = call(router, "ListExports", &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListExports must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|entry| xml_text(entry, "Name").as_deref() == Some(PINNED_EXPORT))
        .find_map(|entry| xml_text(entry, "Value"))
}

/// RULE: see the module header — an update that is CANCELLED never reaches the cleanup phase,
/// so it takes nothing down and everything the stack published for what it would have
/// superseded is still in force.
#[tokio::test]
async fn a_canceled_update_takes_down_nothing_it_would_have_superseded() {
    for row in canceled_rows() {
        let case = row.case;
        let router = router();
        let (code, body) = create_stack(
            &router,
            row.stack,
            &pinned_template(row.first, true, ""),
            &[],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the create must be accepted, got {code} {body}"
        );
        let created = settled_status(case, &router, row.stack).await;
        assert_eq!(
            created,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {created:?}"
        );
        if row.pinned {
            let (code, body) = create_stack(
                &router,
                IMPORTER,
                &pinned_importer_template(),
                &[],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the importing stack must be accepted, got {code} {body}"
            );
            let imported = settled_status(case, &router, IMPORTER).await;
            assert_eq!(
                imported,
                status::CREATE_COMPLETE,
                "case {case:?}: expected the importing stack to complete, got {imported:?}"
            );
        }
        let exported = published_value(case, &router).await;
        assert_eq!(
            exported.as_deref(),
            Some(row.first),
            "case {case:?}: expected {PINNED_EXPORT} to publish {:?}, got {exported:?}",
            row.first
        );

        // The revision: whatever the row restates, which for a pinned row is a value the
        // export cannot take while another stack holds it.
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &pinned_template(row.second, row.keeps_a_sibling, row.suffix),
                ),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the update must be accepted — the stack reports the outcome — got \
             {code} {body}"
        );
        let settled = settled_status(case, &router, row.stack).await;
        let expected_status = if row.pinned {
            status::UPDATE_ROLLBACK_COMPLETE
        } else {
            status::UPDATE_COMPLETE
        };
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the revision to settle in {expected_status:?}, got \
             {settled:?}"
        );

        let recorded = recorded_resources(case, &router, row.stack).await;
        let under_the_logical_id: Vec<&(String, String)> = recorded
            .iter()
            .filter(|(logical, _)| logical == LOGICAL_ID)
            .collect();
        let took_down = deleted_physical_ids(case, &router, row.stack, LOGICAL_ID).await;
        if !row.pinned {
            // The other side of the rule: the update went through, so the resource it replaced
            // really was taken down — and the stack records and publishes the replacement.
            assert!(
                !holds_bucket(&router, row.first).await,
                "case {case:?}: expected the replaced bucket {:?} to be taken down by an update \
                 that completed",
                row.first
            );
            assert!(
                took_down.contains(&row.first.to_string()),
                "case {case:?}: expected the completed update to report taking {:?} down, got \
                 {took_down:?}",
                row.first
            );
            let published = published_value(case, &router).await;
            assert_eq!(
                published.as_deref(),
                Some(row.second),
                "case {case:?}: expected {PINNED_EXPORT} to publish the replacement {:?}, got \
                 {published:?}",
                row.second
            );
            continue;
        }

        // NOTHING the cancelled update would have taken down is gone: not the resource it
        // would have replaced, and not the one the revision dropped.
        assert!(
            holds_bucket(&router, row.first).await,
            "case {case:?}: expected Amazon S3 to still hold {:?}, which the cancelled update \
             would have superseded",
            row.first
        );
        assert!(
            holds_bucket(&router, PINNED_SIBLING).await,
            "case {case:?}: expected Amazon S3 to still hold {PINNED_SIBLING}, which the \
             cancelled update would have dropped"
        );
        // ...and the stack reported no delete of what was in force: the delete a rollback of a
        // replacing update reports is of the REPLACEMENT it had created.
        assert!(
            !took_down.contains(&row.first.to_string()),
            "case {case:?}: expected no delete of {:?} to be reported for a cancelled update, \
             got {took_down:?}",
            row.first
        );
        assert!(
            !deleted_physical_ids(case, &router, row.stack, SIBLING_LOGICAL_ID)
                .await
                .contains(&PINNED_SIBLING.to_string()),
            "case {case:?}: expected no delete of {PINNED_SIBLING} to be reported for a \
             cancelled update"
        );

        // The stack's record, its outputs and its export all still name what is in force.
        assert_eq!(
            under_the_logical_id.len(),
            1,
            "case {case:?}: expected exactly one record under {LOGICAL_ID:?}, got {recorded:?}"
        );
        assert_eq!(
            under_the_logical_id[0].1, row.first,
            "case {case:?}: expected the record to name the bucket in force {:?}, got {:?}",
            row.first, under_the_logical_id[0].1
        );
        let still_published = published_value(case, &router).await;
        assert_eq!(
            still_published.as_deref(),
            Some(row.first),
            "case {case:?}: expected {PINNED_EXPORT} to go on publishing {:?}, got \
             {still_published:?}",
            row.first
        );

        // ...and the cancellation is explained ONCE among the stack's own events.
        let reason = update_canceled_reason(PINNED_EXPORT, IMPORTER);
        let reasons = stack_level_reasons(case, &router, row.stack).await;
        let carrying = reasons
            .iter()
            .filter(|recorded| recorded.contains(&reason))
            .count();
        assert_eq!(
            carrying, 1,
            "case {case:?}: expected exactly ONE stack-level event to carry {reason:?}, got \
             {reasons:?}"
        );
    }
}
