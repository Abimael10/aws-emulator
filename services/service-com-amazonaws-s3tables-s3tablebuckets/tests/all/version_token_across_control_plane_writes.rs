// SPDX-License-Identifier: CC0-1.0

//! What a RUN of control-plane writes does to the one precondition token a client read
//! once, and what finally supersedes it.
//!
//! The rule under test: a table's `versionToken` is the optimistic-concurrency token over
//! the open-table metadata document THIS SERVICE holds for the table, so a write that
//! touches only control-plane state — the coordinates the table is addressed by, a
//! maintenance configuration, the tags on it, the pointer to a metadata file the caller
//! supplied — leaves the token exactly as it was however many such writes are made and in
//! whatever order, and only a catalog commit that writes a NEW metadata document re-mints
//! it. The token a client read before such a run is therefore still the table's
//! precondition afterwards, and the one that is superseded is refused as the modelled
//! conflict while the current one is honoured.
//!
//! The model leaves this to recorded behaviour: `versionToken` is a plain
//! `@length(1, 2048)` string, required on `GetTable`'s output and optional on the
//! mutators' input, documented only as "The version token of the table."
//! (`smithy_query(query="operation", operation="GetTable,RenameTable,DeleteTable")`), so
//! nothing in the model says which write rotates it.
//!
//! `table_version_token_lifecycle.rs` states the rule one write at a time, each from a
//! freshly created table. This suite drives it as a client does: a CHAIN of control-plane
//! writes on one table, with the token asserted after every step, and then the conditional
//! `DeleteTable` pair that the token exists for. The rows span what the rule generalises
//! over — which writes the run is made of, the order they are made in, which coordinate a
//! rename inside the run moves, whether a rename presents the token at all, and how many
//! writes come before the commit.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, TableMaintenanceType,
};

use crate::harness::{Emulator, Reply};
use crate::wire_forms::assert_table_version_token;

/// The region every row works in; every assertion follows the values the creates report,
/// so the region is one choice and not a rule.
const REGION: &str = "us-east-1";

/// The namespace a row's table is created in, and the second namespace a cross-namespace
/// rename inside a run moves it to. Both satisfy the modelled `^[0-9a-z_]*$`.
const HOME_NAMESPACE: &str = "token_home";
const AWAY_NAMESPACE: &str = "token_away";

/// The name the table is created under.
const TABLE: &str = "token_chain_tbl";

/// How this service states a presented `versionToken` that is not the one the table it
/// addresses currently carries: the fixed sentence of the modelled `ConflictException`
/// (HTTP 409), carrying no token, no table name and no other variable part. Spelled out
/// here rather than read from the crate under test, so this suite pins the sentence
/// itself.
const TABLE_VERSION_TOKEN_MISMATCH: &str =
    "Provided version token does not match the table version token.";

