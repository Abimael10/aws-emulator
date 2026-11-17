// SPDX-License-Identifier: CC0-1.0

//! What AWS HealthImaging may do with the REGION it reads out of a request's SigV4
//! credential scope.
//!
//! RULE: the region field of a credential scope is caller-supplied text, so a service uses
//! it only where it is spelled like a Region identifier — the `[a-z0-9-]+` alphabet the
//! model constrains the region segment of this service's `Arn` shape to
//! (`smithy_query(query="shape",shape="Arn")`) — and serves a request carrying anything
//! else in its default Region. Two things follow, and both are asserted below: every ARN
//! the service mints and reports parses back through its OWN ARN-keyed operations, and
//! every in-process client it builds for a request addresses an authority that is
//! reachable, so a DICOM import still reads the emulator's own Amazon S3.
//!
//! The second half is observable only where more than one service is mounted on one
//! engine: the objects the import reads and the manifests it writes are Amazon S3's, the
//! role it runs as is IAM's, and the data store and the job are AWS HealthImaging's. The
//! medical-imaging crate's own suites reach exactly one of the three.
//!
//! The rows vary the one thing the rule generalises over: the TEXT the credential scope's
//! region field carries — identifiers (including one no emulator has heard of and a
//! one-character one, since the rule is a shape and not a list of Region codes) beside text
//! that is not one (uppercase, dots, a colon, an underscore, a space, percent-escapes).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_medicalimaging_ahigatewayservice::state::DEFAULT_REGION;
use smithy_server_runtime::MultiServiceRouter;

// The shared helpers of this test crate, called rather than copied: a suite that writes its
// own drifts from the others (`core/router/tests/all/query_wire.rs`). `form_encode` is the
// one percent-encoder — what an `@httpLabel` segment carrying an ARN's `:` and `/` goes
// through too — `element`/`elements` the one XML element readers, and `authorization` the one
// SigV4 credential scope a request of this crate is signed with.
use crate::cloudformation_provisions_s3tables_table_buckets::authorization;
use crate::query_wire::{element, elements, form_encode};

/// The region the Amazon S3 and IAM calls of this suite are signed for: only the
/// medical-imaging calls vary their credential scope, so the staging never does.
const STAGING_REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The bucket an import's input and output prefixes live in.
const BUCKET: &str = "healthimaging-signing-region";

/// A trust policy that lets AWS HealthImaging assume the role it is attached to; it reaches
/// IAM through the test crate's one form encoder, as any document in a form body does.
const HEALTHIMAGING_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"medical-imaging.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past every
/// span this emulator gives one, so a slow machine does not fail a case the clock would
/// have settled.
const POLL_BUDGET: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// The table: what the credential scope's region field carries
// ---------------------------------------------------------------------------

/// One row: the text a request's credential scope carries in its region field, and the
/// Region the service must then serve that request in.
#[derive(Debug)]
struct ScopeCase {
    case: &'static str,
    /// The text between the date and the signing name of the `Credential=` scope.
    scope_region: &'static str,
    /// The Region the request is served in: the scope's own text when that text is spelled
    /// like a Region identifier, the service's default Region when it is not.
    served_in: &'static str,
    /// Whether the import half of the rule is driven for this row too. A DICOM import is a
    /// whole job per row, so that half takes the three rows that span it — an identifier,
    /// and the two spellings that are not one in the ways a URL authority notices (a colon,
    /// which reads as a port, and a space) — while the ARN half, which costs one create,
    /// takes every row.
    imports: bool,
}

