// SPDX-License-Identifier: CC0-1.0

//! How this service reads the timestamp cutoff members a listing's request
//! carries, and at what RESOLUTION it reports an instant it recorded itself.
//!
//! The second rule is pinned by
//! [`every_recorded_instant_is_reported_at_millisecond_resolution`]: an instant
//! this service records on a record and reports through a modelled timestamp
//! member is kept to whole MILLISECONDS, so the epoch-seconds number a reader
//! gets back carries no finer fraction however precise the host clock that
//! stamped it. Both timestamp shapes this service reports through
//! (`HealthLakeTimestamp` and `DateTime`) carry no `@timestampFormat` trait, so
//! under awsJson1_0 every one of those members is an epoch-seconds JSON number
//! whose fractional part IS the precision of the instant behind it. The bounds
//! of a restore window are not such instants and are pinned by
//! `tests/all/datastore_backup_restore_window.rs` instead.
//!
//! The first rule under test: a listing applies a timestamp cutoff member exactly as
//! that member's own documentation words it — a member that selects records
//! "before" / "after" the instant it names is a STRICT bound, so a record at
//! exactly that instant is reported by neither, while a member that selects
//! records "at or before" / "at or after" it is an INCLUSIVE bound, so a record
//! at exactly that instant is reported by both.
//!
//! The reading each listing below is held to is its OWN modelled member
//! documentation, read with
//! `smithy_query(query="shape", shape="<the request or filter shape>")`:
//!
//! * `ListFHIRExportJobs` — "Limits the response to FHIR export jobs submitted
//!   before a user- specified date." / "... submitted after a user-specified
//!   date." — STRICT;
//! * `ListFHIRImportJobs` — "Limits the response to FHIR import jobs submitted
//!   before a user- specified date." / "... submitted after a user-specified
//!   date." — STRICT;
//! * `ListFHIRDatastores` (`DatastoreFilter`) — "All data stores created before
//!   the specified date are included in the results." / "... created after the
//!   specified date ..." — STRICT;
//! * `ListDataTransformationJobs` — "Filters the results to include only jobs
//!   submitted at or before this timestamp." / "... at or after this
//!   timestamp." — INCLUSIVE.
//!
//! Every call goes over the real awsJson1_0 wire through
//! [`MultiServiceRouter::dispatch`](smithy_server_runtime::MultiServiceRouter)
//! by way of the harness in `fhir_datastore_lifecycle`, so routing,
//! deserialization and the modelled constraints are all exercised, not
//! bypassed.

use std::time::{Duration, Instant};

use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_healthlake_healthlake::ops::types::{
    DataTransformationS3Configuration, InputDataConfig, OutputDataConfig, S3Configuration,
    TransformationInputDataConfig, TransformationOutputDataConfig,
};
use service_com_amazonaws_healthlake_healthlake::state::{
    ComAmazonawsHealthlakeHealthlakeState, ExportJob, ImportJob, JobStatus, TransformationJob,
    TransformationJobStatus,
};

use crate::fhir_datastore_lifecycle::{string_at, HealthLake, SETTLING_DEADLINE};

/// The epoch second every placed instant below is measured from. A fixed
/// instant, so a cutoff can be exactly the instant a record carries rather than
/// something near it.
const BASE_SECS: i64 = 1_700_000_000;

/// The instants the records of each listing are placed at, as `(seconds after
/// BASE_SECS, nanoseconds)`. Two records share a whole second and differ only
/// below it, so a cutoff is exercised against SUB-SECOND precision as well as
/// against whole seconds.
const PLACED: [(i64, u32); 4] = [(10, 0), (20, 0), (20, 250_000_000), (30, 0)];

/// The `DataAccessRoleArn` every placed job carries, satisfying the modelled
/// IAM-role patterns (`^arn:aws(-[^:]+)?:iam::[0-9]{12}:role/.+$`).
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::111122223333:role/HealthLakeCutoffs";

/// A `KmsKeyId` the modelled `EncryptionKeyID` pattern admits.
const KMS_KEY_ID: &str = "1234abcd-12ab-34cd-56ef-1234567890ab";

/// One of the placed instants, as the modelled timestamp a record holds.
fn placed_at(index: usize) -> DateTime {
    let (secs, nanos) = PLACED[index];
    DateTime::from_secs_and_nanos(BASE_SECS + secs, nanos)
}

/// A cutoff value as the wire carries a `timestamp` member under awsJson1_0:
/// epoch seconds, fractional where the instant is not a whole second.
fn cutoff(secs: i64, nanos: u32) -> Value {
    json!((BASE_SECS + secs) as f64 + f64::from(nanos) / 1e9)
}

/// The records one listing was placed with: their identifiers in [`PLACED`]
/// order, beside the scope its requests must name.
struct Placed {
    ids: Vec<String>,
    /// The `DatastoreId` the placed records belong to, empty for a listing that
    /// is not scoped to one.
    scope: String,
}

