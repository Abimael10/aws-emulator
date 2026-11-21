// SPDX-License-Identifier: CC0-1.0

//! The data transformation a destination's `ProcessingConfiguration` asks for, end to end.
//!
//! The rule under test: when a destination's `ProcessingConfiguration` is enabled and names a
//! processor of type `Lambda`, every record of a buffer is handed to the function that
//! processor's `LambdaArn` parameter names BEFORE the buffer is written, and the verdict the
//! function returns for each record is what the destination receives —
//!
//! * `Ok`: the record is delivered carrying the data the function returned, never the bytes it
//!   was handed;
//! * `Dropped`: the record is delivered nowhere at all;
//! * `ProcessingFailed`: the record is not delivered either — it goes to the destination's
//!   `ErrorOutputPrefix` ("a prefix that Firehose evaluates and adds to failed records before
//!   writing them to S3"), carrying its own source bytes;
//!
//! and a record the answer accounts for in none of those ways (an id it never mentions, an id
//! it mentions twice, a `result` that is none of the three, an `Ok` with no data) is a record
//! the transformation did not place, so it goes to the error output too rather than being
//! delivered as the bytes the function was asked to replace. With `S3BackupMode` `Enabled`,
//! the UNTRANSFORMED source record is additionally written to the location
//! `S3BackupConfiguration` names, so the backup is the one copy of what the producer sent
//! while the main prefix holds only the transformed bytes. A configuration whose processing is
//! NOT enabled invokes nothing and delivers the producer's own bytes, which is the control
//! this table carries.
//!
//! Only an end-to-end run can pin this: the function is served by this same emulator's Lambda,
//! so the request Firehose renders is the request a handler RUNS on, the verdicts Firehose
//! applies are that handler's own, and the delivered, failed and backed-up objects are read
//! back out of Amazon S3 over its own wire. Nothing stands between the three services.
//!
//! The rows differ along what the rule generalises over: the verdict each record gets, the
//! shape of the transformation (upper-casing, reversing, appending the function's own name,
//! leaving the body as it was), how many records of a buffer share one verdict, whether the
//! buffer has any record left to deliver at all, whether the destination also backs its source
//! records up, and whether the processing is enabled in the first place.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::delivery_stream_lifecycle::{REGION, ROLE_ARN};

/// The endpoint every control-plane request here reaches the emulator on.
const HOST: &str = "127.0.0.1:9297";

/// The bucket every destination below delivers to.
const BUCKET: &str = "firehose-transform-sink";

/// `BucketARN` of [`BUCKET`], in the form that member's pattern requires.
const BUCKET_ARN: &str = "arn:aws:s3:::firehose-transform-sink";

/// The `firehose:error-output-type` the records a destination's data processing did not place
/// are written under.
const PROCESSING_FAILED_OUTPUT_TYPE: &str = "processing-failed";

/// The function every row's destination transforms its records with, and the module and
/// attribute its deployment package carries the handler as.
const FUNCTION: &str = "firehose-record-transform";
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime the deployment package below is written for.
const RUNTIME: &str = "python3.12";

/// The function's own execution role, satisfying the model's `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long one invocation of that function is given, as its `Timeout`.
const FUNCTION_TIMEOUT: u64 = 20;

/// How long an outcome is waited for before a row fails: far longer than the emulator's own
/// delivery span, so a loaded machine does not fail a row the clock would have served.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often an outcome is polled for while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The handler every row's destination names.
///
/// One function serves the whole table: each record's own data carries the verdict the handler
/// is to return for it and the shape of the transformation, which is the one thing a record
/// can instruct it with — `<verdict>|<shape>|<body>` — so what a row observes is the answer
/// Firehose read rather than a handler written per row.
const HANDLER_SOURCE: &str = r#"
import base64


