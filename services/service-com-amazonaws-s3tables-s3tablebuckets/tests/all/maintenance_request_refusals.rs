// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables refuses a maintenance request it will not act on, and what it
//! acts on instead: the STATUS a configuration carries, the maintenance TYPE a
//! request's path addresses, the numbers a configuration counts in, the
//! compaction STRATEGY and FILE SIZE it names, and the configuration VALUE the
//! body must carry at all.
//!
//! Six rules are under test, each asserted over a table that spans the class it
//! generalises over rather than the one recorded request:
//!
//! * RULE 1 — a configuration's `status` outside the two values its enum
//!   declares is refused with this service's own modelled client-input error
//!   (`BadRequestException`, HTTP 400) and ONE fixed sentence naming the member
//!   and those two values, whatever the spelling that missed the set and
//!   whichever operation carried it. The member sits inside a different
//!   structure in each — `MaintenanceStatus` in a table's and a table bucket's
//!   maintenance configuration, `TableRecordExpirationStatus` in a table's record
//!   expiration configuration, each an enum of `enabled` and `disabled`
//!   (`smithy_query(query="shape",
//!   shape="MaintenanceStatus,TableRecordExpirationStatus")`) — and the refusal is
//!   the same in all of them;
//!
//! * RULE 2 — a maintenance type in the request PATH that names no task of the
//!   resource the path addresses is refused the same way, with the fixed prefix
//!   `Invalid table maintenance type: ` and the type the path carried. Which
//!   tasks a resource has comes from the model: a table's are
//!   `TableMaintenanceType`'s values and a table bucket's are
//!   `TableBucketMaintenanceType`'s (`smithy_query(query="shape",
//!   shape="TableMaintenanceType,TableBucketMaintenanceType")`), which this
//!   service's state enums carry and the rows below enumerate through. A task of
//!   the OTHER resource is one of those unknown types, which is why the same
//!   spelling is served on one endpoint and refused on the other. The refusal is
//!   of the PATH, so it holds whether or not the body can be read into the
//!   operation's input at all — a request naming an unknown task usually carries
//!   that task's settings, and the addressed resource's settings union has no
//!   member for them;
//!
//! * RULE 3 — a member a MAINTENANCE configuration COUNTS in, given a value
//!   outside the positive-integer range the model bounds it at
//!   (`PositiveInteger`,
//!   `@range(1, 2147483647)`), is refused with a sentence naming that member:
//!   `The specified <member> is invalid. Value must be a positive integer.` The
//!   members are the ones the model targets at that shape inside a maintenance
//!   task's settings
//!   (`smithy_query(query="shape", shape="IcebergSnapshotManagementSettings,
//!   IcebergUnreferencedFileRemovalSettings")`:
//!   `minSnapshotsToKeep`, `maxSnapshotAgeHours`, `unreferencedDays`,
//!   `nonCurrentDays`), and the value AT the range's minimum is acted on. The
//!   retention a RECORD EXPIRATION configuration names is bounded at the same
//!   range and is not one of them — see [`COUNTED`];
//!
//! * RULE 4 — the compaction FILE SIZE is the one such member S3 Tables bounds
//!   more narrowly than the model does: a size outside the window it compacts
//!   towards is refused with the sentence that states that window, and the sizes
//!   AT both of its bounds are acted on. A size the model itself rules out is the
//!   same class and carries the same sentence;
//!
//! * RULE 5 — a compaction STRATEGY outside the set the model enumerates
//!   (`smithy_query(query="shape", shape="IcebergCompactionStrategy")`) is
//!   refused with the sentence for that member, whatever the spelling that missed
//!   the set, and every strategy in the set is acted on;
//!
//! * RULE 6 — a maintenance put whose body carries no `value` at all — the member
//!   the model makes `@required` on both maintenance puts — is refused with the
//!   sentence that states the value is not there, and configures nothing. A put
//!   whose settings name ANOTHER task than its path does is NOT refused: the
//!   request is one S3 Tables acts on, and the settings it could not file under
//!   the task the path addressed configure nothing.
//!
//! Every rule is asserted as the WHOLE sentence, which is what proves nothing
//! else of the request is echoed back, and every refused request is shown to
//! leave the configuration the reads report exactly as it was.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::{TableBucketMaintenanceType, TableMaintenanceType};

