// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateUploadJob`, `GetUploadJob` and `GetUploadJobPath`.
//!
//! The rules under test:
//!
//! 1. `CreateUploadJob` creates the metadata for an upload job in a stored domain — "the
//!    metadata is created for the job with the provided field mapping and unique key" —
//!    mints the job's `JobId`, and persists the job in the status a job that has not
//!    started carries, so a following `GetUploadJob` reports exactly the metadata that was
//!    stored; a reference to a domain or a job that is not stored is
//!    `ResourceNotFoundException` (404) and input outside a modelled constraint is refused
//!    with nothing persisted.
//! 2. `GetUploadJobPath` "retrieves the pre-signed URL and client token for uploading the
//!    file associated with the upload job": for every stored job it reports a target of
//!    THIS emulator for that job's file, the data key the job was created with (so two
//!    reads of one job report the same key and no two jobs share one), and the expiry of
//!    the signature it just minted — and a write performed to that target, with the headers
//!    the operation documents, is stored as the job's file.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use md5::{Digest, Md5};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    service_state, MAX_UPLOAD_FILE_BYTES,
};

use crate::domain_profile_lifecycle::{
    error_code, error_message, upload_job_fields, Emulator, REGION,
};

/// `CreateUploadJobRequest$DataExpiry`: "If not provided, the system default of 2 weeks
/// is used."
const DEFAULT_DATA_EXPIRY_DAYS: i64 = 14;

/// The status `GetUploadJobResponse$Status` documents for a job that has not started:
/// "CREATED: The upload job has been created, but has not started processing yet."
const CREATED: &str = "CREATED";

