// SPDX-License-Identifier: CC0-1.0

//! `RenameTable`: what a move of a table does to the one record that holds it, and what
//! the reads of that record report afterwards.
//!
//! RULE 1 — a rename RE-KEYS the single stored table under the coordinates the request
//! names (`newNamespaceName`, `newName`, each left-out one keeping its current value):
//! the table resolves at its new namespace and name, resolves nowhere at the old ones,
//! and keeps its identity — the server-minted table id, and so the `tableARN` and the
//! warehouse location built from it — because the id identifies the table for the life of
//! the record whatever it is renamed to
//! (`smithy_query(query="operation", operation="RenameTable")`: PUT
//! `/tables/{tableBucketARN}/{namespace}/{name}/rename`, 204, no output members, so the
//! move is only observable through the reads).
//!
//! RULE 2 — a rename is a MUTATION of the table's coordinates only: it settles the
//! instant the table was last modified at and LEAVES `versionToken` as it was. The token
//! is the caller's precondition over the table's open-table metadata, and a move changes
//! nothing inside that metadata, so the token a caller read before the move still
//! describes the table afterwards and the next conditional call carrying it is honoured
//! (`smithy_query(query="operation", operation="RenameTable")` documents `versionToken`
//! only as "The version token of the table." and declares no output, so what rotates it
//! is the service's recorded behaviour: a metadata commit, not a rename).
//!
//! RULE 3 — a move the store rules out changes nothing and is refused as its class: a
//! request naming neither new coordinate is a `BadRequestException` (HTTP 400) because it
//! describes no move; coordinates another table already occupies, and a `versionToken`
//! that is not the table's current one, are each the conflict with a previous write
//! `ConflictException` (HTTP 409) names — the destination is settled first, so a move the
//! destination rules out is refused as such whatever view of the table the request
//! carries; a bucket, namespace or table the store does not hold is a `NotFoundException`
//! (HTTP 404).
//!
//! RULE 4 — the wire forms of a table summary hold on the listing a client reads AFTER a
//! rename, exactly as on any other listing: `createdAt`/`modifiedAt` are RFC 3339
//! date-time strings (`@timestampFormat("date-time")` on `TableSummary.createdAt`/
//! `.modifiedAt`) and the id segment after `/table/` in `tableARN` is a canonical
//! hyphenated UUID. `wire_forms.rs` states those two rules over the create-and-read
//! paths; this suite drives them over the post-rename and post-conflict listings.
//!
//! RULE 5 — a rename RE-KEYS the one stored record and changes nothing inside it, so
//! every fact that record holds besides its coordinates moves with it: the metadata file
//! the table is pointed at, the maintenance configuration a put settled on it, the
//! encryption it is stored under, its storage class and its tags are all reported for the
//! table at its new coordinates exactly as they were reported at its old ones, and each
//! of those name-addressed reads resolves nowhere at the coordinates the table left
//! (`GetTableMetadataLocation`, `GetTableMaintenanceConfiguration`, `GetTableEncryption`
//! and `GetTableStorageClass` all address a table by `tableBucketARN`/`namespace`/`name`,
//! and `RenameTable` declares no output, so the move is only observable through them).
//!
//! Each rule is asserted over a TABLE of cases spanning what it generalises over: which
//! coordinate moves (name, namespace, both), name length and form, whether the caller
//! presents its `versionToken`, how many tables share the namespace, which facts the
//! moved record holds, and every class of refusal.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, SseAlgorithm, StorageClass, TableMaintenanceType, BUCKET_NOT_FOUND_MESSAGE,
    NAMESPACE_NOT_FOUND_MESSAGE, TABLE_ALREADY_EXISTS_MESSAGE, TABLE_NOT_FOUND_MESSAGE,
    TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{expected_bucket_arn, served, timestamp_seconds, Emulator, Reply};
use crate::table_maintenance_configuration::{reported_task, Reported};
use crate::wire_forms::{assert_canonical_uuid, summary_of, table_id_of};

/// The region every case in this suite is scoped to; the ARN assertions follow the
/// bucket ARN the create reports, so the region is one choice and not a rule.
const REGION: &str = "us-west-2";

/// The namespace a case's tables are created in, and the second namespace a cross-
/// namespace move goes to. Both satisfy `NamespaceName`'s `^[0-9a-z_]*$`.
const SOURCE_NAMESPACE: &str = "rename_source";
const TARGET_NAMESPACE: &str = "rename_target";

/// The table a case moves, and the sibling that occupies a name in the same namespace.
const MOVED_TABLE: &str = "tbl_one";
const SIBLING_TABLE: &str = "tbl_two";

/// The message a rename that names neither new coordinate is refused with: the fixed
/// sentence the service states that class with, which the recorded refusal of such a
/// request carries.
const NOTHING_TO_RENAME_MESSAGE: &str =
    "Neither a new namespace name nor a new table name is specified.";

/// One table bucket holding two namespaces, with [`MOVED_TABLE`] and [`SIBLING_TABLE`]
/// created in the source namespace: the state every case below starts from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
}

impl Fixture {
    async fn new(bucket: &str, case: &str) -> Self {
        let emulator = Emulator::new();
        let created = emulator
            .create_table_bucket(REGION, &json!({ "name": bucket }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTableBucket: {created:?}"
        );
        let bucket_arn = created.text("arn");
        for namespace in [SOURCE_NAMESPACE, TARGET_NAMESPACE] {
            let reply = emulator
                .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [namespace] }))
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: CreateNamespace {namespace}: {reply:?}"
            );
        }
        for table in [MOVED_TABLE, SIBLING_TABLE] {
            let reply = emulator
                .create_table(
                    REGION,
                    &bucket_arn,
                    SOURCE_NAMESPACE,
                    // `format` is required and `ICEBERG` is the only value the modelled
                    // `OpenTableFormat` enum declares.
                    &json!({ "name": table, "format": OpenTableFormat::Iceberg.as_wire() }),
                )
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: CreateTable {table}: {reply:?}"
            );
        }
        Self {
            emulator,
            bucket_arn,
        }
    }

    /// The table as `GetTable` reports it at `namespace`/`name`.
    async fn read(&self, namespace: &str, name: &str) -> Reply {
        self.emulator
            .get_table(REGION, &self.bucket_arn, namespace, name)
            .await
    }

    /// Every row of `ListTables` over the whole bucket.
    async fn listing(&self) -> Vec<Value> {
        let listed = self
            .emulator
            .list_tables(REGION, &self.bucket_arn, &[])
            .await;
        assert_eq!(listed.status, 200, "ListTables: {listed:?}");
        listed.list("tables")
    }
}