def lambda_handler(event, context):
    answered = []
    for record in event["records"]:
        payload = base64.b64decode(record["data"]).decode("utf-8")
        parts = payload.split("|", 2)
        verdict = parts[0]
        shape = parts[1] if len(parts) > 1 else "same"
        body = parts[2] if len(parts) > 2 else ""
        if shape == "upper":
            transformed = body.upper()
        elif shape == "reverse":
            transformed = body[::-1]
        elif shape == "stamp":
            transformed = body + "|fn=" + context.function_name
        else:
            transformed = body
        encoded = base64.b64encode(transformed.encode("utf-8")).decode("utf-8")
        record_id = record["recordId"]
        if verdict == "omit":
            continue
        if verdict == "twice":
            answered.append({"recordId": record_id, "result": "Ok", "data": encoded})
            answered.append({"recordId": record_id, "result": "Ok", "data": encoded})
            continue
        if verdict == "unknown":
            answered.append({"recordId": record_id, "result": "Maybe", "data": encoded})
            continue
        if verdict == "nodata":
            answered.append({"recordId": record_id, "result": "Ok"})
            continue
        if verdict == "dropped":
            answered.append({"recordId": record_id, "result": "Dropped"})
            continue
        if verdict == "failed":
            answered.append({"recordId": record_id, "result": "ProcessingFailed"})
            continue
        answered.append({"recordId": record_id, "result": "Ok", "data": encoded})
    return {"records": answered}
"#;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// What the handler is instructed to answer for one record, and what that answer makes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// `Ok` with the transformed data: the record is delivered carrying it.
    Transformed,
    /// `Dropped`: the record is delivered nowhere and no error output is written for it.
    Dropped,
    /// `ProcessingFailed`: the record goes to the error output under the code that says the
    /// function marked it so.
    Failed,
    /// The answer never mentions the record.
    Omitted,
    /// The answer mentions the record twice, so which entry is its verdict is not stated.
    Repeated,
    /// The answer states a `result` that is none of the three the protocol defines.
    UnknownResult,
    /// The answer states `Ok` but returns no data for the destination to receive.
    WithoutData,
}

impl Verdict {
    /// The token a record's own data carries to instruct the handler.
    fn token(self) -> &'static str {
        match self {
            Self::Transformed => "ok",
            Self::Dropped => "dropped",
            Self::Failed => "failed",
            Self::Omitted => "omit",
            Self::Repeated => "twice",
            Self::UnknownResult => "unknown",
            Self::WithoutData => "nodata",
        }
    }

    /// The `errorCode` the error-output document of a record under this verdict names, and
    /// `None` for a verdict that writes no error output at all.
    ///
    /// The codes are the ones Firehose's own data-transformation failure table publishes for
    /// each of these answers.
    fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Transformed | Self::Dropped => None,
            Self::Failed => Some("Lambda.ProcessingFailedStatus"),
            Self::Omitted => Some("Lambda.MissingRecordId"),
            Self::Repeated => Some("Lambda.DuplicatedRecordId"),
            Self::UnknownResult | Self::WithoutData => Some("Lambda.JsonProcessingException"),
        }
    }
}

/// The shape of the transformation the handler applies to a record's body.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Upper-cased.
    Upper,
    /// Reversed.
    Reverse,
    /// The body with the function's own name appended, which only a record that really
    /// reached the function can carry.
    Stamp,
    /// The body as it was, so a row can transform nothing and still be transformed.
    Same,
}

impl Shape {
    /// The token a record's own data carries to instruct the handler.
    fn token(self) -> &'static str {
        match self {
            Self::Upper => "upper",
            Self::Reverse => "reverse",
            Self::Stamp => "stamp",
            Self::Same => "same",
        }
    }

    /// What the handler returns for `body` under this shape — the same transformation, stated
    /// here so a row says what the function returns rather than reading it back off the
    /// delivery.
    fn applied(self, body: &str) -> String {
        match self {
            Self::Upper => body.to_uppercase(),
            Self::Reverse => body.chars().rev().collect(),
            Self::Stamp => format!("{body}|fn={FUNCTION}"),
            Self::Same => body.to_string(),
        }
    }
}

