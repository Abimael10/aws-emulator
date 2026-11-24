// SPDX-License-Identifier: CC0-1.0

//! The checksum an Entity Resolution matching job leaves on the Amazon S3 object it writes.
//!
//! The rule under test: an emulated service that stores an object in the account's object
//! store on its OWN behalf names no checksum algorithm of its own, so the object is stored
//! under the algorithm Amazon S3 adds to a write that named none — a `Checksum*` member of a
//! write's answer "is present if the object was uploaded with the `CRC64NVME` checksum
//! algorithm, or if it was uploaded without a checksum (and Amazon S3 added the default
//! checksum, `CRC64NVME`, to the uploaded object)" (`PutObjectOutput$ChecksumCRC64NVME`) —
//! computed over the whole object, because such a write stores it in one request ("for
//! `PutObject` uploads, the checksum type is always `FULL_OBJECT`",
//! `PutObjectRequest$ChecksumType`). Every read path then reports that one checksum and no
//! other: a listing reports `Object$ChecksumAlgorithm` `CRC64NVME` beside `ChecksumType`
//! `FULL_OBJECT` ("the algorithm that was used to create a checksum of the object"), and
//! `GetObject` and `HeadObject` answer `x-amz-checksum-crc64nvme` with
//! `x-amz-checksum-type: FULL_OBJECT` to a read that enabled checksum retrieval ("to retrieve
//! the checksum, this mode must be enabled", `GetObjectRequest$ChecksumMode`) and no checksum
//! header at all to one that did not. Only the DEFAULT belongs to the store: an object whose
//! writer named an algorithm is stored and reported under that algorithm unchanged.
//!
//! Only a cross-service suite can pin that: the write belongs to Entity Resolution, the input
//! table to the AWS Glue Data Catalog and every reading of the written object to Amazon S3,
//! all three over ONE engine exactly as `core/router/src/lib.rs` mounts them. Nothing here
//! writes into a store to arrange what it asserts — the bucket comes from `CreateBucket`, the
//! catalogued table from `CreateDatabase` and `CreateTable`, the input objects from
//! `PutObject`, the output object from a run of `StartMatchingJob`, and every reading of it is
//! taken through `ListObjectsV2`, `GetObject` and `HeadObject`.
//!
//! The rows differ along what the rule generalises over — the bytes the written object ends up
//! holding: how many records the run matched and wrote, how wide each written row is (the
//! workflow's output attributes), whether a written value has to be quoted, and whether the
//! run matched anything at all. No row's digest is a constant: each is checked against what
//! Amazon S3 itself computes under `CRC64NVME` for exactly the bytes the object holds, and
//! that oracle is itself checked once against the PUBLISHED check value of the algorithm.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, element, opt_element};

/// The Region every call below is signed for, and the one every ARN below is scoped to.
const REGION: &str = "us-east-1";

/// The bucket every row's input table lives in and every run writes its output under.
const BUCKET: &str = "entityresolution-output-checksum";

/// The key prefix `OutputSource$outputS3Path` names, under which a run writes its own job's
/// output object.
const OUTPUT_PREFIX: &str = "output";

/// The AWS Glue database every input table is catalogued in.
const DATABASE: &str = "crm";

/// The `ChecksumAlgorithm` Amazon S3 adds to a write that named none, and the response header
/// its value is bound to (`smithy_query(query="operation",operation="GetObject")` reports
/// `ChecksumCRC64NVME -> x-amz-checksum-crc64nvme`). It is one of the values the enum
/// publishes (`smithy_query(query="shape",shape="ChecksumAlgorithm")`).
const DEFAULT_ALGORITHM: &str = "CRC64NVME";
const DEFAULT_ALGORITHM_HEADER: &str = "x-amz-checksum-crc64nvme";

/// `ChecksumType` -> `x-amz-checksum-type`, and the one of its two values
/// (`smithy_query(query="shape",shape="ChecksumType")`: `COMPOSITE | FULL_OBJECT`) a
/// single-request upload carries.
const TYPE_HEADER: &str = "x-amz-checksum-type";
const FULL_OBJECT: &str = "FULL_OBJECT";