/// One move: which coordinates the request names, and whether it presents the caller's
/// current `versionToken`.
#[derive(Debug, Clone, Copy)]
struct Move {
    label: &'static str,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    presents_version_token: bool,
}

/// The moves: they span which coordinate changes (the name, the namespace, both), the
/// length and form of the new name (the modelled one-character minimum, digits and
/// interior underscores, a long name) and whether the caller presents its version token.
const MOVES: &[Move] = &[
    Move {
        label: "the name only, to the shortest name there is",
        new_namespace: None,
        new_name: Some("t"),
        presents_version_token: false,
    },
    Move {
        label: "the name only, to a long digit-and-underscore name",
        new_namespace: None,
        new_name: Some("tbl_one_renamed_2026_q1_0123456789"),
        presents_version_token: true,
    },
    Move {
        label: "the namespace only",
        new_namespace: Some(TARGET_NAMESPACE),
        new_name: None,
        presents_version_token: false,
    },
    Move {
        label: "the namespace and the name together",
        new_namespace: Some(TARGET_NAMESPACE),
        new_name: Some("moved_tbl"),
        presents_version_token: true,
    },
    Move {
        label: "the namespace and the name together, token presented",
        new_namespace: Some(TARGET_NAMESPACE),
        new_name: Some("t0"),
        presents_version_token: true,
    },
];

/// The body a [`Move`] travels as.
fn rename_request(step: &Move, version_token: &str) -> Value {
    let mut request = json!({});
    if let Some(namespace) = step.new_namespace {
        request["newNamespaceName"] = json!(namespace);
    }
    if let Some(name) = step.new_name {
        request["newName"] = json!(name);
    }
    if step.presents_version_token {
        request["versionToken"] = json!(version_token);
    }
    request
}