use crate::harness::{encode, Emulator, Reply};
use crate::table_maintenance_configuration::create_table;
use crate::table_record_expiration::mark_managed;

/// The one client-input error this service's model declares.
const BAD_REQUEST: &str = "BadRequestException";

/// The fixed sentences, spelled out here rather than read from the crate, so this
/// suite pins the wording itself.
const INVALID_STATUS: &str =
    "The specified status is invalid. Value must be either 'enabled' or 'disabled.'";
const INVALID_MAINTENANCE_TYPE: &str = "Invalid table maintenance type: ";
const INVALID_COMPACTION_FILE_SIZE: &str =
    "The specified compaction file size is invalid. Minimum value: 64 MB, maximum value: 512 MB.";
const INVALID_COMPACTION_STRATEGY: &str = "The compaction strategy specified is not valid.";
const VALUE_NOT_SPECIFIED: &str = "The table maintenance configuration value is not specified.";

/// The window S3 Tables compacts a table towards, in MB: the two bounds the
/// file-size sentence states, which are the sizes the service acts on.
const COMPACTION_FILE_SIZE_WINDOW: [i64; 2] = [64, 512];

/// The status the three puts under test answer when the configuration IS acted
/// on: each models 204 (`smithy_query(query="operation",
/// operation="PutTableMaintenanceConfiguration,
/// PutTableBucketMaintenanceConfiguration,
/// PutTableRecordExpirationConfiguration")`).
const CONFIGURED: u16 = 204;

/// The namespace and table name every row works with.
const NAMESPACE: &str = "ns";
const TABLE: &str = "t";

/// The resources one row is served against: a table bucket, a namespace in it,
/// and one table AWS manages (the only kind that takes a record expiration
/// configuration).
struct Fixture {
    region: &'static str,
    bucket_arn: String,
    table_arn: String,
}

async fn fixture(emulator: &Emulator, region: &'static str, bucket: &str) -> Fixture {
    let (bucket_arn, table_arn) = create_table(emulator, region, bucket, NAMESPACE, TABLE).await;
    mark_managed(emulator, &table_arn);
    Fixture {
        region,
        bucket_arn,
        table_arn,
    }
}

/// Which configuration a row addresses — the three this service takes a `status`
/// member in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Configured {
    /// A table's maintenance configuration, addressed by a task in the path.
    TableMaintenance,
    /// A table bucket's maintenance configuration, addressed the same way.
    BucketMaintenance,
    /// A table's record expiration configuration, which is addressed by the
    /// table alone and so takes no task.
    RecordExpiration,
}

impl Configured {
    /// Put `value` as this configuration's value, for the task `task` where the
    /// configuration has one.
    async fn put(
        self,
        emulator: &Emulator,
        fixture: &Fixture,
        task: &str,
        value: &Value,
    ) -> Reply {
        match self {
            Self::TableMaintenance => {
                emulator
                    .put_table_maintenance_configuration(
                        fixture.region,
                        &fixture.bucket_arn,
                        NAMESPACE,
                        TABLE,
                        task,
                        value,
                    )
                    .await
            }
            Self::BucketMaintenance => {
                emulator
                    .put_table_bucket_maintenance_configuration(
                        fixture.region,
                        &fixture.bucket_arn,
                        task,
                        value,
                    )
                    .await
            }
            Self::RecordExpiration => {
                emulator
                    .put_table_record_expiration_configuration(
                        fixture.region,
                        &fixture.table_arn,
                        value,
                    )
                    .await
            }
        }
    }