/// The listings whose requests carry a pair of timestamp cutoff members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    ExportJobs,
    ImportJobs,
    Datastores,
    TransformationJobs,
}

impl Listing {
    const ALL: [Listing; 4] = [
        Listing::ExportJobs,
        Listing::ImportJobs,
        Listing::Datastores,
        Listing::TransformationJobs,
    ];

    fn operation(self) -> &'static str {
        match self {
            Listing::ExportJobs => "ListFHIRExportJobs",
            Listing::ImportJobs => "ListFHIRImportJobs",
            Listing::Datastores => "ListFHIRDatastores",
            Listing::TransformationJobs => "ListDataTransformationJobs",
        }
    }

    /// The member that bounds the listing on the LATE side, and the one that
    /// bounds it on the EARLY side.
    fn members(self) -> (&'static str, &'static str) {
        match self {
            Listing::Datastores => ("CreatedBefore", "CreatedAfter"),
            _ => ("SubmittedBefore", "SubmittedAfter"),
        }
    }

    /// Whether this listing's OWN member documentation spells the inclusive
    /// reading ("at or before" / "at or after") rather than the strict one
    /// ("before" / "after"); this module's header quotes each.
    fn inclusive(self) -> bool {
        self == Listing::TransformationJobs
    }

    /// Place one record of this listing's resource at each instant in
    /// [`PLACED`].
    async fn place(self, service: &HealthLake) -> Placed {
        match self {
            Listing::ExportJobs => {
                let scope = datastore(service, "export cutoffs").await;
                let ids = record_ids("export");
                let store = service
                    .engine
                    .state::<ComAmazonawsHealthlakeHealthlakeState>();
                let mut store = store.lock();
                for (index, id) in ids.iter().enumerate() {
                    store.export_jobs.insert(
                        id.clone(),
                        ExportJob {
                            id: id.clone(),
                            datastore_id: scope.clone(),
                            name: None,
                            status: JobStatus::Completed,
                            pending: None,
                            submitted_at: placed_at(index),
                            ended_at: None,
                            output_data_config: OutputDataConfig::S3Configuration(
                                S3Configuration {
                                    s3_uri: format!("s3://healthlake-export-bucket/{id}"),
                                    kms_key_id: KMS_KEY_ID.to_string(),
                                },
                            ),
                            data_access_role_arn: Some(DATA_ACCESS_ROLE_ARN.to_string()),
                            message: None,
                            client_token: None,
                            request_fingerprint: String::new(),
                        },
                    );
                }
                Placed { ids, scope }
            }
            Listing::ImportJobs => {
                let scope = datastore(service, "import cutoffs").await;
                let ids = record_ids("import");
                let store = service
                    .engine
                    .state::<ComAmazonawsHealthlakeHealthlakeState>();
                let mut store = store.lock();
                for (index, id) in ids.iter().enumerate() {
                    store.import_jobs.insert(
                        id.clone(),
                        ImportJob {
                            id: id.clone(),
                            datastore_id: scope.clone(),
                            name: None,
                            status: JobStatus::Completed,
                            pending: None,
                            submitted_at: placed_at(index),
                            ended_at: None,
                            input_data_config: InputDataConfig::S3Uri(format!(
                                "s3://healthlake-import-bucket/input/{id}"
                            )),
                            output_data_config: Some(OutputDataConfig::S3Configuration(
                                S3Configuration {
                                    s3_uri: format!("s3://healthlake-import-bucket/output/{id}"),
                                    kms_key_id: KMS_KEY_ID.to_string(),
                                },
                            )),
                            data_access_role_arn: Some(DATA_ACCESS_ROLE_ARN.to_string()),
                            message: None,
                            progress_report: None,
                            validation_level: None,
                            // A record this suite seeds was not started through
                            // the API, so it carries no `ClientToken` and there
                            // is no request to fingerprint.
                            client_token: None,
                            request_fingerprint: String::new(),
                        },
                    );
                }
                Placed { ids, scope }
            }
            Listing::TransformationJobs => {
                let ids = record_ids("transform");
                let store = service
                    .engine
                    .state::<ComAmazonawsHealthlakeHealthlakeState>();
                let mut store = store.lock();
                for (index, id) in ids.iter().enumerate() {
                    store.transformation_jobs.insert(
                        id.clone(),
                        TransformationJob {
                            id: id.clone(),
                            name: None,
                            status: TransformationJobStatus::Completed,
                            pending: None,
                            submitted_at: placed_at(index),
                            ended_at: None,
                            input_data_config: TransformationInputDataConfig {
                                s3_uri: "s3://healthlake-transform-input/ccda".to_string(),
                                source_format: None,
                            },
                            output_data_config: TransformationOutputDataConfig {
                                s3_configuration: DataTransformationS3Configuration {
                                    s3_uri: "s3://healthlake-transform-output/fhir".to_string(),
                                    kms_key_id: KMS_KEY_ID.to_string(),
                                },
                            },
                            data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
                            profile_id: None,
                            profile_name: None,
                            profile_version: None,
                            drift_detection_enabled: None,
                            provenance_enabled: None,
                            message: None,
                            progress_report: None,
                            client_token: None,
                        },
                    );
                }
                Placed {
                    ids,
                    scope: String::new(),
                }
            }
            Listing::Datastores => {
                // A data store is MINTED through its own operation — nothing
                // here writes a record the create path would not — and only its
                // creation instant, the member the cutoffs read, is then
                // placed.
                let mut ids = Vec::new();
                for index in 0..PLACED.len() {
                    ids.push(datastore(service, &format!("cutoff store {index}")).await);
                }
                let store = service
                    .engine
                    .state::<ComAmazonawsHealthlakeHealthlakeState>();
                let mut store = store.lock();
                for (index, id) in ids.iter().enumerate() {
                    let placed = store
                        .datastores
                        .get_mut(id)
                        .unwrap_or_else(|| panic!("the created data store {id} is held"));
                    placed.created_at = placed_at(index);
                }
                Placed {
                    ids,
                    scope: String::new(),
                }
            }
        }
    }

    /// One request of this listing, carrying each cutoff that is set and scoped
    /// to `scope` where the listing requires a scope.
    fn request(self, scope: &str, before: Option<Value>, after: Option<Value>) -> Value {
        let (before_member, after_member) = self.members();
        let mut cutoffs = json!({});
        if let Some(before) = before {
            cutoffs[before_member] = before;
        }
        if let Some(after) = after {
            cutoffs[after_member] = after;
        }
        match self {
            Listing::Datastores => {
                let mut request = json!({});
                if cutoffs.as_object().is_some_and(|set| !set.is_empty()) {
                    request["Filter"] = cutoffs;
                }
                request
            }
            Listing::TransformationJobs => cutoffs,
            Listing::ExportJobs | Listing::ImportJobs => {
                let mut request = cutoffs;
                request["DatastoreId"] = json!(scope);
                request
            }
        }
    }

    /// The identifiers one response of this listing reported, sorted.
    fn reported(self, payload: &Value) -> Vec<String> {
        let (list_member, id_member) = match self {
            Listing::ExportJobs => ("ExportJobPropertiesList", "JobId"),
            Listing::ImportJobs => ("ImportJobPropertiesList", "JobId"),
            Listing::Datastores => ("DatastorePropertiesList", "DatastoreId"),
            Listing::TransformationJobs => ("Items", "JobId"),
        };
        let mut reported: Vec<String> = payload[list_member]
            .as_array()
            .unwrap_or_else(|| panic!("{list_member} must be a list, got {payload}"))
            .iter()
            .map(|entry| {
                entry[id_member]
                    .as_str()
                    .unwrap_or_else(|| panic!("every entry carries a {id_member}, got {entry}"))
                    .to_string()
            })
            .collect();
        reported.sort();
        reported
    }
}