/// RULES 1, 2 and 4: a rename re-keys the one stored table, keeps its identity, settles a
/// fresh version token and a new modification instant, and every read afterwards — the
/// describe at the new coordinates, the describe by ARN and the listing row — agrees.
#[tokio::test]
async fn a_rename_moves_the_one_stored_table_to_its_new_coordinates() {
    for (index, step) in MOVES.iter().enumerate() {
        let case = step.label;
        let fixture = Fixture::new(&format!("rename-moves-{index}"), case).await;

        let before = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
        assert_eq!(before.status, 200, "case {case:?}: GetTable: {before:?}");
        let table_arn = before.text("tableARN");
        let table_id = table_id_of(&table_arn, case);
        let version_token = before.text("versionToken");
        let warehouse = before.text("warehouseLocation");
        let created_at = timestamp_seconds(&before.body, "createdAt", case);
        let modified_at = timestamp_seconds(&before.body, "modifiedAt", case);

        let target_namespace = step.new_namespace.unwrap_or(SOURCE_NAMESPACE);
        let target_name = step.new_name.unwrap_or(MOVED_TABLE);
        let renamed = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                SOURCE_NAMESPACE,
                MOVED_TABLE,
                &rename_request(step, &version_token),
            )
            .await;
        assert_eq!(
            renamed.status, 204,
            "case {case:?}: RenameTable: expected 204, got {renamed:?}"
        );

        // RULE 1: the table is at its new coordinates, with the identity it was created
        // with, and nowhere at its old ones.
        let after = fixture.read(target_namespace, target_name).await;
        assert_eq!(
            after.status, 200,
            "case {case:?}: GetTable at the new coordinates: expected 200, got {after:?}"
        );
        assert_eq!(
            after.text("name"),
            target_name,
            "case {case:?}: expected name {target_name:?}, got {body}",
            body = after.body
        );
        assert_eq!(
            after.body.get("namespace"),
            Some(&json!([target_namespace])),
            "case {case:?}: expected namespace [{target_namespace:?}], got {body}",
            body = after.body
        );
        assert_eq!(
            after.text("tableARN"),
            table_arn,
            "case {case:?}: the move changed the table's ARN"
        );
        assert_eq!(
            after.text("warehouseLocation"),
            warehouse,
            "case {case:?}: the move changed the table's warehouse location"
        );
        assert_eq!(
            timestamp_seconds(&after.body, "createdAt", case),
            created_at,
            "case {case:?}: the move changed the instant the table was created at"
        );
        let stale = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
        assert_eq!(
            stale.status, 404,
            "case {case:?}: GetTable at the old coordinates: expected 404, got {stale:?}"
        );
        assert_eq!(
            stale.message(),
            TABLE_NOT_FOUND_MESSAGE,
            "case {case:?}: GetTable at the old coordinates: unexpected message {stale:?}"
        );
        // The ARN is the handle a client stored before the move, and it still resolves —
        // to the record at its new name.
        let by_arn = fixture.emulator.get_table_by_arn(REGION, &table_arn).await;
        assert_eq!(
            by_arn.status, 200,
            "case {case:?}: GetTable by ARN after the move: {by_arn:?}"
        );
        assert_eq!(
            by_arn.text("name"),
            target_name,
            "case {case:?}: GetTable by ARN reports a different name from the describe"
        );

        // RULE 2: the move settles a modification instant at or after the one the record
        // carried before it, and leaves the caller's precondition token untouched.
        let moved_token = after.text("versionToken");
        assert_eq!(
            moved_token, version_token,
            "case {case:?}: expected the pre-rename versionToken {version_token:?}, got \
             {moved_token:?}"
        );
        assert_canonical_uuid(&moved_token, "the versionToken a renamed table carries", case);
        let moved_modified_at = timestamp_seconds(&after.body, "modifiedAt", case);
        assert!(
            moved_modified_at >= modified_at,
            "case {case:?}: expected modifiedAt at or after {modified_at}, got \
             {moved_modified_at}"
        );

        // RULE 1 + RULE 4: the listing holds the moved table under its new coordinates
        // only, and its row carries the wire forms every summary row carries.
        let rows = fixture.listing().await;
        assert_eq!(
            rows.len(),
            2,
            "case {case:?}: expected the two created tables in the listing, got {rows:?}"
        );
        let row = summary_of(&rows, target_name, case);
        assert_eq!(
            row.get("namespace"),
            Some(&json!([target_namespace])),
            "case {case:?}: the listing row reports a different namespace: {row:?}"
        );
        assert_eq!(
            row.get("tableARN").and_then(Value::as_str),
            Some(table_arn.as_str()),
            "case {case:?}: the listing row reports a different tableARN: {row:?}"
        );
        assert_canonical_uuid(
            &table_id_of(table_arn.as_str(), case),
            "the id segment of the post-rename listing row's tableARN",
            case,
        );
        assert_eq!(
            table_id_of(table_arn.as_str(), case),
            table_id,
            "case {case:?}: the move changed the table's id"
        );
        for member in ["createdAt", "modifiedAt"] {
            // Reads the member from the ONE wire form the model declares for it and
            // panics on any other, so this is the date-time assertion for the row.
            timestamp_seconds(row, member, case);
        }
        assert!(
            rows.iter().all(|listed| {
                listed.get("name").and_then(Value::as_str) != Some(MOVED_TABLE)
                    || listed.get("namespace") != Some(&json!([SOURCE_NAMESPACE]))
            }),
            "case {case:?}: the listing still holds the table at its old coordinates: \
             {rows:?}"
        );

        // The moved table is deletable at the coordinates the client now holds, and the
        // namespaces and the bucket it leaves behind are then empty — the move left no
        // second copy anywhere in the store.
        let deleted = fixture
            .emulator
            .delete_table(REGION, &fixture.bucket_arn, target_namespace, target_name, &[])
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: DeleteTable at the new coordinates: {deleted:?}"
        );
        let sibling = fixture
            .emulator
            .delete_table(REGION, &fixture.bucket_arn, SOURCE_NAMESPACE, SIBLING_TABLE, &[])
            .await;
        assert_eq!(
            sibling.status, 204,
            "case {case:?}: DeleteTable of the sibling: {sibling:?}"
        );
        for namespace in [SOURCE_NAMESPACE, TARGET_NAMESPACE] {
            let dropped = fixture
                .emulator
                .delete_namespace(REGION, &fixture.bucket_arn, namespace)
                .await;
            assert_eq!(
                dropped.status, 204,
                "case {case:?}: DeleteNamespace {namespace} after the move: {dropped:?}"
            );
        }
        let bucket = fixture
            .emulator
            .delete_table_bucket(REGION, &fixture.bucket_arn)
            .await;
        assert_eq!(
            bucket.status, 204,
            "case {case:?}: DeleteTableBucket after the move: {bucket:?}"
        );
    }
}

/// How a request addresses the table it would move, and what it asks for.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    label: &'static str,
    /// The namespace path label, or `None` for the source namespace the table lives in.
    namespace: Option<&'static str>,
    /// The name path label, or `None` for the table the fixture created.
    name: Option<&'static str>,
    /// Whether the bucket ARN names a bucket the store does not hold.
    absent_bucket: bool,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    /// A `versionToken` that is not the table's current one, when the case presents one.
    stale_version_token: bool,
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// The refusals: one per class the rule names, including the two conflicts TOGETHER so
/// the order they are settled in is pinned, and every coordinate that can name something
/// the store does not hold.
const REFUSALS: &[Refusal] = &[
    Refusal {
        label: "neither a new namespace nor a new name",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: None,
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: NOTHING_TO_RENAME_MESSAGE,
    },
    Refusal {
        label: "a name the sibling table already occupies",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: Some(SIBLING_TABLE),
        stale_version_token: false,
        status: 409,
        code: "ConflictException",
        message: TABLE_ALREADY_EXISTS_MESSAGE,
    },
    Refusal {
        label: "the name the table already carries",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: Some(MOVED_TABLE),
        stale_version_token: false,
        status: 409,
        code: "ConflictException",
        message: TABLE_ALREADY_EXISTS_MESSAGE,
    },
    Refusal {
        label: "the namespace the table already lives in",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: Some(SOURCE_NAMESPACE),
        new_name: None,
        stale_version_token: false,
        status: 409,
        code: "ConflictException",
        message: TABLE_ALREADY_EXISTS_MESSAGE,
    },
    Refusal {
        label: "a version token that is not the table's",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: Some("free_name"),
        stale_version_token: true,
        status: 409,
        code: "ConflictException",
        message: TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
    },
    Refusal {
        label: "an occupied destination AND a stale version token",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: Some(SIBLING_TABLE),
        stale_version_token: true,
        status: 409,
        code: "ConflictException",
        message: TABLE_ALREADY_EXISTS_MESSAGE,
    },
    Refusal {
        label: "a table the namespace does not hold",
        namespace: None,
        name: Some("absent_table"),
        absent_bucket: false,
        new_namespace: None,
        new_name: Some("free_name"),
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: TABLE_NOT_FOUND_MESSAGE,
    },
    Refusal {
        label: "a source namespace the bucket does not hold",
        namespace: Some("absent_namespace"),
        name: None,
        absent_bucket: false,
        new_namespace: None,
        new_name: Some("free_name"),
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: NAMESPACE_NOT_FOUND_MESSAGE,
    },
    Refusal {
        label: "a destination namespace the bucket does not hold",
        namespace: None,
        name: None,
        absent_bucket: false,
        new_namespace: Some("absent_namespace"),
        new_name: None,
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: NAMESPACE_NOT_FOUND_MESSAGE,
    },
    Refusal {
        label: "a table bucket the store does not hold",
        namespace: None,
        name: None,
        absent_bucket: true,
        new_namespace: None,
        new_name: Some("free_name"),
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: BUCKET_NOT_FOUND_MESSAGE,
    },
];