/// One record a row puts.
#[derive(Debug, Clone)]
struct Put {
    verdict: Verdict,
    shape: Shape,
    body: String,
}

impl Put {
    fn new(verdict: Verdict, shape: Shape, body: &str) -> Self {
        Self {
            verdict,
            shape,
            body: body.to_string(),
        }
    }

    /// The bytes the producer puts, which carry the instruction for the handler.
    fn source(&self) -> String {
        format!("{}|{}|{}", self.verdict.token(), self.shape.token(), self.body)
    }

    /// What the destination's main prefix receives for this record, and `None` for one it
    /// receives nothing for.
    fn delivered(&self, processing_enabled: bool) -> Option<String> {
        if !processing_enabled {
            // Nothing is invoked, so the record is delivered exactly as it was put.
            return Some(self.source());
        }
        (self.verdict == Verdict::Transformed).then(|| self.shape.applied(&self.body))
    }

    /// The `errorCode` the error output names for this record, and `None` for one that is not
    /// written there.
    fn error_code(&self, processing_enabled: bool) -> Option<&'static str> {
        processing_enabled.then(|| self.verdict.error_code()).flatten()
    }
}

/// One row: a stream of its own, whose destination transforms its records with the one
/// function, and the records it puts.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// `ProcessingConfiguration$Enabled`: "enables or disables data processing".
    processing_enabled: bool,
    /// Whether the destination also backs every source record up (`S3BackupMode` `Enabled`
    /// with an `S3BackupConfiguration`).
    backs_up: bool,
    puts: Vec<Put>,
}

impl Row {
    /// The prefix this row's delivered objects are written under.
    fn main_prefix(&self) -> String {
        format!("{}/main/", self.case)
    }

    /// The prefix its failed records are written under: its own `ErrorOutputPrefix` with
    /// `!{firehose:error-output-type}` expanded.
    fn error_prefix(&self) -> String {
        format!("{}/err/{PROCESSING_FAILED_OUTPUT_TYPE}/", self.case)
    }

    /// The prefix its source records are backed up under.
    fn backup_prefix(&self) -> String {
        format!("{}/bkp/", self.case)
    }

    /// The body the main prefix's object carries: the records the transformation left, in
    /// arrival order, each followed by the delimiter the destination's
    /// `AppendDelimiterToRecord` processor asks for.
    ///
    /// Empty when the transformation left nothing to deliver, in which case no object is
    /// written at all.
    fn expected_main(&self) -> Vec<u8> {
        let mut body = String::new();
        for put in &self.puts {
            let Some(delivered) = put.delivered(self.processing_enabled) else {
                continue;
            };
            body.push_str(&delivered);
            if self.processing_enabled {
                body.push('\n');
            }
        }
        body.into_bytes()
    }