/// The identifiers of the placed records of one job listing, one per entry of
/// [`PLACED`].
fn record_ids(prefix: &str) -> Vec<String> {
    (0..PLACED.len())
        .map(|index| format!("{prefix}-cutoff-{index}"))
        .collect()
}

/// Create a data store through its own operation and return its `DatastoreId`.
async fn datastore(service: &HealthLake, name: &str) -> String {
    let (id, _arn) = service
        .create(&json!({ "DatastoreTypeVersion": "R4", "DatastoreName": name }))
        .await;
    id
}

/// The identifiers at `indices` of `ids`, sorted as a response is compared.
fn expected(ids: &[String], indices: &[usize]) -> Vec<String> {
    let mut expected: Vec<String> = indices.iter().map(|index| ids[*index].clone()).collect();
    expected.sort();
    expected
}

/// One row of the cutoff table: the cutoffs a request carries, and which placed
/// records each reading of the pair selects.
struct Row {
    case: &'static str,
    before: Option<Value>,
    after: Option<Value>,
    /// The record indices a STRICT reading of the pair reports.
    strict: &'static [usize],
    /// The record indices an INCLUSIVE reading reports.
    inclusive: &'static [usize],
}

/// The rows the table spans. The records sit at [`PLACED`], so the rows differ
/// along which member is set, whether a cutoff falls ON a record instant,
/// between two of them or outside every one, whether it is a whole second or a
/// fraction of one, and whether the two members together bound a window, a
/// single instant or nothing at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "neither member set",
            before: None,
            after: None,
            strict: &[0, 1, 2, 3],
            inclusive: &[0, 1, 2, 3],
        },
        Row {
            case: "the late bound exactly on a record instant",
            before: Some(cutoff(20, 0)),
            after: None,
            strict: &[0],
            inclusive: &[0, 1],
        },
        Row {
            case: "the late bound exactly on a sub-second record instant",
            before: Some(cutoff(20, 250_000_000)),
            after: None,
            strict: &[0, 1],
            inclusive: &[0, 1, 2],
        },
        Row {
            case: "the late bound past a sub-second record instant",
            before: Some(cutoff(20, 500_000_000)),
            after: None,
            strict: &[0, 1, 2],
            inclusive: &[0, 1, 2],
        },
        Row {
            case: "the late bound on the earliest record instant",
            before: Some(cutoff(10, 0)),
            after: None,
            strict: &[],
            inclusive: &[0],
        },
        Row {
            case: "the late bound before every record",
            before: Some(cutoff(5, 0)),
            after: None,
            strict: &[],
            inclusive: &[],
        },
        Row {
            case: "the early bound exactly on a record instant",
            before: None,
            after: Some(cutoff(20, 0)),
            strict: &[2, 3],
            inclusive: &[1, 2, 3],
        },
        Row {
            case: "the early bound exactly on a sub-second record instant",
            before: None,
            after: Some(cutoff(20, 250_000_000)),
            strict: &[3],
            inclusive: &[2, 3],
        },
        Row {
            case: "the early bound short of a record instant",
            before: None,
            after: Some(cutoff(19, 750_000_000)),
            strict: &[1, 2, 3],
            inclusive: &[1, 2, 3],
        },
        Row {
            case: "the early bound on the latest record instant",
            before: None,
            after: Some(cutoff(30, 0)),
            strict: &[],
            inclusive: &[3],
        },
        Row {
            case: "the early bound past every record",
            before: None,
            after: Some(cutoff(35, 0)),
            strict: &[],
            inclusive: &[],
        },
        Row {
            case: "a window whose bounds are the extreme record instants",
            before: Some(cutoff(30, 0)),
            after: Some(cutoff(10, 0)),
            strict: &[1, 2],
            inclusive: &[0, 1, 2, 3],
        },
        Row {
            case: "both bounds on one record instant",
            before: Some(cutoff(20, 0)),
            after: Some(cutoff(20, 0)),
            strict: &[],
            inclusive: &[1],
        },
        Row {
            case: "a window around one record instant",
            before: Some(cutoff(20, 125_000_000)),
            after: Some(cutoff(19, 875_000_000)),
            strict: &[1],
            inclusive: &[1],
        },
        Row {
            case: "a window whose bounds are the wrong way round",
            before: Some(cutoff(10, 0)),
            after: Some(cutoff(30, 0)),
            strict: &[],
            inclusive: &[],
        },
    ]
}

