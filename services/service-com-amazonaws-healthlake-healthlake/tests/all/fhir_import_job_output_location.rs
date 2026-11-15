// SPDX-License-Identifier: CC0-1.0

//! Where a FHIR import job reports it WROTE, and the rate it reports it was
//! processed at.
//!
//! The rules under test, each over a SET of inputs rather than one case:
//!
//! 1. the output location an import job reports
//!    (`ImportJobProperties$JobOutputDataConfig$S3Configuration$S3Uri`) is not
//!    the location the request supplied: it is that location extended by the
//!    job's OWN folder under it, `<DatastoreId>-FHIR_IMPORT-<JobId>/`, joined by
//!    exactly one `/` whatever the supplied location ended with, with the
//!    supplied `KmsKeyId` unchanged — so the reported location names the job
//!    that wrote it and two jobs sharing one supplied location report two
//!    distinct folders;
//! 2. a job's `JobProgressReport` reports `Throughput`, the rate the run was
//!    processed at: the data it scanned over the span the run occupies, stated
//!    in megabytes per second to one decimal place — present on every reported
//!    run, including one that scanned nothing.
//!
//! `DescribeFHIRImportJob` and `ListFHIRImportJobs` project the ONE job record,
//! so every rule below is asserted on both.
//!
//! Amazon S3 is mounted beside this service on one engine, as the assembled
//! router mounts them, and every input is staged through that service's own
//! `PutObject` — so what an import scans is what Amazon S3 really holds. The
//! helpers that stage it and read a job back are the ones
//! `tests/all/fhir_import_job_start.rs` already has.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_healthlake_healthlake::state::IMPORT_JOB_RUN;

use crate::fhir_import_job_start::{
    create_bucket, describe_job, listed_jobs, ndjson, ok, resource, stage, string_at, BUCKET,
    DATA_ACCESS_ROLE_ARN, KMS_KEY_ID,
};
use crate::fhir_rest_and_validate_source::HealthLake;

/// How long a wait below gives the condition it polls for.
///
/// Generous on purpose, and many times the span the service owes a record: the
/// waits below look again until the condition holds, so a loaded or slow host
/// costs a few more looks rather than a failure.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a wait below LOOKS again at the record it is waiting on.
///
/// Every transition this service owes is materialised by whichever call reads
/// the record next, so waiting for one means looking again until the condition
/// holds — bounded by [`SETTLE_DEADLINE`], which is generous, rather than by
/// how fast the host runs.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Every `JobStatus` an import job never leaves, from the modelled `JobStatus`
/// value set (`smithy_query(query="shape", shape="JobStatus")`).
const TERMINAL_STATUSES: [&str; 5] = [
    "COMPLETED",
    "COMPLETED_WITH_ERRORS",
    "FAILED",
    "CANCEL_COMPLETED",
    "CANCEL_FAILED",
];

/// The megabyte the report's size members and its rate are stated in:
/// `TotalSizeOfScannedFilesInMB` is a size "in MB", and a megabyte is 10^6
/// bytes.
const BYTES_PER_MB: f64 = 1_000_000.0;

/// How close a reported rate must be to the expected one: both are doubles
/// rounded to a tenth, so this admits only representation error.
const RATE_TOLERANCE: f64 = 1e-9;

/// A `StartFHIRImportJob` body reading `input_uri` and writing under
/// `output_uri`.
fn start_request(datastore_id: &str, input_uri: &str, output_uri: &str, kms_key_id: &str) -> Value {
    json!({
        "DatastoreId": datastore_id,
        "DataAccessRoleArn": DATA_ACCESS_ROLE_ARN,
        "InputDataConfig": { "S3Uri": input_uri },
        "JobOutputDataConfig": {
            "S3Configuration": { "S3Uri": output_uri, "KmsKeyId": kms_key_id }
        },
    })
}

