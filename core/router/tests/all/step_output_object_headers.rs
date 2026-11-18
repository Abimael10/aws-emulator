// SPDX-License-Identifier: CC0-1.0

//! The media type and the checksum a SERVICE-written object is read back under.
//!
//! THE RULE: Amazon S3 records the media type an upload DECLARED and reports exactly that on
//! every later read of the object; an upload that declares none is recorded under S3's own
//! default media type for an object with no stated type — `binary/octet-stream`, which the
//! Amazon S3 User Guide gives as the `Content-Type` of an object uploaded without one — and
//! that default, not anything the transport filled in, is what the read reports. It holds
//! whoever made the write: an object an emulated service's own write path stored (here the
//! Amazon S3 transfers of an Amazon EMR step's command line) declares only what the SERVICE
//! declared, so one that stated no media type is read back as S3's default, exactly as a
//! client's own `PutObject` carrying no `Content-Type` is. Beside it, "if it was uploaded
//! without a checksum … Amazon S3 added the default checksum, `CRC64NVME`, to the uploaded
//! object" (`PutObjectOutput$ChecksumCRC64NVME`), so such a read also reports a
//! `FULL_OBJECT` CRC64NVME computed over the stored bytes.
//!
//! The rows span what the rule generalises over: WHO wrote the object (a step's upload of a
//! file it wrote, an upload from the step's own standard input, a re-upload after a download,
//! a copy made inside the store, and a client's own `PutObject`), whether the write declared
//! a media type at all, WHICH type it declared (a structured one, and the generic
//! `application/octet-stream` a writer that really means it declares), and the bytes stored
//! (a short line, a two-line file, a structured document, and the input the checksum
//! algorithm publishes a check value for).
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine, every seeded object is written
//! through S3's own `CreateBucket`/`PutObject`, and every object a row asserts is read back
//! through `GetObject` — so what is asserted is the record a real write of the store left.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region both services are addressed in: the one whose `CreateBucket` needs no location
/// constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every object of this suite lives in.
const BUCKET: &str = "emr-step-output-media-types";

/// S3's own default media type for an object stored with no `Content-Type` of its own.
const DEFAULT_MEDIA_TYPE: &str = "binary/octet-stream";

/// The GENERIC payload media type: what the wire protocol fills in for an un-annotated binary
/// payload, and a perfectly ordinary type for a writer to declare on purpose — the two are
/// told apart by whether the write declared it.
const GENERIC_MEDIA_TYPE: &str = "application/octet-stream";

/// A structured media type, declared on a seeded object with its own parameter.
const CSV_MEDIA_TYPE: &str = "text/csv; charset=utf-8";

/// "If it was uploaded without a checksum … Amazon S3 added the default checksum, `CRC64NVME`,
/// to the uploaded object" (`PutObjectOutput$ChecksumCRC64NVME`), and "for `PutObject`
/// uploads, the checksum type is always `FULL_OBJECT`" (`PutObjectRequest$ChecksumType`).
const FULL_OBJECT_CHECKSUM_TYPE: &str = "FULL_OBJECT";

/// The input the CRC-64/NVME algorithm publishes a check value for, in the CRC catalogue's
/// own form: the nine ASCII digits.
const CHECK_INPUT: &[u8] = b"123456789";

/// That published check value: the CRC-64/NVME digest of [`CHECK_INPUT`] is
/// `0xAE8B14860A799888` (the CRC catalogue's `check` for `CRC-64/NVME`), which pins the
/// reported value to the algorithm itself rather than to this emulator's own computation.
const CHECK_VALUE: u64 = 0xAE8B_1486_0A79_9888;

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// How long a poll waits for a step to settle: both provisioning phases, the step's own span
/// and one command's transfers, with room to spare.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

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
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261109/{REGION}/\
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
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261109/{REGION}/s3/\
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
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: answered.to_vec(),
    }
}

