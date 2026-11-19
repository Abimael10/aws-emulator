// SPDX-License-Identifier: CC0-1.0

//! Three rules a client that reads a table back after a write depends on, each
//! driven end to end over the wire.
//!
//! RULE 1 — `GetTable` takes EXACTLY one whole addressing form: the table's own
//! ARN alone, or the table bucket ARN, the namespace and the table name together
//! (`smithy_query(query="operation", operation="GetTable")` makes all four members
//! optional `@httpQuery` members, which is what lets one request carry both).
//! A request naming the table twice over is refused with `BadRequestException`
//! (400) and the one fixed sentence naming both forms, BEFORE anything is
//! resolved — so the refusal does not depend on whether the two identifiers agree,
//! and neither identifier is read. The rows here span how the two identifiers
//! RELATE: the same table twice, two live tables of one namespace, two namespaces
//! of one bucket, two buckets, and a triple naming coordinates nothing holds. The
//! subset-by-subset coverage of the four members lives in `get_table_addressing`.
//!
//! RULE 2 — `UntagResource` (DELETE /tag/{resourceArn}?tagKeys=…, 204, empty body)
//! takes from the resource its `resourceArn` names exactly the keys the request
//! names and nothing else, and a later `ListTagsForResource` is the authoritative
//! view of what is left. Tags are keyed by the resource, not by its coordinates,
//! so the rule holds across a `RenameTable` that moves the table: the ARN keeps
//! naming the same resource, and the removal is read back through it.
//!
//! RULE 3 — `UpdateTableMetadataLocation` records the pointer the caller supplies
//! under the caller's `versionToken` as its precondition, and answers with the
//! token the table carries rather than a fresh one: the token the caller holds
//! stays the table's, so the next conditional write it makes is honoured. Driven
//! over a RUN of writes, because a single write cannot tell a carried token from a
//! re-minted one that happens to be read back; a token that is not the table's is
//! the modelled `ConflictException` (409) and leaves the pointer where it was.

use serde_json::{json, Value};

use crate::harness::{listed_tags, served, tag_map, tags_body, Emulator, Reply};
use crate::wire_forms::assert_table_version_token;

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The namespaces the cases work in, and the two table names they key. All
/// satisfy the modelled `^[0-9a-z_]*$`.
const NAMESPACE: &str = "sales";
const OTHER_NAMESPACE: &str = "ops";
const SUBJECT: &str = "subject_table";
const BYSTANDER: &str = "bystander_table";

/// The fixed sentence a request naming a table by neither whole form is refused
/// with, and the one a `versionToken` that is not the table's is refused with.
/// Both are spelled out here rather than read from the crate under test, so this
/// suite pins the wording itself.
const INVALID_ADDRESSING: &str = "Invalid input: Must provide either table arn alone OR all of \
                                  (table bucket arn, namespace, table name) together";
const TOKEN_MISMATCH: &str = "Provided version token does not match the table version token.";

/// Create a table bucket holding `namespaces`, and answer its ARN.
async fn table_bucket(emulator: &Emulator, name: &str, namespaces: &[&str]) -> String {
    let arn = served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": name }))
            .await,
        "CreateTableBucket",
        name,
    )
    .text("arn");
    for namespace in namespaces {
        served(
            emulator
                .create_namespace(REGION, &arn, &json!({ "namespace": [namespace] }))
                .await,
            "CreateNamespace",
            name,
        );
    }
    arn
}

/// Create one table and answer its ARN. `ICEBERG` is the only value the modelled
/// `OpenTableFormat` declares, and `format` is required.
async fn table(emulator: &Emulator, bucket_arn: &str, namespace: &str, name: &str) -> String {
    served(
        emulator
            .create_table(
                REGION,
                bucket_arn,
                namespace,
                &json!({ "name": name, "format": "ICEBERG" }),
            )
            .await,
        "CreateTable",
        name,
    )
    .text("tableARN")
}