/// Create a data store and look at it until it reports `ACTIVE` — the one status
/// an import can be started against — then answer its `DatastoreId`.
async fn active_datastore(service: &HealthLake) -> String {
    let created = ok(
        service,
        "CreateFHIRDatastore",
        &json!({ "DatastoreTypeVersion": "R4", "DatastoreName": "import output location" }),
    )
    .await;
    let id = string_at(&created, "DatastoreId");
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = ok(
            service,
            "DescribeFHIRDatastore",
            &json!({ "DatastoreId": &id }),
        )
        .await;
        if described["DatastoreProperties"]["DatastoreStatus"] == json!("ACTIVE") {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "the data store {id} did not reach ACTIVE within {SETTLE_DEADLINE:?}, last read \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at `DescribeFHIRImportJob` until the job reports a status it never
/// leaves, and answer the `ImportJobProperties` of that read.
async fn settled_job(service: &HealthLake, datastore_id: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = describe_job(service, datastore_id, job_id).await;
        let status = described["JobStatus"].as_str().unwrap_or_default();
        if TERMINAL_STATUSES.contains(&status) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "the import job {job_id} did not settle within {SETTLE_DEADLINE:?}, last read \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `JobOutputDataConfig` the listing reports for one job, so every rule
/// below is asserted on the OTHER reader of the same record too.
async fn listed_output(service: &HealthLake, datastore_id: &str, job_id: &str) -> Value {
    let listed = listed_jobs(service, datastore_id).await;
    listed
        .iter()
        .find(|job| job["JobId"] == json!(job_id))
        .unwrap_or_else(|| panic!("the listing must report the job {job_id}, got {listed:?}"))
        ["JobOutputDataConfig"]
        .clone()
}

/// One supplied output location to start a job with: the rule generalises over
/// the FORM the supplied location takes and over the form of the key that
/// encrypts it.
struct Supplied {
    case: &'static str,
    /// `JobOutputDataConfig$S3Configuration$S3Uri`, within the modelled `S3Uri`
    /// pattern (`^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$`).
    s3_uri: String,
    /// `$KmsKeyId`, within the modelled `EncryptionKeyID` pattern.
    kms_key_id: &'static str,
}

/// The supplied-location table: a bare bucket, a prefix with and without a
/// trailing slash, a prefix of several segments carrying the dots and dashes the
/// modelled pattern admits, a prefix that already ends in several slashes, and a
/// KMS key given as an ARN rather than as a bare id.
fn supplied_locations() -> Vec<Supplied> {
    vec![
        Supplied {
            case: "a bare bucket, no prefix at all",
            s3_uri: format!("s3://{BUCKET}"),
            kms_key_id: KMS_KEY_ID,
        },
        Supplied {
            case: "a prefix without a trailing slash",
            s3_uri: format!("s3://{BUCKET}/output"),
            kms_key_id: KMS_KEY_ID,
        },
        Supplied {
            case: "the same prefix with a trailing slash",
            s3_uri: format!("s3://{BUCKET}/output/"),
            kms_key_id: KMS_KEY_ID,
        },
        Supplied {
            case: "a deep prefix whose segments carry dots and dashes",
            s3_uri: format!("s3://{BUCKET}/out-put/nested.dir/r4/"),
            kms_key_id: KMS_KEY_ID,
        },
        Supplied {
            case: "a prefix ending in more than one slash",
            s3_uri: format!("s3://{BUCKET}/output//"),
            kms_key_id: KMS_KEY_ID,
        },
        Supplied {
            case: "a KMS key given as an ARN",
            s3_uri: format!("s3://{BUCKET}/encrypted/"),
            kms_key_id: "arn:aws:kms:us-east-1:111122223333:key/abcd1234",
        },
    ]
}

#[tokio::test]
async fn a_job_reports_its_own_folder_under_the_supplied_output_location() {
    let service = HealthLake::with_object_store();
    create_bucket(&service).await;
    stage(
        &service,
        "located/patients.ndjson",
        &ndjson(&[resource("Patient", Some("pat-located"), "located")]),
    )
    .await;
    let datastore_id = active_datastore(&service).await;
    let input_uri = format!("s3://{BUCKET}/located/");

    for supplied in supplied_locations() {
        let case = supplied.case;
        let started = ok(
            &service,
            "StartFHIRImportJob",
            &start_request(
                &datastore_id,
                &input_uri,
                &supplied.s3_uri,
                supplied.kms_key_id,
            ),
        )
        .await;
        let job_id = string_at(&started, "JobId");
        let described = settled_job(&service, &datastore_id, &job_id).await;
        let reported = &described["JobOutputDataConfig"]["S3Configuration"];

        // The supplied location, extended by this job's own folder: one `/`
        // joins them whatever the supplied location ended with, and the folder
        // is named for the data store and the job.
        let expected = format!(
            "{}/{datastore_id}-FHIR_IMPORT-{job_id}/",
            supplied.s3_uri.trim_end_matches('/')
        );
        assert_eq!(
            reported["S3Uri"],
            json!(expected),
            "case {case:?}: the job must report its own folder under the supplied location, \
             expected {expected:?}, got {reported}"
        );
        assert_eq!(
            reported["KmsKeyId"],
            json!(supplied.kms_key_id),
            "case {case:?}: the reported KmsKeyId is the supplied one, got {reported}"
        );

        let uri = reported["S3Uri"].as_str().unwrap_or_default().to_string();
        // A client parses the data store and the job back out of the folder the
        // job reports, so its last segment is exactly that pair.
        let folder = uri
            .strip_suffix('/')
            .unwrap_or(&uri)
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            folder,
            format!("{datastore_id}-FHIR_IMPORT-{job_id}"),
            "case {case:?}: the reported folder names the data store and the job, got {uri:?}"
        );
        // Still a location the modelled `S3Uri` admits: an `s3://` URI of at
        // most 1024 characters, with no empty path segment introduced by the
        // join.
        assert!(
            uri.starts_with("s3://") && uri.len() <= 1024,
            "case {case:?}: the reported location must stay within the modelled S3Uri bounds, \
             got {uri:?}"
        );
        assert!(
            !uri["s3://".len()..].contains("//"),
            "case {case:?}: the join must leave no empty path segment, got {uri:?}"
        );

        // The listing projects the same record, so it reports the same location.
        let listed = listed_output(&service, &datastore_id, &job_id).await;
        assert_eq!(
            listed, described["JobOutputDataConfig"],
            "case {case:?}: the listing and the describe must report the SAME output location"
        );
    }
}

#[tokio::test]
async fn two_jobs_under_one_supplied_location_report_distinct_folders() {
    let service = HealthLake::with_object_store();
    create_bucket(&service).await;
    stage(
        &service,
        "shared/patients.ndjson",
        &ndjson(&[resource("Patient", Some("pat-shared"), "shared")]),
    )
    .await;
    let datastore_id = active_datastore(&service).await;
    let input_uri = format!("s3://{BUCKET}/shared/");
    let output_uri = format!("s3://{BUCKET}/shared-output/");

    let mut reported: Vec<(String, String)> = Vec::new();
    for case in ["the first job", "the second job"] {
        let started = ok(
            &service,
            "StartFHIRImportJob",
            &start_request(&datastore_id, &input_uri, &output_uri, KMS_KEY_ID),
        )
        .await;
        let job_id = string_at(&started, "JobId");
        let described = settled_job(&service, &datastore_id, &job_id).await;
        let uri = described["JobOutputDataConfig"]["S3Configuration"]["S3Uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            uri.contains(&format!("{datastore_id}-FHIR_IMPORT-{job_id}")),
            "case {case:?}: each job reports the folder named for ITSELF, got {uri:?}"
        );
        reported.push((job_id, uri));
    }

    let (first_job, first_uri) = &reported[0];
    let (second_job, second_uri) = &reported[1];
    assert_ne!(
        first_uri, second_uri,
        "two jobs writing under one supplied location must report distinct folders, got \
         {first_job}: {first_uri:?} and {second_job}: {second_uri:?}"
    );
}

/// One row of the rate table: the objects to stage, the location the import
/// reads, how many bytes it therefore scans, and the rate it must report.
struct Rate {
    case: &'static str,
    /// `(key, contents)` for each object staged before the start.
    objects: Vec<(String, String)>,
    /// The `InputDataConfig$S3Uri` the start names.
    input_uri: String,
    /// How many bytes the run scans across those objects.
    scanned_bytes: usize,
    /// `JobProgressReport$Throughput`: the scanned megabytes over the span the
    /// run occupies, to one decimal place.
    expected: f64,
}

/// One FHIR resource serialized to exactly `bytes` bytes, padded in a content
/// member, so a row's scanned size is the size it states.
fn sized_resource(id: &str, bytes: usize) -> String {
    let unpadded = json!({ "resourceType": "Patient", "id": id, "language": "" }).to_string();
    let padding = bytes.saturating_sub(unpadded.len());
    let line =
        json!({ "resourceType": "Patient", "id": id, "language": "a".repeat(padding) }).to_string();
    assert_eq!(
        line.len(),
        unpadded.len() + padding,
        "the padded resource must be the size the row states"
    );
    line
}

/// The rate table: the rule generalises over how much data the run scanned and
/// over how many objects it scanned it from, so the rows differ along both — a
/// single tiny resource (a rate that rounds to zero, reported all the same), one
/// object of 30 kB, one of 200 kB, the same 200 kB split over two objects under
/// one prefix, and an input location holding nothing at all.
///
/// The span a run occupies is `state::IMPORT_JOB_RUN` (500 ms), so the expected
/// rate is the scanned megabytes doubled, rounded to a tenth: 0.03 MB -> 0.1,
/// 0.2 MB -> 0.4.
fn rate_rows() -> Vec<Rate> {
    let small = ndjson(&[resource("Patient", Some("pat-rate-small"), "small")]);
    let medium = sized_resource("pat-rate-medium", 30_000);
    let large = sized_resource("pat-rate-large", 200_000);
    let half_a = sized_resource("pat-rate-half-a", 100_000);
    let half_b = sized_resource("pat-rate-half-b", 100_000);
    vec![
        Rate {
            case: "an input location holding nothing",
            objects: Vec::new(),
            input_uri: format!("s3://{BUCKET}/empty/"),
            scanned_bytes: 0,
            expected: 0.0,
        },
        Rate {
            case: "one tiny resource, a rate that rounds to zero",
            objects: vec![("small/patients.ndjson".to_string(), small.clone())],
            input_uri: format!("s3://{BUCKET}/small/"),
            scanned_bytes: small.len(),
            expected: 0.0,
        },
        Rate {
            case: "one object of 30 kB",
            objects: vec![("medium/patients.ndjson".to_string(), medium.clone())],
            input_uri: format!("s3://{BUCKET}/medium/"),
            scanned_bytes: medium.len(),
            expected: 0.1,
        },
        Rate {
            case: "one object of 200 kB",
            objects: vec![("large/patients.ndjson".to_string(), large.clone())],
            input_uri: format!("s3://{BUCKET}/large/"),
            scanned_bytes: large.len(),
            expected: 0.4,
        },
        Rate {
            case: "200 kB split over two objects under one prefix",
            objects: vec![
                ("halves/part-a.ndjson".to_string(), half_a.clone()),
                ("halves/part-b.ndjson".to_string(), half_b.clone()),
            ],
            input_uri: format!("s3://{BUCKET}/halves/"),
            scanned_bytes: half_a.len() + half_b.len(),
            expected: 0.4,
        },
    ]
}

/// The rate the rule states for a run that scanned `bytes`: the scanned
/// megabytes over the span the run occupies, to one decimal place.
fn rate_of(bytes: usize) -> f64 {
    let per_second = (bytes as f64 / BYTES_PER_MB) / IMPORT_JOB_RUN.as_secs_f64();
    (per_second * 10.0).round() / 10.0
}

#[tokio::test]
async fn every_run_reports_the_rate_it_was_processed_at() {
    let mut measured: Vec<(&'static str, usize, f64)> = Vec::new();
    for row in rate_rows() {
        let case = row.case;
        // The row's stated rate is the one the rule gives for its scanned size,
        // so a row cannot assert a number the rule does not.
        let by_rule = rate_of(row.scanned_bytes);
        assert!(
            (by_rule - row.expected).abs() < RATE_TOLERANCE,
            "case {case:?}: the row must state the rate the rule gives for {} bytes, expected \
             {by_rule}, states {}",
            row.scanned_bytes,
            row.expected
        );

        let service = HealthLake::with_object_store();
        create_bucket(&service).await;
        for (key, body) in &row.objects {
            stage(&service, key, body).await;
        }
        let datastore_id = active_datastore(&service).await;
        let started = ok(
            &service,
            "StartFHIRImportJob",
            &start_request(
                &datastore_id,
                &row.input_uri,
                &format!("s3://{BUCKET}/rate-output/"),
                KMS_KEY_ID,
            ),
        )
        .await;
        let job_id = string_at(&started, "JobId");
        let described = settled_job(&service, &datastore_id, &job_id).await;
        let report = &described["JobProgressReport"];

        assert!(
            report["Throughput"].is_number(),
            "case {case:?}: every reported run states a Throughput, got {report}"
        );
        let actual = report["Throughput"].as_f64().unwrap_or_default();
        assert!(
            (actual - row.expected).abs() < RATE_TOLERANCE,
            "case {case:?}: expected Throughput {}, got {actual}",
            row.expected
        );
        // The size the rate is measured over is the one the report states.
        let scanned_mb = report["TotalSizeOfScannedFilesInMB"]
            .as_f64()
            .unwrap_or_default();
        assert!(
            (scanned_mb - row.scanned_bytes as f64 / BYTES_PER_MB).abs() < RATE_TOLERANCE,
            "case {case:?}: the report must state the {} bytes the run scanned, got {report}",
            row.scanned_bytes
        );

        // The listing projects the same record, so it reports the same rate.
        let listed = listed_jobs(&service, &datastore_id).await;
        let entry = listed
            .iter()
            .find(|job| job["JobId"] == json!(job_id))
            .unwrap_or_else(|| panic!("case {case:?}: the listing must report the job {job_id}"));
        assert_eq!(
            entry["JobProgressReport"], described["JobProgressReport"],
            "case {case:?}: the listing and the describe must report the SAME progress report"
        );
        measured.push((case, row.scanned_bytes, actual));
    }

    // A rate is of the data the run took in, so more data over the same span is
    // never a lower rate.
    measured.sort_by_key(|(_, bytes, _)| *bytes);
    for pair in measured.windows(2) {
        let (lower_case, lower_bytes, lower_rate) = pair[0];
        let (higher_case, higher_bytes, higher_rate) = pair[1];
        assert!(
            higher_rate >= lower_rate,
            "case {higher_case:?} scanned {higher_bytes} bytes and must not report a lower rate \
             ({higher_rate}) than case {lower_case:?} at {lower_bytes} bytes ({lower_rate})"
        );
    }
}
