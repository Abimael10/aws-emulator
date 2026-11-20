// SPDX-License-Identifier: CC0-1.0

//! What every Amazon S3 object-listing surface reports for a stored object's checksum, and
//! where an Amazon AppFlow flow run's output object is stored.
//!
//! RULE: a listed object carries the checksum the WRITE that stored it recorded — the
//! algorithm as the flattened `ChecksumAlgorithm` element ("the algorithm that was used to
//! create a checksum of the object": `Object$ChecksumAlgorithm` and
//! `ObjectVersion$ChecksumAlgorithm`, both a `ChecksumAlgorithmList` carrying
//! `xmlFlattened`) and the `ChecksumType` that write settled ("the checksum type that is
//! used to calculate the object's checksum value", one of `[COMPOSITE, FULL_OBJECT]`) —
//! read off the one record a read of the same object reports, so a listing and a read never
//! disagree. It holds on every surface that lists objects and for every writer, and the key
//! it reports is the one the writer composed (`Object$Key`, echoed verbatim).
//!
//! Two services are needed for the service-integration row: the object is written by an
//! AppFlow flow run and reported by Amazon S3's own listings, so both are mounted here on
//! the ONE engine the router mounts them on, and every step goes over the wire — the AppFlow
//! calls as the `restJson1` requests a client sends, the reads and listings as the
//! path-style Amazon S3 requests a client sends.
//!
//! The rows differ along what the rule generalises over: the WRITER (a flow run, a one-shot
//! write, a multipart assembly), the algorithm the write named (none, a CRC, a SHA), the
//! type it settles (a digest of the whole object, a composition over the parts' digests),
//! the bytes stored, the destination prefix a flow writes under, and the listing surface the
//! entry is read from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one the buckets are created in.
const REGION: &str = "us-east-1";

/// The `ChecksumAlgorithm` Amazon S3 itself adds to a write that named none: a checksum
/// member "is present if the object was uploaded with the `CRC64NVME` checksum algorithm, or
/// if it was uploaded without a checksum (and Amazon S3 added the default checksum,
/// `CRC64NVME`, to the uploaded object)" (`PutObjectOutput$ChecksumCRC64NVME`).
const DEFAULT_ALGORITHM: &str = "CRC64NVME";

/// A digest over all of the object's own bytes — the type a single whole-object write
/// settles ("for `PutObject` uploads, the checksum type is always `FULL_OBJECT`",
/// `PutObjectRequest$ChecksumType`), and the one `CRC64NVME` always carries ("the CRC64NVME
/// checksum is always a full object checksum", `ChecksumCRC64NVME`).
const FULL_OBJECT: &str = "FULL_OBJECT";

/// A composition over the parts' digests — the other value `ChecksumType` publishes
/// (`smithy_query(query="shape",shape="com.amazonaws.s3#ChecksumType")` reports `[COMPOSITE,
/// FULL_OBJECT]`), which "determines how part-level checksums are combined to create an
/// object-level checksum for multipart objects"
/// (`CreateMultipartUploadRequest$ChecksumType`).
const COMPOSITE: &str = "COMPOSITE";

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body to
/// return.
const POLICY_STORED: u16 = 204;

/// How long a flow run is given to reach a terminal status before the poll gives up:
/// generous, because it is a deadline on a slow host rather than a measurement of anything.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two reads of a run: short enough to cost nothing, long
/// enough not to spin.
const BETWEEN_READS: Duration = Duration::from_millis(20);

/// The actions AppFlow's published bucket policies grant for the two ends of a run: the
/// listing and the read a source end takes, and the write a destination end takes.
const EVERY_RUN_ACTION: [&str; 3] = ["s3:ListBucket", "s3:GetObject", "s3:PutObject"];

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