/// Assert `reply` is the one refusal RULE 1 states.
fn assert_named_twice(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("BadRequestException"),
        "case {case:?}: expected the modelled BadRequestException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        INVALID_ADDRESSING,
        "case {case:?}: expected the fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

/// RULE 1: whatever the two identifiers of an over-specified read name — one
/// table, two live tables, two buckets, coordinates nothing holds — the read is
/// refused the same way, and no table is touched.
#[tokio::test]
async fn a_read_that_names_a_table_twice_is_refused_however_the_two_identifiers_relate() {
    let emulator = Emulator::new();
    let first = table_bucket(
        &emulator,
        "named-twice-first",
        &[NAMESPACE, OTHER_NAMESPACE],
    )
    .await;
    let second = table_bucket(&emulator, "named-twice-second", &[NAMESPACE]).await;

    let subject = table(&emulator, &first, NAMESPACE, SUBJECT).await;
    let sibling = table(&emulator, &first, NAMESPACE, BYSTANDER).await;
    let across = table(&emulator, &first, OTHER_NAMESPACE, "across_table").await;
    let elsewhere = table(&emulator, &second, NAMESPACE, "elsewhere_table").await;

    // Every table below resolves by its own ARN, so each row names TWO
    // identifiers the store can reach and the refusal is of the request's shape.
    let live = [
        ("the subject table", subject.clone()),
        ("its namespace sibling", sibling.clone()),
        ("the table in the other namespace", across.clone()),
        ("the table in the other bucket", elsewhere.clone()),
    ];
    let mut before: Vec<(&str, String, Value)> = Vec::new();
    for (label, arn) in &live {
        let read = served(
            emulator.get_table_by_arn(REGION, arn).await,
            "GetTable by ARN",
            label,
        );
        before.push((label, arn.clone(), read.body));
    }

    let rows: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "the ARN and the triple naming the same table",
            vec![
                ("tableArn", subject.clone()),
                ("tableBucketARN", first.clone()),
                ("namespace", NAMESPACE.to_string()),
                ("name", SUBJECT.to_string()),
            ],
        ),
        (
            "the ARN of one table and the triple of its namespace sibling",
            vec![
                ("tableArn", subject.clone()),
                ("tableBucketARN", first.clone()),
                ("namespace", NAMESPACE.to_string()),
                ("name", BYSTANDER.to_string()),
            ],
        ),
        (
            "the ARN of one table and the triple of a table in another namespace",
            vec![
                ("tableArn", subject.clone()),
                ("tableBucketARN", first.clone()),
                ("namespace", OTHER_NAMESPACE.to_string()),
                ("name", "across_table".to_string()),
            ],
        ),
        (
            "the ARN of one table and the triple of a table in another bucket",
            vec![
                ("tableArn", subject.clone()),
                ("tableBucketARN", second.clone()),
                ("namespace", NAMESPACE.to_string()),
                ("name", "elsewhere_table".to_string()),
            ],
        ),
        (
            "the ARN of one bucket's table beside the other bucket's ARN",
            vec![
                ("tableArn", elsewhere.clone()),
                ("tableBucketARN", first.clone()),
            ],
        ),
        (
            "the ARN and a triple naming a namespace the bucket does not hold",
            vec![
                ("tableArn", subject.clone()),
                ("tableBucketARN", first.clone()),
                ("namespace", "never_created".to_string()),
                ("name", SUBJECT.to_string()),
            ],
        ),
        (
            "the ARN and a triple naming a table the namespace does not hold",
            vec![
                ("tableArn", across.clone()),
                ("tableBucketARN", first.clone()),
                ("namespace", NAMESPACE.to_string()),
                ("name", "never_created".to_string()),
            ],
        ),
    ];

    for (case, query) in rows {
        let reply = emulator.get_table_with(REGION, &query).await;
        assert_named_twice(&reply, case);
    }

    // A refused read resolved nothing, so every table reads back exactly as it
    // did before: the refusals above cannot have answered about, or written to,
    // whichever identifier the handler might have preferred.
    for (label, arn, body) in &before {
        let read = served(
            emulator.get_table_by_arn(REGION, arn).await,
            "GetTable by ARN after the refusals",
            label,
        );
        assert_eq!(
            &read.body, body,
            "case {label:?}: a refused read changed the stored record, got {actual}",
            actual = read.body
        );
    }
}

/// One removal case: where the table moves first, which keys the request names,
/// and which keys the table must carry afterwards.
struct Removal {
    label: &'static str,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    remove: &'static [&'static str],
    expected: &'static [&'static str],
}

/// The keys every table in RULE 2's cases starts out carrying.
const RESIDENT: &[&str] = &["cost_center", "owner", "stage"];

/// The rows, differing along where the table is when the keys are removed, how
/// many keys the request names, and whether a named key is resident at all.
const REMOVALS: &[Removal] = &[
    Removal {
        label: "one key of three, the table left where it is",
        new_namespace: None,
        new_name: None,
        remove: &["stage"],
        expected: &["cost_center", "owner"],
    },
    Removal {
        label: "one key of three, after the table is renamed in place",
        new_namespace: None,
        new_name: Some("renamed_table"),
        remove: &["stage"],
        expected: &["cost_center", "owner"],
    },
    Removal {
        label: "two keys of three, after the table moves to another namespace",
        new_namespace: Some(OTHER_NAMESPACE),
        new_name: None,
        remove: &["owner", "cost_center"],
        expected: &["stage"],
    },
    Removal {
        label: "every key, after the table is moved and renamed",
        new_namespace: Some(OTHER_NAMESPACE),
        new_name: Some("moved_table"),
        remove: RESIDENT,
        expected: &[],
    },
    Removal {
        label: "a key the table does not carry, after it is renamed",
        new_namespace: None,
        new_name: Some("renamed_again"),
        remove: &["absent_key"],
        expected: RESIDENT,
    },
];