/// `ChecksumMode` -> `x-amz-checksum-mode`, and its only value
/// (`smithy_query(query="shape",shape="ChecksumMode")`: `ENABLED`).
const MODE_HEADER: &str = "x-amz-checksum-mode";
const ENABLED: &str = "ENABLED";

/// `PutObject`'s own `ChecksumAlgorithm` header
/// (`smithy_query(query="operation",operation="PutObject")` reports
/// `ChecksumAlgorithm -> x-amz-sdk-checksum-algorithm`), which is how a write below NAMES an
/// algorithm — and how the oracle asks Amazon S3 for the `CRC64NVME` of given bytes.
const ALGORITHM_HEADER: &str = "x-amz-sdk-checksum-algorithm";

/// Every `x-amz-checksum-*` header `GetObjectOutput` binds a `Checksum*` member to
/// (`smithy_query(query="operation",operation="GetObject")` reports `ChecksumCRC32 ->
/// x-amz-checksum-crc32`, `ChecksumCRC32C`, `ChecksumCRC64NVME`, `ChecksumSHA1`,
/// `ChecksumSHA256`, `ChecksumSHA512`, `ChecksumMD5`, `ChecksumXXHASH64`, `ChecksumXXHASH3`,
/// `ChecksumXXHASH128`) — the ten a read of one object must report exactly one of.
const CHECKSUM_HEADERS: [&str; 10] = [
    "x-amz-checksum-crc32",
    "x-amz-checksum-crc32c",
    "x-amz-checksum-crc64nvme",
    "x-amz-checksum-sha1",
    "x-amz-checksum-sha256",
    "x-amz-checksum-sha512",
    "x-amz-checksum-md5",
    "x-amz-checksum-xxhash64",
    "x-amz-checksum-xxhash3",
    "x-amz-checksum-xxhash128",
];

/// Every value the `ChecksumAlgorithm` enum publishes
/// (`smithy_query(query="shape",shape="ChecksumAlgorithm")` reports exactly these ten), each
/// of which a write may NAME — so the round-trip below is asserted over the whole published
/// set rather than over one algorithm.
const CHECKSUM_ALGORITHMS: [&str; 10] = [
    "CRC32",
    "CRC32C",
    "SHA1",
    "SHA256",
    "CRC64NVME",
    "SHA512",
    "MD5",
    "XXHASH64",
    "XXHASH3",
    "XXHASH128",
];

/// The CRC-64/NVME of the nine ASCII digits `123456789`, Base64 encoded as a checksum value
/// is.
///
/// A PUBLISHED constant, not a value read out of this emulator: `0xAE8B14860A799888` is the
/// check value of the CRC-64/NVME algorithm over that string, and `rosUhgp5mIg=` is the Base64
/// of its eight bytes most-significant first. It is what pins the oracle below to the
/// algorithm rather than to whatever this emulator computes.
const CHECK_VECTOR_RECORD: &[u8] = b"123456789";
const CHECK_VECTOR_CRC64NVME: &str = "rosUhgp5mIg=";

/// How long a phase waits for the condition it is about — a job reporting a terminal status.
///
/// Generous on purpose: nothing here asserts how long anything took, so this is only the point
/// at which waiting is called a failure.
const PHASE_DEADLINE: Duration = Duration::from_secs(120);

/// How long each look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    /// The value of the response header `name`, or `None` when the answer carries none.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The answer's body as text, for a message naming what came back.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The answer's body read as JSON, or `Value::Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the routing key
/// the router claims a request by. Nothing here is a credential: this router runs with request
/// authentication off, so no signature is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let (parts, body) = router.dispatch(request).await.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: body.to_vec(),
    }
}

/// One path-style Amazon S3 request.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    send(router, "s3", method, path, headers, body).await
}