/// Which listing reports the object: the three operations that list the objects a bucket
/// holds, each addressed the way the model binds it and each with its own entry container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// `ListObjectsV2` — `GET /<bucket>?list-type=2`.
    ListObjectsV2,
    /// `ListObjects` — `GET /<bucket>`, the listing with no `list-type` discriminator.
    ListObjects,
    /// `ListObjectVersions` — `GET /<bucket>?versions`.
    ListObjectVersions,
}

impl Surface {
    /// Every surface, so a row is asserted on all of them rather than on the one the issue
    /// named.
    fn every() -> [Surface; 3] {
        [
            Surface::ListObjectsV2,
            Surface::ListObjects,
            Surface::ListObjectVersions,
        ]
    }

    /// The query this listing is addressed by, narrowed to `prefix`.
    fn query(self, prefix: &str) -> String {
        match self {
            Surface::ListObjectsV2 => format!("?list-type=2&prefix={prefix}"),
            Surface::ListObjects => format!("?prefix={prefix}"),
            Surface::ListObjectVersions => format!("?versions&prefix={prefix}"),
        }
    }

    /// The element one listed object arrives in: `Contents` for the two object listings,
    /// `Version` for the version listing (`ListObjectVersionsOutput$Versions`).
    fn container(self) -> &'static str {
        match self {
            Surface::ListObjectsV2 | Surface::ListObjects => "Contents",
            Surface::ListObjectVersions => "Version",
        }
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