/// One control-plane write in a run. None of them writes the table's metadata document,
/// so none of them may touch the token.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// A plain re-read of the table: a read may change nothing a later caller sees.
    Reread,
    /// `PutTableMaintenanceConfiguration` for one task: service configuration of the
    /// table, outside its metadata document.
    Maintenance {
        task: TableMaintenanceType,
        status: &'static str,
    },
    /// `RenameTable`, moving the name, the namespace or both, presenting the token the
    /// client holds when `with_token` is set (the member is optional on this operation).
    Rename {
        new_namespace: Option<&'static str>,
        new_name: Option<&'static str>,
        with_token: bool,
    },
    /// `TagResource` on the table's ARN, attaching this key.
    Tag(&'static str),
    /// `UntagResource` of a key a previous step attached.
    Untag(&'static str),
    /// `UpdateTableMetadataLocation`: the caller tells the service which metadata file it
    /// wrote, under the token it holds as the precondition. The service records a pointer
    /// the caller supplied; it writes no metadata document of its own.
    MetadataPointer(&'static str),
}

/// One row: a run of control-plane writes made on one table, in this order.
#[derive(Debug, Clone, Copy)]
struct Run {
    label: &'static str,
    steps: &'static [Step],
}

/// The runs.
const RUNS: &[Run] = &[
    Run {
        label: "a re-read, a maintenance put, a rename of the name and tags",
        steps: &[
            Step::Reread,
            Step::Maintenance {
                task: TableMaintenanceType::IcebergCompaction,
                status: "disabled",
            },
            Step::Rename {
                new_namespace: None,
                new_name: Some("token_chain_renamed"),
                with_token: false,
            },
            Step::Tag("stage"),
        ],
    },
    Run {
        label: "the rename first, across namespaces and presenting the token, then the rest",
        steps: &[
            Step::Rename {
                new_namespace: Some(AWAY_NAMESPACE),
                new_name: Some("t0"),
                with_token: true,
            },
            Step::Maintenance {
                task: TableMaintenanceType::IcebergSnapshotManagement,
                status: "enabled",
            },
            Step::Tag("owner"),
            Step::Untag("owner"),
        ],
    },
    Run {
        label: "a namespace-only rename between two conditional metadata-pointer writes",
        steps: &[
            Step::MetadataPointer("/metadata/00001-aabbccddeeff00112233445566778899.metadata.json"),
            Step::Rename {
                new_namespace: Some(AWAY_NAMESPACE),
                new_name: None,
                with_token: true,
            },
            Step::MetadataPointer("/metadata/snapshots/2026/v7.metadata.json.gz"),
            Step::Reread,
        ],
    },
    Run {
        label: "two renames in a row, each presenting the token the client read once",
        steps: &[
            Step::Rename {
                new_namespace: None,
                new_name: Some("token_chain_moved_1"),
                with_token: true,
            },
            Step::Rename {
                new_namespace: Some(AWAY_NAMESPACE),
                new_name: Some("token_chain_moved_2"),
                with_token: true,
            },
            Step::Reread,
        ],
    },
    Run {
        label: "no rename at all: tags and both maintenance tasks",
        steps: &[
            Step::Tag("stage"),
            Step::Untag("stage"),
            Step::Maintenance {
                task: TableMaintenanceType::IcebergCompaction,
                status: "enabled",
            },
            Step::Maintenance {
                task: TableMaintenanceType::IcebergSnapshotManagement,
                status: "disabled",
            },
        ],
    },
    Run {
        label: "a single rename and nothing else",
        steps: &[Step::Rename {
            new_namespace: None,
            new_name: Some("t"),
            with_token: false,
        }],
    },
];

/// One table bucket holding two namespaces, with one Iceberg table in the first of them:
/// what every row starts from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
    table_arn: String,
    warehouse_location: String,
    /// Where the table is now, which a rename inside the run moves.
    namespace: String,
    name: String,
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
        for namespace in [HOME_NAMESPACE, AWAY_NAMESPACE] {
            let made = emulator
                .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [namespace] }))
                .await;
            assert_eq!(
                made.status, 200,
                "case {case:?}: CreateNamespace {namespace:?}: {made:?}"
            );
        }
        // `format` is required and `ICEBERG` is the only value the modelled
        // `OpenTableFormat` enum declares; the metadata the create describes is what
        // gives the table a metadata document for a commit to supersede.
        let table = emulator
            .create_table(
                REGION,
                &bucket_arn,
                HOME_NAMESPACE,
                &json!({
                    "name": TABLE,
                    "format": OpenTableFormat::Iceberg.as_wire(),
                    "metadata": { "iceberg": { "schema": { "fields": [
                        { "name": "id", "type": "long", "required": true },
                        { "name": "label", "type": "string" },
                    ]}}},
                }),
            )
            .await;
        assert_eq!(table.status, 200, "case {case:?}: CreateTable: {table:?}");
        let read = emulator
            .get_table(REGION, &bucket_arn, HOME_NAMESPACE, TABLE)
            .await;
        assert_eq!(read.status, 200, "case {case:?}: GetTable: {read:?}");
        Self {
            emulator,
            bucket_arn,
            table_arn: read.text("tableARN"),
            warehouse_location: read.text("warehouseLocation"),
            namespace: HOME_NAMESPACE.to_string(),
            name: TABLE.to_string(),
        }
    }

    /// `GetTable` at the table's current coordinates.
    async fn describe(&self) -> Reply {
        self.emulator
            .get_table(REGION, &self.bucket_arn, &self.namespace, &self.name)
            .await
    }

    /// Make one step of the run, presenting `held` wherever the step carries the client's
    /// token.
    async fn take(&mut self, step: Step, held: &str, case: &str) {
        match step {
            Step::Reread => {
                let read = self.describe().await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the re-read of the table: {read:?}"
                );
            }
            Step::Maintenance { task, status } => {
                let put = self
                    .emulator
                    .put_table_maintenance_configuration(
                        REGION,
                        &self.bucket_arn,
                        &self.namespace,
                        &self.name,
                        task.as_wire(),
                        &json!({ "status": status }),
                    )
                    .await;
                assert_eq!(
                    put.status, 204,
                    "case {case:?}: PutTableMaintenanceConfiguration for {task:?}: {put:?}"
                );
            }
            Step::Rename {
                new_namespace,
                new_name,
                with_token,
            } => {
                let mut request = json!({});
                if let Some(namespace) = new_namespace {
                    request["newNamespaceName"] = json!(namespace);
                }
                if let Some(name) = new_name {
                    request["newName"] = json!(name);
                }
                if with_token {
                    request["versionToken"] = json!(held);
                }
                let renamed = self
                    .emulator
                    .rename_table(
                        REGION,
                        &self.bucket_arn,
                        &self.namespace,
                        &self.name,
                        &request,
                    )
                    .await;
                assert_eq!(
                    renamed.status, 204,
                    "case {case:?}: RenameTable {request}: {renamed:?}"
                );
                if let Some(namespace) = new_namespace {
                    self.namespace = namespace.to_string();
                }
                if let Some(name) = new_name {
                    self.name = name.to_string();
                }
            }
            Step::Tag(key) => {
                let tagged = self
                    .emulator
                    .tag_resource(REGION, &self.table_arn, &json!({ key: "probe" }))
                    .await;
                assert_eq!(
                    tagged.status, 200,
                    "case {case:?}: TagResource {key:?}: {tagged:?}"
                );
            }
            Step::Untag(key) => {
                let removed = self
                    .emulator
                    .untag_resource(REGION, &self.table_arn, &[key])
                    .await;
                assert_eq!(
                    removed.status, 204,
                    "case {case:?}: UntagResource {key:?}: {removed:?}"
                );
            }
            Step::MetadataPointer(file) => {
                let location = format!("{warehouse}{file}", warehouse = self.warehouse_location);
                let updated = self
                    .emulator
                    .update_table_metadata_location(
                        REGION,
                        &self.bucket_arn,
                        &self.namespace,
                        &self.name,
                        &json!({ "metadataLocation": location, "versionToken": held }),
                    )
                    .await;
                assert_eq!(
                    updated.status, 200,
                    "case {case:?}: UpdateTableMetadataLocation under the token the client \
                     holds: {updated:?}"
                );
                assert_eq!(
                    updated.text("versionToken"),
                    held,
                    "case {case:?}: the conditional write answered with a token the client \
                     never presented: {updated:?}"
                );
            }
        }
    }

    /// The catalog commit that supersedes the table's metadata document: it adds a sort
    /// order and makes it the table's default, which is the service writing a new
    /// metadata document for the table.
    async fn commit(&self, case: &str) -> Reply {
        let committed = self
            .emulator
            .commit_table(
                REGION,
                &self.bucket_arn,
                &self.namespace,
                &self.name,
                &json!({
                    "requirements": [],
                    "updates": [
                        { "action": "add-sort-order", "sort-order": { "fields": [
                            {
                                "source-id": 1,
                                "transform": "identity",
                                "direction": "asc",
                                "null-order": "nulls-first",
                            },
                        ]}},
                        // `-1` names the order the same commit added, as the Iceberg
                        // REST catalog specification defines the reference.
                        { "action": "set-default-sort-order", "sort-order-id": -1 },
                    ],
                }),
            )
            .await;
        assert_eq!(
            committed.status, 200,
            "case {case:?}: the catalog commit: {committed:?}"
        );
        committed
    }
}