/// One AWS Glue operation over its `awsJson1_1` wire.
async fn glue(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    send(
        router,
        "glue",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AWSGlue.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await
}

/// One Entity Resolution operation over its `restJson1` wire.
async fn entityresolution(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map_or_else(Vec::new, |value| value.to_string().into_bytes());
    send(
        router,
        "entityresolution",
        method,
        path,
        &[("content-type", "application/json")],
        payload,
    )
    .await
}

/// The emulator the suite drives: the three services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_entityresolution_awsveniceservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// The AWS Glue table ARN an `InputSource$inputSourceARN` carries for `table`
/// (`smithy_query(query="shape",shape="InputSource")` fixes
/// `^arn:…:glue:<region>:<account>:table/<database>/<table>$`).
fn glue_table_arn(table: &str) -> String {
    format!(
        "arn:aws:glue:{REGION}:{}:table/{DATABASE}/{table}",
        engine::emulator_account_id()
    )
}

/// The role every workflow below is created with.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/entityresolution-output-checksum",
        engine::emulator_account_id()
    )
}

/// One row: the input table a run reads and the shape of the output table it writes, which
/// together decide the BYTES of the object whose checksum is asserted.
struct Row {
    case: &'static str,
    /// Names this row's table, schema mapping, workflow and key prefixes, so rows cannot
    /// collide.
    name: &'static str,
    /// The object staged at the table's location, as the text it holds.
    table_csv: String,
    /// `OutputSource$output`: the attributes each written row carries beside the match
    /// columns.
    output_attributes: Vec<&'static str>,
    /// `ruleBasedProperties.rules[0].matchingKeys`.
    matching_keys: Vec<&'static str>,
}

/// A long input table, so one row's written object is far larger than the others'.
fn long_table() -> String {
    let mut table = String::from("id,email,phone\n");
    for record in 0..60 {
        table.push_str(&format!(
            "r{record},person{}@example.com,555-{record:04}\n",
            record / 2
        ));
    }
    table
}

/// The rows: one per form the written object's bytes take.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a matched pair and a record standing alone",
            name: "pair",
            table_csv: "id,email,phone\n\
                        r1,ada@example.com,555-0100\n\
                        r2,ada@example.com,555-0101\n\
                        r3,bob@example.com,555-0102\n"
                .to_string(),
            output_attributes: vec!["id", "email"],
            matching_keys: vec!["email"],
        },
        Row {
            case: "sixty records, so the written object is long",
            name: "long",
            table_csv: long_table(),
            output_attributes: vec!["id", "email"],
            matching_keys: vec!["email"],
        },
        Row {
            case: "a written value carrying the delimiter and a quote, so the bytes are escaped",
            name: "quoted",
            table_csv: "id,email,phone\n\
                        r1,ada@example.com,\"555-0100, ext \"\"7\"\"\"\n\
                        r2,bob@example.com,\"555-0100, ext \"\"7\"\"\"\n"
                .to_string(),
            output_attributes: vec!["id", "phone"],
            matching_keys: vec!["phone"],
        },
        Row {
            case: "a workflow whose output names every attribute, so each written row is wider",
            name: "wide",
            table_csv: "id,email,phone\n\
                        r1,ada@example.com,555-0100\n\
                        r2,zoe@example.com,555-0100\n"
                .to_string(),
            output_attributes: vec!["id", "email", "phone"],
            matching_keys: vec!["phone"],
        },
        Row {
            case: "a run that matched nothing, each record its own group",
            name: "unmatched",
            table_csv: "id,email,phone\n\
                        r1,ada@example.com,555-0100\n\
                        r2,bob@example.com,555-0200\n"
                .to_string(),
            output_attributes: vec!["id", "email"],
            matching_keys: vec!["email"],
        },
    ]
}

/// The key prefix one row's input object is staged under, which is its table's location.
fn input_prefix(row: &Row) -> String {
    format!("in/{}/", row.name)
}

/// The key prefix one row's run writes its output object under.
fn output_prefix(row: &Row) -> String {
    format!("{OUTPUT_PREFIX}/{}/", row.name)
}