/// RULE 2: the tag set a read reports after a removal is the resource's keys MINUS
/// exactly the ones named, through the ARN that keeps naming the table across a
/// move, and no other resource is touched.
#[tokio::test]
async fn a_tag_removal_takes_only_the_named_keys_from_the_table_its_arn_names() {
    for (index, row) in REMOVALS.iter().enumerate() {
        let label = row.label;
        let emulator = Emulator::new();
        let bucket_arn = table_bucket(
            &emulator,
            &format!("untag-across-move-{index}"),
            &[NAMESPACE, OTHER_NAMESPACE],
        )
        .await;
        let subject = table(&emulator, &bucket_arn, NAMESPACE, SUBJECT).await;
        let bystander = table(&emulator, &bucket_arn, NAMESPACE, BYSTANDER).await;

        // Both tables start out carrying the same keys, so the removal is shown
        // to take them from the one resource its ARN names.
        let resident = tag_map(RESIDENT);
        for resource in [&subject, &bystander] {
            served(
                emulator
                    .tag_resource(REGION, resource, &tags_body(&resident))
                    .await,
                "TagResource",
                label,
            );
        }

        // The token the caller holds for the subject table, which a rename
        // presents as its precondition.
        let held = served(
            emulator.get_table_by_arn(REGION, &subject).await,
            "GetTable by ARN",
            label,
        )
        .text("versionToken");
        assert_table_version_token(&held, "the token the create minted", label);

        let mut namespace = NAMESPACE.to_string();
        let mut name = SUBJECT.to_string();
        if row.new_namespace.is_some() || row.new_name.is_some() {
            let mut request = json!({ "versionToken": held });
            if let Some(moved_to) = row.new_namespace {
                request["newNamespaceName"] = json!(moved_to);
            }
            if let Some(renamed_to) = row.new_name {
                request["newName"] = json!(renamed_to);
            }
            let renamed = emulator
                .rename_table(REGION, &bucket_arn, &namespace, &name, &request)
                .await;
            assert_eq!(
                renamed.status, 204,
                "case {label:?}: the rename must be served, got {status} {body}",
                status = renamed.status,
                body = renamed.body
            );
            namespace = row.new_namespace.unwrap_or(NAMESPACE).to_string();
            name = row.new_name.unwrap_or(SUBJECT).to_string();
        }

        // Tags are the resource's, not its coordinates': the move left them as
        // they were.
        let carried = listed_tags(&emulator, REGION, &subject, label).await;
        assert_eq!(
            carried, resident,
            "case {label:?}: expected the moved table to still carry {resident:?}, got \
             {carried:?}"
        );

        let removed = emulator.untag_resource(REGION, &subject, row.remove).await;
        assert_eq!(
            removed.status, 204,
            "case {label:?}: the removal must be served, got {status} {body}",
            status = removed.status,
            body = removed.body
        );

        let expected = tag_map(row.expected);
        let after = listed_tags(&emulator, REGION, &subject, label).await;
        assert_eq!(
            after, expected,
            "case {label:?}: expected the tags {expected:?} after the removal, got {after:?}"
        );
        let untouched = listed_tags(&emulator, REGION, &bystander, label).await;
        assert_eq!(
            untouched, resident,
            "case {label:?}: expected another table's tags to be left alone, got {untouched:?}"
        );

        // And the table itself is the one the ARN named all along, still under
        // the token the caller holds: a tag write touches neither.
        let read = served(
            emulator.get_table(REGION, &bucket_arn, &namespace, &name).await,
            "GetTable at the table's coordinates",
            label,
        );
        assert_eq!(
            read.text("tableARN"),
            subject,
            "case {label:?}: expected the coordinates to reach {subject}, got {actual}",
            actual = read.text("tableARN")
        );
        assert_eq!(
            read.text("versionToken"),
            held,
            "case {label:?}: expected the table to keep the token {held:?}, got {actual:?}",
            actual = read.text("versionToken")
        );
    }
}

/// The metadata files RULE 3's chain of writes points the table at, in order: the
/// next file of the metadata directory, a file under a deeper prefix, and the
/// Gzip-compressed form ("If the metadata file is Gzip-compressed,
/// .metadata.json.gz" — `smithy_query(query="shape", shape="MetadataLocation")`).
const METADATA_FILES: &[&str] = &[
    "/metadata/00001-0f1e2d3c4b5a69788796a5b4c3d2e1f0.metadata.json",
    "/metadata/snapshots/2026/03/00002-aabbccddeeff00112233445566778899.metadata.json",
    "/metadata/00003-99887766554433221100ffeeddccbbaa.metadata.json.gz",
];