    /// Everything the read of this configuration reports, so a refused put can be
    /// shown to have left it alone.
    async fn read(self, emulator: &Emulator, fixture: &Fixture) -> Value {
        let reply = match self {
            Self::TableMaintenance => {
                emulator
                    .get_table_maintenance_configuration(
                        fixture.region,
                        &fixture.bucket_arn,
                        NAMESPACE,
                        TABLE,
                    )
                    .await
            }
            Self::BucketMaintenance => {
                emulator
                    .get_table_bucket_maintenance_configuration(fixture.region, &fixture.bucket_arn)
                    .await
            }
            Self::RecordExpiration => {
                emulator
                    .get_table_record_expiration_configuration(fixture.region, &fixture.table_arn)
                    .await
            }
        };
        assert_eq!(
            reply.status, 200,
            "the read of {self:?} must be served, got {status} {body}",
            status = reply.status,
            body = reply.body
        );
        reply.body
    }

    /// The status the read `body` reports for `task`: a maintenance read reports
    /// one entry per task, and a record expiration read one configuration for the
    /// table.
    fn reported_status(self, body: &Value, task: &str) -> Option<String> {
        let configuration = body.get("configuration")?;
        let value = match self {
            Self::TableMaintenance | Self::BucketMaintenance => configuration.get(task)?,
            Self::RecordExpiration => configuration,
        };
        value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Every configuration that carries a status member, with the task addressing it
/// where it has one: both of a table's maintenance tasks, a table bucket's one,
/// and a table's record expiration.
fn status_targets() -> Vec<(Configured, &'static str)> {
    let mut targets: Vec<(Configured, &'static str)> = TableMaintenanceType::ALL
        .iter()
        .map(|task| (Configured::TableMaintenance, task.as_wire()))
        .collect();
    targets.extend(
        TableBucketMaintenanceType::ALL
            .iter()
            .map(|task| (Configured::BucketMaintenance, task.as_wire())),
    );
    targets.push((Configured::RecordExpiration, ""));
    targets
}

/// Assert that `reply` is this service's own 400 refusal, carrying `message`
/// whole.
fn assert_refused(reply: &Reply, case: &str, message: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(BAD_REQUEST),
        "case {case:?}: expected error type {BAD_REQUEST}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected {message:?}, got {actual:?}",
        actual = reply.message()
    );
}

#[tokio::test]
async fn a_status_outside_the_modelled_set_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "us-east-1", "maintenance-status-refusals").await;

    // The spellings that miss the value set: the two values upper-cased and
    // capitalised, one with a trailing and one with a leading space, a word of
    // another vocabulary, the two values joined, and no value at all.
    let outside = [
        "ENABLED",
        "DISABLED",
        "Enabled",
        "disabled ",
        " enabled",
        "on",
        "enabled,disabled",
        "",
    ];

    for (target, task) in status_targets() {
        let before = target.read(&emulator, &fixture).await;
        for status in outside {
            let case = format!("{target:?} for {task:?} with the status {status:?}");
            let refused = target
                .put(&emulator, &fixture, task, &json!({ "status": status }))
                .await;
            assert_refused(&refused, &case, INVALID_STATUS);
            assert_eq!(
                target.read(&emulator, &fixture).await,
                before,
                "case {case:?}: the refused put must leave the configuration as it was"
            );
        }
    }
}

#[tokio::test]
async fn each_status_the_model_declares_is_acted_on() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "eu-west-1", "maintenance-status-accepted").await;

    // The accepted rows keep the refusals honest: BOTH values the enum declares
    // are statuses this service acts on, in every configuration that carries one,
    // and the read reports the one the put settled.
    for (target, task) in status_targets() {
        for status in ["enabled", "disabled"] {
            let case = format!("{target:?} for {task:?} with the status {status:?}");
            let put = target
                .put(&emulator, &fixture, task, &json!({ "status": status }))
                .await;
            assert_eq!(
                put.status, CONFIGURED,
                "case {case:?}: expected status {CONFIGURED}, got {actual} {body}",
                actual = put.status,
                body = put.body
            );
            let read = target.read(&emulator, &fixture).await;
            assert_eq!(
                target.reported_status(&read, task).as_deref(),
                Some(status),
                "case {case:?}: the read must report the status the put settled, got {read}"
            );
        }
    }
}

/// One row of the maintenance-type table: the endpoint a put addresses, the type
/// its path carries, and the body it sends.
struct TypeCase {
    label: String,
    /// Which resource's maintenance endpoint the row addresses.
    target: Configured,
    maintenance_type: String,
    value: Value,
}

