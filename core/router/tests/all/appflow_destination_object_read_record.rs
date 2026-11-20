// SPDX-License-Identifier: CC0-1.0

//! What a READ of an object ANOTHER service wrote reports, driven over the wire.
//!
//! RULE: a read of a stored object answers from the record the WRITE left behind — the bytes
//! exactly as they were stored (so each transferred record's fields stand in the order the
//! source record presented them), the full-object checksum of those bytes under the
//! algorithm Amazon S3 settles for a write that named none, and the server-side-encryption
//! mode together with the KMS key the object was sealed under — whichever service performed
//! the write and whichever reader asks. A read reports it; it never recomputes it from the
//! request and never changes it.
//!
//! The members those facts travel on are the model's own: `GetObjectOutput$ChecksumCRC64NVME`
//! -> `x-amz-checksum-crc64nvme` and `ChecksumType` -> `x-amz-checksum-type` (`COMPOSITE` or
//! `FULL_OBJECT`), reported only when the request enabled the mode that retrieves them ("to
//! retrieve the checksum, this mode must be enabled", `GetObjectRequest$ChecksumMode`), and
//! `ServerSideEncryption` -> `x-amz-server-side-encryption` beside `SSEKMSKeyId` ->
//! `x-amz-server-side-encryption-aws-kms-key-id`, which carry no such gate.
//!
//! Two services are needed: the object is written by an Amazon AppFlow flow run and served by
//! Amazon S3, so both are mounted here on the ONE engine the router mounts them on and every
//! step goes over the wire — the AppFlow calls as the `restJson1` requests a client sends,
//! the reads as the path-style Amazon S3 requests a client sends.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the source document (an
//! array of records, one record per line, a single record), the ORDER its records carry their
//! fields in (out of alphabetical order, reverse alphabetical, already alphabetical, two
//! records disagreeing), how many records and fields there are, the destination prefix the
//! object lands under, and whether the flow names an encryption key of its own or leaves
//! AppFlow to settle one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one the buckets are created in.
const REGION: &str = "us-east-1";

/// The response header a stored object's default checksum travels on: `x-amz-checksum-`
/// followed by the algorithm in lower case (`ChecksumCRC64NVME` ->
/// `x-amz-checksum-crc64nvme`). `CRC64NVME` is the algorithm Amazon S3 adds to a write that
/// named none — a checksum member "is present if the object was uploaded with the `CRC64NVME`
/// checksum algorithm, or if it was uploaded without a checksum (and Amazon S3 added the
/// default checksum, `CRC64NVME`, to the uploaded object)"
/// (`PutObjectOutput$ChecksumCRC64NVME`).
const CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";

/// The response header `ChecksumType` travels on.
const CHECKSUM_TYPE_HEADER: &str = "x-amz-checksum-type";

/// The request header that asks a read for the checksum at all
/// (`GetObjectRequest$ChecksumMode`), and its one enabling value
/// (`smithy_query(query="shape",shape="com.amazonaws.s3#ChecksumMode")` publishes `ENABLED`).
const CHECKSUM_MODE_HEADER: &str = "x-amz-checksum-mode";
const CHECKSUM_MODE_ENABLED: &str = "ENABLED";

/// A digest over all of the object's own bytes — the type a single whole-object write settles
/// ("for `PutObject` uploads, the checksum type is always `FULL_OBJECT`",
/// `PutObjectRequest$ChecksumType`), and the one `CRC64NVME` always carries ("the CRC64NVME
/// checksum is always a full object checksum", `ChecksumCRC64NVME`).
const FULL_OBJECT: &str = "FULL_OBJECT";

/// The two response headers an object's server-side encryption is reported on.
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KMS_KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The `ServerSideEncryption` value that states a KMS key sealed the object — one of the
/// values that enum publishes.
const SSE_KMS: &str = "aws:kms";

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body.
const POLICY_STORED: u16 = 204;

/// The actions AppFlow's published bucket policies grant for the two ends of a run: the
/// listing and the read a source end takes, and the write a destination end takes.
const EVERY_RUN_ACTION: [&str; 3] = ["s3:ListBucket", "s3:GetObject", "s3:PutObject"];