/// RULE 3: every write of the chain answers with the token the caller presented —
/// the table's own — and a token that is not the table's is refused and records
/// nothing.
#[tokio::test]
async fn successive_metadata_location_writes_answer_the_token_the_caller_still_holds() {
    let emulator = Emulator::new();
    let bucket_arn = table_bucket(&emulator, "metadata-location-chain", &[NAMESPACE]).await;
    table(&emulator, &bucket_arn, NAMESPACE, SUBJECT).await;

    let created = served(
        emulator
            .get_table(REGION, &bucket_arn, NAMESPACE, SUBJECT)
            .await,
        "GetTable",
        "the chain",
    );
    let held = created.text("versionToken");
    assert_table_version_token(&held, "the token the create minted", "the chain");
    let warehouse = created.text("warehouseLocation");

    let mut recorded = created.optional_text("metadataLocation");
    for file in METADATA_FILES {
        let case = format!("writing {file}");
        let location = format!("{warehouse}{file}");
        let updated = served(
            emulator
                .update_table_metadata_location(
                    REGION,
                    &bucket_arn,
                    NAMESPACE,
                    SUBJECT,
                    &json!({ "metadataLocation": location, "versionToken": held }),
                )
                .await,
            "UpdateTableMetadataLocation",
            &case,
        );
        assert_eq!(
            updated.text("versionToken"),
            held,
            "case {case:?}: expected the write to answer the token {held:?} the caller \
             presented, got {actual:?}",
            actual = updated.text("versionToken")
        );
        assert_eq!(
            updated.text("metadataLocation"),
            location,
            "case {case:?}: expected the write to record {location}, got {actual}",
            actual = updated.text("metadataLocation")
        );

        // Both reads of the table agree with the write: the pointer moved, the
        // token did not.
        let read = served(
            emulator
                .get_table_metadata_location(REGION, &bucket_arn, NAMESPACE, SUBJECT)
                .await,
            "GetTableMetadataLocation",
            &case,
        );
        assert_eq!(
            read.text("metadataLocation"),
            location,
            "case {case:?}: expected the read to report {location}, got {actual}",
            actual = read.text("metadataLocation")
        );
        assert_eq!(
            read.text("versionToken"),
            held,
            "case {case:?}: expected the read to report the token {held:?}, got {actual:?}",
            actual = read.text("versionToken")
        );
        let described = served(
            emulator
                .get_table(REGION, &bucket_arn, NAMESPACE, SUBJECT)
                .await,
            "GetTable",
            &case,
        );
        assert_eq!(
            described.text("versionToken"),
            held,
            "case {case:?}: expected GetTable to report the token {held:?}, got {actual:?}",
            actual = described.text("versionToken")
        );
        recorded = described.optional_text("metadataLocation");
        assert_eq!(
            recorded.as_deref(),
            Some(location.as_str()),
            "case {case:?}: expected GetTable to report {location}, got {recorded:?}"
        );
    }

    // A token that is not the table's is the modelled conflict, and the pointer
    // the chain settled stays where it is.
    let case = "a token the table does not carry";
    let stale = emulator
        .update_table_metadata_location(
            REGION,
            &bucket_arn,
            NAMESPACE,
            SUBJECT,
            &json!({
                "metadataLocation": format!("{warehouse}/metadata/00004-probe.metadata.json"),
                "versionToken": format!("{held}0"),
            }),
        )
        .await;
    assert_eq!(
        stale.status, 409,
        "case {case:?}: expected status 409, got {status} {body}",
        status = stale.status,
        body = stale.body
    );
    assert_eq!(
        stale.error_code.as_deref(),
        Some("ConflictException"),
        "case {case:?}: expected the modelled ConflictException, got {actual:?}",
        actual = stale.error_code
    );
    assert_eq!(
        stale.message(),
        TOKEN_MISMATCH,
        "case {case:?}: expected the fixed sentence, got {actual:?}",
        actual = stale.message()
    );
    let after = served(
        emulator
            .get_table_metadata_location(REGION, &bucket_arn, NAMESPACE, SUBJECT)
            .await,
        "GetTableMetadataLocation",
        case,
    );
    assert_eq!(
        after.optional_text("metadataLocation"),
        recorded,
        "case {case:?}: a refused write moved the pointer, got {actual:?}",
        actual = after.optional_text("metadataLocation")
    );
    assert_eq!(
        after.text("versionToken"),
        held,
        "case {case:?}: expected the table to keep the token {held:?}, got {actual:?}",
        actual = after.text("versionToken")
    );
}