/// The rows. The identifiers are not a list of real Region codes on purpose: the rule is
/// the ALPHABET the model admits for the region segment of an ARN, so a code no emulator
/// has heard of and a one-character one are identifiers too, while the cited `us-east-1:X`
/// is one row among six spellings that are not identifiers.
fn scope_cases() -> Vec<ScopeCase> {
    vec![
        ScopeCase {
            case: "a Region identifier",
            scope_region: "us-east-1",
            served_in: "us-east-1",
            imports: true,
        },
        ScopeCase {
            case: "a Region identifier of another geography",
            scope_region: "ap-southeast-4",
            served_in: "ap-southeast-4",
            imports: false,
        },
        ScopeCase {
            case: "a Region identifier no emulator has heard of",
            scope_region: "zz-moon-7",
            served_in: "zz-moon-7",
            imports: false,
        },
        ScopeCase {
            case: "a one-character Region identifier",
            scope_region: "q",
            served_in: "q",
            imports: false,
        },
        ScopeCase {
            case: "uppercase letters",
            scope_region: "US-East-1",
            served_in: DEFAULT_REGION,
            imports: false,
        },
        ScopeCase {
            case: "a dotted name",
            scope_region: "us.east.1",
            served_in: DEFAULT_REGION,
            imports: false,
        },
        ScopeCase {
            case: "a colon in the field",
            scope_region: "us-east-1:X",
            served_in: DEFAULT_REGION,
            imports: true,
        },
        ScopeCase {
            case: "underscores",
            scope_region: "us_east_1",
            served_in: DEFAULT_REGION,
            imports: false,
        },
        ScopeCase {
            case: "a space in the field",
            scope_region: "us east 1",
            served_in: DEFAULT_REGION,
            imports: true,
        },
        ScopeCase {
            case: "percent-escapes",
            scope_region: "us%2Deast%2D1",
            served_in: DEFAULT_REGION,
            imports: false,
        },
    ]
}

/// Whether `value` is spelled in the alphabet the model constrains the region segment of
/// the `Arn` shape to: `[a-z0-9-]+`, from
/// `smithy_query(query="shape",shape="Arn")` —
/// `^arn:aws((-us-gov)|(-iso)|(-iso-b)|(-cn))?:medical-imaging:[a-z0-9-]+:[0-9]{12}:…$`.
///
/// Written out here rather than asked of the crate, so what the reported ARN is held to is
/// the model's own rule.
fn is_region_alphabet(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

/// What one call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text, for a JSON or XML answer.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The member at `path` of a JSON body, as text.
    fn string(&self, path: &[&str]) -> String {
        let body = self.json();
        let mut cursor = &body;
        for key in path {
            cursor = &cursor[*key];
        }
        cursor.as_str().unwrap_or_default().to_string()
    }
}