/// A maintenance configuration value carrying the settings of `task`, so a row
/// can send a body whose settings union names the very task its path does — which
/// the addressed resource's union has no member for when the task is not one of
/// its own, leaving a body this service cannot read into the input at all.
fn settings_of(task: &str) -> Value {
    json!({ "status": "enabled", "settings": { task: {} } })
}

/// A body whose every member reads into the input of either maintenance put.
fn readable_value() -> Value {
    json!({ "status": "enabled" })
}

fn type_cases() -> Vec<TypeCase> {
    let mut cases = Vec::new();

    // A task of the OTHER resource: a table bucket's on the table endpoint, and
    // each of a table's on the bucket endpoint. The first is sent twice — once
    // with a body that reads into the input and once with the unknown task's own
    // settings, which no member of a table's settings union can hold — because
    // the path is what named no task either way.
    for task in TableBucketMaintenanceType::ALL {
        for (shape, value) in [
            ("a readable body", readable_value()),
            ("the unknown task's own settings", settings_of(task.as_wire())),
        ] {
            cases.push(TypeCase {
                label: format!("a table bucket's task {task:?} on the table endpoint, {shape}"),
                target: Configured::TableMaintenance,
                maintenance_type: task.as_wire().to_string(),
                value,
            });
        }
    }
    for task in TableMaintenanceType::ALL {
        cases.push(TypeCase {
            label: format!(
                "a table's task {task:?} on the table bucket endpoint, the unknown task's own \
                 settings"
            ),
            target: Configured::BucketMaintenance,
            maintenance_type: task.as_wire().to_string(),
            value: settings_of(task.as_wire()),
        });
    }

    // Spellings that name no task of either resource: the casing variants of a
    // task's own name, a name of another vocabulary, one carrying a space (which
    // travels escaped and comes back whole in the refusal), and each resource's
    // task name with a character added.
    let misspelled = [
        "icebergcompaction".to_string(),
        "ICEBERGCOMPACTION".to_string(),
        "IcebergCompaction".to_string(),
        "iceberg Compaction".to_string(),
        "icebergVacuum".to_string(),
        format!("{}s", TableMaintenanceType::IcebergCompaction.as_wire()),
        format!(
            "{}s",
            TableBucketMaintenanceType::IcebergUnreferencedFileRemoval.as_wire()
        ),
    ];
    for spelling in misspelled {
        for (endpoint, target) in [
            ("table", Configured::TableMaintenance),
            ("table bucket", Configured::BucketMaintenance),
        ] {
            cases.push(TypeCase {
                label: format!("{spelling:?} on the {endpoint} endpoint"),
                target,
                maintenance_type: spelling.clone(),
                value: readable_value(),
            });
        }
    }

    cases
}

#[tokio::test]
async fn a_maintenance_type_naming_no_task_of_the_resource_is_refused_for_the_type() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "us-west-2", "maintenance-type-refusals").await;

    let before = [
        (
            Configured::TableMaintenance,
            Configured::TableMaintenance.read(&emulator, &fixture).await,
        ),
        (
            Configured::BucketMaintenance,
            Configured::BucketMaintenance
                .read(&emulator, &fixture)
                .await,
        ),
    ];

    for case in type_cases() {
        let refused = case
            .target
            .put(
                &emulator,
                &fixture,
                &case.maintenance_type,
                &case.value,
            )
            .await;
        assert_refused(
            &refused,
            &case.label,
            &format!(
                "{INVALID_MAINTENANCE_TYPE}{maintenance_type}",
                maintenance_type = case.maintenance_type
            ),
        );
        // A refused path configures nothing, on either resource.
        for (target, reported) in &before {
            assert_eq!(
                &target.read(&emulator, &fixture).await,
                reported,
                "case {label:?}: the refused put must leave {target:?} as it was",
                label = case.label
            );
        }
    }
}