/// Create `BUCKET`.
async fn given_bucket(router: &MultiServiceRouter) {
    let answer = s3(router, "PUT", &format!("/{BUCKET}"), &[], Vec::new()).await;
    assert_eq!(
        answer.status,
        200,
        "setup: the bucket must be created, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Create the one AWS Glue database every input table is catalogued in.
async fn given_database(router: &MultiServiceRouter) {
    let answer = glue(
        router,
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: the Glue database must be created, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Stage one row's input object, catalogue its table, map its schema and create its workflow.
async fn given_workflow(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let staged = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{}people.csv", input_prefix(row)),
        &[],
        row.table_csv.clone().into_bytes(),
    )
    .await;
    assert_eq!(
        staged.status,
        200,
        "case {case:?}: the input object must be stored, got {} {}",
        staged.status,
        staged.text()
    );

    let catalogued = glue(
        router,
        "CreateTable",
        json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": row.name,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{}", input_prefix(row)),
                    "Columns": [
                        { "Name": "id", "Type": "string" },
                        { "Name": "email", "Type": "string" },
                        { "Name": "phone", "Type": "string" },
                    ],
                },
            },
        }),
    )
    .await;
    assert_eq!(
        catalogued.status,
        200,
        "case {case:?}: the Glue table must be created, got {} {}",
        catalogued.status,
        catalogued.text()
    );

    let mapped = entityresolution(
        router,
        "POST",
        "/schemas",
        Some(json!({
            "schemaName": row.name,
            "mappedInputFields": [
                { "fieldName": "id", "type": "UNIQUE_ID" },
                { "fieldName": "email", "type": "EMAIL_ADDRESS", "matchKey": "email" },
                { "fieldName": "phone", "type": "PHONE_NUMBER", "matchKey": "phone" },
            ],
        })),
    )
    .await;
    assert_eq!(
        mapped.status,
        200,
        "case {case:?}: the schema mapping must be created, got {} {}",
        mapped.status,
        mapped.text()
    );

    let created = entityresolution(
        router,
        "POST",
        "/matchingworkflows",
        Some(json!({
            "workflowName": row.name,
            "inputSourceConfig": [{
                "inputSourceARN": glue_table_arn(row.name),
                "schemaName": row.name,
            }],
            "outputSourceConfig": [{
                "outputS3Path": format!("s3://{BUCKET}/{OUTPUT_PREFIX}"),
                "output": row
                    .output_attributes
                    .iter()
                    .map(|name| json!({ "name": name }))
                    .collect::<Vec<Value>>(),
            }],
            "resolutionTechniques": {
                "resolutionType": "RULE_MATCHING",
                "ruleBasedProperties": {
                    "rules": [{ "ruleName": "theRule", "matchingKeys": row.matching_keys }],
                    "attributeMatchingModel": "ONE_TO_ONE",
                },
            },
            "roleArn": role_arn(),
        })),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: the workflow must be created, got {} {}",
        created.status,
        created.text()
    );
}