/// One seeded object: a client's own `PutObject`, declaring `media_type` where a row needs a
/// declared one and nothing at all where it needs an undeclared write.
struct Seed {
    case: &'static str,
    key: &'static str,
    body: &'static [u8],
    media_type: Option<&'static str>,
}

/// The objects a client writes before any cluster is launched.
fn seeds() -> Vec<Seed> {
    vec![
        Seed {
            case: "a client's write declaring a structured media type",
            key: "in/typed.csv",
            body: b"left,right\n1,2\n",
            media_type: Some(CSV_MEDIA_TYPE),
        },
        Seed {
            case: "a client's write declaring the generic media type on purpose",
            key: "in/generic.bin",
            body: b"\x00\x01\x02 generic bytes\n",
            media_type: Some(GENERIC_MEDIA_TYPE),
        },
        Seed {
            case: "a client's write declaring no media type at all",
            key: "in/untyped.txt",
            body: b"no type was declared for these bytes\n",
            media_type: None,
        },
        Seed {
            case: "a client's write of the published check input, declaring no media type",
            key: "in/check.bin",
            body: CHECK_INPUT,
            media_type: None,
        },
    ]
}

/// One row whose object an Amazon EMR STEP materializes: the command line the step submits,
/// the key it leaves behind, and what a read of that key must report.
struct StepRow {
    label: &'static str,
    /// The step's `Args`, as the command line arrives.
    args: Vec<String>,
    key: String,
    expected_media_type: &'static str,
    expected_body: &'static [u8],
    /// The Amazon EMR application the cluster carries, which is what the step's own work
    /// needs on it.
    cluster_application: &'static str,
    /// The Spark application this row submits, as `(key, source)`, uploaded before the launch
    /// — `None` for a row whose step runs a command line only.
    application: Option<(&'static str, &'static str)>,
}

/// The Spark application one row submits: it writes its argument's path through the Hadoop
/// filesystem, which is where a Spark step's output part files are written — with no media
/// type and no checksum algorithm of its own.
const SPARK_APPLICATION: &str = r#"
import sys

from pyspark.sql import SparkSession

spark = SparkSession.builder.getOrCreate()
path = spark._jvm.org.apache.hadoop.fs.Path(sys.argv[1])
out = path.getFileSystem(spark._jsc.hadoopConfiguration()).create(path, True)
out.write("spark wrote these bytes")
out.close()
spark.stop()
"#;

/// A command line wrapped in a shell program, which is the form the CLI's `--steps` writes.
fn shell(program: &str) -> Vec<String> {
    vec!["bash".to_string(), "-c".to_string(), program.to_string()]
}