#[tokio::test]
async fn each_task_the_model_declares_for_a_resource_is_acted_on() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "ap-northeast-1", "maintenance-type-accepted").await;

    // The accepted rows: every task the model declares for a resource is one its
    // own endpoint configures, carrying that task's own settings.
    for (target, task) in status_targets() {
        if target == Configured::RecordExpiration {
            continue;
        }
        let case = format!("{target:?} for {task:?}");
        let put = target
            .put(&emulator, &fixture, task, &settings_of(task))
            .await;
        assert_eq!(
            put.status, CONFIGURED,
            "case {case:?}: a task of its own resource must be configured, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = target.read(&emulator, &fixture).await;
        assert_eq!(
            target.reported_status(&read, task).as_deref(),
            Some("enabled"),
            "case {case:?}: the read must report the configuration the put settled, got {read}"
        );
    }
}

/// One row of the counted-member table: the configuration a put addresses, the
/// task in its path, the settings member it counts in, and the value it carries.
struct CountedCase {
    target: Configured,
    task: &'static str,
    /// The member the settings carry the number in — the name the refusal must
    /// state.
    member: &'static str,
    /// The structure the settings put that member in, or `None` for the record
    /// expiration configuration, whose settings carry it directly.
    settings_of_task: Option<&'static str>,
}

impl CountedCase {
    /// The configuration value carrying `number` in this row's member.
    fn value(&self, number: i64) -> Value {
        let counted = json!({ self.member: number });
        let settings = match self.settings_of_task {
            Some(task) => json!({ task: counted }),
            None => counted,
        };
        json!({ "status": "enabled", "settings": settings })
    }
}

/// Every member of this model a MAINTENANCE configuration counts in, with the
/// put that carries it: the snapshot-management pair on a table and the
/// unreferenced-file removal pair on a table bucket.
///
/// The retention a record-expiration configuration names
/// (`TableRecordExpirationSettings.days`) is bounded at the same range and is
/// deliberately NOT here: S3 Tables states no sentence of its own for it, so a
/// retention below the minimum is the parameter validator's refusal
/// (`parameter_validation_refusals.rs` pins that one) and a retention above the
/// maximum refuses nothing at all (`table_record_expiration.rs`).
const COUNTED: &[CountedCase] = &[
    CountedCase {
        target: Configured::TableMaintenance,
        task: "icebergSnapshotManagement",
        member: "minSnapshotsToKeep",
        settings_of_task: Some("icebergSnapshotManagement"),
    },
    CountedCase {
        target: Configured::TableMaintenance,
        task: "icebergSnapshotManagement",
        member: "maxSnapshotAgeHours",
        settings_of_task: Some("icebergSnapshotManagement"),
    },
    CountedCase {
        target: Configured::BucketMaintenance,
        task: "icebergUnreferencedFileRemoval",
        member: "unreferencedDays",
        settings_of_task: Some("icebergUnreferencedFileRemoval"),
    },
    CountedCase {
        target: Configured::BucketMaintenance,
        task: "icebergUnreferencedFileRemoval",
        member: "nonCurrentDays",
        settings_of_task: Some("icebergUnreferencedFileRemoval"),
    },
];

#[tokio::test]
async fn a_counted_member_outside_the_positive_integer_range_is_refused_naming_the_member() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "us-east-2", "counted-member-refusals").await;

    // The values below the range the model bounds each member at: zero, and
    // negative numbers of either size.
    for case in COUNTED {
        let before = case.target.read(&emulator, &fixture).await;
        for number in [0, -1, -365] {
            let label = format!("{member} of {number}", member = case.member);
            let refused = case
                .target
                .put(&emulator, &fixture, case.task, &case.value(number))
                .await;
            assert_refused(
                &refused,
                &label,
                &format!(
                    "The specified {member} is invalid. Value must be a positive integer.",
                    member = case.member
                ),
            );
            assert_eq!(
                case.target.read(&emulator, &fixture).await,
                before,
                "case {label:?}: the refused put must leave the configuration as it was"
            );
        }
    }
}