    /// `PUT /<bucket>` and `PUT /<bucket>?policy` — a bucket, with the policy AppFlow asks a
    /// caller to place on a bucket it reads or writes.
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
            .s3(
                "PUT",
                &format!("/{bucket}?policy"),
                &[],
                policy.into_bytes(),
            )
            .await;
        assert_eq!(
            placed.status,
            POLICY_STORED,
            "expected PutBucketPolicy on {bucket} to be stored, got {} {:?}",
            placed.status,
            placed.text()
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject`, with the headers the row names.
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Answer {
        self.s3("PUT", &format!("/{bucket}/{key}"), headers, body.to_vec())
            .await
    }

    /// An object assembled by a multipart upload of ONE part: the initiation carrying the
    /// checksum members the row names, the part, and the completion naming it.
    async fn multipart_object(
        &self,
        case: &str,
        bucket: &str,
        key: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) {
        let initiated = self
            .s3("POST", &format!("/{bucket}/{key}?uploads"), headers, Vec::new())
            .await;
        assert_eq!(
            initiated.status,
            200,
            "case {case:?}: expected the initiation to succeed, got {} {:?}",
            initiated.status,
            initiated.text()
        );
        let upload_id = element(&initiated.text(), "UploadId").unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the initiation to report an UploadId, got {:?}",
                initiated.text()
            )
        });
        let uploaded = self
            .s3(
                "PUT",
                &format!("/{bucket}/{key}?partNumber=1&uploadId={upload_id}&x-id=UploadPart"),
                &[],
                body.to_vec(),
            )
            .await;
        assert_eq!(
            uploaded.status,
            200,
            "case {case:?}: expected the part write to succeed, got {} {:?}",
            uploaded.status,
            uploaded.text()
        );
        let etag = uploaded.header("etag").unwrap_or_else(|| {
            panic!("case {case:?}: expected the part write to report an ETag header")
        });
        // A completion of an upload whose object-level checksum is COMPOSED of its parts'
        // checksums has to state each part's own checksum, so this fixture names the one the
        // part write answered. An upload under no algorithm, under the algorithm that is
        // always a full-object checksum, or asking for a full-object checksum composes
        // nothing from the parts and names none.
        let header_value = |name: &str| {
            headers
                .iter()
                .find(|(header, _)| header.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.to_string())
        };
        let part_checksum = header_value("x-amz-checksum-algorithm")
            .filter(|algorithm| {
                algorithm != "CRC64NVME"
                    && header_value("x-amz-checksum-type").as_deref() != Some(FULL_OBJECT)
            })
            .map(|algorithm| {
                let header = format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase());
                let value = uploaded.header(&header).unwrap_or_else(|| {
                    panic!("case {case:?}: expected the part write to report {header}")
                });
                format!("<Checksum{algorithm}>{value}</Checksum{algorithm}>")
            })
            .unwrap_or_default();
        let payload = format!(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{etag}</ETag>\
             {part_checksum}</Part></CompleteMultipartUpload>"
        );
        let completed = self
            .s3(
                "POST",
                &format!("/{bucket}/{key}?uploadId={upload_id}"),
                &[],
                payload.into_bytes(),
            )
            .await;
        assert_eq!(
            completed.status,
            200,
            "case {case:?}: expected the completion to succeed, got {} {:?}",
            completed.status,
            completed.text()
        );
    }

    /// `GET /<bucket>/<key>` with the checksum mode enabled — the READ whose checksum the
    /// listing's pair has to agree with ("to retrieve the checksum, this mode must be
    /// enabled", `GetObjectRequest$ChecksumMode`).
    async fn read_with_checksum(&self, bucket: &str, key: &str) -> Answer {
        self.s3(
            "GET",
            &format!("/{bucket}/{key}"),
            &[("x-amz-checksum-mode", "ENABLED")],
            Vec::new(),
        )
        .await
    }

    /// One listing of `bucket` narrowed to `prefix`, through `surface`.
    async fn listing(&self, bucket: &str, surface: Surface, prefix: &str) -> Answer {
        let answer = self
            .s3(
                "GET",
                &format!("/{bucket}{}", surface.query(prefix)),
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "expected {surface:?} of {bucket} to succeed, got {} {:?}",
            answer.status,
            answer.text()
        );
        answer
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

/// Every `<container>…</container>` fragment of `document`, in document order — one per
/// listed object.
fn entries(document: &str, container: &str) -> Vec<String> {
    document
        .split(&format!("<{container}>"))
        .skip(1)
        .filter_map(|chunk| {
            chunk
                .split_once(&format!("</{container}>"))
                .map(|(entry, _)| entry.to_string())
        })
        .collect()
}

/// How many times `<name>` opens inside one entry — what tells ONE flattened
/// `ChecksumAlgorithm` element from a nested list or from a repeated one.
fn occurrences(entry: &str, name: &str) -> usize {
    entry.matches(&format!("<{name}>")).count()
}

/// Assert that `surface`'s listing reports `key`, and that its entry carries exactly the
/// algorithm and type the write recorded.
fn assert_listed_pair(
    case: &str,
    surface: Surface,
    listing: &str,
    key: &str,
    algorithm: &str,
    checksum_type: &str,
) {
    let entry = entries(listing, surface.container())
        .into_iter()
        .find(|entry| element(entry, "Key").as_deref() == Some(key))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}/{surface:?}: expected an entry for {key:?}, got body {listing:?}"
            )
        });
    assert_eq!(
        occurrences(&entry, "ChecksumAlgorithm"),
        1,
        "case {case:?}/{surface:?}: expected one flattened ChecksumAlgorithm element for \
         {key:?}, got entry {entry:?}"
    );
    assert_eq!(
        element(&entry, "ChecksumAlgorithm").as_deref(),
        Some(algorithm),
        "case {case:?}/{surface:?}: expected ChecksumAlgorithm {algorithm:?} for {key:?}, got \
         entry {entry:?}"
    );
    assert_eq!(
        element(&entry, "ChecksumType").as_deref(),
        Some(checksum_type),
        "case {case:?}/{surface:?}: expected ChecksumType {checksum_type:?} for {key:?}, got \
         entry {entry:?}"
    );
}

/// Assert that a READ of the same object reports the same pair: the value travels on the
/// header its algorithm names (`x-amz-checksum-<algorithm>`, the binding every `Checksum*`
/// member carries), beside the same `ChecksumType` — which is what makes the listing's pair
/// the object's own record rather than a constant.
fn assert_read_agrees(case: &str, read: &Answer, algorithm: &str, checksum_type: &str) {
    let carried = format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase());
    let value = read.header(&carried).unwrap_or_default();
    assert!(
        !value.is_empty(),
        "case {case:?}: expected the read to report {carried}, got headers {:?}",
        read.headers
    );
    assert_eq!(
        read.header("x-amz-checksum-type").as_deref(),
        Some(checksum_type),
        "case {case:?}: expected the read to report ChecksumType {checksum_type:?}, got {:?}",
        read.header("x-amz-checksum-type")
    );
}

/// How a row's object was written.
enum Written {
    /// One `PutObject` of all the bytes, naming the algorithm given (or none).
    OneShot(Option<&'static str>),
    /// A multipart upload initiated with the algorithm and the `ChecksumType` given (or
    /// neither), completed from its parts.
    Multipart {
        algorithm: Option<&'static str>,
        requested_type: Option<&'static str>,
    },
}

/// One row of the stored-object table.
struct Stored {
    case: &'static str,
    key: &'static str,
    written: Written,
    body: Vec<u8>,
    /// The `ChecksumAlgorithm` every surface must report for it.
    algorithm: &'static str,
    /// The `ChecksumType` every surface must report for it.
    checksum_type: &'static str,
}

/// `length` bytes spanning the whole 0..=255 range from a fixed seed — a 32-bit LCG (the
/// "ANSI C" multiplier and increment), taking the high byte of each step — so a row's
/// payload is byte-varied without anything random.
fn seeded(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 24) as u8
        })
        .collect()
}

/// The table. The rows differ along the writer, the algorithm the write named, the type the
/// write settles and the bytes stored; the expected types are the model's own rule — a
/// one-shot write settles a whole-object checksum, a multipart assembly a composition over
/// its parts' checksums, except under an algorithm whose checksum "is always a full object
/// checksum" or when the initiation asks for a whole-object one under a CRC.
fn stored_rows() -> Vec<Stored> {
    vec![
        Stored {
            case: "a one-shot write naming no algorithm",
            key: "written/plain",
            written: Written::OneShot(None),
            body: b"a,b\n1,2\n".to_vec(),
            algorithm: DEFAULT_ALGORITHM,
            checksum_type: FULL_OBJECT,
        },
        Stored {
            case: "an empty one-shot write naming a CRC",
            key: "written/empty-crc",
            written: Written::OneShot(Some("CRC32")),
            body: Vec::new(),
            algorithm: "CRC32",
            checksum_type: FULL_OBJECT,
        },
        Stored {
            case: "a one-shot write of byte-varied bytes naming a SHA",
            key: "written/varied-sha",
            written: Written::OneShot(Some("SHA256")),
            body: seeded(0x5eed_1234, 4_096),
            algorithm: "SHA256",
            checksum_type: FULL_OBJECT,
        },
        Stored {
            case: "a multipart assembly naming no algorithm",
            key: "assembled/plain",
            written: Written::Multipart {
                algorithm: None,
                requested_type: None,
            },
            body: b"the assembled bytes".to_vec(),
            algorithm: DEFAULT_ALGORITHM,
            checksum_type: FULL_OBJECT,
        },
        Stored {
            case: "a multipart assembly naming a SHA",
            key: "assembled/sha",
            written: Written::Multipart {
                algorithm: Some("SHA256"),
                requested_type: None,
            },
            body: b"the assembled bytes, digested".to_vec(),
            algorithm: "SHA256",
            checksum_type: COMPOSITE,
        },
        Stored {
            case: "a multipart assembly naming a CRC",
            key: "assembled/crc",
            written: Written::Multipart {
                algorithm: Some("CRC32"),
                requested_type: None,
            },
            body: seeded(0x1234_5678, 1_024),
            algorithm: "CRC32",
            checksum_type: COMPOSITE,
        },
        Stored {
            case: "a multipart assembly asked for a whole-object CRC",
            key: "assembled/crc-whole",
            written: Written::Multipart {
                algorithm: Some("CRC32"),
                requested_type: Some(FULL_OBJECT),
            },
            body: b"the whole of the assembled bytes".to_vec(),
            algorithm: "CRC32",
            checksum_type: FULL_OBJECT,
        },
    ]
}

#[tokio::test]
async fn every_objects_listing_reports_the_checksum_pair_the_write_recorded() {
    let emulator = Emulator::new();
    let bucket = "listing-checksum-projection";
    emulator.bucket_granting_appflow(bucket).await;

    let rows = stored_rows();
    for row in &rows {
        let case = row.case;
        match row.written {
            Written::OneShot(algorithm) => {
                let headers: Vec<(&str, &str)> = algorithm
                    .map(|algorithm| vec![("x-amz-sdk-checksum-algorithm", algorithm)])
                    .unwrap_or_default();
                let stored = emulator
                    .put_object(bucket, row.key, &headers, &row.body)
                    .await;
                assert_eq!(
                    stored.status,
                    200,
                    "case {case:?}: expected the write to succeed, got {} {:?}",
                    stored.status,
                    stored.text()
                );
            }
            Written::Multipart {
                algorithm,
                requested_type,
            } => {
                let mut headers: Vec<(&str, &str)> = Vec::new();
                if let Some(algorithm) = algorithm {
                    headers.push(("x-amz-checksum-algorithm", algorithm));
                }
                if let Some(requested) = requested_type {
                    headers.push(("x-amz-checksum-type", requested));
                }
                emulator
                    .multipart_object(case, bucket, row.key, &headers, &row.body)
                    .await;
            }
        }

        // The READ is the record every surface has to agree with.
        let read = emulator.read_with_checksum(bucket, row.key).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the read to be served, got {} {:?}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.body.as_ref(),
            row.body.as_slice(),
            "case {case:?}: expected the read to carry the stored bytes, got {} of them",
            read.body.len()
        );
        assert_read_agrees(case, &read, row.algorithm, row.checksum_type);
    }

    // One listing per surface reports every object the bucket holds, each with its own pair.
    for surface in Surface::every() {
        let listing = emulator.listing(bucket, surface, "").await.text();
        for row in &rows {
            assert_listed_pair(
                row.case,
                surface,
                &listing,
                row.key,
                row.algorithm,
                row.checksum_type,
            );
        }
    }
}

/// One row of the flow-run table: the destination prefix a flow writes under, and the key
/// elements its output object must sit beneath.
struct Run {
    case: &'static str,
    /// The `bucketPrefix` the destination names, or `None` for a destination naming none.
    bucket_prefix: Option<&'static str>,
    /// The elements ahead of the flow name and the run id — empty for no prefix.
    expected_parent: &'static str,
    /// The records the source location holds.
    source: &'static str,
}

/// The table: the rows differ along whether the destination names a prefix at all, how many
/// elements it has, and how many records the run moves — none of which changes the key
/// layout or the checksum pair.
fn run_rows() -> Vec<Run> {
    vec![
        Run {
            case: "a destination naming no prefix",
            bucket_prefix: None,
            expected_parent: "",
            source: r#"[{"id":"1"},{"id":"2"}]"#,
        },
        Run {
            case: "a one-element destination prefix",
            bucket_prefix: Some("output"),
            expected_parent: "output",
            source: r#"[{"id":"1"}]"#,
        },
        Run {
            case: "a nested destination prefix",
            bucket_prefix: Some("archive/raw/2026"),
            expected_parent: "archive/raw/2026",
            source: r#"[{"id":"1"},{"id":"2"},{"id":"3"}]"#,
        },
    ]
}

/// A flow run writes ONE object, at the destination prefix then the flow name then the run's
/// own id, and every listing surface reports it at that key with the checksum Amazon S3
/// records for a whole-object write that names no algorithm — the same pair a client's own
/// `PutObject` of the same bytes gets, because a service integration's write is a write like
/// any other.
#[tokio::test]
async fn a_flow_run_output_is_listed_at_its_key_with_the_checksum_its_write_recorded() {
    for (index, row) in run_rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let source_bucket = "flow-listing-source";
        let destination_bucket = "flow-listing-destination";
        emulator.bucket_granting_appflow(source_bucket).await;
        emulator
            .bucket_granting_appflow(destination_bucket)
            .await;
        let seeded = emulator
            .put_object(
                source_bucket,
                "inbound/orders.json",
                &[],
                row.source.as_bytes(),
            )
            .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: expected the source object to be stored, got {} {:?}",
            seeded.status,
            seeded.text()
        );

        let flow_name = format!("flow-listing-{index}");
        let mut destination_properties = json!({ "bucketName": destination_bucket });
        if let Some(prefix) = row.bucket_prefix {
            destination_properties["bucketPrefix"] = json!(prefix);
        }
        let created = emulator
            .appflow(
                "/create-flow",
                &json!({
                    "flowName": flow_name,
                    "triggerConfig": { "triggerType": "OnDemand" },
                    "sourceFlowConfig": {
                        "connectorType": "S3",
                        "sourceConnectorProperties": {
                            "S3": { "bucketName": source_bucket, "bucketPrefix": "inbound" }
                        }
                    },
                    "destinationFlowConfigList": [{
                        "connectorType": "S3",
                        "destinationConnectorProperties": { "S3": destination_properties }
                    }],
                    "tasks": [{
                        "taskType": "Map",
                        "sourceFields": ["id"],
                        "destinationField": "id",
                        "connectorOperator": { "S3": "PROJECTION" },
                        "taskProperties": {}
                    }],
                }),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected CreateFlow to succeed, got {} {:?}",
            created.status,
            created.text()
        );

        let started = emulator
            .appflow("/start-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            started.status,
            200,
            "case {case:?}: expected StartFlow to succeed, got {} {:?}",
            started.status,
            started.text()
        );
        let execution_id = started.json()["executionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !execution_id.is_empty(),
            "case {case:?}: expected StartFlow to report the run's id, got {:?}",
            started.text()
        );
        let runs = emulator.settled_runs(case, &flow_name).await;
        assert_eq!(
            runs.first()
                .and_then(|run| run.get("executionStatus"))
                .and_then(Value::as_str),
            Some("Successful"),
            "case {case:?}: expected the run to succeed, got {runs:?}"
        );

        // The key a client composes the documented way: the configured prefix, the flow
        // name, the run id — and nothing else.
        let flow_prefix = if row.expected_parent.is_empty() {
            flow_name.clone()
        } else {
            format!("{}/{flow_name}", row.expected_parent)
        };
        let expected_key = format!("{flow_prefix}/{execution_id}");

        let read = emulator
            .read_with_checksum(destination_bucket, &expected_key)
            .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the run's output to be served at {expected_key:?}, got {} \
             {:?}",
            read.status,
            read.text()
        );
        assert!(
            !read.body.is_empty(),
            "case {case:?}: expected the run's output to carry the records it moved, got an \
             empty object"
        );
        assert_read_agrees(case, &read, DEFAULT_ALGORITHM, FULL_OBJECT);

        for surface in Surface::every() {
            let listing = emulator
                .listing(destination_bucket, surface, &flow_prefix)
                .await
                .text();
            let keys: Vec<Option<String>> = entries(&listing, surface.container())
                .iter()
                .map(|entry| element(entry, "Key"))
                .collect();
            assert_eq!(
                keys,
                vec![Some(expected_key.clone())],
                "case {case:?}/{surface:?}: expected the run's output listed at \
                 {expected_key:?} and nothing else, got body {listing:?}"
            );
            assert_listed_pair(
                case,
                surface,
                &listing,
                &expected_key,
                DEFAULT_ALGORITHM,
                FULL_OBJECT,
            );
        }
    }
}
