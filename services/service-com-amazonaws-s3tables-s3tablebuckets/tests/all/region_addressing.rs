// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the Region half of an ARN: which Region's
//! endpoint answers for a resource the request names by ARN.
//!
//! The rule under test: an ARN is an absolute identity and carries the Region
//! its resource lives in, while this endpoint serves ONE Region — the one the
//! request is signed for — so a request naming a resource by an ARN of another
//! Region addresses nothing this endpoint can answer for, and is refused as
//! client input: HTTP 400, the modeled `BadRequestException`, with the fixed
//! sentence below. It is NOT a not-found, and the refusal is taken before
//! anything is resolved, so it is the same answer whether or not a resource of
//! that name exists in the Region the ARN names, and a refused write changes
//! nothing.
//!
//! An ARN whose Region field is empty names no Region at all, which is not a
//! mismatch: such a request resolves against this endpoint's own Region and
//! comes back as the modeled not-found.
//!
//! The rows span what the rule generalises over: the input member the ARN
//! travels in (a table bucket ARN, a table's own ARN, a taggable resource's
//! ARN), reads and writes, operations addressing a bucket and operations
//! addressing a table inside one, several pairs of Regions, and an ARN naming a
//! bucket that exists in its Region beside one that does not.

use serde_json::json;
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::TableMaintenanceType;

use super::harness::{expected_bucket_arn, Emulator, Reply};
use super::table_maintenance_configuration::{create_table, reported_task, Reported};

/// The fixed sentence this service states an ARN of another Region with.
/// Written out here rather than read from the service's own constants.
const REGION_MISMATCH: &str = "The provided Region does not match the Region of the resource.";

/// The fixed sentence this service states a table bucket the store does not
/// hold with.
const NO_BUCKET: &str = "The specified bucket does not exist.";

/// The namespace and table every bucket of this suite holds.
const NAMESPACE: &str = "events";
const TABLE: &str = "user_events";

/// One pair of Regions: where the resource lives, and where the request that
/// names it is made.
struct Row {
    label: &'static str,
    home: &'static str,
    calling: &'static str,
}

/// The pairs the rule is graded on.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a bucket of the US east, called from the US west",
            home: "us-east-1",
            calling: "us-west-2",
        },
        Row {
            label: "a bucket of the US west, called from the US east",
            home: "us-west-2",
            calling: "us-east-1",
        },
        Row {
            label: "a bucket of Ireland, called from Frankfurt",
            home: "eu-west-1",
            calling: "eu-central-1",
        },
        Row {
            label: "a bucket of Sydney, called from São Paulo",
            home: "ap-southeast-2",
            calling: "sa-east-1",
        },
    ]
}

/// Assert `reply` is the refusal this rule states.
fn assert_region_mismatch(reply: &Reply, case: &str, operation: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?} on {operation}: expected 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("BadRequestException"),
        "case {case:?} on {operation}: expected BadRequestException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        REGION_MISMATCH,
        "case {case:?} on {operation}: expected {REGION_MISMATCH:?}, got {actual:?}",
        actual = reply.message()
    );
}

/// Every call of this suite, made in `region` against `bucket_arn` and the table
/// inside it, each with the operation it addresses.
async fn calls(
    emulator: &Emulator,
    region: &str,
    bucket_arn: &str,
    table_arn: &str,
) -> Vec<(&'static str, Reply)> {
    vec![
        (
            "GetTableBucket",
            emulator.get_table_bucket(region, bucket_arn).await,
        ),
        (
            "ListNamespaces",
            emulator.list_namespaces(region, bucket_arn, &[]).await,
        ),
        (
            "GetTableMaintenanceConfiguration",
            emulator
                .get_table_maintenance_configuration(region, bucket_arn, NAMESPACE, TABLE)
                .await,
        ),
        (
            "PutTableMaintenanceConfiguration",
            emulator
                .put_table_maintenance_configuration(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    TABLE,
                    TableMaintenanceType::IcebergCompaction.as_wire(),
                    &json!({ "status": "disabled", "settings": {
                        "icebergCompaction": { "targetFileSizeMB": 128, "strategy": "binpack" }
                    }}),
                )
                .await,
        ),
        (
            "GetTable",
            emulator.get_table_by_arn(region, table_arn).await,
        ),
        (
            "ListTagsForResource",
            emulator.list_tags_for_resource(region, table_arn).await,
        ),
        (
            "TagResource",
            emulator
                .tag_resource(region, bucket_arn, &json!({ "owner": "analytics" }))
                .await,
        ),
    ]
}