#[tokio::test]
async fn a_counted_member_at_the_range_minimum_is_acted_on() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "ca-central-1", "counted-member-accepted").await;

    // The accepted rows keep the refusals honest: the value AT the range's
    // minimum is one this service acts on, for every member it counts in.
    for case in COUNTED {
        let label = format!("{member} of 1", member = case.member);
        let put = case
            .target
            .put(&emulator, &fixture, case.task, &case.value(1))
            .await;
        assert_eq!(
            put.status, CONFIGURED,
            "case {label:?}: expected status {CONFIGURED}, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = case.target.read(&emulator, &fixture).await;
        assert!(
            read.to_string().contains(&format!("\"{}\":1", case.member)),
            "case {label:?}: the read must report the number the put settled, got {read}"
        );
    }
}

#[tokio::test]
async fn a_compaction_file_size_outside_the_window_is_refused_stating_the_window() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "eu-central-1", "compaction-size-refusals").await;
    let [smallest, largest] = COMPACTION_FILE_SIZE_WINDOW;
    let compaction = TableMaintenanceType::IcebergCompaction.as_wire();

    let before = Configured::TableMaintenance.read(&emulator, &fixture).await;
    // One step under and over the window, far outside it on both sides, and the
    // values the MODEL itself rules out (zero and negative), which are the same
    // class: a size this service does not compact towards.
    for size in [smallest - 1, largest + 1, 1, 2_147_483_647, 0, -1] {
        let label = format!("a target file size of {size} MB");
        let refused = Configured::TableMaintenance
            .put(
                &emulator,
                &fixture,
                compaction,
                &json!({
                    "status": "enabled",
                    "settings": { "icebergCompaction": { "targetFileSizeMB": size } },
                }),
            )
            .await;
        assert_refused(&refused, &label, INVALID_COMPACTION_FILE_SIZE);
        assert_eq!(
            Configured::TableMaintenance.read(&emulator, &fixture).await,
            before,
            "case {label:?}: the refused put must leave the configuration as it was"
        );
    }

    // The accepted rows: the sizes AT both bounds of the window are acted on, and
    // the read reports them.
    for size in COMPACTION_FILE_SIZE_WINDOW {
        let label = format!("a target file size of {size} MB");
        let put = Configured::TableMaintenance
            .put(
                &emulator,
                &fixture,
                compaction,
                &json!({
                    "status": "enabled",
                    "settings": { "icebergCompaction": { "targetFileSizeMB": size } },
                }),
            )
            .await;
        assert_eq!(
            put.status, CONFIGURED,
            "case {label:?}: expected status {CONFIGURED}, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let read = Configured::TableMaintenance.read(&emulator, &fixture).await;
        assert!(
            read.to_string().contains(&format!("\"targetFileSizeMB\":{size}")),
            "case {label:?}: the read must report the size the put settled, got {read}"
        );
    }
}

#[tokio::test]
async fn a_compaction_strategy_outside_the_modelled_set_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "eu-west-2", "compaction-strategy-refusals").await;
    let compaction = TableMaintenanceType::IcebergCompaction.as_wire();

    let before = Configured::TableMaintenance.read(&emulator, &fixture).await;
    // The spellings that miss the modelled value set: a strategy's own name
    // capitalised and upper-cased, one with a trailing space, the hyphenated one
    // spelled with an underscore, a strategy of another vocabulary, and no value
    // at all.
    for strategy in [
        "Sort", "BINPACK", "binpack ", "z_order", "interleaved", "",
    ] {
        let label = format!("the strategy {strategy:?}");
        let refused = Configured::TableMaintenance
            .put(
                &emulator,
                &fixture,
                compaction,
                &json!({
                    "status": "enabled",
                    "settings": { "icebergCompaction": { "strategy": strategy } },
                }),
            )
            .await;
        assert_refused(&refused, &label, INVALID_COMPACTION_STRATEGY);
        assert_eq!(
            Configured::TableMaintenance.read(&emulator, &fixture).await,
            before,
            "case {label:?}: the refused put must leave the configuration as it was"
        );
    }

    // The accepted rows: every strategy the model declares is one this service
    // compacts under, on a table whose metadata carries the sort order the two
    // ordering strategies need (which `compaction_sort_order.rs` is what pins).
    for strategy in s3tables::state::IcebergCompactionStrategy::ALL {
        let label = format!("the strategy {:?}", strategy.as_wire());
        let put = Configured::TableMaintenance
            .put(
                &emulator,
                &fixture,
                compaction,
                &json!({
                    "status": "enabled",
                    "settings": { "icebergCompaction": { "strategy": strategy.as_wire() } },
                }),
            )
            .await;
        assert_eq!(
            put.status, CONFIGURED,
            "case {label:?}: expected status {CONFIGURED}, got {status} {body}",
            status = put.status,
            body = put.body
        );
    }
}