/// The three services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_medicalimaging_ahigatewayservice::EmulatedService::new(engine),
    );
    router
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One AWS HealthImaging call, signed for `scope_region`. The `host` is the same whatever
/// that field carries, so the credential scope is the only thing a row varies.
async fn imaging(
    router: &MultiServiceRouter,
    scope_region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("medical-imaging.{STAGING_REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("medical-imaging", scope_region)),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One AWS HealthImaging call that has to succeed.
async fn imaging_ok(
    router: &MultiServiceRouter,
    case: &str,
    scope_region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let answer = imaging(router, scope_region, method, path, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer
}

/// One Amazon S3 REST call, path-style as the emulator serves it.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut with_credential: Vec<(&str, &str)> = headers.to_vec();
    let credential = authorization("s3", STAGING_REGION);
    with_credential.push(("authorization", &credential));
    dispatch(
        router,
        method,
        &format!("s3.{STAGING_REGION}.amazonaws.com"),
        path,
        &with_credential,
        body,
    )
    .await
}

/// One IAM awsQuery call that has to succeed, answering its body.
async fn iam_ok(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let answer = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam", STAGING_REGION)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup {case:?}: expected HTTP 200 from IAM, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// One IAM role created with `trust` as its trust policy, answering its ARN.
async fn role(router: &MultiServiceRouter, name: &str, trust: &str) -> String {
    let created = iam_ok(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}\
             &AssumeRolePolicyDocument={}",
            form_encode(name),
            form_encode(trust)
        ),
    )
    .await;
    element(&created, "Arn")
}

/// Create a data store under `scope_region` and poll `GetDatastore` until it reports
/// `ACTIVE`, answering its id. The deadline bounds a hung emulator rather than the span
/// being waited for.
async fn active_datastore(
    router: &MultiServiceRouter,
    case: &str,
    scope_region: &str,
    name: &str,
    token: &str,
) -> String {
    let datastore_id = create_datastore(router, case, scope_region, name, token).await;
    settle_datastore(router, case, scope_region, &datastore_id).await;
    datastore_id
}

/// `POST /datastore` under `scope_region`, answering the minted id WITHOUT waiting for the
/// activation the clock owes it: a caller creates every store it wants and settles once, so
/// a table of rows pays about one activation span rather than one per row.
async fn create_datastore(
    router: &MultiServiceRouter,
    case: &str,
    scope_region: &str,
    name: &str,
    token: &str,
) -> String {
    let created = imaging_ok(
        router,
        case,
        scope_region,
        "POST",
        "/datastore",
        Some(json!({ "datastoreName": name, "clientToken": token })),
    )
    .await;
    created.string(&["datastoreId"])
}

/// Poll `GetDatastore` until it reports `ACTIVE`.
async fn settle_datastore(
    router: &MultiServiceRouter,
    case: &str,
    scope_region: &str,
    datastore_id: &str,
) {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let read = imaging_ok(
            router,
            case,
            scope_region,
            "GET",
            &format!("/datastore/{datastore_id}"),
            None,
        )
        .await;
        if read.string(&["datastoreProperties", "datastoreStatus"]) == "ACTIVE" {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the data store never reached ACTIVE, last {:?}",
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// What the service mints: the ARN
// ---------------------------------------------------------------------------

/// RULE (above): the Region a request is served in is the credential scope's region field
/// when that field is spelled like a Region identifier and the service's default Region
/// otherwise, so every `datastoreArn` reported names a region segment the model's `Arn`
/// pattern admits — and therefore one the service's own ARN-keyed tagging routes read back.
#[tokio::test]
async fn a_reported_datastore_arn_names_a_region_identifier_whatever_the_scope_carries() {
    let router = emulator();
    let rows = scope_cases();

    // Every row's data store is created before any is settled: the activation is a span the
    // clock owes each of them, and they run concurrently.
    let mut minted: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        minted.push(
            create_datastore(
                &router,
                row.case,
                row.scope_region,
                &format!("scope.store-{index}"),
                &format!("scope-token-{index}"),
            )
            .await,
        );
    }

    for (row, datastore_id) in rows.iter().zip(minted) {
        let case = row.case;
        settle_datastore(&router, case, row.scope_region, &datastore_id).await;

        let read = imaging_ok(
            &router,
            case,
            row.scope_region,
            "GET",
            &format!("/datastore/{datastore_id}"),
            None,
        )
        .await;
        let arn = read.string(&["datastoreProperties", "datastoreArn"]);
        assert_eq!(
            arn,
            format!("arn:aws:medical-imaging:{}:{ACCOUNT}:datastore/{datastore_id}", row.served_in),
            "case {case:?}: expected the ARN to name the Region the request is served in, got \
             {arn:?}"
        );
        let region_segment = arn.split(':').nth(3).unwrap_or_default().to_string();
        assert!(
            is_region_alphabet(&region_segment),
            "case {case:?}: expected the ARN's region segment to be spelled in the alphabet the \
             model admits, got {region_segment:?} in {arn:?}"
        );

        // The ARN the service just reported, fed straight back to the service: the three
        // ARN-keyed tagging routes read it, which is only true of an ARN its own parser
        // admits.
        let tagged = imaging(
            &router,
            row.scope_region,
            "POST",
            &format!("/tags/{}", form_encode(&arn)),
            Some(json!({ "tags": { "suite": "signing-region" } })),
        )
        .await;
        assert_eq!(
            tagged.status, 200,
            "case {case:?}: expected TagResource on the reported ARN {arn:?} to succeed, got {} \
             with {:?}",
            tagged.status,
            tagged.text()
        );
        let listed = imaging(
            &router,
            row.scope_region,
            "GET",
            &format!("/tags/{}", form_encode(&arn)),
            None,
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected ListTagsForResource on the reported ARN {arn:?} to succeed, \
             got {} with {:?}",
            listed.status,
            listed.text()
        );
        assert_eq!(
            listed.json()["tags"],
            json!({ "suite": "signing-region" }),
            "case {case:?}: expected the tag written through the reported ARN to read back, got \
             {:?}",
            listed.text()
        );
        let untagged = imaging(
            &router,
            row.scope_region,
            "DELETE",
            &format!("/tags/{}?tagKeys=suite", form_encode(&arn)),
            None,
        )
        .await;
        assert_eq!(
            untagged.status, 200,
            "case {case:?}: expected UntagResource on the reported ARN {arn:?} to succeed, got {} \
             with {:?}",
            untagged.status,
            untagged.text()
        );

        // The Region the ARN names is also the one the resource LIVES in: a request signed
        // for it reaches the data store, and one signed for any other Region identifier
        // reaches nothing.
        let at_home = imaging(
            &router,
            row.served_in,
            "GET",
            &format!("/datastore/{datastore_id}"),
            None,
        )
        .await;
        assert_eq!(
            at_home.status, 200,
            "case {case:?}: expected the data store to be readable in {:?}, got {} with {:?}",
            row.served_in,
            at_home.status,
            at_home.text()
        );
        let elsewhere = if row.served_in == "eu-west-1" {
            "ap-south-1"
        } else {
            "eu-west-1"
        };
        let away = imaging(
            &router,
            elsewhere,
            "GET",
            &format!("/datastore/{datastore_id}"),
            None,
        )
        .await;
        assert_eq!(
            away.status, 404,
            "case {case:?}: expected HTTP 404 from {elsewhere:?}, got {} with {:?}",
            away.status,
            away.text()
        );
    }
}

// ---------------------------------------------------------------------------
// What the service ADDRESSES: the import's object store
// ---------------------------------------------------------------------------

/// The prefix under [`BUCKET`] each row's job writes its results to.
const OUTPUT_ROOT: &str = "job-output";

/// The one object each row stages under its input prefix: not a DICOM Part 10 file, so it
/// describes no image set — what is being pinned here is that the import READ the prefix
/// from the emulator's own Amazon S3, which it reports in its FAILURE manifest.
const STAGED_KEY: &str = "notes.txt";

/// Poll `GetDICOMImportJob` until the job reports a terminal `jobStatus`, and answer that
/// read. The deadline bounds a hung emulator rather than the span being waited for.
async fn settled_job(
    router: &MultiServiceRouter,
    case: &str,
    scope_region: &str,
    datastore_id: &str,
    job_id: &str,
) -> Answer {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let read = imaging_ok(
            router,
            case,
            scope_region,
            "GET",
            &format!("/getDICOMImportJob/datastore/{datastore_id}/job/{job_id}"),
            None,
        )
        .await;
        let status = read.string(&["jobProperties", "jobStatus"]);
        if status == "COMPLETED" || status == "FAILED" {
            return read;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the import job never reached a terminal status, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every object key Amazon S3 holds under `prefix`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let listed = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListObjectsV2({BUCKET}/{prefix}) must succeed, got {} with {:?}",
        listed.status,
        listed.text()
    );
    elements(&listed.text(), "Key").into_iter().collect()
}

/// One object's body.
async fn object_body(router: &MultiServiceRouter, case: &str, key: &str) -> String {
    let read = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[], Bytes::new()).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetObject({BUCKET}/{key}) must succeed, got {} with {:?}",
        read.status,
        read.text()
    );
    read.text()
}