/// How long a flow run is given to reach a terminal status before the poll gives up:
/// generous, because it is a deadline on a slow host rather than a measurement of anything.
const RUN_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll waits between two reads of a run: short enough to cost nothing, long
/// enough not to spin.
const BETWEEN_READS: Duration = Duration::from_millis(20);

/// One wire answer: the status, the response headers and the body.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answer {
    /// The body as text, which is what every XML answer here is read out of.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The body as JSON, which is what the AppFlow answers carry.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Amazon S3 and Amazon AppFlow mounted on ONE engine, exactly as the router mounts them,
/// with request authentication off so each call carries a credential scope — which is what
/// names the Region and the service — without a computed signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_appflow_sandstoneconfigurationservicelambda::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// One call of `method` to `path` on `signing_name`'s endpoint in [`REGION`].
    async fn call(
        &self,
        method: &str,
        signing_name: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com{path}"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let (parts, body) = self.router.dispatch(request).await.into_parts();
        Answer {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
        }
    }

    /// One Amazon S3 request.
    async fn s3(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        self.call(method, "s3", path, headers, body).await
    }

    /// One AppFlow request: the operation's own URI, with the request as the JSON payload.
    async fn appflow(&self, path: &str, body: &Value) -> Answer {
        self.call(
            "POST",
            "appflow",
            path,
            &[("content-type", "application/json")],
            serde_json::to_vec(body).expect("serialize the AppFlow request"),
        )
        .await
    }

    /// `PUT /<bucket>` then `PUT /<bucket>?policy` — a bucket carrying the policy AppFlow asks
    /// a caller to place on a bucket it reads or writes.
    async fn bucket_granting_appflow(&self, bucket: &str) {
        let created = self.s3("PUT", &format!("/{bucket}"), &[], Vec::new()).await;
        assert_eq!(
            created.status,
            200,
            "expected CreateBucket of {bucket} to succeed, got {} {:?}",
            created.status,
            created.text()
        );
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowAppFlow",
                "Effect": "Allow",
                "Principal": { "Service": "appflow.amazonaws.com" },
                "Action": EVERY_RUN_ACTION,
                "Resource": [
                    format!("arn:aws:s3:::{bucket}"),
                    format!("arn:aws:s3:::{bucket}/*"),
                ],
            }],
        })
        .to_string();
        let placed = self
            .s3("PUT", &format!("/{bucket}?policy"), &[], policy.into_bytes())
            .await;
        assert_eq!(
            placed.status,
            POLICY_STORED,
            "expected PutBucketPolicy on {bucket} to be stored, got {} {:?}",
            placed.status,
            placed.text()
        );
    }

    /// `GET /<bucket>/<key>` — `GetObject`, with the headers the case asks with.
    async fn get_object(&self, bucket: &str, key: &str, headers: &[(&str, &str)]) -> Answer {
        self.s3("GET", &format!("/{bucket}/{key}"), headers, Vec::new())
            .await
    }

    /// `GET /<bucket>/<key>?attributes` — `GetObjectAttributes`, asking for the checksum
    /// container and the object's size.
    async fn get_object_attributes(&self, bucket: &str, key: &str) -> Answer {
        self.s3(
            "GET",
            &format!("/{bucket}/{key}?attributes"),
            &[("x-amz-object-attributes", "Checksum, ObjectSize")],
            Vec::new(),
        )
        .await
    }

    /// The runs of `flow_name`, newest first, read again until the newest has reached a
    /// terminal status.
    ///
    /// The terminal status is owed by the CLOCK and materialised by whichever call reads it
    /// next, so the condition is POLLED for rather than waited out: nothing here depends on
    /// how fast the host runs, and the deadline only turns a run that never settles into a
    /// failure instead of a hang.
    async fn settled_runs(&self, case: &str, flow_name: &str) -> Vec<Value> {
        let deadline = Instant::now() + RUN_DEADLINE;
        loop {
            let answer = self
                .appflow(
                    "/describe-flow-execution-records",
                    &json!({ "flowName": flow_name }),
                )
                .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected DescribeFlowExecutionRecords to succeed, got {} {:?}",
                answer.status,
                answer.text()
            );
            let runs = answer.json()["flowExecutions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let running = runs.first().is_some_and(|run| {
                matches!(
                    run.get("executionStatus").and_then(Value::as_str),
                    Some("InProgress" | "CancelStarted")
                )
            });
            if !running {
                return runs;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the newest run of {flow_name:?} to reach a terminal \
                 status, got {runs:?}"
            );
            std::thread::sleep(BETWEEN_READS);
        }
    }
}