#[tokio::test]
async fn a_maintenance_put_carrying_no_configuration_value_is_refused_for_the_value() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "ap-southeast-2", "maintenance-value-absent").await;

    // Both maintenance puts, each addressed with a task of its own resource, and
    // each sent with a body that carries no `value` member: an empty object, and
    // an object carrying something else entirely.
    let puts: Vec<(Configured, &str, String)> = vec![
        (
            Configured::TableMaintenance,
            TableMaintenanceType::IcebergCompaction.as_wire(),
            format!(
                "/tables/{bucket}/{NAMESPACE}/{TABLE}/maintenance/{task}",
                bucket = encode(&fixture.bucket_arn),
                task = TableMaintenanceType::IcebergCompaction.as_wire()
            ),
        ),
        (
            Configured::BucketMaintenance,
            TableBucketMaintenanceType::IcebergUnreferencedFileRemoval.as_wire(),
            format!(
                "/buckets/{bucket}/maintenance/{task}",
                bucket = encode(&fixture.bucket_arn),
                task = TableBucketMaintenanceType::IcebergUnreferencedFileRemoval.as_wire()
            ),
        ),
    ];

    for (target, task, path) in puts {
        let before = target.read(&emulator, &fixture).await;
        for body in [json!({}), json!({ "status": "enabled" })] {
            let label = format!("{target:?} for {task:?} with the body {body}");
            let refused = emulator
                .call("PUT", &path, fixture.region, Some(&body))
                .await;
            assert_refused(&refused, &label, VALUE_NOT_SPECIFIED);
            assert_eq!(
                target.read(&emulator, &fixture).await,
                before,
                "case {label:?}: the refused put must leave the configuration as it was"
            );
        }
    }
}

#[tokio::test]
async fn settings_naming_another_task_are_acted_on_and_configure_nothing() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator, "us-east-1", "maintenance-settings-other").await;
    let compaction = TableMaintenanceType::IcebergCompaction.as_wire();
    let snapshots = TableMaintenanceType::IcebergSnapshotManagement.as_wire();

    // Each of a table's tasks addressed with the settings of the other: the put
    // is acted on, the status it carries settles the task the PATH named, and
    // neither task's settings are the ones the body named.
    for (addressed, other, other_settings) in [
        (
            compaction,
            snapshots,
            json!({ "icebergSnapshotManagement": { "minSnapshotsToKeep": 7 } }),
        ),
        (
            snapshots,
            compaction,
            json!({ "icebergCompaction": { "targetFileSizeMB": 128 } }),
        ),
    ] {
        let label = format!("{addressed} addressed with the settings of {other}");
        let before = Configured::TableMaintenance.read(&emulator, &fixture).await;
        let put = Configured::TableMaintenance
            .put(
                &emulator,
                &fixture,
                addressed,
                &json!({ "status": "disabled", "settings": other_settings }),
            )
            .await;
        assert_eq!(
            put.status, CONFIGURED,
            "case {label:?}: expected status {CONFIGURED}, got {status} {body}",
            status = put.status,
            body = put.body
        );

        let read = Configured::TableMaintenance.read(&emulator, &fixture).await;
        assert_eq!(
            Configured::TableMaintenance
                .reported_status(&read, addressed)
                .as_deref(),
            Some("disabled"),
            "case {label:?}: the status must settle the task the path named, got {read}"
        );
        for task in [addressed, other] {
            let settings = |body: &Value| {
                body.get("configuration")
                    .and_then(|configuration| configuration.get(task))
                    .and_then(|value| value.get("settings"))
                    .cloned()
            };
            assert_eq!(
                settings(&read),
                settings(&before),
                "case {label:?}: {task}'s settings must be the ones it had, got {read}"
            );
        }
    }
}