    /// The records the error output holds, as `(source bytes, errorCode)` in arrival order.
    fn expected_failures(&self) -> Vec<(String, &'static str)> {
        self.puts
            .iter()
            .filter_map(|put| {
                put.error_code(self.processing_enabled)
                    .map(|code| (put.source(), code))
            })
            .collect()
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every-record-transformed",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Transformed, Shape::Upper, "alpha-one"),
                // The function's own name, which a record only carries if it really reached
                // the function.
                Put::new(Verdict::Transformed, Shape::Stamp, "beta-two"),
            ],
        },
        Row {
            case: "one-record-dropped",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Transformed, Shape::Reverse, "keep-me"),
                Put::new(Verdict::Dropped, Shape::Same, "lose-me"),
            ],
        },
        Row {
            case: "one-record-processing-failed",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Transformed, Shape::Upper, "good-one"),
                Put::new(Verdict::Failed, Shape::Same, "bad-one"),
            ],
        },
        Row {
            case: "every-record-dropped",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Dropped, Shape::Same, "gone-one"),
                Put::new(Verdict::Dropped, Shape::Upper, "gone-two"),
            ],
        },
        Row {
            case: "every-record-processing-failed",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Failed, Shape::Same, "doomed-one"),
                Put::new(Verdict::Failed, Shape::Reverse, "doomed-two"),
            ],
        },
        Row {
            case: "the-answer-omits-a-record",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Omitted, Shape::Same, "ghost"),
                Put::new(Verdict::Transformed, Shape::Upper, "present"),
            ],
        },
        Row {
            case: "the-answer-repeats-a-record",
            processing_enabled: true,
            backs_up: false,
            puts: vec![
                Put::new(Verdict::Repeated, Shape::Upper, "double"),
                Put::new(Verdict::Transformed, Shape::Upper, "single"),
            ],
        },
        Row {
            case: "the-answer-states-an-unknown-result",
            processing_enabled: true,
            backs_up: false,
            puts: vec![Put::new(Verdict::UnknownResult, Shape::Same, "undecided")],
        },
        Row {
            case: "the-answer-returns-no-data",
            processing_enabled: true,
            backs_up: false,
            puts: vec![Put::new(Verdict::WithoutData, Shape::Same, "bodyless")],
        },
        Row {
            case: "the-backup-keeps-the-source-record",
            processing_enabled: true,
            backs_up: true,
            puts: vec![
                Put::new(Verdict::Transformed, Shape::Reverse, "backed-up-one"),
                Put::new(Verdict::Transformed, Shape::Upper, "backed-up-two"),
            ],
        },
        Row {
            // The control: the same processor, with processing switched off. Nothing is
            // invoked and the producer's own bytes are what the destination receives.
            case: "processing-disabled-delivers-the-source",
            processing_enabled: false,
            backs_up: false,
            puts: vec![Put::new(Verdict::Transformed, Shape::Upper, "untouched")],
        },
    ]
}

// ---------------------------------------------------------------------------
// The emulator
// ---------------------------------------------------------------------------