/// RULE 3: every class of move the store rules out is refused with its own status, error
/// code and fixed sentence, and the table is exactly where it was before the request.
#[tokio::test]
async fn a_refused_rename_states_its_class_and_moves_nothing() {
    for (index, refusal) in REFUSALS.iter().enumerate() {
        let case = refusal.label;
        let fixture = Fixture::new(&format!("rename-refusals-{index}"), case).await;

        let before = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
        assert_eq!(before.status, 200, "case {case:?}: GetTable: {before:?}");
        let version_token = before.text("versionToken");
        let table_arn = before.text("tableARN");
        let modified_at = timestamp_seconds(&before.body, "modifiedAt", case);

        let mut request = json!({});
        if let Some(namespace) = refusal.new_namespace {
            request["newNamespaceName"] = json!(namespace);
        }
        if let Some(name) = refusal.new_name {
            request["newName"] = json!(name);
        }
        if refusal.stale_version_token {
            // The sibling's token: a real token of this service's own minting that is
            // not the token of the table being moved.
            let sibling = fixture.read(SOURCE_NAMESPACE, SIBLING_TABLE).await;
            assert_eq!(sibling.status, 200, "case {case:?}: GetTable: {sibling:?}");
            request["versionToken"] = json!(sibling.text("versionToken"));
        }
        let bucket_arn = if refusal.absent_bucket {
            expected_bucket_arn(REGION, "rename-absent-bucket")
        } else {
            fixture.bucket_arn.clone()
        };
        let refused = fixture
            .emulator
            .rename_table(
                REGION,
                &bucket_arn,
                refusal.namespace.unwrap_or(SOURCE_NAMESPACE),
                refusal.name.unwrap_or(MOVED_TABLE),
                &request,
            )
            .await;
        assert_eq!(
            refused.status, refusal.status,
            "case {case:?}: expected status {}, got {refused:?}",
            refusal.status
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(refusal.code),
            "case {case:?}: expected error code {:?}, got {refused:?}",
            refusal.code
        );
        assert_eq!(
            refused.message(),
            refusal.message,
            "case {case:?}: expected message {:?}, got {actual:?}",
            refusal.message,
            actual = refused.message()
        );

        // Nothing moved and nothing was touched: the table is at its old coordinates with
        // the token and the modification instant it had, and the sibling is still there.
        let after = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
        assert_eq!(
            after.status, 200,
            "case {case:?}: the table is gone after a refused rename: {after:?}"
        );
        assert_eq!(
            after.text("versionToken"),
            version_token,
            "case {case:?}: a refused rename settled a new versionToken"
        );
        assert_eq!(
            after.text("tableARN"),
            table_arn,
            "case {case:?}: a refused rename changed the table's ARN"
        );
        assert_eq!(
            timestamp_seconds(&after.body, "modifiedAt", case),
            modified_at,
            "case {case:?}: a refused rename changed the instant the table was last \
             modified at"
        );
        let sibling = fixture.read(SOURCE_NAMESPACE, SIBLING_TABLE).await;
        assert_eq!(
            sibling.status, 200,
            "case {case:?}: the sibling table is gone after a refused rename: {sibling:?}"
        );
        let rows = fixture.listing().await;
        assert_eq!(
            rows.len(),
            2,
            "case {case:?}: expected the two created tables in the listing, got {rows:?}"
        );
        // RULE 4 on the listing a client reads after a refused rename — the path the
        // conflict matrix re-lists on.
        for table in [MOVED_TABLE, SIBLING_TABLE] {
            let row = summary_of(&rows, table, case);
            for member in ["createdAt", "modifiedAt"] {
                timestamp_seconds(row, member, case);
            }
            let row_arn = row
                .get("tableARN")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: listing row {row:?} has no tableARN"));
            assert_canonical_uuid(
                &table_id_of(row_arn, case),
                "the id segment of a post-conflict listing row's tableARN",
                case,
            );
        }
    }
}