#[tokio::test]
async fn an_arn_of_another_region_is_refused_as_client_input() {
    let emulator = Emulator::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = format!(
            "{label} ({home} -> {calling})",
            label = row.label,
            home = row.home,
            calling = row.calling
        );
        let (bucket_arn, table_arn) = create_table(
            &emulator,
            row.home,
            &format!("region-addressing-{index}"),
            NAMESPACE,
            TABLE,
        )
        .await;

        for (operation, reply) in calls(&emulator, row.calling, &bucket_arn, &table_arn).await {
            assert_region_mismatch(&reply, &case, operation);
        }

        // The refusals named the Region and nothing else: in the Region the ARN
        // names, every one of those calls is served, and neither the refused put
        // nor the refused tagging left a trace.
        for (operation, reply) in calls(&emulator, row.home, &bucket_arn, &table_arn).await {
            assert!(
                (200..300).contains(&reply.status),
                "case {case:?} on {operation}: the same call in the ARN's own Region must be \
                 served, got {status} {body}",
                status = reply.status,
                body = reply.body
            );
        }
    }
}

#[tokio::test]
async fn the_region_is_judged_before_anything_is_resolved_or_written() {
    let emulator = Emulator::new();
    let home = "eu-west-1";
    let calling = "us-east-1";
    let (bucket_arn, table_arn) =
        create_table(&emulator, home, "region-before-lookup", NAMESPACE, TABLE).await;

    // A bucket that exists in the Region its ARN names and one that does not are
    // refused the same way from another Region: nothing is resolved before the
    // Region is judged.
    let absent = expected_bucket_arn(home, "region-before-lookup-absent");
    for (case, arn) in [
        ("a bucket of that Region", bucket_arn.clone()),
        ("a bucket of that Region that was never created", absent),
    ] {
        let read = emulator
            .get_table_maintenance_configuration(calling, &arn, NAMESPACE, TABLE)
            .await;
        assert_region_mismatch(&read, case, "GetTableMaintenanceConfiguration");
    }

    // And the writes those calls would have made never happened: the table still
    // runs under the published default and carries no tag.
    let reported = reported_task(
        &emulator
            .get_table_maintenance_configuration(home, &bucket_arn, NAMESPACE, TABLE)
            .await,
        "after the refusals",
        TableMaintenanceType::IcebergCompaction,
    );
    assert_eq!(
        reported,
        Reported::default_compaction(),
        "after the refusals: the table must still run under the published default, got \
         {reported:?}"
    );
    let tags = emulator.list_tags_for_resource(home, &table_arn).await;
    assert_eq!(
        tags.body.get("tags"),
        Some(&json!({})),
        "after the refusals: the table must carry no tag, got {body}",
        body = tags.body
    );
}

#[tokio::test]
async fn an_arn_naming_no_region_is_resolved_against_this_endpoint() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let (bucket_arn, _) =
        create_table(&emulator, region, "region-unnamed", NAMESPACE, TABLE).await;

    // The modeled `TableBucketARN` pattern admits an empty Region field
    // (`^(arn:aws[-a-z0-9]*:[a-z0-9]+:[-a-z0-9]*:[0-9]{12}:bucket/[a-z0-9_-]{3,63})$`),
    // so such an ARN is read: it names no Region to disagree with this
    // endpoint's, and nothing is stored under it.
    let unnamed = bucket_arn.replace(&format!(":{region}:"), "::");
    let read = emulator.get_table_bucket(region, &unnamed).await;
    assert_eq!(
        read.status, 404,
        "an ARN naming no Region must resolve against this endpoint, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.error_code.as_deref(),
        Some("NotFoundException"),
        "an ARN naming no Region must answer the modeled not-found, got {actual:?}",
        actual = read.error_code
    );
    assert_eq!(
        read.message(),
        NO_BUCKET,
        "an ARN naming no Region must answer {NO_BUCKET:?}, got {actual:?}",
        actual = read.message()
    );
}