/// Firehose, Lambda and Amazon S3 on ONE shared engine, exactly as the router mounts them: the
/// function a destination's processing names is served by Lambda in this same process, and the
/// destination's locations are the Amazon S3 service's own store.
///
/// IAM is deliberately not mounted, as in this crate's other delivery suites: a destination
/// role nothing can be asked about withholds nothing, so what a row observes is the delivery
/// itself.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    /// Dispatch one request as it arrives on the wire, answering its status and body.
    async fn send(
        &self,
        service: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (u16, bytes::Bytes) {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{path}"))
            .header("host", HOST)
            .header("authorization", authorization(service));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.body().clone())
    }

    /// One Firehose `awsJson1_1` operation that must succeed.
    async fn firehose_ok(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .send(
                "firehose",
                "POST",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", &format!("Firehose_20150804.{operation}")),
                ],
                serde_json::to_vec(&body).expect("the request body serializes"),
            )
            .await;
        let text = String::from_utf8_lossy(&answered).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must succeed, got {status} {text}"
        );
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// One Lambda `restJson1` control-plane call, answering its status and JSON body.
    async fn lambda(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("the request body serializes"))
            .unwrap_or_default();
        let (status, answered) = self
            .send(
                "lambda",
                method,
                path,
                &[("content-type", "application/json")],
                payload,
            )
            .await;
        let text = String::from_utf8_lossy(&answered).to_string();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// `PUT /<bucket>` — Amazon S3's `CreateBucket`, so the destinations exist.
    async fn create_bucket(&self) {
        let (status, body) = self
            .send(
                "s3",
                "PUT",
                &format!("/{BUCKET}"),
                &[],
                format!(
                    "<CreateBucketConfiguration \
                     xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                )
                .into_bytes(),
            )
            .await;
        assert_eq!(
            status,
            200,
            "setup: the destination bucket must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key the bucket holds under `prefix`, read through `ListObjectsV2`.
    async fn keys_under(&self, case: &str, prefix: &str) -> Vec<String> {
        let (status, body) = self
            .send(
                "s3",
                "GET",
                &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
                &[],
                Vec::new(),
            )
            .await;
        let text = String::from_utf8_lossy(&body).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: listing {prefix:?} must succeed, got {status} {text}"
        );
        let mut keys = keys_of(&text);
        keys.sort();
        keys
    }

    /// The bytes the bucket holds at `key`, read through `GetObject`.
    async fn object(&self, case: &str, key: &str) -> Vec<u8> {
        let (status, body) = self
            .send("s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new())
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: reading {key:?} must succeed, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        body.to_vec()
    }

    /// How many records are still buffered for `stream`, read out of this service's own store:
    /// a buffer that has been delivered holds none of them any more.
    fn buffered(&self, stream: &str) -> usize {
        let state = self.engine.state::<
            service_com_amazonaws_firehose_firehose_20150804::ComAmazonawsFirehoseFirehose20150804State,
        >();
        let store = state.lock();
        store
            .streams
            .get(stream)
            .map(|stream| stream.records.len())
            .unwrap_or_default()
    }

    /// Wait until `ready` holds, or fail naming `case` and what did not happen. Nothing here
    /// drives a delivery: what makes it hold is the emulator's own delivery clock, which is
    /// what a producer that makes no further call observes.
    async fn wait_until(&self, case: &str, expected: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {expected} within {DEADLINE:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait until at least one object is written under `prefix`, and answer the keys.
    async fn wait_for_keys(&self, case: &str, prefix: &str) -> Vec<String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let keys = self.keys_under(case, prefix).await;
            if !keys.is_empty() {
                return keys;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected an object under {prefix:?} within {DEADLINE:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The one object written under `prefix`, as its bytes.
    async fn only_object_under(&self, case: &str, prefix: &str) -> Vec<u8> {
        let keys = self.wait_for_keys(case, prefix).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: one buffer is delivered as one object under {prefix:?}, got {keys:?}"
        );
        self.object(case, &keys[0]).await
    }
}

/// A SigV4 `Authorization` header whose credential scope names `service` — the routing key the
/// router claims a request by. Neither the key id nor the signature is a credential: this
/// harness turns request authentication off, so nothing here is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Every `<Key>` of a `ListObjectsV2` answer, read with an XML reader rather than by slicing
/// the body.
fn keys_of(document: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(document);
    let mut keys = Vec::new();
    let mut inside = false;
    let mut current = String::new();
    loop {
        match reader
            .read_event()
            .unwrap_or_else(|error| panic!("the listing must be well-formed XML: {error}"))
        {
            quick_xml::events::Event::Start(start) => {
                inside = start.name().as_ref() == b"Key";
                current.clear();
            }
            quick_xml::events::Event::Text(text) if inside => {
                current.push_str(&text.xml10_content().unwrap_or_default());
            }
            quick_xml::events::Event::End(end) => {
                if end.name().as_ref() == b"Key" {
                    keys.push(current.clone());
                }
                inside = false;
                current.clear();
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    keys
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
///
/// `pub(crate)` so a sibling suite that also needs a function to transform with — the
/// de-aggregation suite, whose sub-records are handed to one — packages it the same way
/// instead of copying this.
pub(crate) fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the function every row's processing names, wait until Lambda reports it `Active` and
/// answer the ARN Lambda minted for it — which is what the processor's `LambdaArn` parameter
/// carries.
async fn given_transform(emulator: &Emulator) -> String {
    let (status, created) = emulator
        .lambda(
            "POST",
            "/2015-03-31/functions",
            Some(json!({
                "FunctionName": FUNCTION,
                "Role": EXECUTION_ROLE,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": {
                    "ZipFile": aws_smithy_types::base64::encode(package(HANDLER_SOURCE)),
                },
            })),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction must succeed, got {status} {created}"
    );
    let function_arn = created
        .get("FunctionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the create must answer a FunctionArn, got {created}"))
        .to_string();

    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, reported) = emulator
            .lambda(
                "GET",
                &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration must succeed, got {status} {reported}"
        );
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return function_arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {FUNCTION} Active within {DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until Firehose reports `stream` `ACTIVE`, which is when it takes a put: the create
/// leaves it `CREATING` and the clock settles it.
async fn await_active(emulator: &Emulator, stream: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emulator
            .firehose_ok(
                stream,
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        if described["DeliveryStreamDescription"]["DeliveryStreamStatus"].as_str()
            == Some("ACTIVE")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {stream:?}: the stream must become ACTIVE within {DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The destination configuration of `row`: the one bucket, under this row's own prefixes, with
/// the data processing that names `function_arn` — beside the `AppendDelimiterToRecord`
/// processor, so the records of one delivered object are told apart by the new line that
/// processor adds.
fn destination(row: &Row, function_arn: &str) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "BucketARN": BUCKET_ARN,
        "Prefix": row.main_prefix(),
        "ErrorOutputPrefix": format!("{}/err/!{{firehose:error-output-type}}/", row.case),
        // The shortest buffering the hints admit, so a row waits for the emulator's own
        // delivery span and nothing longer.
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        "ProcessingConfiguration": {
            "Enabled": row.processing_enabled,
            "Processors": [
                {
                    "Type": "Lambda",
                    "Parameters": [
                        { "ParameterName": "LambdaArn", "ParameterValue": function_arn },
                    ],
                },
                { "Type": "AppendDelimiterToRecord" },
            ],
        },
    });
    if row.backs_up {
        let members = config.as_object_mut().expect("the configuration is an object");
        members.insert("S3BackupMode".to_string(), json!("Enabled"));
        members.insert(
            "S3BackupConfiguration".to_string(),
            json!({
                "RoleARN": ROLE_ARN,
                "BucketARN": BUCKET_ARN,
                "Prefix": row.backup_prefix(),
            }),
        );
    }
    config
}

#[tokio::test]
async fn the_lambda_processor_transforms_every_record_and_its_verdict_decides_where_it_lands() {
    let emulator = Emulator::new();
    emulator.create_bucket().await;
    let function_arn = given_transform(&emulator).await;
    let rows = rows();

    // Every row's stream is created first, so the one `CREATING` -> `ACTIVE` span covers the
    // whole table.
    for row in &rows {
        emulator
            .firehose_ok(
                row.case,
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": row.case,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration": destination(row, &function_arn),
                }),
            )
            .await;
    }
    for row in &rows {
        await_active(&emulator, row.case).await;
    }

    // Every row's records are put before any outcome is read, so the rows' delivery windows
    // run beside each other rather than one after the other.
    for row in &rows {
        let entries: Vec<Value> = row
            .puts
            .iter()
            .map(|put| json!({ "Data": aws_smithy_types::base64::encode(put.source()) }))
            .collect();
        let answered = emulator
            .firehose_ok(
                row.case,
                "PutRecordBatch",
                json!({ "DeliveryStreamName": row.case, "Records": entries }),
            )
            .await;
        assert_eq!(
            answered["FailedPutCount"].as_i64(),
            Some(0),
            "case {:?}: every record of the batch must be accepted, got {answered}",
            row.case
        );
    }

    for row in &rows {
        let case = row.case;

        // What the transformation left to deliver, and nothing else: the main prefix never
        // holds a record the function dropped, marked failed or left unaccounted for, and
        // never the bytes it was asked to replace.
        let expected_main = row.expected_main();
        if expected_main.is_empty() {
            // The delivery still HAPPENED — the buffer was handed over — it just left no
            // object behind, which is what the records being gone from the buffer says.
            emulator
                .wait_until(case, "the delivered buffer is handed over", || {
                    emulator.buffered(case) == 0
                })
                .await;
        } else {
            let delivered = emulator.only_object_under(case, &row.main_prefix()).await;
            assert_eq!(
                String::from_utf8_lossy(&delivered),
                String::from_utf8_lossy(&expected_main),
                "case {case:?}: the main prefix holds what the transformation returned, got {:?}",
                String::from_utf8_lossy(&delivered)
            );
        }

        // The records the transformation did not place, under the destination's own error
        // output prefix, each carrying its SOURCE bytes and the code that says what the
        // answer did with it.
        let expected_failures = row.expected_failures();
        if !expected_failures.is_empty() {
            let keys = emulator.wait_for_keys(case, &row.error_prefix()).await;
            let mut documents = Vec::new();
            for key in &keys {
                let body = emulator.object(case, key).await;
                for line in String::from_utf8_lossy(&body).lines() {
                    documents.push(serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
                        panic!("case {case:?}: every failed line is a JSON document, got {error}")
                    }));
                }
            }
            assert_eq!(
                documents.len(),
                expected_failures.len(),
                "case {case:?}: one document per record the transformation did not place, got \
                 {documents:?}"
            );
            for (source, code) in &expected_failures {
                let document = documents
                    .iter()
                    .find(|document| {
                        document["rawData"]
                            .as_str()
                            .and_then(|raw| aws_smithy_types::base64::decode(raw).ok())
                            .is_some_and(|decoded| decoded == source.as_bytes())
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the error output carries {source:?} with its own \
                             bytes, got {documents:?}"
                        )
                    });
                assert_eq!(
                    document["errorCode"].as_str(),
                    Some(*code),
                    "case {case:?}: the document of {source:?} names how the transformation \
                     failed, got {document}"
                );
                assert!(
                    document["errorMessage"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty()),
                    "case {case:?}: the document of {source:?} states what failed, got {document}"
                );
                assert_eq!(
                    document["lambdaARN"].as_str(),
                    Some(function_arn.as_str()),
                    "case {case:?}: the document of {source:?} names the function that judged \
                     it, got {document}"
                );
                assert!(
                    document["arrivalTimestamp"].as_u64().unwrap_or_default() > 0
                        && document["attemptEndingTimestamp"]
                            .as_u64()
                            .unwrap_or_default()
                            > 0,
                    "case {case:?}: the document of {source:?} says when the record arrived and \
                     when the attempt ended, got {document}"
                );
            }
        }

        // The backup, where the destination asked for one: the SOURCE records, which the main
        // prefix is now the one place they are NOT.
        if row.backs_up {
            let backed_up = emulator.only_object_under(case, &row.backup_prefix()).await;
            let backed_up = String::from_utf8_lossy(&backed_up).to_string();
            for put in &row.puts {
                let source = put.source();
                assert!(
                    backed_up.contains(&source),
                    "case {case:?}: the backup holds the source record {source:?}, got \
                     {backed_up:?}"
                );
                let transformed = put.shape.applied(&put.body);
                assert!(
                    !backed_up.contains(&transformed),
                    "case {case:?}: the backup holds the record as the producer sent it, not the \
                     transformed {transformed:?}, got {backed_up:?}"
                );
            }
        }
    }

    // Now that every row's expected objects are there, what must NOT have been written: a row
    // whose transformation left nothing to deliver has no delivered object, one whose records
    // the transformation all placed has no error output, and a row that configured no backup
    // has no backup object.
    for row in &rows {
        let case = row.case;
        if row.expected_main().is_empty() {
            let keys = emulator.keys_under(case, &row.main_prefix()).await;
            assert!(
                keys.is_empty(),
                "case {case:?}: a buffer the transformation left nothing of is delivered \
                 nowhere, got {keys:?}"
            );
        }
        if row.expected_failures().is_empty() {
            let keys = emulator
                .keys_under(case, &format!("{case}/err/"))
                .await;
            assert!(
                keys.is_empty(),
                "case {case:?}: a buffer the transformation placed every record of has no error \
                 output, got {keys:?}"
            );
        }
        if !row.backs_up {
            let keys = emulator.keys_under(case, &row.backup_prefix()).await;
            assert!(
                keys.is_empty(),
                "case {case:?}: a destination that backs nothing up writes no backup object, \
                 got {keys:?}"
            );
        }
    }
}
