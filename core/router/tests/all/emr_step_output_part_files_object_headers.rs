// SPDX-License-Identifier: CC0-1.0

//! The media type and the full-object checksum the OUTPUT PART FILES of an Amazon EMR step are
//! read back under.
//!
//! THE RULE: an object a service's own write path materializes in Amazon S3 without declaring a
//! `Content-Type` and without naming a checksum algorithm is stored the way S3 stores any such
//! upload — under S3's own default media type for an object with no stated type
//! (`binary/octet-stream`, the `Content-Type` the Amazon S3 User Guide gives an object uploaded
//! without one, never the generic filler a transport puts on an un-annotated payload), and with
//! the checksum S3 adds to an upload that named none: "if it was uploaded without a checksum …
//! Amazon S3 added the default checksum, `CRC64NVME`, to the uploaded object"
//! (`PutObjectOutput$ChecksumCRC64NVME`), of type `FULL_OBJECT` ("for `PutObject` uploads, the
//! checksum type is always `FULL_OBJECT`", `PutObjectRequest$ChecksumType`) computed over the
//! bytes stored. Every later read reports both — `GetObject` binds `ContentType ->
//! Content-Type` and `ChecksumCRC64NVME -> x-amz-checksum-crc64nvme`, and `HeadObject` the same
//! — with the checksum headers carried only for a read that asks for them ("to retrieve the
//! checksum, this mode must be enabled", `GetObjectRequest$ChecksumMode`).
//!
//! The rows span what the rule generalises over for the write path a job's output really takes:
//! every object Spark's own text writer leaves in the output directory a step named — the part
//! files, which differ in their BYTES and their SIZE, and the EMPTY success marker that writer
//! closes a directory with, a zero-byte object whose checksum is the algorithm's digest of no
//! bytes rather than no checksum at all. Which keys those are is DISCOVERED through
//! `ListObjectsV2` rather than spelled out here, so the rows follow the writer's own partition
//! boundaries instead of pinning them; what the suite computes for itself is the dataset the
//! part files must hold between them.
//!
//! Every expectation is anchored outside the step's own write: the checksum an object of that
//! directory reports must equal the one Amazon S3 records for a CLIENT's `PutObject` of the same
//! bytes that names neither a media type nor an algorithm, and that control chain is in turn
//! anchored to the algorithm by the CRC-64/NVME check value its catalogue publishes.
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine, the application is uploaded through
//! S3's own `PutObject` and every object asserted is read back through S3's own `ListObjectsV2`,
//! `GetObject` and `HeadObject` — so what is asserted is the record a real write of the store
//! left.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region both services are addressed in: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket the application and every object of this suite lives in.
const BUCKET: &str = "emr-step-output-part-files";

/// S3's own default media type for an object stored with no `Content-Type` of its own.
const DEFAULT_MEDIA_TYPE: &str = "binary/octet-stream";

/// The generic payload media type a transport fills in for an un-annotated binary payload,
/// which is never what a read of an object stored without a declared type reports.
const GENERIC_MEDIA_TYPE: &str = "application/octet-stream";

/// The header the default checksum's value travels on (`ChecksumCRC64NVME ->
/// x-amz-checksum-crc64nvme`).
const DEFAULT_CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";

/// The checksum type a single whole-object write records: "for `PutObject` uploads, the checksum
/// type is always `FULL_OBJECT`" (`PutObjectRequest$ChecksumType`).
const FULL_OBJECT_CHECKSUM_TYPE: &str = "FULL_OBJECT";

/// The input the CRC-64/NVME algorithm publishes a check value for, in the CRC catalogue's own
/// form: the nine ASCII digits.
const CHECK_INPUT: &[u8] = b"123456789";

/// That published check value: the CRC-64/NVME digest of [`CHECK_INPUT`] is `0xAE8B14860A799888`
/// (the CRC catalogue's `check` for `CRC-64/NVME`), which anchors the controls below to the
/// algorithm itself rather than to this emulator's own computation.
const CHECK_VALUE: u64 = 0xAE8B_1486_0A79_9888;

/// The roles the launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The key the submitted application is stored under, the way `spark-submit` finds one.
const APPLICATION_KEY: &str = "apps/save-text.py";

/// The directory the application writes its output into.
const OUTPUT_PREFIX: &str = "out/squares";

/// The name of the empty marker Spark's text writer closes an output directory with.
const SUCCESS_MARKER: &str = "_SUCCESS";

/// The name every part file of a written directory begins with.
const PART_FILE_PREFIX: &str = "part-";

/// How long a poll waits for the step to settle: both provisioning phases, the step's own span
/// and one application run, with room to spare. The wait is on the CONDITION — it looks again
/// until the step has settled — so a loaded host delays this suite instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The application the step submits: it writes its dataset through Spark's own text writer,
/// which is how a job's output part files are written — declaring no media type and naming no
/// checksum algorithm. The writer leaves one part file per partition plus the empty success
/// marker that closes the directory.
const APPLICATION: &str = r#"
import sys

from pyspark import SparkContext