/// Start one row's job, and answer the `jobId` it was admitted under.
async fn start_job(router: &MultiServiceRouter, row: &Row) -> String {
    let started = entityresolution(
        router,
        "POST",
        &format!("/matchingworkflows/{}/jobs", row.name),
        None,
    )
    .await;
    assert_eq!(
        started.status,
        200,
        "case {:?}: StartMatchingJob must answer 200, got {} {}",
        row.case,
        started.status,
        started.text()
    );
    started.json()["jobId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look at one row's job until it reports a terminal status, and assert it SUCCEEDED.
async fn settled(router: &MultiServiceRouter, row: &Row, job_id: &str) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let read = entityresolution(
            router,
            "GET",
            &format!("/matchingworkflows/{}/jobs/{job_id}", row.name),
            None,
        )
        .await;
        let body = read.json();
        let status = body["status"].as_str().unwrap_or_default().to_string();
        if status == "SUCCEEDED" {
            return;
        }
        assert_ne!(
            status,
            "FAILED",
            "case {:?}: the run must succeed, got {}",
            row.case,
            read.text()
        );
        assert!(
            Instant::now() < deadline,
            "case {:?}: the job must reach a terminal status, last read {}",
            row.case,
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `ListObjectsV2` answer for `prefix`, as its XML text.
async fn listing(router: &MultiServiceRouter, case: &str, prefix: &str) -> String {
    let answer = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: listing {prefix:?} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// What Amazon S3 itself computes for `body` under `algorithm`: the oracle a row's digest is
/// compared against, taken by uploading those same bytes through a `PutObject` that NAMES the
/// algorithm and reading the checksum the write answers with.
async fn s3_checksum_of(
    router: &MultiServiceRouter,
    case: &str,
    algorithm: &str,
    name: &str,
    body: &[u8],
) -> String {
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/oracle/{name}"),
        &[(ALGORITHM_HEADER, algorithm)],
        body.to_vec(),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: the oracle upload must succeed, got {} {}",
        written.status,
        written.text()
    );
    let reported_type = written.header(TYPE_HEADER);
    assert_eq!(
        reported_type.as_deref(),
        Some(FULL_OBJECT),
        "case {case:?}: a single-request upload is stored {FULL_OBJECT}, got {reported_type:?}"
    );
    let header = format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase());
    written.header(&header).unwrap_or_else(|| {
        panic!("case {case:?}: the oracle upload must report {header}, got none")
    })
}

#[tokio::test]
async fn a_matching_jobs_output_object_carries_the_stores_default_full_object_checksum() {
    let router = emulator();
    given_bucket(&router).await;
    given_database(&router).await;

    // The oracle answers for the algorithm, not for this emulator: over the published check
    // vector it must answer the published digest.
    let published = s3_checksum_of(
        &router,
        "the published CRC-64/NVME check value",
        DEFAULT_ALGORITHM,
        "check-vector",
        CHECK_VECTOR_RECORD,
    )
    .await;
    assert_eq!(
        published, CHECK_VECTOR_CRC64NVME,
        "the {DEFAULT_ALGORITHM} of the check vector is the published value, got {published:?}"
    );

    // Every workflow is created and started before any job is awaited, so the suite spends ONE
    // clock-owed QUEUED -> RUNNING -> terminal span rather than one per row.
    let rows = rows();
    let mut jobs: Vec<String> = Vec::new();
    for row in &rows {
        given_workflow(&router, row).await;
        jobs.push(start_job(&router, row).await);
    }

    // Every row's digest, so one written object's checksum cannot be another's.
    let mut digests: BTreeSet<String> = BTreeSet::new();

    for (row, job_id) in rows.iter().zip(&jobs) {
        let case = row.case;
        settled(&router, row, job_id).await;

        let prefix = output_prefix(row);
        let found = listing(&router, case, &prefix).await;
        let entries = blocks_of(&found, "Contents");
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: the run must write exactly one object under {prefix:?}, got {found}"
        );
        let entry = entries[0];
        let key = element(entry, "Key");
        assert!(
            key.starts_with(&prefix),
            "case {case:?}: the written object must live under {prefix:?}, got {key:?}"
        );

        // The listing reports the algorithm the store applied to a write that named none, and
        // the type a whole-object write carries.
        let listed_algorithm = opt_element(entry, "ChecksumAlgorithm");
        assert_eq!(
            listed_algorithm.as_deref(),
            Some(DEFAULT_ALGORITHM),
            "case {case:?}: the listing must report ChecksumAlgorithm {DEFAULT_ALGORITHM}, got \
             {listed_algorithm:?}"
        );
        let listed_type = opt_element(entry, "ChecksumType");
        assert_eq!(
            listed_type.as_deref(),
            Some(FULL_OBJECT),
            "case {case:?}: the listing must report ChecksumType {FULL_OBJECT}, got \
             {listed_type:?}"
        );

        // A read that did NOT enable checksum retrieval is told nothing about the checksum.
        let plain = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new()).await;
        assert_eq!(
            plain.status,
            200,
            "case {case:?}: GET {key} must succeed, got {} {}",
            plain.status,
            plain.text()
        );
        for header in CHECKSUM_HEADERS {
            let reported = plain.header(header);
            assert!(
                reported.is_none(),
                "case {case:?}: a read that did not enable the checksum mode reports no \
                 {header}, got {reported:?}"
            );
        }
        let reported_type = plain.header(TYPE_HEADER);
        assert!(
            reported_type.is_none(),
            "case {case:?}: a read that did not enable the checksum mode reports no \
             {TYPE_HEADER}, got {reported_type:?}"
        );
        let stored = plain.body.clone();
        assert!(
            !stored.is_empty(),
            "case {case:?}: the written object must hold the output table, got no bytes"
        );

        // What Amazon S3 computes for exactly those stored bytes under the default algorithm.
        let oracle = s3_checksum_of(&router, case, DEFAULT_ALGORITHM, row.name, &stored).await;

        // Both reads that report an object's metadata report that one checksum, as a
        // full-object one, and report no other algorithm's.
        for method in ["GET", "HEAD"] {
            let answer = s3(
                &router,
                method,
                &format!("/{BUCKET}/{key}"),
                &[(MODE_HEADER, ENABLED)],
                Vec::new(),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: {method} {key} must succeed, got {} {}",
                answer.status,
                answer.text()
            );
            let digest = answer.header(DEFAULT_ALGORITHM_HEADER);
            assert_eq!(
                digest.as_deref(),
                Some(oracle.as_str()),
                "case {case:?}: {method} must report the stored object's {DEFAULT_ALGORITHM}, \
                 got {digest:?}"
            );
            let answered_type = answer.header(TYPE_HEADER);
            assert_eq!(
                answered_type.as_deref(),
                Some(FULL_OBJECT),
                "case {case:?}: {method} must report {TYPE_HEADER} {FULL_OBJECT}, got \
                 {answered_type:?}"
            );
            let others: Vec<&str> = CHECKSUM_HEADERS
                .into_iter()
                .filter(|header| *header != DEFAULT_ALGORITHM_HEADER)
                .filter(|header| answer.header(header).is_some())
                .collect();
            assert!(
                others.is_empty(),
                "case {case:?}: {method} must report only {DEFAULT_ALGORITHM_HEADER}, also got \
                 {others:?}"
            );
        }
        digests.insert(oracle);
    }

    // The rows' objects hold different bytes, so each reports its own digest: a value copied
    // from a constant, or from another object, would collapse them.
    assert_eq!(
        digests.len(),
        rows.len(),
        "the rows' written objects hold different bytes, so each reports its own digest, got \
         {digests:?}"
    );
}