/// RULES 1 and 2 as an INVARIANT over a chain of moves: however many times a table is
/// renamed, there is still exactly ONE record, its identity never changes, every move
/// leaves the token the first read reported — so each move can present the token the
/// caller has held all along — and the coordinates it is reachable at are the ones the
/// last move named.
#[tokio::test]
async fn a_chain_of_renames_keeps_one_record_with_one_identity() {
    // Each step moves the table again: within the namespace, across to the second
    // namespace, back, and to a one-character name.
    let chain: [(&str, &str); 4] = [
        (SOURCE_NAMESPACE, "step_one"),
        (TARGET_NAMESPACE, "step_two"),
        (SOURCE_NAMESPACE, "step_three"),
        (TARGET_NAMESPACE, "x"),
    ];
    let case = "a chain of four renames";
    let fixture = Fixture::new("rename-chain", case).await;

    let first = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
    assert_eq!(first.status, 200, "case {case:?}: GetTable: {first:?}");
    let table_arn = first.text("tableARN");
    let created_at = timestamp_seconds(&first.body, "createdAt", case);
    // The one token the caller holds for the whole chain: no move rotates it, so every
    // move in the chain presents this same value as its precondition.
    let version_token = first.text("versionToken");
    let mut at = (SOURCE_NAMESPACE, MOVED_TABLE);

    for (namespace, name) in chain {
        let renamed = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                at.0,
                at.1,
                &json!({
                    "newNamespaceName": namespace,
                    "newName": name,
                    "versionToken": version_token,
                }),
            )
            .await;
        assert_eq!(
            renamed.status, 204,
            "case {case:?}: RenameTable to {namespace}/{name}: {renamed:?}"
        );

        let read = fixture.read(namespace, name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTable at {namespace}/{name}: {read:?}"
        );
        assert_eq!(
            read.text("tableARN"),
            table_arn,
            "case {case:?}: the move to {namespace}/{name} changed the table's ARN"
        );
        assert_eq!(
            timestamp_seconds(&read.body, "createdAt", case),
            created_at,
            "case {case:?}: the move to {namespace}/{name} changed createdAt"
        );
        let token = read.text("versionToken");
        assert_eq!(
            token, version_token,
            "case {case:?}: the move to {namespace}/{name} rotated the version token to \
             {token:?}, expected {version_token:?}"
        );

        // Exactly one record: the two created tables, no copy left behind at any
        // coordinate the chain passed through.
        let rows = fixture.listing().await;
        assert_eq!(
            rows.len(),
            2,
            "case {case:?}: expected one record for the moved table and one for the \
             sibling after the move to {namespace}/{name}, got {rows:?}"
        );
        let row = summary_of(&rows, name, case);
        assert_eq!(
            row.get("namespace"),
            Some(&json!([namespace])),
            "case {case:?}: the listing row reports a different namespace: {row:?}"
        );
        at = (namespace, name);
    }
}

/// One client workflow: the coordinates each of two successive moves names, with the ONE
/// `versionToken` the client read before the first move presented on both of them and on
/// the delete that ends the workflow.
#[derive(Debug, Clone, Copy)]
struct Workflow {
    label: &'static str,
    first_namespace: Option<&'static str>,
    first_name: Option<&'static str>,
    second_namespace: Option<&'static str>,
    second_name: Option<&'static str>,
}

/// The workflows: they span which coordinate each move names (the name only, the
/// namespace only, both) and so every order two moves can take.
const WORKFLOWS: &[Workflow] = &[
    Workflow {
        label: "the name, then the namespace",
        first_namespace: None,
        first_name: Some("moved_once"),
        second_namespace: Some(TARGET_NAMESPACE),
        second_name: None,
    },
    Workflow {
        label: "the namespace, then the name",
        first_namespace: Some(TARGET_NAMESPACE),
        first_name: None,
        second_namespace: None,
        second_name: Some("moved_twice"),
    },
    Workflow {
        label: "both coordinates, then both again",
        first_namespace: Some(TARGET_NAMESPACE),
        first_name: Some("t0"),
        second_namespace: Some(SOURCE_NAMESPACE),
        second_name: Some("t1"),
    },
    Workflow {
        label: "the name, then the name again",
        first_namespace: None,
        first_name: Some("m"),
        second_namespace: None,
        second_name: Some("m_0123456789_renamed"),
    },
];

/// RULE 2 end to end: a rename does not invalidate the precondition token the client
/// holds, so the whole workflow a client runs with the token it read ONCE is honoured —
/// a second conditional rename, the conditional delete at the coordinates the last move
/// named, and then the teardown of the namespace the table left and of the bucket.
#[tokio::test]
async fn the_token_a_client_read_before_a_rename_still_guards_the_table_afterwards() {
    for (index, flow) in WORKFLOWS.iter().enumerate() {
        let case = flow.label;
        let fixture = Fixture::new(&format!("rename-workflow-{index}"), case).await;

        let before = fixture.read(SOURCE_NAMESPACE, MOVED_TABLE).await;
        assert_eq!(before.status, 200, "case {case:?}: GetTable: {before:?}");
        // The one token the client ever reads: everything below presents it.
        let version_token = before.text("versionToken");
        let table_arn = before.text("tableARN");

        let first_namespace = flow.first_namespace.unwrap_or(SOURCE_NAMESPACE);
        let first_name = flow.first_name.unwrap_or(MOVED_TABLE);
        let mut request = json!({ "versionToken": version_token });
        if let Some(namespace) = flow.first_namespace {
            request["newNamespaceName"] = json!(namespace);
        }
        if let Some(name) = flow.first_name {
            request["newName"] = json!(name);
        }
        let first = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                SOURCE_NAMESPACE,
                MOVED_TABLE,
                &request,
            )
            .await;
        assert_eq!(
            first.status, 204,
            "case {case:?}: the first RenameTable: expected 204, got {first:?}"
        );

        let second_namespace = flow.second_namespace.unwrap_or(first_namespace);
        let second_name = flow.second_name.unwrap_or(first_name);
        let mut request = json!({ "versionToken": version_token });
        if let Some(namespace) = flow.second_namespace {
            request["newNamespaceName"] = json!(namespace);
        }
        if let Some(name) = flow.second_name {
            request["newName"] = json!(name);
        }
        let second = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                first_namespace,
                first_name,
                &request,
            )
            .await;
        assert_eq!(
            second.status, 204,
            "case {case:?}: the second RenameTable, presenting the token the client read \
             before the first move: expected 204, got {second:?}"
        );

        // The table the client has been moving is the one record it created, at the
        // coordinates the last move named, still carrying that token.
        let after = fixture.read(second_namespace, second_name).await;
        assert_eq!(
            after.status, 200,
            "case {case:?}: GetTable at {second_namespace}/{second_name}: expected 200, \
             got {after:?}"
        );
        assert_eq!(
            after.text("tableARN"),
            table_arn,
            "case {case:?}: the two moves changed the table's ARN"
        );
        assert_eq!(
            after.text("versionToken"),
            version_token,
            "case {case:?}: expected the token the client read, got {after:?}"
        );

        // The conditional delete the client ends with: its token is the one it read
        // before either move, so the delete is honoured and the record is gone.
        let deleted = fixture
            .emulator
            .delete_table(
                REGION,
                &fixture.bucket_arn,
                second_namespace,
                second_name,
                &[("versionToken", version_token.clone())],
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: DeleteTable presenting the token the client read: expected \
             204, got {deleted:?}"
        );
        let gone = fixture.read(second_namespace, second_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: GetTable after the delete: expected 404, got {gone:?}"
        );

        // Teardown converges: the namespaces the table passed through hold nothing of
        // it, so each is deletable and so is the bucket.
        let sibling = fixture
            .emulator
            .delete_table(REGION, &fixture.bucket_arn, SOURCE_NAMESPACE, SIBLING_TABLE, &[])
            .await;
        assert_eq!(
            sibling.status, 204,
            "case {case:?}: DeleteTable of the sibling: {sibling:?}"
        );
        for namespace in [SOURCE_NAMESPACE, TARGET_NAMESPACE] {
            let dropped = fixture
                .emulator
                .delete_namespace(REGION, &fixture.bucket_arn, namespace)
                .await;
            assert_eq!(
                dropped.status, 204,
                "case {case:?}: DeleteNamespace {namespace} after the moves: {dropped:?}"
            );
        }
        let bucket = fixture
            .emulator
            .delete_table_bucket(REGION, &fixture.bucket_arn)
            .await;
        assert_eq!(
            bucket.status, 204,
            "case {case:?}: DeleteTableBucket after the moves: {bucket:?}"
        );
    }
}