sc = SparkContext.getOrCreate()
(
    sc.parallelize(range(1, 11), 3)
    .filter(lambda value: value % 2 == 0)
    .map(lambda value: value * value)
    .saveAsTextFile(sys.argv[1])
)
sc.stop()
"#;

/// What the part files must hold BETWEEN them, computed here rather than read back from the
/// emulator: the squares of the even values in `1..=10`, which is what the application above
/// asks for. How they are spread over the part files is the writer's own partitioning and no
/// part of this rule.
fn expected_values() -> BTreeSet<u32> {
    (1..=10u32)
        .filter(|value| value % 2 == 0)
        .map(|value| value * value)
        .collect()
}

/// Amazon EMR and Amazon S3 on one engine, the shape the router mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One `awsJson1_1` Amazon EMR call that must be served.
async fn emr_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let host = format!("elasticmapreduce.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261116/{REGION}/\
                 elasticmapreduce/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// What one Amazon S3 request answered.
struct Answered {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Answered {
    /// The value of one response header, as the answer carried it.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// One path-style Amazon S3 request, carrying `extra` headers on top of the addressing ones.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Answered {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261116/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        );
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let request = request
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: answered.to_vec(),
    }
}

/// `PutObject` of `body` at `key`, declaring neither a media type nor a checksum algorithm —
/// the client-side write the controls of this suite are made with.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &[u8]) {
    let written = s3(router, "PUT", &format!("/{BUCKET}/{key}"), &[], body).await;
    assert!(
        (200..300).contains(&written.status),
        "case {case:?}: PutObject {key} must succeed, got {} {:?}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
}

/// Read `key` back, asking for the checksum the write stored — "to retrieve the checksum, this
/// mode must be enabled" (`GetObjectRequest$ChecksumMode`).
async fn read_back(router: &MultiServiceRouter, method: &str, key: &str) -> Answered {
    s3(
        router,
        method,
        &format!("/{BUCKET}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        &[],
    )
    .await
}

/// Every key the store holds under `prefix`, as `ListObjectsV2` reports them.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let listed = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        &[],
    )
    .await;
    let text = String::from_utf8_lossy(&listed.body).into_owned();
    assert!(
        (200..300).contains(&listed.status),
        "case {case:?}: ListObjectsV2 under {prefix} must succeed, got {} {text}",
        listed.status
    );
    text.split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// A cluster launched with one `command-runner.jar` step submitting the application, kept alive
/// so the step's own end is what is read rather than the cluster's shutdown.
async fn launch(router: &MultiServiceRouter) -> String {
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": "step-output-part-files",
            "ReleaseLabel": "emr-7.9.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": "Spark" }],
            "Steps": [{
                "Name": "save-as-text-file",
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": {
                    "Jar": "command-runner.jar",
                    "Args": [
                        "spark-submit",
                        "--deploy-mode",
                        "cluster",
                        format!("s3://{BUCKET}/{APPLICATION_KEY}"),
                        format!("s3://{BUCKET}/{OUTPUT_PREFIX}"),
                    ],
                },
            }],
            "Instances": {
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            },
        }),
    )
    .await;
    launched["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering the steps read.
async fn drained(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let listed = emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
        let steps: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
        let states: Vec<String> = steps
            .iter()
            .map(|step| {
                step["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if !states.is_empty() && states.iter().all(|state| TERMINAL.contains(&state.as_str())) {
            return steps;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The values one part file holds, read as the lines of text the writer wrote.
fn values_in(case: &str, bytes: &[u8]) -> Vec<u32> {
    let text = String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|_| panic!("case {case:?}: expected the part file to hold text"));
    text.lines()
        .map(|line| {
            line.trim().parse::<u32>().unwrap_or_else(|error| {
                panic!("case {case:?}: expected a value per line, got {line:?} ({error})")
            })
        })
        .collect()
}

/// Every object the step's output directory holds reports S3's default media type and the
/// default full-object checksum of its own bytes, to both readers — and reports no checksum to
/// a read that does not ask for one.
#[tokio::test]
async fn a_steps_output_part_files_report_s3s_default_media_type_and_checksum() {
    let router = emulator();
    let created = s3(&router, "PUT", &format!("/{BUCKET}"), &[], &[]).await;
    assert!(
        (200..300).contains(&created.status),
        "CreateBucket must succeed, got {} {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    put_object(
        &router,
        "the application",
        APPLICATION_KEY,
        APPLICATION.as_bytes(),
    )
    .await;

    // The control chain is only worth comparing against if S3's default checksum really is the
    // published CRC-64/NVME, so one client write anchors it to that algorithm's check value.
    put_object(
        &router,
        "the published check input",
        "control/check",
        CHECK_INPUT,
    )
    .await;
    let anchored = read_back(&router, "GET", "control/check").await;
    let published = ::aws_smithy_types::base64::encode(CHECK_VALUE.to_be_bytes());
    assert_eq!(
        anchored.header(DEFAULT_CHECKSUM_HEADER),
        Some(published.as_str()),
        "case {:?}: expected the published CRC-64/NVME check value {published:?}, got {:?}",
        "the published check input, stored by a client",
        anchored.header(DEFAULT_CHECKSUM_HEADER)
    );

    let cluster_id = launch(&router).await;
    let steps = drained("the submitted application", &router, &cluster_id).await;
    let step = steps
        .first()
        .unwrap_or_else(|| panic!("expected the submitted step back, got {steps:?}"));
    assert_eq!(
        step["Status"]["State"].as_str(),
        Some("COMPLETED"),
        "the step whose application writes the output must complete, got {step}"
    );

    // WHICH objects the writer left is the store's answer, not this suite's: the directory is
    // listed, and every key it reports is a row.
    let directory = format!("{OUTPUT_PREFIX}/");
    let keys = keys_under(&router, "the written output directory", &directory).await;
    let part_files: Vec<&String> = keys
        .iter()
        .filter(|key| {
            key.strip_prefix(&directory)
                .is_some_and(|name| name.starts_with(PART_FILE_PREFIX))
        })
        .collect();
    assert!(
        !part_files.is_empty(),
        "expected the writer to leave the dataset in part files, got {keys:?}"
    );
    let marker = format!("{directory}{SUCCESS_MARKER}");
    assert!(
        keys.contains(&marker),
        "expected the writer to close the directory with {marker}, got {keys:?}"
    );

    let mut checksums: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    let mut held_values: BTreeSet<u32> = BTreeSet::new();
    for (ordinal, key) in keys.iter().enumerate() {
        let case = key.as_str();
        let read = read_back(&router, "GET", key).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the object the step wrote to be served, got {} {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let bytes = read.body.clone();

        // The CONTROL for exactly these bytes: a client's own write naming neither a media type
        // nor an algorithm, which is what S3's own defaults apply to.
        let control_key = format!("control/{ordinal}");
        put_object(&router, case, &control_key, &bytes).await;
        let control = read_back(&router, "GET", &control_key).await;
        let expected_checksum = control.header(DEFAULT_CHECKSUM_HEADER).unwrap_or_default();
        assert!(
            !expected_checksum.is_empty(),
            "case {case:?}: the control object must carry S3's default checksum, got {:?}",
            control.headers
        );
        assert_eq!(
            control.header("content-type"),
            Some(DEFAULT_MEDIA_TYPE),
            "case {case:?}: the control write names no media type, so it must be stored under \
             S3's default, got {:?}",
            control.header("content-type")
        );

        for (method, answer) in [
            ("GetObject", read),
            ("HeadObject", read_back(&router, "HEAD", key).await),
        ] {
            assert_eq!(
                answer.status, 200,
                "case {case:?}/{method}: expected the object to be served, got {}",
                answer.status
            );
            assert_eq!(
                answer.header("content-type"),
                Some(DEFAULT_MEDIA_TYPE),
                "case {case:?}/{method}: an object written with no media type reports S3's own \
                 default, never the generic {GENERIC_MEDIA_TYPE:?}, got {:?}",
                answer.header("content-type")
            );
            assert_eq!(
                answer.header("x-amz-checksum-type"),
                Some(FULL_OBJECT_CHECKSUM_TYPE),
                "case {case:?}/{method}: expected a {FULL_OBJECT_CHECKSUM_TYPE} checksum, got \
                 {:?}",
                answer.header("x-amz-checksum-type")
            );
            assert_eq!(
                answer.header(DEFAULT_CHECKSUM_HEADER),
                Some(expected_checksum),
                "case {case:?}/{method}: expected the checksum S3 records for these bytes \
                 ({expected_checksum:?}), got {:?}",
                answer.header(DEFAULT_CHECKSUM_HEADER)
            );
        }

        // A read that does not ask for the checksum carries none, and still reports the media
        // type the object was stored under.
        let unasked = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[], &[]).await;
        assert_eq!(
            unasked.header(DEFAULT_CHECKSUM_HEADER),
            None,
            "case {case:?}: a read that does not enable the checksum mode carries no checksum, \
             got {:?}",
            unasked.header(DEFAULT_CHECKSUM_HEADER)
        );
        assert_eq!(
            unasked.header("content-type"),
            Some(DEFAULT_MEDIA_TYPE),
            "case {case:?}: the media type is reported whatever the checksum mode, got {:?}",
            unasked.header("content-type")
        );

        if *key == marker {
            assert!(
                bytes.is_empty(),
                "case {case:?}: the success marker is an empty object, got {:?}",
                String::from_utf8_lossy(&bytes)
            );
        } else {
            held_values.extend(values_in(case, &bytes));
        }
        checksums.insert(bytes, expected_checksum.to_string());
    }

    // The part files hold the dataset the application computed, between them — so the objects
    // these headers describe are the job's real output rather than any object at that prefix.
    assert_eq!(
        held_values,
        expected_values(),
        "expected the part files to hold the squares of the even values between them, got \
         {held_values:?}"
    );

    // And the checksum describes the BYTES: the objects of the directory hold different byte
    // strings, so no two of them report the same value.
    let distinct: BTreeSet<&String> = checksums.values().collect();
    assert_eq!(
        distinct.len(),
        checksums.len(),
        "expected objects holding different bytes to report different checksums, got \
         {checksums:?}"
    );
}