#[tokio::test]
async fn every_listing_reads_its_cutoffs_as_its_own_documentation_words_them() {
    for listing in Listing::ALL {
        let service = HealthLake::new();
        let placed = listing.place(&service).await;

        for row in rows() {
            let request = listing.request(&placed.scope, row.before.clone(), row.after.clone());
            let payload = service.ok(listing.operation(), &request).await;
            let reported = listing.reported(&payload);
            let wanted = expected(
                &placed.ids,
                if listing.inclusive() {
                    row.inclusive
                } else {
                    row.strict
                },
            );
            assert_eq!(
                reported,
                wanted,
                "case {:?} of {}: the listing must report {wanted:?}, got {reported:?}",
                row.case,
                listing.operation()
            );
        }
    }
}

#[tokio::test]
async fn two_adjacent_cutoff_calls_agree_about_a_boundary_instant() {
    // The invariant: for ANY instant, the page bounded late by it and the page
    // bounded early by it cover every record exactly once under a strict
    // reading — a record at the instant itself is in neither page — while an
    // inclusive reading puts a record at that instant in BOTH pages.
    for listing in Listing::ALL {
        let service = HealthLake::new();
        let placed = listing.place(&service).await;

        for (index, instant) in PLACED.iter().enumerate() {
            let at = cutoff(instant.0, instant.1);
            let before = listing.reported(
                &service
                    .ok(
                        listing.operation(),
                        &listing.request(&placed.scope, Some(at.clone()), None),
                    )
                    .await,
            );
            let after = listing.reported(
                &service
                    .ok(
                        listing.operation(),
                        &listing.request(&placed.scope, None, Some(at.clone())),
                    )
                    .await,
            );
            // Every record sharing this instant — the records the two pages
            // have to agree about: both report them under an inclusive
            // reading, neither does under a strict one.
            let sharing: Vec<usize> = PLACED
                .iter()
                .enumerate()
                .filter(|(_, other)| *other == &PLACED[index])
                .map(|(other, _)| other)
                .collect();
            let at_the_instant = expected(&placed.ids, &sharing);
            let case = format!("{} at {at}", listing.operation());

            for record in &at_the_instant {
                let reported = (before.contains(record), after.contains(record));
                assert_eq!(
                    reported,
                    (listing.inclusive(), listing.inclusive()),
                    "case {case:?}: the record {record:?} at the cutoff instant must be \
                     reported by both pages only under the inclusive reading, got \
                     (before, after) = {reported:?}"
                );
            }

            let overlap: Vec<String> = before
                .iter()
                .filter(|id| after.contains(id))
                .cloned()
                .collect();
            let wanted_overlap = if listing.inclusive() {
                at_the_instant.clone()
            } else {
                Vec::new()
            };
            assert_eq!(
                overlap, wanted_overlap,
                "case {case:?}: the two pages may overlap only on the records at the cutoff \
                 instant, and only under the inclusive reading, must be {wanted_overlap:?}, \
                 got {overlap:?}"
            );

            let mut covered: Vec<String> = before.iter().chain(after.iter()).cloned().collect();
            covered.sort();
            covered.dedup();
            let mut wanted = placed.ids.clone();
            if !listing.inclusive() {
                wanted.retain(|id| !at_the_instant.contains(id));
            }
            wanted.sort();
            assert_eq!(
                covered, wanted,
                "case {case:?}: the two pages together must cover {wanted:?}, got {covered:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_placed_record_holds_the_instant_it_was_placed_at() {
    // Every expectation above is built from the placed instants, so a record
    // must really hold the instant it was placed at — including the sub-second
    // part, which the stored timestamp keeps and a cutoff comparison reads.
    let service = HealthLake::new();
    let placed = Listing::ExportJobs.place(&service).await;
    let store = service
        .engine
        .state::<ComAmazonawsHealthlakeHealthlakeState>();
    let store = store.lock();
    for (index, id) in placed.ids.iter().enumerate() {
        let job = store
            .export_jobs
            .get(id)
            .unwrap_or_else(|| panic!("the placed job {id} is held"));
        let at = placed_at(index);
        assert_eq!(
            (job.submitted_at.secs(), job.submitted_at.subsec_nanos()),
            (at.secs(), at.subsec_nanos()),
            "case {id:?}: the placed record must hold {at:?}, got {:?}",
            job.submitted_at
        );
    }
}

// ---------------------------------------------------------------------------
// The RESOLUTION a recorded instant is reported at.
// ---------------------------------------------------------------------------

/// Every member this service reports a RECORDED instant through, as the model
/// names it, read with `smithy_query(query="shape", shape="<reported shape>")`:
/// `CreatedAt` on `DatastoreProperties`; `SubmitTime` and `EndTime` on
/// `ImportJobProperties`, `ExportJobProperties`, `TransformationJobProperties`
/// and `TransformationJobSummary`; `LastUpdatedAt` on the data transformation
/// profile shapes; and `DeletionTime` on
/// `DeleteDataTransformationProfileResponse`.
///
/// `DatastoreBackupStatus`'s own members (`BackupEnabledAt`,
/// `EarliestRestorePoint`, `LatestRestorePoint`) are NOT here: they are the
/// bounds of the restore window a caller's `RestorePointTime` is admitted or
/// refused against rather than instants recorded on a record, and
/// `tests/all/datastore_backup_restore_window.rs` pins them to the instants the
/// clock read.
const RECORDED_INSTANT_MEMBERS: [&str; 5] = [
    "CreatedAt",
    "DeletionTime",
    "EndTime",
    "LastUpdatedAt",
    "SubmitTime",
];

/// How far past a whole millisecond a reported epoch-seconds number may sit.
///
/// A millisecond-resolution instant near the present needs more significant
/// digits than a JSON number's double carries exactly, so reading one back and
/// scaling it to milliseconds lands a few ten-thousandths of a millisecond off a
/// whole one. That representation noise is what this admits — a thousandth of a
/// millisecond, several times the noise and a thousand times under the
/// resolution being asserted, so an instant carrying any real sub-millisecond
/// fraction is still caught.
const PRECISION_SLACK_MILLIS: f64 = 1e-3;

/// How long a poll waits between two reads of a job whose run the clock still
/// owes. Short next to the run span, so a settled job is seen promptly, and the
/// wait itself is bounded by [`SETTLING_DEADLINE`] rather than by this.
const JOB_POLL_PAUSE: Duration = Duration::from_millis(20);

/// Each terminal value the two modelled job-status enums declare (`JobStatus`
/// and `TransformationJobStatus`): a job in one of these has ended, so its
/// `EndTime` is recorded.
const TERMINAL_JOB_STATUSES: [&str; 5] = [
    "COMPLETED",
    "COMPLETED_WITH_ERRORS",
    "FAILED",
    "CANCEL_COMPLETED",
    "CANCEL_FAILED",
];

/// The `DataAccessRoleArn` every start below carries.
const PRECISION_ROLE_ARN: &str = "arn:aws:iam::111122223333:role/HealthLakePrecision";

/// One read a case asserts: the operation that answered it, the body, and the
/// recorded-instant members that body MUST carry — so a case cannot pass by
/// reporting no timestamp at all.
struct Reported {
    read: &'static str,
    body: Value,
    required: &'static [&'static str],
}

/// Which operation of this service RECORDS the instants a row then reads back.
///
/// The rows span the class the rule generalises over: every record kind this
/// service stamps (a data store, a data store whose continuous backup is
/// enabled, all three job records, and a data transformation profile), reported
/// through both a single-record read and a listing, and reached through every
/// operation that writes one of those instants — a create, a start, an update, a
/// publish and a delete.
#[derive(Clone, Copy, Debug)]
enum Recorder {
    Datastore,
    ImportJob,
    ExportJob,
    TransformationJob,
    Profile,
}

impl Recorder {
    const ALL: [Recorder; 5] = [
        Recorder::Datastore,
        Recorder::ImportJob,
        Recorder::ExportJob,
        Recorder::TransformationJob,
        Recorder::Profile,
    ];

    fn case(self) -> &'static str {
        match self {
            Recorder::Datastore => "a created data store, preloaded",
            Recorder::ImportJob => "a started FHIR import job",
            Recorder::ExportJob => "a started FHIR export job",
            Recorder::TransformationJob => "a started data transformation job",
            Recorder::Profile => "a data transformation profile, through its whole lifecycle",
        }
    }

    /// Record this row's instants through the operations that write them, and
    /// answer every read that reports them back.
    async fn drive(self, service: &HealthLake) -> Vec<Reported> {
        match self {
            Recorder::Datastore => {
                // Preloaded, so the create also stamps the FHIR resources it
                // generates — the instants the store sweep below reads off the
                // records themselves.
                let (id, _) = service
                    .create(&json!({
                        "DatastoreTypeVersion": "R4",
                        "DatastoreName": "precision.store",
                        "PreloadDataConfig": { "PreloadDataType": "SYNTHEA" },
                    }))
                    .await;
                vec![
                    Reported {
                        read: "DescribeFHIRDatastore",
                        body: service.poll_until_status(&id, "ACTIVE").await,
                        required: &["CreatedAt"],
                    },
                    Reported {
                        read: "ListFHIRDatastores",
                        body: service.ok("ListFHIRDatastores", &json!({})).await,
                        required: &["CreatedAt"],
                    },
                ]
            }
            Recorder::ImportJob => {
                let datastore_id = active_datastore(service, "precision.import.store").await;
                let started = service
                    .ok(
                        "StartFHIRImportJob",
                        &json!({
                            "DatastoreId": &datastore_id,
                            "DataAccessRoleArn": PRECISION_ROLE_ARN,
                            "InputDataConfig": { "S3Uri": "s3://precision-import-bucket/incoming" },
                            "JobOutputDataConfig": { "S3Configuration": {
                                "S3Uri": "s3://precision-import-bucket/output",
                                "KmsKeyId": KMS_KEY_ID,
                            } },
                        }),
                    )
                    .await;
                let job_id = string_at(&started, "JobId");
                vec![
                    Reported {
                        read: "DescribeFHIRImportJob",
                        body: poll_until_ended(
                            service,
                            "DescribeFHIRImportJob",
                            &json!({ "DatastoreId": &datastore_id, "JobId": &job_id }),
                            "ImportJobProperties",
                        )
                        .await,
                        required: &["SubmitTime", "EndTime"],
                    },
                    Reported {
                        read: "ListFHIRImportJobs",
                        body: service
                            .ok(
                                "ListFHIRImportJobs",
                                &json!({ "DatastoreId": &datastore_id }),
                            )
                            .await,
                        required: &["SubmitTime", "EndTime"],
                    },
                ]
            }
            Recorder::ExportJob => {
                let datastore_id = active_datastore(service, "precision.export.store").await;
                let started = service
                    .ok(
                        "StartFHIRExportJob",
                        &json!({
                            "DatastoreId": &datastore_id,
                            "DataAccessRoleArn": PRECISION_ROLE_ARN,
                            "OutputDataConfig": { "S3Configuration": {
                                "S3Uri": "s3://precision-export-bucket/outgoing",
                                "KmsKeyId": KMS_KEY_ID,
                            } },
                        }),
                    )
                    .await;
                let job_id = string_at(&started, "JobId");
                vec![
                    Reported {
                        read: "DescribeFHIRExportJob",
                        body: poll_until_ended(
                            service,
                            "DescribeFHIRExportJob",
                            &json!({ "DatastoreId": &datastore_id, "JobId": &job_id }),
                            "ExportJobProperties",
                        )
                        .await,
                        required: &["SubmitTime", "EndTime"],
                    },
                    Reported {
                        read: "ListFHIRExportJobs",
                        body: service
                            .ok(
                                "ListFHIRExportJobs",
                                &json!({ "DatastoreId": &datastore_id }),
                            )
                            .await,
                        required: &["SubmitTime", "EndTime"],
                    },
                ]
            }
            Recorder::TransformationJob => {
                let (profile_id, _) =
                    crate::start_data_transformation_job::profile(service, "precision-transform")
                        .await;
                let started = service
                    .ok(
                        "StartDataTransformationJob",
                        &json!({
                            "ClientToken": "precision-transformation-job",
                            "ProfileId": &profile_id,
                            "DataAccessRoleArn": PRECISION_ROLE_ARN,
                            "InputDataConfig": { "S3Uri": "s3://precision-transform-in/ccda" },
                            "OutputDataConfig": { "S3Configuration": {
                                "S3Uri": "s3://precision-transform-out/r4",
                                "KmsKeyId": KMS_KEY_ID,
                            } },
                        }),
                    )
                    .await;
                let job_id = string_at(&started, "JobId");
                vec![
                    Reported {
                        read: "DescribeDataTransformationJob",
                        body: poll_until_ended(
                            service,
                            "DescribeDataTransformationJob",
                            &json!({ "JobId": &job_id }),
                            "TransformationJobProperties",
                        )
                        .await,
                        required: &["SubmitTime", "EndTime"],
                    },
                    Reported {
                        read: "ListDataTransformationJobs",
                        body: service.ok("ListDataTransformationJobs", &json!({})).await,
                        required: &["SubmitTime", "EndTime"],
                    },
                ]
            }
            Recorder::Profile => {
                let created = service
                    .ok(
                        "CreateDataTransformationProfile",
                        &json!({
                            "ProfileName": "precision-profile",
                            "SourceFormat": "CCDA",
                            "Source": { "ProfileMapping": { "ProfileMapping": {
                                "sections/allergies.map": "AllergyIntolerance <- allergy",
                            } } },
                        }),
                    )
                    .await;
                let profile_id = string_at(&created, "ProfileId");
                let read = service
                    .ok(
                        "GetDataTransformationProfile",
                        &json!({ "ProfileId": &profile_id }),
                    )
                    .await;
                let updated = service
                    .ok(
                        "UpdateDataTransformationProfile",
                        &json!({
                            "ProfileId": &profile_id,
                            "ProfileMapping": { "sections/vitals.map": "Observation <- vitals" },
                        }),
                    )
                    .await;
                let published = service
                    .ok(
                        "PublishDataTransformationProfile",
                        &json!({ "ProfileId": &profile_id, "SourceFormat": "CCDA" }),
                    )
                    .await;
                let listed = service
                    .ok(
                        "ListDataTransformationProfiles",
                        &json!({ "SourceFormat": "CCDA" }),
                    )
                    .await;
                let versions = service
                    .ok(
                        "ListDataTransformationProfileVersions",
                        &json!({ "ProfileId": &profile_id }),
                    )
                    .await;
                let deleted = service
                    .ok(
                        "DeleteDataTransformationProfile",
                        &json!({ "ProfileId": &profile_id }),
                    )
                    .await;
                vec![
                    Reported {
                        read: "CreateDataTransformationProfile",
                        body: created,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "GetDataTransformationProfile",
                        body: read,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "UpdateDataTransformationProfile",
                        body: updated,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "PublishDataTransformationProfile",
                        body: published,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "ListDataTransformationProfiles",
                        body: listed,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "ListDataTransformationProfileVersions",
                        body: versions,
                        required: &["LastUpdatedAt"],
                    },
                    Reported {
                        read: "DeleteDataTransformationProfile",
                        body: deleted,
                        required: &["DeletionTime"],
                    },
                ]
            }
        }
    }
}

/// Create a data store and look until it reports the settled `ACTIVE` status a
/// job may be started against, answering its `DatastoreId`.
async fn active_datastore(service: &HealthLake, name: &str) -> String {
    let (id, _) = service
        .create(&json!({ "DatastoreTypeVersion": "R4", "DatastoreName": name }))
        .await;
    service.poll_until_status(&id, "ACTIVE").await;
    id
}

/// Look at `operation` until the job it reports has reached a status it never
/// leaves, and answer that whole response.
///
/// The transition a started job owes is materialised by whichever call reads the
/// record next, so waiting for one means LOOKING again until it has fallen due —
/// never sleeping for the span, which would tie the outcome to how fast the host
/// runs.
async fn poll_until_ended(
    service: &HealthLake,
    operation: &str,
    request: &Value,
    properties: &str,
) -> Value {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    loop {
        let read = service.ok(operation, request).await;
        let status = read[properties]["JobStatus"].as_str().unwrap_or_default();
        if TERMINAL_JOB_STATUSES.contains(&status) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "{operation} did not report an ended job within {SETTLING_DEADLINE:?}, \
             last read {read}"
        );
        tokio::time::sleep(JOB_POLL_PAUSE).await;
    }
}