/// The text `<name>` carries in `document`, or `None` when it carries no such element.
fn element(document: &str, name: &str) -> Option<String> {
    document
        .split_once(&format!("<{name}>"))
        .and_then(|(_, tail)| tail.split_once(&format!("</{name}>")))
        .map(|(value, _)| value.to_string())
}

/// One record as the source file presents it: its fields in SOURCE order.
type Record = &'static [(&'static str, &'static str)];

/// Which shape the source document writes its records in — all three are JSON, and a flow
/// declares only the file type, not the shape.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// An array of records.
    Array,
    /// One record per line.
    Lines,
    /// A single record, on its own.
    Single,
}

/// `record` as one JSON object, its fields in the order the record carries them.
fn json_object(record: Record) -> String {
    let fields: Vec<String> = record
        .iter()
        .map(|(name, value)| format!("\"{name}\":\"{value}\""))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// The bytes the source object holds: `records` written in `shape`.
fn source_document(shape: Shape, records: &[Record]) -> String {
    let objects: Vec<String> = records.iter().copied().map(json_object).collect();
    match shape {
        Shape::Array => format!("[{}]", objects.join(",")),
        Shape::Lines => objects
            .iter()
            .map(|object| format!("{object}\n"))
            .collect::<String>(),
        Shape::Single => objects.join(""),
    }
}

/// The bytes the destination object is expected to hold: one record per line, each with its
/// own fields in the order the SOURCE record presented them.
fn expected_output(records: &[Record]) -> String {
    records
        .iter()
        .copied()
        .map(|record| format!("{}\n", json_object(record)))
        .collect()
}

/// One row of the table.
struct Row {
    case: &'static str,
    shape: Shape,
    records: &'static [Record],
    /// The `bucketPrefix` the destination names, or `None` for a destination naming none.
    bucket_prefix: Option<&'static str>,
    /// The `kmsArn` the flow's definition names, or `None` for a definition naming none —
    /// the case AppFlow settles with the key it manages for the account.
    kms_arn: Option<&'static str>,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an array whose records carry their fields out of alphabetical order",
            shape: Shape::Array,
            records: &[
                &[("id", "1"), ("city", "lisbon"), ("temp", "21")],
                &[("id", "2"), ("city", "oslo"), ("temp", "4")],
                &[("id", "3"), ("city", "lima"), ("temp", "18")],
            ],
            bucket_prefix: None,
            kms_arn: None,
        },
        Row {
            case: "one record per line, fields in reverse alphabetical order",
            shape: Shape::Lines,
            records: &[
                &[("z", "1"), ("m", "2"), ("a", "3")],
                &[("z", "4"), ("m", "5"), ("a", "6")],
            ],
            bucket_prefix: Some("output"),
            kms_arn: Some(
                "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555",
            ),
        },
        Row {
            case: "a single record under a nested destination prefix",
            shape: Shape::Single,
            records: &[&[("region", "eu-west-1"), ("account", "42"), ("alias", "nord")]],
            bucket_prefix: Some("archive/raw/2026"),
            kms_arn: Some(
                "arn:aws:kms:us-east-1:000000000000:key/66666666-7777-8888-9999-aaaaaaaaaaaa",
            ),
        },
        Row {
            case: "two records that disagree about the order of the same two fields",
            shape: Shape::Array,
            records: &[&[("a", "1"), ("b", "2")], &[("b", "3"), ("a", "4")]],
            bucket_prefix: Some("output/"),
            kms_arn: None,
        },
    ]
}

/// What one started run has to be checked against once it has settled.
struct Started {
    case: &'static str,
    flow_name: String,
    /// The key the run's output object is expected at: the destination prefix, the flow name
    /// and the run's own id.
    key: String,
    /// The bytes the destination object is expected to hold, exactly.
    expected: String,
    /// The key the SAME bytes are mirrored to by a client's own `PutObject`, whose reported
    /// checksum the run's object has to agree with.
    mirror_key: String,
    /// The key the definition named, when it named one.
    named_kms_arn: Option<&'static str>,
}