/// A command line as the step's own argument list, with no shell at all.
fn argv(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

fn step_rows() -> Vec<StepRow> {
    vec![
        StepRow {
            label: "a file the step wrote, uploaded with no media type of its own",
            args: shell(&format!(
                "echo hello > /tmp/out.txt && aws s3 cp /tmp/out.txt s3://{BUCKET}/out/plain.txt"
            )),
            key: "out/plain.txt".to_string(),
            expected_media_type: DEFAULT_MEDIA_TYPE,
            expected_body: b"hello\n",
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "the published check input, uploaded with no media type of its own",
            args: shell(&format!(
                "echo -n 123456789 > /tmp/check.bin && \
                 aws s3 cp /tmp/check.bin s3://{BUCKET}/out/check.bin"
            )),
            key: "out/check.bin".to_string(),
            expected_media_type: DEFAULT_MEDIA_TYPE,
            expected_body: CHECK_INPUT,
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "a two-line file uploaded from the step's own standard input",
            args: shell(&format!(
                "echo one > /tmp/two.txt && echo two >> /tmp/two.txt && \
                 cat /tmp/two.txt | aws s3 cp - s3://{BUCKET}/out/streamed.txt"
            )),
            key: "out/streamed.txt".to_string(),
            expected_media_type: DEFAULT_MEDIA_TYPE,
            expected_body: b"one\ntwo\n",
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "an object downloaded and uploaded again, which declares no media type",
            args: shell(&format!(
                "aws s3 cp s3://{BUCKET}/in/typed.csv /tmp/again.csv && \
                 aws s3 cp /tmp/again.csv s3://{BUCKET}/out/reuploaded.csv"
            )),
            key: "out/reuploaded.csv".to_string(),
            expected_media_type: DEFAULT_MEDIA_TYPE,
            expected_body: b"left,right\n1,2\n",
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "an object copied in the store, which keeps the type its source declared",
            args: argv(&format!(
                "aws s3 cp s3://{BUCKET}/in/typed.csv s3://{BUCKET}/out/copied.csv"
            )),
            key: "out/copied.csv".to_string(),
            expected_media_type: CSV_MEDIA_TYPE,
            expected_body: b"left,right\n1,2\n",
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "an object whose source declared the generic type, copied in the store",
            args: argv(&format!(
                "aws s3 cp s3://{BUCKET}/in/generic.bin s3://{BUCKET}/out/copied.bin"
            )),
            key: "out/copied.bin".to_string(),
            expected_media_type: GENERIC_MEDIA_TYPE,
            expected_body: b"\x00\x01\x02 generic bytes\n",
            cluster_application: "Hadoop",
            application: None,
        },
        StepRow {
            label: "an output part file a submitted Spark application wrote through Hadoop",
            args: argv(&format!(
                "spark-submit --deploy-mode cluster s3://{BUCKET}/apps/write.py \
                 s3://{BUCKET}/out/part-00000"
            )),
            key: "out/part-00000".to_string(),
            expected_media_type: DEFAULT_MEDIA_TYPE,
            expected_body: b"spark wrote these bytes",
            cluster_application: "Spark",
            application: Some(("apps/write.py", SPARK_APPLICATION)),
        },
    ]
}

/// A cluster launched with one `command-runner.jar` step carrying `args`, kept alive so the
/// step's own end is what is read rather than the cluster's shutdown.
async fn launch(
    router: &MultiServiceRouter,
    name: &str,
    args: &[String],
    application: &str,
) -> String {
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": name,
            "ReleaseLabel": "emr-7.9.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": application }],
            "Steps": [{
                "Name": name,
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
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

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering the states read.
async fn drained(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> Vec<String> {
    let started = Instant::now();
    loop {
        let listed = emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
        let states: Vec<String> = listed["Steps"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|step| {
                step["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if !states.is_empty() && states.iter().all(|state| TERMINAL.contains(&state.as_str())) {
            return states;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read `key` back with the checksum the write stored asked for — "to retrieve the checksum,
/// this mode must be enabled" (`GetObjectRequest$ChecksumMode`).
async fn read_back(router: &MultiServiceRouter, key: &str) -> Answered {
    s3(
        router,
        "GET",
        &format!("/{BUCKET}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        &[],
    )
    .await
}

/// What one read must report: the bytes, the media type, and the default full-object checksum
/// over those bytes.
fn assert_read(case: &str, read: &Answered, expected_media_type: &str, expected_body: &[u8]) {
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected the object to be served, got {} {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        read.body,
        expected_body,
        "case {case:?}: expected the stored bytes {:?}, got {:?}",
        String::from_utf8_lossy(expected_body),
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        read.header("content-type"),
        Some(expected_media_type),
        "case {case:?}: expected the media type {expected_media_type:?}, got {:?}",
        read.header("content-type")
    );
    assert_eq!(
        read.header("x-amz-checksum-type"),
        Some(FULL_OBJECT_CHECKSUM_TYPE),
        "case {case:?}: expected a {FULL_OBJECT_CHECKSUM_TYPE} checksum, got {:?}",
        read.header("x-amz-checksum-type")
    );
    let checksum = read.header("x-amz-checksum-crc64nvme");
    assert!(
        checksum.is_some_and(|value| !value.is_empty()),
        "case {case:?}: expected the stored CRC64NVME of the object, got {checksum:?}"
    );
    if expected_body == CHECK_INPUT {
        let published = ::aws_smithy_types::base64::encode(CHECK_VALUE.to_be_bytes());
        assert_eq!(
            checksum,
            Some(published.as_str()),
            "case {case:?}: expected the published CRC-64/NVME check value {published:?}, got \
             {checksum:?}"
        );
    }
}

/// Every row: the media type a read reports is the one the WRITE declared, or S3's own
/// default where the write declared none — whoever made the write — and the read also reports
/// the default full-object checksum computed over the stored bytes.
#[tokio::test]
async fn a_read_reports_the_media_type_its_write_declared_or_s3s_own_default() {
    let router = emulator();
    let created = s3(&router, "PUT", &format!("/{BUCKET}"), &[], &[]).await;
    assert!(
        (200..300).contains(&created.status),
        "CreateBucket must succeed, got {} {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    // The client's own writes first: they are both the control for the rule and the sources
    // the step rows read from.
    let seeds = seeds();
    for seed in &seeds {
        let extra: Vec<(&str, &str)> = seed
            .media_type
            .map(|media_type| vec![("content-type", media_type)])
            .unwrap_or_default();
        let written = s3(
            &router,
            "PUT",
            &format!("/{BUCKET}/{}", seed.key),
            &extra,
            seed.body,
        )
        .await;
        assert!(
            (200..300).contains(&written.status),
            "case {:?}: PutObject {} must succeed, got {} {:?}",
            seed.case,
            seed.key,
            written.status,
            String::from_utf8_lossy(&written.body)
        );
    }
    for seed in &seeds {
        let read = read_back(&router, seed.key).await;
        assert_read(
            seed.case,
            &read,
            seed.media_type.unwrap_or(DEFAULT_MEDIA_TYPE),
            seed.body,
        );
    }

    // One cluster per step row, all launched before any is polled, so the rows provision
    // alongside each other rather than one after another. A row that submits a Spark
    // application uploads it first, the way `spark-submit` finds one in Amazon S3.
    let rows = step_rows();
    let mut launched: Vec<String> = Vec::new();
    for row in &rows {
        if let Some((key, source)) = row.application {
            let written = s3(
                &router,
                "PUT",
                &format!("/{BUCKET}/{key}"),
                &[],
                source.as_bytes(),
            )
            .await;
            assert!(
                (200..300).contains(&written.status),
                "case {:?}: the application must be stored at {key}, got {} {:?}",
                row.label,
                written.status,
                String::from_utf8_lossy(&written.body)
            );
        }
        launched.push(launch(&router, row.label, &row.args, row.cluster_application).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let states = drained(row.label, &router, cluster_id).await;
        assert_eq!(
            states,
            vec!["COMPLETED".to_string()],
            "case {:?}: the step that writes the object must complete, got {states:?}",
            row.label
        );
        let read = read_back(&router, &row.key).await;
        assert_read(row.label, &read, row.expected_media_type, row.expected_body);
    }

    // And the checksum describes the BYTES rather than the path that wrote them: two objects
    // holding the same bytes report the same value whoever stored them, and two holding
    // different bytes report different ones.
    let csv_by_client = read_back(&router, "in/typed.csv").await;
    let csv_by_step = read_back(&router, "out/reuploaded.csv").await;
    assert_eq!(
        csv_by_step.header("x-amz-checksum-crc64nvme"),
        csv_by_client.header("x-amz-checksum-crc64nvme"),
        "case {:?}: the same bytes carry the same checksum however they were stored, got {:?} \
         beside {:?}",
        "the same bytes written by a step and by a client",
        csv_by_step.header("x-amz-checksum-crc64nvme"),
        csv_by_client.header("x-amz-checksum-crc64nvme")
    );
    let other = read_back(&router, "out/streamed.txt").await;
    assert_ne!(
        other.header("x-amz-checksum-crc64nvme"),
        csv_by_step.header("x-amz-checksum-crc64nvme"),
        "case {:?}: different bytes carry different checksums, got {:?} for both",
        "two step-written objects holding different bytes",
        other.header("x-amz-checksum-crc64nvme")
    );
}