/// Every recorded instant `body` reports, as `(member, epoch seconds)` pairs,
/// found wherever it carries one: a single reported record, a listing's entries
/// and a nested structure alike.
fn reported_instants(body: &Value, found: &mut Vec<(String, f64)>) {
    match body {
        Value::Object(members) => {
            for (member, value) in members {
                if RECORDED_INSTANT_MEMBERS.contains(&member.as_str()) {
                    if let Some(seconds) = value.as_f64() {
                        found.push((member.clone(), seconds));
                    }
                }
                reported_instants(value, found);
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                reported_instants(entry, found);
            }
        }
        _ => {}
    }
}

/// How far `seconds` sits past a whole millisecond, in milliseconds.
fn sub_millisecond_residue(seconds: f64) -> f64 {
    let millis = seconds * 1_000.0;
    (millis - millis.round()).abs()
}

/// Every instant the STORE holds, read off the records themselves: the same rule
/// checked exactly, free of the rounding a JSON number brings.
fn stored_instants(service: &HealthLake) -> Vec<(String, DateTime)> {
    let state = service
        .engine
        .state::<ComAmazonawsHealthlakeHealthlakeState>();
    let store = state.lock();
    let mut held: Vec<(String, DateTime)> = Vec::new();
    for (id, datastore) in &store.datastores {
        held.push((format!("data store {id} created"), datastore.created_at));
        // `backup_enabled_at` is deliberately absent: it bounds the restore
        // window rather than recording a change to the record, and is held at
        // the resolution that bound is compared at.
        for ((resource_type, resource_id), resource) in &datastore.fhir_resources {
            held.push((
                format!("data store {id} {resource_type}/{resource_id} last updated"),
                resource.last_updated,
            ));
        }
    }
    for (id, job) in &store.import_jobs {
        held.push((format!("import job {id} submitted"), job.submitted_at));
        if let Some(at) = job.ended_at {
            held.push((format!("import job {id} ended"), at));
        }
    }
    for (id, job) in &store.export_jobs {
        held.push((format!("export job {id} submitted"), job.submitted_at));
        if let Some(at) = job.ended_at {
            held.push((format!("export job {id} ended"), at));
        }
    }
    for (id, job) in &store.transformation_jobs {
        held.push((format!("transformation job {id} submitted"), job.submitted_at));
        if let Some(at) = job.ended_at {
            held.push((format!("transformation job {id} ended"), at));
        }
    }
    for (id, profile) in &store.profiles {
        held.push((format!("profile {id} created"), profile.created_at));
        held.push((
            format!("profile {id} draft last updated"),
            profile.draft.last_updated_at,
        ));
        for (version, record) in &profile.published {
            held.push((
                format!("profile {id} version {version} last updated"),
                record.last_updated_at,
            ));
        }
    }
    held
}