/// Whether `job_id` is the 32 lowercase hex characters `uuid`'s modelled pattern
/// `^[a-f0-9]{32}$` requires.
fn is_modelled_job_id(job_id: &str) -> bool {
    job_id.len() == 32 && job_id.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// A create persists the metadata the request carried, and the read reports it — over a
/// table of requests differing in the size of the field mapping, the length of the unique
/// key, the display name, and whether `DataExpiry` is stated at all.
#[tokio::test]
async fn a_create_persists_the_job_and_the_read_reports_it() {
    let emulator = Emulator::new();
    emulator.create_domain("uploaddom", 30).await;

    let rows: Vec<(&str, Value, i64)> = vec![
        (
            "no DataExpiry, so the documented default applies",
            json!({
                "DisplayName": "contacts.csv",
                "Fields": upload_job_fields("email", "EMAIL_ADDRESS", "_profile.EmailAddress"),
                "UniqueKey": "email",
            }),
            DEFAULT_DATA_EXPIRY_DAYS,
        ),
        (
            "a DataExpiry at the modelled minimum",
            json!({
                "DisplayName": "one-day.csv",
                "Fields": upload_job_fields("phone", "PHONE_NUMBER", "_profile.PhoneNumber"),
                "UniqueKey": "phone",
                "DataExpiry": 1,
            }),
            1,
        ),
        (
            "a DataExpiry at the modelled maximum",
            json!({
                "DisplayName": "d",
                "Fields": upload_job_fields("account", "STRING", "_profile.AccountNumber"),
                "UniqueKey": "account",
                "DataExpiry": 1098,
            }),
            1098,
        ),
        (
            "a display name at the maximum length and a mapping of several columns",
            json!({
                "DisplayName": "n".repeat(255),
                "Fields": {
                    "first_name": { "ContentType": "NAME", "Source": "_source.first_name", "Target": "_profile.FirstName" },
                    "last.name": { "ContentType": "NAME", "Source": "_source.last.name", "Target": "_profile.LastName" },
                    "e-mail": { "ContentType": "EMAIL_ADDRESS", "Source": "_source.e-mail", "Target": "_profile.EmailAddress" },
                    "loyalty_id": { "ContentType": "NUMBER", "Source": "_source.loyalty_id", "Target": "_profile.Attributes.LoyaltyId" },
                },
                "UniqueKey": "e-mail",
                "DataExpiry": 90,
            }),
            90,
        ),
        (
            "a unique key at the maximum length and a field carrying only a target",
            json!({
                "DisplayName": "long-key.csv",
                "Fields": { "k": { "Target": "_profile.Attributes.K" } },
                "UniqueKey": "k".repeat(1000),
                "DataExpiry": 2,
            }),
            2,
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, body, expected_expiry) in rows {
        let (status, response) = emulator.create_upload_job("uploaddom", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {body:?}, got {status} {response:?}"
        );
        let job_id = response
            .get("JobId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            is_modelled_job_id(&job_id),
            "case {case:?}: expected a 32-character lowercase hex JobId, got {response:?}"
        );
        assert!(
            !minted.contains(&job_id),
            "case {case:?}: every create must mint its own JobId, got {job_id:?} again \
             after {minted:?}"
        );
        minted.push(job_id.clone());

        let (status, read) = emulator.get_upload_job("uploaddom", &job_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetUploadJob 200, got {status} {read:?}"
        );
        assert_eq!(
            read.get("JobId").and_then(Value::as_str),
            Some(job_id.as_str()),
            "case {case:?}: expected the read to report JobId {job_id:?}, got {read:?}"
        );
        for member in ["DisplayName", "Fields", "UniqueKey"] {
            assert_eq!(
                read.get(member),
                body.get(member),
                "case {case:?}: expected the read to report {member} as the create stored \
                 it ({:?}), got {:?}",
                body.get(member),
                read.get(member)
            );
        }
        assert_eq!(
            read.get("DataExpiry").and_then(Value::as_i64),
            Some(expected_expiry),
            "case {case:?}: expected DataExpiry {expected_expiry}, got {read:?}"
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some(CREATED),
            "case {case:?}: a job nobody started must report {CREATED}, got {read:?}"
        );
        // Nothing has processed this job, so it reports no completion and no reason.
        for member in ["CompletedAt", "StatusReason"] {
            assert_eq!(
                read.get(member),
                None,
                "case {case:?}: a job nobody started must not report {member}, got {read:?}"
            );
        }
        // `ResultsSummary` is part of the stored job, so a read of it always reports the
        // structure; it is the counters inside that a job nobody started leaves unset.
        // Asserted in full by `upload_job_results_summary`.
        assert!(
            read.get("ResultsSummary").is_some_and(Value::is_object),
            "case {case:?}: expected a ResultsSummary object, got {read:?}"
        );
        assert!(
            read.get("CreatedAt").is_some(),
            "case {case:?}: expected CreatedAt, got {read:?}"
        );

        // The read changed nothing a later caller sees.
        let (status, again) = emulator.get_upload_job("uploaddom", &job_id).await;
        assert_eq!(
            (status, &again),
            (200, &read),
            "case {case:?}: two reads of an unchanged job must be identical, got {read:?} \
             then {again:?}"
        );
    }
}

/// A job belongs to the domain it was created in: a read of the same id through another
/// domain does not find it, and each domain reports only its own jobs.
#[tokio::test]
async fn a_job_is_reachable_only_through_the_domain_that_holds_it() {
    let emulator = Emulator::new();
    emulator.create_domain("owningdom", 30).await;
    emulator.create_domain("otherdom", 30).await;

    let request = json!({
        "DisplayName": "scoped.csv",
        "Fields": upload_job_fields("email", "EMAIL_ADDRESS", "_profile.EmailAddress"),
        "UniqueKey": "email",
    });
    let (status, response) = emulator.create_upload_job("owningdom", request.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the create\": expected 200, got {status} {response:?}"
    );
    let job_id = response
        .get("JobId")
        .and_then(Value::as_str)
        .expect("a minted JobId")
        .to_string();

    let (status, read) = emulator.get_upload_job("owningdom", &job_id).await;
    assert_eq!(
        status, 200,
        "case \"a read through the owning domain\": expected 200, got {status} {read:?}"
    );

    let (status, read) = emulator.get_upload_job("otherdom", &job_id).await;
    assert_eq!(
        (status, error_code(&read)),
        (404, "ResourceNotFoundException"),
        "case \"a read through another domain\": expected 404, got {status} {read:?}"
    );
}

/// The refusals: a domain that is not stored, a job id the domain does not hold, and
/// input outside a modelled constraint — each bound of `DataExpiry` and `DisplayName`,
/// one past each, and a missing required member. Every refused create persists nothing.
#[tokio::test]
async fn the_upload_job_operations_refuse_an_absent_resource_and_invalid_input() {
    let emulator = Emulator::new();
    emulator.create_domain("boundsjobdom", 30).await;

    let valid = json!({
        "DisplayName": "valid.csv",
        "Fields": upload_job_fields("email", "EMAIL_ADDRESS", "_profile.EmailAddress"),
        "UniqueKey": "email",
    });

    let (status, body) = emulator.create_upload_job("nosuchdomain", valid.clone()).await;
    assert_eq!(
        (status, error_code(&body)),
        (404, "ResourceNotFoundException"),
        "case \"a create in a domain that is not stored\": expected 404, got {status} {body:?}"
    );

    // A well-formed id (32 hex characters) no create minted.
    let absent_id = "0".repeat(32);
    let (status, body) = emulator.get_upload_job("nosuchdomain", &absent_id).await;
    assert_eq!(
        (status, error_code(&body)),
        (404, "ResourceNotFoundException"),
        "case \"a read in a domain that is not stored\": expected 404, got {status} {body:?}"
    );

    let (status, body) = emulator.get_upload_job("boundsjobdom", &absent_id).await;
    assert_eq!(
        (status, error_code(&body)),
        (404, "ResourceNotFoundException"),
        "case \"a read of a job the domain does not hold\": expected 404, got {status} {body:?}"
    );

    // `DisplayName` is `string1To255`, len 1..255; `UniqueKey` is `text`, len 1..1000;
    // `DataExpiry` is `expirationDaysInteger`, range 1..1098; a `FieldMap` value is an
    // `ObjectTypeField` whose `ContentType` is the `FieldContentType` enum.
    let constraints: Vec<(&str, Value, u16)> = vec![
        (
            "a DisplayName at the minimum length",
            json!({ "DisplayName": "d", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email" }),
            200,
        ),
        (
            "a DisplayName one past the maximum length",
            json!({ "DisplayName": "n".repeat(256), "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email" }),
            400,
        ),
        (
            "a DisplayName below the minimum length",
            json!({ "DisplayName": "", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email" }),
            400,
        ),
        (
            "no DisplayName at all",
            json!({ "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email" }),
            400,
        ),
        (
            "no Fields at all",
            json!({ "DisplayName": "nofields.csv", "UniqueKey": "email" }),
            400,
        ),
        (
            "no UniqueKey at all",
            json!({ "DisplayName": "nokey.csv", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress") }),
            400,
        ),
        (
            "a UniqueKey below the minimum length",
            json!({ "DisplayName": "emptykey.csv", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "" }),
            400,
        ),
        (
            "a UniqueKey one past the maximum length",
            json!({ "DisplayName": "longkey.csv", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "k".repeat(1001) }),
            400,
        ),
        (
            "a DataExpiry below the modelled minimum",
            json!({ "DisplayName": "zeroexpiry.csv", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email", "DataExpiry": 0 }),
            400,
        ),
        (
            "a DataExpiry one past the modelled maximum",
            json!({ "DisplayName": "pastexpiry.csv", "Fields": upload_job_fields("email", "STRING", "_profile.EmailAddress"), "UniqueKey": "email", "DataExpiry": 1099 }),
            400,
        ),
        (
            "a ContentType outside the modelled enum",
            json!({ "DisplayName": "badtype.csv", "Fields": upload_job_fields("email", "TELEPHONE", "_profile.EmailAddress"), "UniqueKey": "email" }),
            400,
        ),
    ];

    for (case, body, expected_status) in constraints {
        let (status, response) = emulator.create_upload_job("boundsjobdom", body.clone()).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for {body:?}, got {status} \
             {response:?}"
        );
        if expected_status == 200 {
            let job_id = response
                .get("JobId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                is_modelled_job_id(job_id),
                "case {case:?}: expected a 32-character lowercase hex JobId, got {response:?}"
            );
            continue;
        }
        assert_eq!(
            error_code(&response),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {response:?}"
        );
        assert_eq!(
            response.get("JobId"),
            None,
            "case {case:?}: a refused create must mint no JobId, got {response:?}"
        );
    }
}

/// The bytes of the data key an upload target's writes are encrypted under: 32, the size
/// the `AES256` algorithm the operation documents takes.
const DATA_KEY_BYTES: usize = 32;

/// The longest `Url` the model admits: `stringTo2048`, len 0..2048.
const MAX_URL_LENGTH: usize = 2048;

/// Epoch seconds now, the unit a restJson1 body carries a `timestamp` in.
fn epoch_seconds_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// The three headers `GetUploadJobPathResponse$ClientToken` documents a write to the
/// published target must carry: "x-amz-server-side-encryption-customer-key: Client Token,
/// x-amz-server-side-encryption-customer-key-MD5: MD5 Client Token,
/// x-amz-server-side-encryption-customer-algorithm: AES256" — the digest computed here the
/// way a client computes it, from the key the response handed back.
fn upload_headers(algorithm: &str, client_token: &str, digest_of: &str) -> Vec<(String, String)> {
    let key = ::aws_smithy_types::base64::decode(digest_of).unwrap_or_default();
    let digest = ::aws_smithy_types::base64::encode(Md5::digest(&key));
    vec![
        (
            "x-amz-server-side-encryption-customer-algorithm".to_string(),
            algorithm.to_string(),
        ),
        (
            "x-amz-server-side-encryption-customer-key".to_string(),
            client_token.to_string(),
        ),
        (
            "x-amz-server-side-encryption-customer-key-MD5".to_string(),
            digest,
        ),
    ]
}

/// The headers of a write that presents `client_token` exactly as `GetUploadJobPath`
/// reported it.
fn authorized_headers(client_token: &str) -> Vec<(String, String)> {
    upload_headers("AES256", client_token, client_token)
}

/// One request to an upload target, with the headers a caller was told to send.
async fn write_to_target(
    emulator: &Emulator,
    url: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> (u16, Value) {
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.send_raw("PUT", url, &borrowed, body).await
}

/// The file the store holds for one upload job, read through the service's own state so a
/// write is asserted against what was PERSISTED rather than against the answer to it.
fn stored_file(emulator: &Emulator, domain: &str, job_id: &str) -> Option<Vec<u8>> {
    let state = service_state(&*emulator.engine).expect("the service store");
    let store = state.lock();
    store
        .domain(REGION, domain)?
        .upload_jobs
        .get(job_id)?
        .uploaded_file
        .clone()
}

/// One created upload job and the upload target `GetUploadJobPath` publishes for it: the
/// `JobId`, the `Url` as handed out, and the `ClientToken` its writes are authorized with.
async fn job_with_upload_target(
    emulator: &Emulator,
    domain: &str,
    display_name: &str,
) -> (String, String, String) {
    let (status, created) = emulator
        .create_upload_job(
            domain,
            json!({
                "DisplayName": display_name,
                "Fields": upload_job_fields("email", "EMAIL_ADDRESS", "_profile.EmailAddress"),
                "UniqueKey": "email",
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {display_name:?}: expected CreateUploadJob 200, got {status} {created:?}"
    );
    let job_id = created
        .get("JobId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {display_name:?}: no JobId in {created:?}"))
        .to_string();

    let (status, path) = emulator.get_upload_job_path(domain, &job_id).await;
    assert_eq!(
        status, 200,
        "case {display_name:?}: expected GetUploadJobPath 200, got {status} {path:?}"
    );
    let url = path
        .get("Url")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {display_name:?}: no Url in {path:?}"))
        .to_string();
    let client_token = path
        .get("ClientToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {display_name:?}: no ClientToken in {path:?}"))
        .to_string();
    (job_id, url, client_token)
}

/// Every stored job's upload path is a target of THIS emulator for that job's file,
/// carrying the data key the job was created with and the expiry of the signature the call
/// minted — over a table of jobs differing in the size of the field mapping, the display
/// name, whether `DataExpiry` is stated, and the domain that holds them.
#[tokio::test]
async fn the_upload_path_of_a_stored_job_is_a_target_of_this_emulator() {
    let emulator = Emulator::new();
    emulator.create_domain("pathdom", 30).await;
    emulator.create_domain("pathotherdom", 30).await;

    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "one mapped column and no DataExpiry",
            "pathdom",
            json!({
                "DisplayName": "contacts.csv",
                "Fields": upload_job_fields("email", "EMAIL_ADDRESS", "_profile.EmailAddress"),
                "UniqueKey": "email",
            }),
        ),
        (
            "a DataExpiry at the modelled minimum",
            "pathdom",
            json!({
                "DisplayName": "d",
                "Fields": upload_job_fields("phone", "PHONE_NUMBER", "_profile.PhoneNumber"),
                "UniqueKey": "phone",
                "DataExpiry": 1,
            }),
        ),
        (
            "a display name at the maximum length and a mapping of several columns",
            "pathdom",
            json!({
                "DisplayName": "n".repeat(255),
                "Fields": {
                    "first_name": { "ContentType": "NAME", "Source": "_source.first_name", "Target": "_profile.FirstName" },
                    "e-mail": { "ContentType": "EMAIL_ADDRESS", "Source": "_source.e-mail", "Target": "_profile.EmailAddress" },
                    "loyalty_id": { "ContentType": "NUMBER", "Source": "_source.loyalty_id", "Target": "_profile.Attributes.LoyaltyId" },
                },
                "UniqueKey": "e-mail",
                "DataExpiry": 1098,
            }),
        ),
        (
            "a job of another domain, with a unique key at the maximum length",
            "pathotherdom",
            json!({
                "DisplayName": "elsewhere.csv",
                "Fields": { "k": { "Target": "_profile.Attributes.K" } },
                "UniqueKey": "k".repeat(1000),
            }),
        ),
    ];

    let mut keys: Vec<String> = Vec::new();
    for (case, domain, body) in rows {
        let (status, created) = emulator.create_upload_job(domain, body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateUploadJob 200 for {body:?}, got {status} {created:?}"
        );
        let job_id = created
            .get("JobId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no JobId in {created:?}"))
            .to_string();

        let before = epoch_seconds_now();
        let (status, path) = emulator.get_upload_job_path(domain, &job_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetUploadJobPath 200, got {status} {path:?}"
        );

        let url = path.get("Url").and_then(Value::as_str).unwrap_or_default();
        assert!(
            url.len() <= MAX_URL_LENGTH && url.starts_with("https://"),
            "case {case:?}: expected an https target within the modelled 2048 characters, \
             got {url:?}"
        );
        // The target names the job it was asked for, so it cannot address another job's
        // file, and it leads back to THIS emulator rather than to a host only AWS serves.
        let expected_path = format!("/domains/{domain}/upload-jobs/{job_id}/file");
        assert!(
            url.contains(&expected_path),
            "case {case:?}: expected the target to name the stored job at {expected_path:?}, \
             got {url:?}"
        );
        assert!(
            !url.contains("amazonaws.com"),
            "case {case:?}: expected a target of this emulator, got {url:?}"
        );
        // A PRESIGNED target: it carries its own SigV4 authorisation, scoped to this
        // service, which is what lets a caller follow it with no Authorization header.
        for marker in [
            "X-Amz-Signature=",
            "X-Amz-Expires=",
            "%2Fprofile%2Faws4_request",
        ] {
            assert!(
                url.contains(marker),
                "case {case:?}: expected a presigned target carrying {marker:?}, got {url:?}"
            );
        }

        let client_token = path
            .get("ClientToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let decoded = ::aws_smithy_types::base64::decode(&client_token);
        assert!(
            decoded.is_ok_and(|key| key.len() == DATA_KEY_BYTES),
            "case {case:?}: expected the client token to be a base64 256-bit data key, \
             got {client_token:?}"
        );
        assert!(
            (1..=1000).contains(&client_token.len()),
            "case {case:?}: expected a token within the modelled 1..1000 characters, got \
             {client_token:?}"
        );
        assert!(
            !keys.contains(&client_token),
            "case {case:?}: every job's data key is its own, got {client_token:?} again \
             after {keys:?}"
        );
        keys.push(client_token.clone());

        let valid_until = path.get("ValidUntil").and_then(Value::as_f64);
        assert!(
            valid_until.is_some_and(|at| at > before),
            "case {case:?}: expected ValidUntil after the call ({before}), got {path:?}"
        );
        assert!(
            valid_until.is_some_and(|at| at <= before + 7_200.0),
            "case {case:?}: expected ValidUntil within the window the target is signed \
             for, got {path:?}"
        );

        // The key belongs to the JOB, not to the call: a second read reports the same one,
        // so a file written under the first answer stays readable.
        let (status, again) = emulator.get_upload_job_path(domain, &job_id).await;
        assert_eq!(
            (status, again.get("ClientToken")),
            (200, path.get("ClientToken")),
            "case {case:?}: expected the job's own data key on every read, got {again:?} \
             after {path:?}"
        );

        // Reading the path is a read: the job itself is unchanged, and no file appeared.
        let (status, read) = emulator.get_upload_job(domain, &job_id).await;
        assert_eq!(
            (status, read.get("Status").and_then(Value::as_str)),
            (200, Some(CREATED)),
            "case {case:?}: reading the upload path must not move the job, got {read:?}"
        );
        assert_eq!(
            stored_file(&emulator, domain, &job_id),
            None,
            "case {case:?}: reading the upload path must not store a file"
        );
    }
}

/// The upload path of a domain or a job that is not stored is `ResourceNotFoundException`
/// (404), worded as the upload-job surface words each absence — over a table of domain
/// names spanning the modelled `name` shape, and the two ways a job can be absent.
#[tokio::test]
async fn the_upload_path_refuses_a_domain_or_job_that_is_not_stored() {
    let emulator = Emulator::new();
    emulator.create_domain("pathrefusedom", 30).await;
    emulator.create_domain("pathsecondom", 30).await;
    let (job_id, _, _) = job_with_upload_target(&emulator, "pathrefusedom", "held.csv").await;
    // A well-formed id (32 lowercase hex characters) no create minted.
    let absent_id = "0".repeat(32);

    let names: Vec<(&str, String)> = vec![
        ("a name at the modelled minimum length", "a".to_string()),
        ("a plain name", "nosuchdomain".to_string()),
        (
            "a name carrying the dash and underscore the pattern allows",
            "probe-cp_missing-01".to_string(),
        ),
        ("a name of digits only", "0123456789".to_string()),
        ("a name at the modelled maximum length", "d".repeat(64)),
        (
            "a name differing from a stored one only in case",
            "PATHREFUSEDOM".to_string(),
        ),
    ];
    for (case, name) in names {
        let expected = format!("Domain with name {name} could not be found");
        let (status, body) = emulator.get_upload_job_path(&name, &absent_id).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (404, "ResourceNotFoundException", expected.as_str()),
            "case {case:?}: expected 404 ResourceNotFoundException {expected:?}, got \
             {status} {body:?}"
        );
    }

    let jobs: Vec<(&str, &str, String)> = vec![
        (
            "a job id no create minted",
            "pathrefusedom",
            absent_id.clone(),
        ),
        (
            "a job held by another domain",
            "pathsecondom",
            job_id.clone(),
        ),
    ];
    for (case, domain, id) in jobs {
        let expected = format!("The upload job {id} does not exist in domain {domain}");
        let (status, body) = emulator.get_upload_job_path(domain, &id).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (404, "ResourceNotFoundException", expected.as_str()),
            "case {case:?}: expected 404 ResourceNotFoundException {expected:?}, got \
             {status} {body:?}"
        );
    }
}

/// A write performed to the published target, with the headers the operation documents,
/// becomes the job's file — over a table of bodies differing in size and content, including
/// a re-write of the same target and the largest file this emulator keeps. Each job's
/// target writes only that job's file.
#[tokio::test]
async fn a_write_to_the_published_upload_target_becomes_the_jobs_file() {
    let emulator = Emulator::new();
    emulator.create_domain("uploadtargetdom", 30).await;
    let (job_id, url, client_token) =
        job_with_upload_target(&emulator, "uploadtargetdom", "first.csv").await;
    let (other_id, other_url, other_token) =
        job_with_upload_target(&emulator, "uploadtargetdom", "second.csv").await;
    let headers = authorized_headers(&client_token);

    let rows: Vec<(&str, Vec<u8>)> = vec![
        (
            "a one-column CSV",
            b"email\nada@example.test\n".to_vec(),
        ),
        ("a single byte", vec![b'x']),
        (
            "several rows of several columns, with non-ASCII text",
            "email,name\nada@example.test,Ada Lovelace\njose@example.test,José\n"
                .as_bytes()
                .to_vec(),
        ),
        (
            "a re-write of the same target",
            b"email\ngrace@example.test\n".to_vec(),
        ),
        (
            "the largest file this emulator keeps for one job",
            vec![b'r'; MAX_UPLOAD_FILE_BYTES],
        ),
    ];
    for (case, body) in rows {
        let (status, answer) = write_to_target(&emulator, &url, &headers, body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the published target to take the write, got {status} \
             {answer:?}"
        );
        let stored = stored_file(&emulator, "uploadtargetdom", &job_id);
        assert_eq!(
            stored.as_deref(),
            Some(body.as_slice()),
            "case {case:?}: expected the write to become the job's file ({} bytes), got {:?} \
             bytes",
            body.len(),
            stored.as_ref().map(Vec::len)
        );
        // One target writes ONE job's file: the other job of the same domain is untouched.
        assert_eq!(
            stored_file(&emulator, "uploadtargetdom", &other_id),
            None,
            "case {case:?}: a write to one target must not reach another job"
        );
    }

    // The other job's own target takes its own write, under its own key.
    let other_body = b"email\nhopper@example.test\n".to_vec();
    let (status, answer) = write_to_target(
        &emulator,
        &other_url,
        &authorized_headers(&other_token),
        other_body.clone(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the second job's target\": expected 200, got {status} {answer:?}"
    );
    assert_eq!(
        stored_file(&emulator, "uploadtargetdom", &other_id).as_deref(),
        Some(other_body.as_slice()),
        "case \"the second job's target\": expected the write to become that job's file"
    );
}

/// The upload target refuses everything that is not the one documented write: a request
/// that does not present the job's own data key under the documented headers, a method
/// other than the write, a target naming a job that is not stored, and a file larger than
/// this emulator keeps — each leaving the job's file as it was.
#[tokio::test]
async fn the_upload_target_refuses_a_write_it_cannot_authorize() {
    let emulator = Emulator::new();
    emulator.create_domain("uploadrefusedom", 30).await;
    let (job_id, url, client_token) =
        job_with_upload_target(&emulator, "uploadrefusedom", "refused.csv").await;
    // Another job's key: a well-formed data key this target was never handed out with.
    let (_, _, other_token) =
        job_with_upload_target(&emulator, "uploadrefusedom", "other.csv").await;
    let body = b"email\nada@example.test\n".to_vec();

    let unauthorized: Vec<(&str, Vec<(String, String)>)> = vec![
        ("no encryption headers at all", Vec::new()),
        (
            "another job's data key",
            authorized_headers(&other_token),
        ),
        (
            "the job's key with a digest of another key",
            upload_headers("AES256", &client_token, &other_token),
        ),
        (
            "the job's key with no digest at all",
            authorized_headers(&client_token)
                .into_iter()
                .filter(|(name, _)| !name.ends_with("-MD5"))
                .collect(),
        ),
        (
            "an algorithm the operation does not document",
            upload_headers("AES128", &client_token, &client_token),
        ),
    ];
    for (case, headers) in unauthorized {
        let (status, answer) = write_to_target(&emulator, &url, &headers, body.clone()).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the target to refuse the write, got {status} {answer:?}"
        );
        assert_eq!(
            stored_file(&emulator, "uploadrefusedom", &job_id),
            None,
            "case {case:?}: a refused write must store nothing"
        );
    }

    let authorized = authorized_headers(&client_token);
    let borrowed: Vec<(&str, &str)> = authorized
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    // A target is WRITTEN, not read: any other method is refused.
    let (status, answer) = emulator.send_raw("GET", &url, &borrowed, Vec::new()).await;
    assert_eq!(
        status, 405,
        "case \"a read of the upload target\": expected 405, got {status} {answer:?}"
    );

    // A target naming a job, or a domain, this store does not hold is not a destination.
    let absent_targets: Vec<(&str, String)> = vec![
        (
            "a job id no create minted",
            format!("/domains/uploadrefusedom/upload-jobs/{}/file", "0".repeat(32)),
        ),
        (
            "a domain that is not stored",
            format!("/domains/nosuchdomain/upload-jobs/{job_id}/file"),
        ),
    ];
    for (case, path) in absent_targets {
        let uri = format!("https://profile.us-east-1.amazonaws.com{path}");
        let (status, answer) = write_to_target(&emulator, &uri, &authorized, body.clone()).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for a target naming nothing stored, got {status} \
             {answer:?}"
        );
    }

    // One byte past what this emulator keeps for one job's file.
    let (status, answer) = write_to_target(
        &emulator,
        &url,
        &authorized,
        vec![b'r'; MAX_UPLOAD_FILE_BYTES + 1],
    )
    .await;
    assert_eq!(
        status, 413,
        "case \"a file past the bound\": expected 413, got {status} {answer:?}"
    );
    assert_eq!(
        stored_file(&emulator, "uploadrefusedom", &job_id),
        None,
        "case \"a file past the bound\": a refused write must store nothing"
    );

    // The refusals are about the request, not the target: the documented write still lands.
    let (status, answer) = write_to_target(&emulator, &url, &authorized, body.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the documented write\": expected 200, got {status} {answer:?}"
    );
    assert_eq!(
        stored_file(&emulator, "uploadrefusedom", &job_id).as_deref(),
        Some(body.as_slice()),
        "case \"the documented write\": expected the write to become the job's file"
    );
}