/// The rule, over every run: the token the client read once survives the whole run of
/// control-plane writes, the commit afterwards supersedes it, and the pair of conditional
/// deletes that follow agree about which token is the table's.
#[tokio::test]
async fn control_plane_writes_leave_the_token_and_only_a_metadata_commit_supersedes_it() {
    for (index, run) in RUNS.iter().enumerate() {
        let case = run.label;
        let mut fixture = Fixture::new(&format!("token-chain-{index}"), case).await;

        // The one token the client ever reads: everything below presents it.
        let before = fixture.describe().await;
        assert_eq!(before.status, 200, "case {case:?}: GetTable: {before:?}");
        let held = before.text("versionToken");
        assert_table_version_token(&held, "the token the create minted", case);

        // After EVERY step of the run, the table still reports the token the client
        // holds — so no write in the run invalidated the precondition it carries, and a
        // step that itself presents the token is honoured rather than refused.
        for step in run.steps {
            fixture.take(*step, &held, case).await;
            let after = fixture.describe().await;
            assert_eq!(
                after.status, 200,
                "case {case:?}: GetTable after {step:?}: {after:?}"
            );
            let reported = after.text("versionToken");
            assert_eq!(
                reported, held,
                "case {case:?}: {step:?} left the table reporting {reported:?}, expected the \
                 token {held:?} the client read"
            );
        }

        // The commit is the one write that supersedes the token: it writes a new metadata
        // document for the table, so the token names that document and not the old one.
        fixture.commit(case).await;
        let committed = fixture.describe().await;
        assert_eq!(
            committed.status, 200,
            "case {case:?}: GetTable after the commit: {committed:?}"
        );
        let current = committed.text("versionToken");
        assert_table_version_token(&current, "the token the commit minted", case);
        assert_ne!(
            current, held,
            "case {case:?}: the commit wrote a new metadata document and the table still \
             reports {held:?}, so a caller cannot tell the document moved"
        );

        // What that means for the two conditional deletes: the superseded token is the
        // modelled conflict, stated by its fixed sentence, and deletes nothing.
        let stale = fixture
            .emulator
            .delete_table(
                REGION,
                &fixture.bucket_arn,
                &fixture.namespace,
                &fixture.name,
                &[("versionToken", held.clone())],
            )
            .await;
        assert_eq!(
            stale.status, 409,
            "case {case:?}: DeleteTable presenting the superseded token {held:?}: expected \
             409, got {stale:?}"
        );
        assert_eq!(
            stale.error_code.as_deref(),
            Some("ConflictException"),
            "case {case:?}: unexpected error code on the superseded-token delete: {stale:?}"
        );
        assert_eq!(
            stale.message(),
            TABLE_VERSION_TOKEN_MISMATCH,
            "case {case:?}: unexpected message on the superseded-token delete: {stale:?}"
        );
        let survived = fixture.describe().await;
        assert_eq!(
            survived.status, 200,
            "case {case:?}: the refused delete removed the table: {survived:?}"
        );
        assert_eq!(
            survived.text("versionToken"),
            current,
            "case {case:?}: the refused delete changed the table's token: {survived:?}"
        );

        // And the token the commit minted is honoured.
        let deleted = fixture
            .emulator
            .delete_table(
                REGION,
                &fixture.bucket_arn,
                &fixture.namespace,
                &fixture.name,
                &[("versionToken", current.clone())],
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: DeleteTable presenting the table's current token: expected 204, \
             got {deleted:?}"
        );
        let gone = fixture.describe().await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the table must be gone after the honoured delete: {gone:?}"
        );
    }
}