/// One case of [`a_rename_carries_every_fact_the_moved_record_holds`]: which coordinates
/// the move names, and what the record it moves holds.
#[derive(Debug, Clone, Copy)]
struct Carried {
    label: &'static str,
    new_namespace: Option<&'static str>,
    new_name: Option<&'static str>,
    /// Whether the create describes the table's open-table metadata, so the record holds
    /// a metadata document and a metadata file an `UpdateTableMetadataLocation` then
    /// moves. The cases span both, because `metadataLocation` is an OPTIONAL output
    /// member of `GetTableMetadataLocation` — absent until a metadata file exists for the
    /// table — and because a table the control plane holds and has no metadata document
    /// for is no table of the Iceberg REST catalog.
    holds_metadata: bool,
}

/// The cases: they span which coordinate the move names — the name, the namespace, both —
/// the length and form of the new name, and whether the moved record holds open-table
/// metadata at all.
const CARRIED: &[Carried] = &[
    Carried {
        label: "the name only, on a record that holds a metadata file",
        new_namespace: None,
        new_name: Some("carried_tbl_0123456789"),
        holds_metadata: true,
    },
    Carried {
        label: "the namespace only, on a record that holds a metadata file",
        new_namespace: Some(TARGET_NAMESPACE),
        new_name: None,
        holds_metadata: true,
    },
    Carried {
        label: "the namespace and the name together",
        new_namespace: Some(TARGET_NAMESPACE),
        new_name: Some("c"),
        holds_metadata: true,
    },
    Carried {
        label: "the name only, on a record that holds no metadata file",
        new_namespace: None,
        new_name: Some("c0"),
        holds_metadata: false,
    },
];

/// The table each case moves: created beside the two the fixture holds, so the move is
/// graded on a record whose own configuration the case settled.
const CARRIED_TABLE: &str = "carried_tbl";

/// The metadata file the cases that hold one point the table at, as a path under the
/// table's own warehouse location — the only place `UpdateTableMetadataLocation` accepts a
/// metadata file at (`table_metadata_location.rs` is what pins that rule).
const CARRIED_METADATA_FILE: &str =
    "/metadata/00001-3f6c1b0e8a2d4e5f9b7c8d9e0f112233.metadata.json";

/// The schema the cases that describe the table's metadata create it with: one field, in
/// the member names the model gives them (`smithy_query(query="shape",
/// shape="IcebergSchema,SchemaField")`: a required `fields` list of `name`/`type`, with an
/// optional `required`).
fn carried_schema() -> Value {
    json!({ "fields": [{ "name": "event_id", "type": "long", "required": true }] })
}

/// The compaction target file size each case settles on the table: inside the window S3
/// Tables compacts towards, and not the 512 MB default, so a read after the move reports
/// what the put persisted rather than the default a task with no configuration runs under.
const CARRIED_TARGET_FILE_SIZE_MB: i64 = 128;

/// The compaction strategy each case settles: a modeled
/// `com.amazonaws.s3tables#IcebergCompactionStrategy` value that needs no sort order in
/// the table's metadata, and not the `auto` default.
const CARRIED_STRATEGY: &str = "binpack";

/// The maintenance status each case settles: the modeled `MaintenanceStatus` value that is
/// not the default a task runs under.
const CARRIED_STATUS: &str = "disabled";

/// The tags each case attaches to the table before it moves.
fn carried_tags() -> Value {
    json!({ "environment": "production", "owner": "analytics" })
}

/// The `value` member of the `PutTableMaintenanceConfiguration` each case sends.
fn carried_maintenance() -> Value {
    json!({
        "status": CARRIED_STATUS,
        "settings": {
            "icebergCompaction": {
                "targetFileSizeMB": CARRIED_TARGET_FILE_SIZE_MB,
                "strategy": CARRIED_STRATEGY,
            },
        },
    })
}

/// What the name-addressed reads of one table's own configuration report for it: the body
/// of each, so a comparison across a move names the read that disagrees.
struct Facts {
    metadata_location: Reply,
    maintenance: Reply,
    encryption: Reply,
    storage_class: Reply,
}