#[tokio::test]
async fn every_recorded_instant_is_reported_at_millisecond_resolution() {
    // One service for every row, so the store sweep at the end sees every
    // record each row wrote.
    let service = HealthLake::new();

    for recorder in Recorder::ALL {
        let case = recorder.case();
        for reported in recorder.drive(&service).await {
            let read = reported.read;
            let mut found: Vec<(String, f64)> = Vec::new();
            reported_instants(&reported.body, &mut found);

            for member in reported.required {
                assert!(
                    found.iter().any(|(name, _)| name == member),
                    "case {case:?}: {read} must report a {member}, got {:?} in {}",
                    found,
                    reported.body
                );
            }

            for (member, seconds) in &found {
                let residue = sub_millisecond_residue(*seconds);
                assert!(
                    residue <= PRECISION_SLACK_MILLIS,
                    "case {case:?}: {read} must report {member} at millisecond resolution, \
                     got {seconds} ({residue} ms past a whole millisecond)"
                );
            }
        }
    }

    // And the exact form of the same rule, off the records rather than off a
    // response: nothing this service recorded holds a sub-millisecond fraction
    // at all, so no later read of it can report one.
    let held = stored_instants(&service);
    assert!(
        held.len() > Recorder::ALL.len(),
        "the rows must leave the records they stamped in the store, got {held:?}"
    );
    for (record, at) in &held {
        assert_eq!(
            at.subsec_nanos() % 1_000_000,
            0,
            "the instant recorded for the {record} must hold whole milliseconds, got {at:?}"
        );
    }
}