#[tokio::test]
async fn a_write_that_names_an_algorithm_keeps_it_and_only_an_unnamed_one_takes_the_default() {
    let router = emulator();
    given_bucket(&router).await;

    // One row per value the enum publishes, plus the write that names none.
    let mut cases: Vec<(String, Option<&str>, &str)> = CHECKSUM_ALGORITHMS
        .iter()
        .map(|algorithm| {
            (
                format!("a write naming {algorithm}"),
                Some(*algorithm),
                *algorithm,
            )
        })
        .collect();
    cases.push((
        "a write naming no algorithm at all".to_string(),
        None,
        DEFAULT_ALGORITHM,
    ));

    for (case, named, expected) in cases {
        let key = format!("named/{}", named.unwrap_or("none").to_ascii_lowercase());
        let headers: Vec<(&str, &str)> = named
            .map(|algorithm| vec![(ALGORITHM_HEADER, algorithm)])
            .unwrap_or_default();
        let body = format!("the object of {case}\n").into_bytes();
        let written = s3(&router, "PUT", &format!("/{BUCKET}/{key}"), &headers, body).await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the write must succeed, got {} {}",
            written.status,
            written.text()
        );

        let found = listing(&router, &case, &key).await;
        let entry = blocks_of(&found, "Contents")
            .into_iter()
            .find(|entry| opt_element(entry, "Key").as_deref() == Some(key.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: the listing must hold {key:?}, got {found}"));
        let listed_algorithm = opt_element(entry, "ChecksumAlgorithm");
        assert_eq!(
            listed_algorithm.as_deref(),
            Some(expected),
            "case {case:?}: the listing must report ChecksumAlgorithm {expected}, got \
             {listed_algorithm:?}"
        );
        let listed_type = opt_element(entry, "ChecksumType");
        assert_eq!(
            listed_type.as_deref(),
            Some(FULL_OBJECT),
            "case {case:?}: the listing must report ChecksumType {FULL_OBJECT}, got \
             {listed_type:?}"
        );
    }
}