impl Facts {
    /// Every read, at the coordinates given, each asserted to be served.
    async fn read(emulator: &Emulator, bucket_arn: &str, at: (&str, &str), case: &str) -> Self {
        let (namespace, name) = at;
        Self {
            metadata_location: served(
                emulator
                    .get_table_metadata_location(REGION, bucket_arn, namespace, name)
                    .await,
                "GetTableMetadataLocation",
                case,
            ),
            maintenance: served(
                emulator
                    .get_table_maintenance_configuration(REGION, bucket_arn, namespace, name)
                    .await,
                "GetTableMaintenanceConfiguration",
                case,
            ),
            encryption: served(
                emulator
                    .get_table_encryption(REGION, bucket_arn, namespace, name)
                    .await,
                "GetTableEncryption",
                case,
            ),
            storage_class: served(
                emulator
                    .get_table_storage_class(REGION, bucket_arn, namespace, name)
                    .await,
                "GetTableStorageClass",
                case,
            ),
        }
    }

    /// Each read of `self` beside the one `other` answers for the same operation, so a
    /// comparison across a move is of one operation's two answers and names which read
    /// disagrees.
    fn against<'a>(&'a self, other: &'a Self) -> [(&'static str, &'a Reply, &'a Reply); 4] {
        [
            (
                "GetTableMetadataLocation",
                &self.metadata_location,
                &other.metadata_location,
            ),
            (
                "GetTableMaintenanceConfiguration",
                &self.maintenance,
                &other.maintenance,
            ),
            ("GetTableEncryption", &self.encryption, &other.encryption),
            (
                "GetTableStorageClass",
                &self.storage_class,
                &other.storage_class,
            ),
        ]
    }
}