#[tokio::test]
async fn a_read_reports_the_record_the_transferring_write_left_behind() {
    let emulator = Emulator::new();
    let source_bucket = "read-record-source";
    let destination_bucket = "read-record-destination";
    emulator.bucket_granting_appflow(source_bucket).await;
    emulator
        .bucket_granting_appflow(destination_bucket)
        .await;

    // Every run is STARTED first, so the span each one's terminal status is owed after runs
    // alongside the others rather than after the previous row's assertions.
    let mut started = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let source_prefix = format!("inbound/{index}");
        let seeded = emulator
            .s3(
                "PUT",
                &format!("/{source_bucket}/{source_prefix}/records"),
                &[],
                source_document(row.shape, row.records).into_bytes(),
            )
            .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: expected the source object to be stored, got {} {:?}",
            seeded.status,
            seeded.text()
        );

        let flow_name = format!("read-record-flow-{index}");
        let mut destination_properties = json!({ "bucketName": destination_bucket });
        if let Some(prefix) = row.bucket_prefix {
            destination_properties["bucketPrefix"] = json!(prefix);
        }
        let mut definition = json!({
            "flowName": flow_name,
            "triggerConfig": { "triggerType": "OnDemand" },
            "sourceFlowConfig": {
                "connectorType": "S3",
                "sourceConnectorProperties": {
                    "S3": { "bucketName": source_bucket, "bucketPrefix": source_prefix }
                }
            },
            "destinationFlowConfigList": [{
                "connectorType": "S3",
                "destinationConnectorProperties": { "S3": destination_properties }
            }],
            // Every field each record carries is written, under the name it was read as.
            "tasks": [{ "taskType": "Map_all", "sourceFields": [] }],
        });
        if let Some(kms_arn) = row.kms_arn {
            definition["kmsArn"] = json!(kms_arn);
        }
        let created = emulator.appflow("/create-flow", &definition).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected CreateFlow to succeed, got {} {:?}",
            created.status,
            created.text()
        );

        let run = emulator
            .appflow("/start-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            run.status,
            200,
            "case {case:?}: expected StartFlow to succeed, got {} {:?}",
            run.status,
            run.text()
        );
        let execution_id = run.json()["executionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !execution_id.is_empty(),
            "case {case:?}: expected StartFlow to report the run's id, got {:?}",
            run.text()
        );

        // The key a client composes the documented way: the configured prefix, the flow name,
        // the run id.
        let parent = row
            .bucket_prefix
            .map(|prefix| format!("{}/", prefix.trim_end_matches('/')))
            .unwrap_or_default();
        started.push(Started {
            case,
            key: format!("{parent}{flow_name}/{execution_id}"),
            flow_name,
            expected: expected_output(row.records),
            mirror_key: format!("mirror/{index}"),
            named_kms_arn: row.kms_arn,
        });
    }

    for subject in &started {
        let case = subject.case;
        let runs = emulator.settled_runs(case, &subject.flow_name).await;
        assert_eq!(
            runs.first()
                .and_then(|run| run.get("executionStatus"))
                .and_then(Value::as_str),
            Some("Successful"),
            "case {case:?}: expected the run to succeed, got {runs:?}"
        );

        // The key the FLOW records is the key its output is sealed under: the definition's
        // own, or the one AppFlow settled for a definition naming none.
        let described = emulator
            .appflow("/describe-flow", &json!({ "flowName": subject.flow_name }))
            .await;
        let flow_key = described.json()["kmsArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            flow_key.starts_with("arn:aws:kms:"),
            "case {case:?}: expected the flow to record a KMS key, got {:?}",
            described.text()
        );
        if let Some(named) = subject.named_kms_arn {
            assert_eq!(
                flow_key, named,
                "case {case:?}: expected the flow to keep the key it named, got {flow_key:?}"
            );
        }

        // 1. The bytes a read serves are the bytes the write stored: one record per line,
        //    each record's fields in the order its SOURCE record presented them.
        let read = emulator
            .get_object(
                destination_bucket,
                &subject.key,
                &[(CHECKSUM_MODE_HEADER, CHECKSUM_MODE_ENABLED)],
            )
            .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetObject of {:?} to be served, got {} {:?}",
            subject.key,
            read.status,
            read.text()
        );
        let served = read.text();
        assert_eq!(
            served, subject.expected,
            "case {case:?}: expected the object to hold {:?}, got {served:?}",
            subject.expected
        );

        // 2. The read reports a full-object checksum, and it is the digest of THOSE bytes:
        //    the value a client's own `PutObject` of the same bytes records for them.
        let mirrored = emulator
            .s3(
                "PUT",
                &format!("/{destination_bucket}/{}", subject.mirror_key),
                &[],
                read.body.to_vec(),
            )
            .await;
        assert_eq!(
            mirrored.status,
            200,
            "case {case:?}: expected the mirror write to succeed, got {} {:?}",
            mirrored.status,
            mirrored.text()
        );
        let digest = mirrored.header(CHECKSUM_HEADER).unwrap_or_default();
        assert!(
            !digest.is_empty(),
            "case {case:?}: expected the mirror write to report {CHECKSUM_HEADER}, got headers \
             {:?}",
            mirrored.headers
        );
        assert_eq!(
            read.header(CHECKSUM_HEADER).as_deref(),
            Some(digest.as_str()),
            "case {case:?}: expected the read to report the digest of its own bytes \
             ({digest:?}), got {:?}",
            read.header(CHECKSUM_HEADER)
        );
        assert_eq!(
            read.header(CHECKSUM_TYPE_HEADER).as_deref(),
            Some(FULL_OBJECT),
            "case {case:?}: expected a whole-object checksum, got {:?}",
            read.header(CHECKSUM_TYPE_HEADER)
        );

        // 3. And it reports the encryption the write settled: the mode, and the key the flow
        //    records — not the bucket's own SSE-S3 default with no key at all.
        assert_eq!(
            read.header(SSE_HEADER).as_deref(),
            Some(SSE_KMS),
            "case {case:?}: expected the read to report {SSE_KMS:?}, got {:?}",
            read.header(SSE_HEADER)
        );
        assert_eq!(
            read.header(SSE_KMS_KEY_ID_HEADER).as_deref(),
            Some(flow_key.as_str()),
            "case {case:?}: expected the read to report the flow's key {flow_key:?}, got {:?}",
            read.header(SSE_KMS_KEY_ID_HEADER)
        );

        // 4. The object's OTHER readers answer from that one record too: the attributes read
        //    reports the same digest and type beside the size of the same bytes.
        let attributes = emulator
            .get_object_attributes(destination_bucket, &subject.key)
            .await;
        assert_eq!(
            attributes.status,
            200,
            "case {case:?}: expected GetObjectAttributes of {:?} to be served, got {} {:?}",
            subject.key,
            attributes.status,
            attributes.text()
        );
        let document = attributes.text();
        assert_eq!(
            (
                element(&document, "ChecksumCRC64NVME"),
                element(&document, "ChecksumType"),
                element(&document, "ObjectSize"),
            ),
            (
                Some(digest.clone()),
                Some(FULL_OBJECT.to_string()),
                Some(subject.expected.len().to_string()),
            ),
            "case {case:?}: expected the attributes to answer from the same record, got body \
             {document:?}"
        );

        // 5. A read that does NOT enable the checksum mode retrieves no checksum — the
        //    encryption report carries no such gate — and neither read changed what the next
        //    caller sees.
        let plain = emulator
            .get_object(destination_bucket, &subject.key, &[])
            .await;
        assert_eq!(
            plain.status,
            200,
            "case {case:?}: expected the second GetObject to be served, got {} {:?}",
            plain.status,
            plain.text()
        );
        assert_eq!(
            (
                plain.header(CHECKSUM_HEADER),
                plain.header(CHECKSUM_TYPE_HEADER)
            ),
            (None, None),
            "case {case:?}: expected no checksum without {CHECKSUM_MODE_HEADER}, got {:?} / \
             {:?}",
            plain.header(CHECKSUM_HEADER),
            plain.header(CHECKSUM_TYPE_HEADER)
        );
        assert_eq!(
            (
                plain.text(),
                plain.header(SSE_HEADER),
                plain.header(SSE_KMS_KEY_ID_HEADER)
            ),
            (
                subject.expected.clone(),
                Some(SSE_KMS.to_string()),
                Some(flow_key.clone())
            ),
            "case {case:?}: expected the second read to serve the same record, got {:?} / {:?} \
             / {:?}",
            plain.text(),
            plain.header(SSE_HEADER),
            plain.header(SSE_KMS_KEY_ID_HEADER)
        );
    }
}