/// The same rule where the control-plane writes come AFTER a commit: a commit mints a
/// token, the control-plane writes that follow it leave that token, and a second commit
/// supersedes it in turn — so the token tracks the metadata document and nothing else,
/// however the two kinds of write are interleaved.
#[tokio::test]
async fn a_run_of_control_plane_writes_after_a_commit_leaves_the_token_the_commit_minted() {
    let case = "control-plane writes between two commits";
    let mut fixture = Fixture::new("token-chain-interleaved", case).await;

    let created = fixture.describe().await;
    assert_eq!(created.status, 200, "case {case:?}: GetTable: {created:?}");
    let minted_by_create = created.text("versionToken");

    fixture.commit(case).await;
    let after_first = fixture.describe().await;
    assert_eq!(
        after_first.status, 200,
        "case {case:?}: GetTable after the first commit: {after_first:?}"
    );
    let after_commit = after_first.text("versionToken");
    assert_table_version_token(&after_commit, "the token the first commit minted", case);
    assert_ne!(
        after_commit, minted_by_create,
        "case {case:?}: the first commit left the token the create minted"
    );

    // Every control-plane write this suite knows, on the table the commit just wrote.
    let steps = [
        Step::Rename {
            new_namespace: Some(AWAY_NAMESPACE),
            new_name: Some("token_after_commit"),
            with_token: true,
        },
        Step::Maintenance {
            task: TableMaintenanceType::IcebergCompaction,
            status: "disabled",
        },
        Step::Tag("stage"),
        Step::Untag("stage"),
        Step::MetadataPointer("/metadata/00009-00112233445566778899aabbccddeeff.metadata.json"),
        Step::Reread,
    ];
    for step in steps {
        fixture.take(step, &after_commit, case).await;
        let read = fixture.describe().await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTable after {step:?}: {read:?}"
        );
        let reported = read.text("versionToken");
        assert_eq!(
            reported, after_commit,
            "case {case:?}: {step:?} left the table reporting {reported:?}, expected the \
             token {after_commit:?} the commit minted"
        );
    }

    // The second commit supersedes it, and the metadata document it answers with carries
    // the sort order the run never touched — the document, not the control plane, is what
    // the token tracks.
    let second = fixture.commit(case).await;
    let document = second
        .body
        .get("metadata")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the commit answers a metadata document: {second:?}"));
    let default_order = document
        .get("default-sort-order-id")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    assert_ne!(
        default_order, 0,
        "case {case:?}: the commit must make the order it added the table's default, got \
         {default_order}"
    );
    let after_second = fixture.describe().await;
    assert_eq!(
        after_second.status, 200,
        "case {case:?}: GetTable after the second commit: {after_second:?}"
    );
    let latest = after_second.text("versionToken");
    assert_table_version_token(&latest, "the token the second commit minted", case);
    assert_ne!(
        latest, after_commit,
        "case {case:?}: the second commit left the token the first one minted"
    );
}