/// RULE 5: a move re-keys the record and changes nothing inside it, so every read of the
/// table's own configuration answers at the new coordinates what it answered at the old
/// ones, and answers nothing at all at the coordinates the table left.
#[tokio::test]
async fn a_rename_carries_every_fact_the_moved_record_holds() {
    for (index, case) in CARRIED.iter().enumerate() {
        let label = case.label;
        let fixture = Fixture::new(&format!("rename-carries-{index}"), label).await;
        let bucket_arn = fixture.bucket_arn.clone();
        let emulator = &fixture.emulator;

        // The table this case moves, created in the source namespace beside the two the
        // fixture holds — and created with a storage class and an encryption configuration
        // that are NOT the service defaults, so each read of them is a fact this record
        // carries rather than the answer any record would give.
        let key_arn = emulator.create_kms_key(REGION).await;
        let mut create = json!({
            "name": CARRIED_TABLE,
            "format": OpenTableFormat::Iceberg.as_wire(),
            "encryptionConfiguration": {
                "sseAlgorithm": SseAlgorithm::AwsKms.as_wire(),
                "kmsKeyArn": key_arn,
            },
            "storageClassConfiguration": {
                "storageClass": StorageClass::IntelligentTiering.as_wire(),
            },
        });
        if case.holds_metadata {
            create["metadata"] = json!({ "iceberg": { "schema": carried_schema() } });
        }
        let created = emulator
            .create_table(REGION, &bucket_arn, SOURCE_NAMESPACE, &create)
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateTable {CARRIED_TABLE}: {created:?}"
        );
        let table_arn = created.text("tableARN");

        // The facts the record holds before it moves: a maintenance configuration a put
        // settled on the table, tags a caller attached to it, and — on the cases that
        // hold one — the metadata file the table is pointed at.
        let put = emulator
            .put_table_maintenance_configuration(
                REGION,
                &bucket_arn,
                SOURCE_NAMESPACE,
                CARRIED_TABLE,
                TableMaintenanceType::IcebergCompaction.as_wire(),
                &carried_maintenance(),
            )
            .await;
        assert_eq!(
            put.status, 204,
            "case {label:?}: PutTableMaintenanceConfiguration: {put:?}"
        );
        let tagged = emulator
            .tag_resource(REGION, &table_arn, &carried_tags())
            .await;
        assert_eq!(tagged.status, 200, "case {label:?}: TagResource: {tagged:?}");

        let mut expected_location = None;
        if case.holds_metadata {
            let before = served(
                emulator
                    .get_table_metadata_location(
                        REGION,
                        &bucket_arn,
                        SOURCE_NAMESPACE,
                        CARRIED_TABLE,
                    )
                    .await,
                "GetTableMetadataLocation",
                label,
            );
            let location = format!(
                "{warehouse}{CARRIED_METADATA_FILE}",
                warehouse = before.text("warehouseLocation")
            );
            let updated = emulator
                .update_table_metadata_location(
                    REGION,
                    &bucket_arn,
                    SOURCE_NAMESPACE,
                    CARRIED_TABLE,
                    &json!({
                        "metadataLocation": location,
                        "versionToken": before.text("versionToken"),
                    }),
                )
                .await;
            assert_eq!(
                updated.status, 200,
                "case {label:?}: UpdateTableMetadataLocation: {updated:?}"
            );
            expected_location = Some(location);
        }

        // The baseline: what each read answers while the table is still where it was
        // created, with the configuration the put settled actually in force — so the
        // comparison after the move is against a record that holds something.
        let before =
            Facts::read(emulator, &bucket_arn, (SOURCE_NAMESPACE, CARRIED_TABLE), label).await;
        assert_eq!(
            before.metadata_location.optional_text("metadataLocation"),
            expected_location,
            "case {label:?}: GetTableMetadataLocation before the move: expected \
             {expected_location:?}, got {reply:?}",
            reply = before.metadata_location
        );
        let expected_compaction = Reported::Compaction(
            Some(CARRIED_STATUS.to_string()),
            Some(CARRIED_TARGET_FILE_SIZE_MB),
            Some(CARRIED_STRATEGY.to_string()),
        );
        let settled = reported_task(
            &before.maintenance,
            label,
            TableMaintenanceType::IcebergCompaction,
        );
        assert_eq!(
            settled, expected_compaction,
            "case {label:?}: before the move the put must be in force: expected \
             {expected_compaction:?}, got {settled:?}"
        );
        // The two configurations the create settled, so the comparison across the move is
        // of a carried fact and not of one default against another.
        assert_eq!(
            before
                .encryption
                .body
                .get("encryptionConfiguration")
                .and_then(|configuration| configuration.get("sseAlgorithm"))
                .and_then(Value::as_str),
            Some(SseAlgorithm::AwsKms.as_wire()),
            "case {label:?}: before the move GetTableEncryption must report the \
             configuration the create named: {reply:?}",
            reply = before.encryption
        );
        assert_eq!(
            before
                .storage_class
                .body
                .get("storageClassConfiguration")
                .and_then(|configuration| configuration.get("storageClass"))
                .and_then(Value::as_str),
            Some(StorageClass::IntelligentTiering.as_wire()),
            "case {label:?}: before the move GetTableStorageClass must report the class the \
             create named: {reply:?}",
            reply = before.storage_class
        );

        // The token the caller holds for the record it is about to move: the metadata
        // write advanced it, so this is the one the conditional rename presents.
        let current = fixture.read(SOURCE_NAMESPACE, CARRIED_TABLE).await;
        assert_eq!(current.status, 200, "case {label:?}: GetTable: {current:?}");
        let version_token = current.text("versionToken");

        let target_namespace = case.new_namespace.unwrap_or(SOURCE_NAMESPACE);
        let target_name = case.new_name.unwrap_or(CARRIED_TABLE);
        let mut request = json!({ "versionToken": version_token });
        if let Some(namespace) = case.new_namespace {
            request["newNamespaceName"] = json!(namespace);
        }
        if let Some(name) = case.new_name {
            request["newName"] = json!(name);
        }
        let renamed = emulator
            .rename_table(
                REGION,
                &bucket_arn,
                SOURCE_NAMESPACE,
                CARRIED_TABLE,
                &request,
            )
            .await;
        assert_eq!(
            renamed.status, 204,
            "case {label:?}: RenameTable: expected 204, got {renamed:?}"
        );

        // Every read answers for the table at its new coordinates what it answered for it
        // at its old ones: the move re-keyed the record and changed nothing in it.
        let after = Facts::read(emulator, &bucket_arn, (target_namespace, target_name), label).await;
        for (operation, expected, actual) in before.against(&after) {
            assert_eq!(
                actual.body, expected.body,
                "case {label:?}: {operation} at {target_namespace}/{target_name} must report \
                 what it reported before the move: expected {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(
            after.metadata_location.optional_text("metadataLocation"),
            expected_location,
            "case {label:?}: the metadata file the moved table is pointed at: expected \
             {expected_location:?}, got {reply:?}",
            reply = after.metadata_location
        );
        let carried = reported_task(
            &after.maintenance,
            label,
            TableMaintenanceType::IcebergCompaction,
        );
        assert_eq!(
            carried, expected_compaction,
            "case {label:?}: the maintenance configuration the moved table runs under: \
             expected {expected_compaction:?}, got {carried:?}"
        );

        // And nothing answers for the coordinates the table left: there is one record,
        // not a copy at each end of the move.
        for (operation, reply) in [
            (
                "GetTableMetadataLocation",
                emulator
                    .get_table_metadata_location(
                        REGION,
                        &bucket_arn,
                        SOURCE_NAMESPACE,
                        CARRIED_TABLE,
                    )
                    .await,
            ),
            (
                "GetTableMaintenanceConfiguration",
                emulator
                    .get_table_maintenance_configuration(
                        REGION,
                        &bucket_arn,
                        SOURCE_NAMESPACE,
                        CARRIED_TABLE,
                    )
                    .await,
            ),
            (
                "GetTableEncryption",
                emulator
                    .get_table_encryption(REGION, &bucket_arn, SOURCE_NAMESPACE, CARRIED_TABLE)
                    .await,
            ),
            (
                "GetTableStorageClass",
                emulator
                    .get_table_storage_class(REGION, &bucket_arn, SOURCE_NAMESPACE, CARRIED_TABLE)
                    .await,
            ),
        ] {
            assert_eq!(
                reply.status, 404,
                "case {label:?}: {operation} at the coordinates the table left: expected 404, \
                 got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                TABLE_NOT_FOUND_MESSAGE,
                "case {label:?}: {operation} at the coordinates the table left: unexpected \
                 message {reply:?}"
            );
        }

        // The tags are the record's too, and the ARN they are addressed by is the one the
        // move left alone, so the moved table still answers for them.
        let tags = emulator.list_tags_for_resource(REGION, &table_arn).await;
        assert_eq!(
            tags.status, 200,
            "case {label:?}: ListTagsForResource after the move: {tags:?}"
        );
        assert_eq!(
            tags.body.get("tags"),
            Some(&carried_tags()),
            "case {label:?}: the tags the moved table carries: {tags:?}"
        );

        // The Iceberg REST catalog resolves the same record at the coordinates the move
        // named, and nothing at the ones it left — and a record that holds no metadata
        // document is no table of that catalog at either end of the move, because the
        // catalog serves a table out of its open-table metadata and a table that has none
        // has no document to serve (`iceberg_rest_catalog.rs` drives the load of such a
        // table, which the catalog refuses with its own no-such-table error).
        let resolves = if case.holds_metadata { 204 } else { 404 };
        let exists = emulator
            .table_exists(REGION, &bucket_arn, target_namespace, target_name)
            .await;
        assert_eq!(
            exists.status, resolves,
            "case {label:?}: the catalog at {target_namespace}/{target_name}: expected \
             {resolves}, got {exists:?}"
        );
        let gone = emulator
            .table_exists(REGION, &bucket_arn, SOURCE_NAMESPACE, CARRIED_TABLE)
            .await;
        assert_eq!(
            gone.status, 404,
            "case {label:?}: the catalog must resolve nothing at the coordinates the table \
             left, got {gone:?}"
        );
    }
}