/// RULE (above): the in-process clients a request's work builds are addressed to the Region
/// the request is served in, which is a Region identifier whatever the credential scope
/// carried — so a DICOM import submitted under any credential scope still lists and reads
/// the input prefix out of the emulator's own Amazon S3, and still writes its result
/// manifests back to the output prefix it was given.
#[tokio::test]
async fn a_dicom_import_reads_the_emulators_object_store_whatever_the_scope_carries() {
    let router = emulator();
    let created = s3(&router, "PUT", &format!("/{BUCKET}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let role_arn = role(&router, "healthimaging-scope-import", HEALTHIMAGING_TRUST).await;

    // One job per row that drives this half, all submitted before any is polled: each job's
    // completion is a span the clock owes, and they run concurrently.
    let rows = scope_cases();
    let mut submitted: Vec<(usize, String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if !row.imports {
            continue;
        }
        let case = row.case;
        let input_prefix = format!("input-{index}/");
        let written = s3(
            &router,
            "PUT",
            &format!("/{BUCKET}/{input_prefix}{STAGED_KEY}"),
            &[("content-type", "application/octet-stream")],
            Bytes::from_static(b"these bytes are not a DICOM file"),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: PutObject({input_prefix}{STAGED_KEY}) must succeed, got {} with {:?}",
            written.status,
            written.text()
        );

        let datastore_id = active_datastore(
            &router,
            case,
            row.scope_region,
            &format!("import.store-{index}"),
            &format!("import-store-token-{index}"),
        )
        .await;
        let job = imaging_ok(
            &router,
            case,
            row.scope_region,
            "POST",
            &format!("/startDICOMImportJob/datastore/{datastore_id}"),
            Some(json!({
                "clientToken": format!("import-job-token-{index}"),
                "dataAccessRoleArn": role_arn,
                "inputS3Uri": format!("s3://{BUCKET}/{input_prefix}"),
                "outputS3Uri": format!("s3://{BUCKET}/{OUTPUT_ROOT}-{index}/"),
            })),
        )
        .await;
        assert_eq!(
            job.string(&["jobStatus"]),
            "SUBMITTED",
            "case {case:?}: expected SUBMITTED from the submit, got {:?}",
            job.text()
        );
        submitted.push((index, datastore_id, job.string(&["jobId"])));
    }

    for (index, datastore_id, job_id) in submitted {
        let row = &rows[index];
        let case = row.case;
        let input_prefix = format!("input-{index}/");

        // The input prefix was ANSWERED: it holds an object, so the import had something to
        // scan and the job completed. A job whose input no object store answered about is
        // the one thing this cannot be — that job owes no manifests at all.
        let settled = settled_job(&router, case, row.scope_region, &datastore_id, &job_id).await;
        assert_eq!(
            settled.string(&["jobProperties", "jobStatus"]),
            "COMPLETED",
            "case {case:?}: expected the job to complete over the object the prefix holds, got \
             {:?}",
            settled.text()
        );

        // And the results are objects in the emulator's own store, beneath the job's own
        // folder under the output prefix the submit named.
        let folder = format!("{OUTPUT_ROOT}-{index}/{datastore_id}-DicomImport-{job_id}/");
        let written = keys_under(&router, case, &folder).await;
        let expected: BTreeSet<String> = BTreeSet::from([
            format!("{folder}job-output-manifest.json"),
            format!("{folder}SUCCESS/success.ndjson"),
            format!("{folder}FAILURE/failure.ndjson"),
            format!("{folder}WARNING/warning.ndjson"),
        ]);
        assert_eq!(
            written, expected,
            "case {case:?}: expected the finished job's manifests under {folder:?}, got {written:?}"
        );

        // The object the import READ is named in the manifest of what it could not import:
        // the prefix was not merely listed, its one object was fetched and parsed.
        let failures = object_body(&router, case, &format!("{folder}FAILURE/failure.ndjson")).await;
        let named: Vec<String> = failures
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let record: Value = serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("case {case:?}: every line of the failure manifest must be JSON: {error}")
                });
                record["inputFile"].as_str().unwrap_or_default().to_string()
            })
            .collect();
        assert_eq!(
            named,
            vec![format!("{input_prefix}{STAGED_KEY}")],
            "case {case:?}: expected the failure manifest to name the object the import read, got \
             {named:?}"
        );
    }
}
