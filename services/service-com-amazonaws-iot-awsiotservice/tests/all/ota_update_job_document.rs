// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the JOB DOCUMENT an OTA update deploys: `CreateOTAUpdate`
//! models no `document` member, so the document the update's AWS IoT job hands its
//! targets is one the service GENERATES from the request.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `POST /otaUpdates/{otaUpdateId}` and then `GET /jobs/{jobId}/job-document`
//! for the `awsIotJobId` the create reported, signed for the `iot` signing name — and
//! also reads the SHARED job-execution store the IoT Jobs data plane serves, which is
//! where the copy a device is handed lives.
//!
//! The rules under test:
//!
//! * the deployed job's document is a single `afr_ota` object stating, per requested
//!   file and in request order, that file's name as the path the device writes, its id
//!   (the id it has in its stream, or its position in `files`), its `fileType` and the
//!   download location its `fileLocation.s3Location` names, beside the transfer
//!   `protocols` the request asked for and the `streamname` its files are served from;
//! * what the model documents as not sent to a device as part of the job document — a
//!   file's `attributes`, the update's `additionalParameters` — is not in it;
//! * the code-signing material a file carries is stated under the keys a device reads
//!   it by;
//! * both views `GetJobDocument` selects between report that document (an OTA update
//!   names no substitution parameters), and every targeted device's execution carries
//!   the same copy;
//! * a request whose files would generate a document past what `JobDocument` may hold
//!   is refused and records nothing.
//!
//! The tables span what the rules generalise over — how many files one update carries
//! and in which location form, which protocols it names, whether a pre-signed lifetime
//! is asked for, which code-signing material a file supplies, and how large the
//! generated document grows — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::ota_updates::{
    DEFAULT_PRESIGNED_URL_EXPIRY_SEC, MAX_PRESIGNED_URL_EXPIRY_SEC, MIN_PRESIGNED_URL_EXPIRY_SEC,
};
use service_com_amazonaws_iot_awsiotservice::state::{
    MAX_JOB_DOCUMENT_BYTES, OTA_JOB_DOCUMENT_ROOT, OTA_UPDATE_JOB_ID_PREFIX,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::JobExecutionStore;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for; the download locations the document states are
/// scoped to it.
const REGION: &str = "us-east-1";
/// An IAM role ARN of the shape `CreateOTAUpdate$roleArn` is modelled with (len
/// 20..2048); the operation neither assumes nor resolves it.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ota-update-role";
/// The bucket the firmware images of these cases live in.
const BUCKET: &str = "firmware-images";

/// One address this emulator is served at, as the core publishes it from its data port.
/// The port is the operating system's in a real run; what matters here is that a download
/// location the generated document states is an address on THIS endpoint, which a device
/// can then fetch.
const SERVED_AT: &str = "https://127.0.0.1:41287";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service, and how the service picks
/// the region its locations are scoped to.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }
}

/// The emulator under test: ONE shared engine with the IoT service and the Amazon S3 the
/// firmware images live in mounted on it — the shape `core/router/src/lib.rs` mounts them
/// in — served at [`SERVED_AT`]. The engine handle is kept so a case can read the SHARED
/// job-execution store the control plane writes the device-facing copy of the document
/// into.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.set_url_base(
            EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"),
        );
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_ota_update(&self, ota_update_id: &str, request: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/otaUpdates/{ota_update_id}"),
            Some(request),
        )
        .await
    }

    async fn get_ota_update(&self, ota_update_id: &str) -> Reply {
        self.call("GET", &format!("/otaUpdates/{ota_update_id}"), None)
            .await
    }

    /// `GetJobDocument` with `beforeSubstitution` left out, which is the modelled
    /// default (`false`).
    async fn job_document(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}/job-document"), None)
            .await
    }

    /// `GetJobDocument` with `beforeSubstitution` spelled out.
    async fn job_document_before(&self, job_id: &str, before_substitution: bool) -> Reply {
        self.call(
            "GET",
            &format!("/jobs/{job_id}/job-document?beforeSubstitution={before_substitution}"),
            None,
        )
        .await
    }

    /// The document the SHARED store holds for `thing_name`'s execution of a job: what
    /// the device is handed.
    fn execution_document(&self, thing_name: &str, job_id: &str) -> Option<String> {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .executions_for(thing_name)
            .iter()
            .find(|record| record.job_id == job_id)
            .and_then(|record| record.job_document.clone())
    }

    /// One Amazon S3 request through the same router, signed for the `s3` scope so the
    /// Amazon S3 mounted beside the IoT service claims it.
    async fn s3(&self, method: &str, path: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/s3/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }

    /// `PUT /<bucket>` — `CreateBucket` — so a staged firmware image has a bucket to live
    /// in.
    async fn given_bucket(&self) {
        let (status, body) = self.s3("PUT", &format!("/{BUCKET}"), Vec::new()).await;
        assert_eq!(
            status,
            200,
            "the firmware bucket must be creatable, got {status} with {:?}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject` — staging `image` as the firmware a file names.
    async fn given_image(&self, key: &str, image: &[u8]) {
        let (status, body) = self.s3("PUT", &object_path(key), image.to_vec()).await;
        assert_eq!(
            status,
            200,
            "the firmware image {key} must be stageable, got {status} with {:?}",
            String::from_utf8_lossy(&body)
        );
    }

    /// A GET of a download location the document handed a device, answered with its status
    /// and the bytes it served.
    ///
    /// The location is an absolute URL, so what is fetched is the ADDRESS it names: its
    /// path and query go back through the very router that minted it — which is what makes
    /// it a location a device can follow — and the credential scope the pre-signed query
    /// carries is what names the service that answers it. No `Authorization` header is
    /// sent: a pre-signed URL carries its own credentials.
    async fn fetch(&self, location: &str) -> (u16, Vec<u8>) {
        let request = http::Request::builder()
            .method("GET")
            .uri(location)
            .header("host", url_host(location))
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }
}

/// The `host[:port]` of an absolute URL — the `Host` header a client following it sends.
fn url_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The path of an absolute URL, without its query: the resource the URL addresses.
fn url_path(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let from_path = after_scheme
        .find('/')
        .map_or("/", |start| &after_scheme[start..]);
    from_path.split(['?', '#']).next().unwrap_or("/")
}

/// The request path an object of [`BUCKET`] is addressed by: the bucket and the key, with
/// the key's own `/` separators kept and every other character outside RFC 3986's
/// unreserved set percent-encoded.
///
/// Hand-written for the same reason the sibling suites' encoders are: it is a few lines of
/// test-only encoding, not worth a dependency entry in the workspace lock file.
fn object_path(key: &str) -> String {
    let encoded: String = key
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    format!("/{BUCKET}/{encoded}")
}

/// The thing ARN a target entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The id of the AWS IoT job an OTA update deploys through.
fn job_id_of(ota_update_id: &str) -> String {
    format!("{OTA_UPDATE_JOB_ID_PREFIX}{ota_update_id}")
}

/// A file located in S3, the form `OTAUpdateFile$fileLocation.s3Location` names.
fn s3_file(file_name: &str, key: &str) -> Value {
    json!({
        "fileName": file_name,
        "fileType": 0,
        "fileVersion": "1.0.0",
        "fileLocation": {"s3Location": {"bucket": BUCKET, "key": key}}
    })
}

/// A file located in an IoT stream, the other form `fileLocation` names.
fn stream_file(file_name: &str, stream_id: &str, file_id: i64) -> Value {
    json!({
        "fileName": file_name,
        "fileLocation": {"stream": {"streamId": stream_id, "fileId": file_id}}
    })
}

/// The `afr_ota` object of a document, or a panic naming what was reported instead.
fn afr_ota(case: &str, document: &str) -> Value {
    let parsed: Value = serde_json::from_str(document)
        .unwrap_or_else(|error| panic!("case {case:?}: expected JSON, got {document:?}: {error}"));
    let object = parsed.as_object().unwrap_or_else(|| {
        panic!("case {case:?}: expected a JSON object, got {parsed:?}");
    });
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<&str>>(),
        vec![OTA_JOB_DOCUMENT_ROOT],
        "case {case:?}: expected the document to hang off one {OTA_JOB_DOCUMENT_ROOT} \
         member, got {parsed:?}"
    );
    object[OTA_JOB_DOCUMENT_ROOT].clone()
}

/// One row of the document table.
struct DocumentCase {
    case: &'static str,
    ota_update_id: &'static str,
    /// The things the update targets, each as a `thing/…` ARN.
    targets: &'static [&'static str],
    /// The `files` member, always at least one (modelled `len 1..`).
    files: Vec<Value>,
    /// The `protocols` member, when the row names any. The catalog they are drawn from
    /// is the modelled `Protocol` enum, `MQTT | HTTP`
    /// (`smithy_query(query="shape",shape="Protocols,Protocol")`), with `Protocols`
    /// modelled `len 1..2`.
    protocols: Option<Vec<&'static str>>,
    /// `awsJobPresignedUrlConfig.expiresInSec`, when the row asks for one. The values
    /// come from the member's own documentation: "Valid values are 60 - 3600, the
    /// default value is 1800 seconds".
    expires_in_sec: Option<i64>,
    /// The `additionalParameters` the row carries, "which are name-value pairs. They
    /// won't be sent to devices as a part of the Job document".
    additional_parameters: Option<Value>,
    /// Text the generated document must NOT carry: what the model documents as never
    /// sent to a device as part of the job document.
    withheld: Vec<&'static str>,
}

impl DocumentCase {
    /// The request body this row sends.
    fn request(&self) -> Value {
        let mut request = json!({
            "targets": self.targets.iter().map(|name| thing_arn(name)).collect::<Vec<String>>(),
            "files": self.files.clone(),
            "roleArn": ROLE_ARN,
        });
        if let Some(protocols) = &self.protocols {
            request["protocols"] = json!(protocols);
        }
        if let Some(seconds) = self.expires_in_sec {
            request["awsJobPresignedUrlConfig"] = json!({"expiresInSec": seconds});
        }
        if let Some(parameters) = &self.additional_parameters {
            request["additionalParameters"] = parameters.clone();
        }
        request
    }
}

/// RULE: the job an OTA update deploys carries a document the service generates from
/// the request — one `afr_ota` object naming the protocols and stream the update
/// transfers over and, per requested file, that file's path, id, type and download
/// location — and that document is what the job's reader and the targeted devices' own
/// executions report.
#[tokio::test]
async fn an_ota_update_deploys_a_job_document_generated_from_the_request() {
    let emulator = Emulator::new();

    let cases: Vec<DocumentCase> = vec![
        DocumentCase {
            case: "one S3 file, no protocols and no pre-signed lifetime",
            ota_update_id: "ota-doc-minimal",
            targets: &["doc-sensor-a"],
            files: vec![s3_file("firmware.bin", "firmware.bin")],
            protocols: None,
            expires_in_sec: None,
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "an S3 object under a prefix, over HTTP, at the longest pre-signed lifetime",
            ota_update_id: "ota-doc-http",
            targets: &["doc-sensor-b", "doc-sensor-c"],
            files: vec![s3_file("image.bin", "firmware/2.0.0/image.bin")],
            protocols: Some(vec!["HTTP"]),
            expires_in_sec: Some(3600),
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "a versioned S3 object at the shortest pre-signed lifetime",
            ota_update_id: "ota-doc-versioned",
            targets: &["doc-sensor-d"],
            files: vec![json!({
                "fileName": "firmware.bin",
                "fileLocation": {
                    "s3Location": {"bucket": BUCKET, "key": "firmware.bin", "version": "objver-7"}
                }
            })],
            protocols: Some(vec!["HTTP"]),
            expires_in_sec: Some(60),
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "one streamed file over MQTT",
            ota_update_id: "ota-doc-mqtt",
            targets: &["doc-sensor-e"],
            files: vec![stream_file("firmware.bin", "stream-one", 0)],
            protocols: Some(vec!["MQTT"]),
            expires_in_sec: None,
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "three files, both protocols, a stream beside two S3 objects",
            ota_update_id: "ota-doc-mixed",
            targets: &["doc-sensor-f", "doc-sensor-g"],
            files: vec![
                s3_file("firmware.bin", "firmware.bin"),
                stream_file("bootloader.bin", "stream-two", 7),
                s3_file("config.json", "configs/config.json"),
            ],
            protocols: Some(vec!["HTTP", "MQTT"]),
            expires_in_sec: Some(1800),
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "what is documented as never sent to a device stays out",
            ota_update_id: "ota-doc-withheld",
            targets: &["doc-sensor-h"],
            files: vec![json!({
                "fileName": "firmware.bin",
                "fileType": 255,
                "fileLocation": {"s3Location": {"bucket": BUCKET, "key": "firmware.bin"}},
                "attributes": {"board": "rev-c"}
            })],
            protocols: Some(vec!["HTTP"]),
            expires_in_sec: None,
            additional_parameters: Some(json!({"channel": "beta"})),
            withheld: vec!["board", "rev-c", "channel", "beta"],
        },
        DocumentCase {
            case: "an S3 location naming no key states no download location",
            ota_update_id: "ota-doc-partial-location",
            targets: &["doc-sensor-i"],
            files: vec![json!({
                "fileName": "firmware.bin",
                "fileLocation": {"s3Location": {"bucket": BUCKET}}
            })],
            protocols: Some(vec!["HTTP"]),
            expires_in_sec: None,
            additional_parameters: None,
            withheld: Vec::new(),
        },
        DocumentCase {
            case: "a file the request named no name for",
            ota_update_id: "ota-doc-unnamed-file",
            targets: &["doc-sensor-j"],
            files: vec![json!({
                "fileLocation": {"s3Location": {"bucket": BUCKET, "key": "unnamed.bin"}}
            })],
            protocols: None,
            expires_in_sec: None,
            additional_parameters: None,
            withheld: Vec::new(),
        },
    ];

    for case in &cases {
        for thing_name in case.targets {
            assert_eq!(
                emulator.create_thing(thing_name).await.status,
                200,
                "case {:?}: the target device must be registrable",
                case.case
            );
        }

        let request = case.request();
        let created = emulator.create_ota_update(case.ota_update_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected 200, got {} {:?}",
            case.case, created.status, created.body
        );
        let job_id = created.text("awsIotJobId").unwrap_or_default();
        assert_eq!(
            job_id,
            job_id_of(case.ota_update_id),
            "case {:?}: expected the job id derived from the update id, got {:?}",
            case.case,
            created.body
        );

        // The deployed job HAS a document, and it is the generated one.
        let reply = emulator.job_document(&job_id).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected 200 from the job's document reader, got {} {:?}",
            case.case, reply.status, reply.body
        );
        let document = reply.text("document").unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the deployed job to report a document, got {:?}",
                case.case, reply.body
            )
        });
        assert!(
            !document.is_empty() && document.len() <= MAX_JOB_DOCUMENT_BYTES,
            "case {:?}: expected a non-empty document of at most {MAX_JOB_DOCUMENT_BYTES} \
             bytes, got {} bytes",
            case.case,
            document.len()
        );

        let update = afr_ota(case.case, &document);
        assert_eq!(
            update.get("protocols"),
            request.get("protocols"),
            "case {:?}: expected the protocols the request named, got {update:?}",
            case.case
        );

        // The stream the files are served from is the one the first streamed file
        // names; an update whose files are all in S3 names none.
        let expected_stream = case.files.iter().find_map(|file| {
            file.pointer("/fileLocation/stream/streamId")
                .and_then(Value::as_str)
        });
        assert_eq!(
            update.get("streamname").and_then(Value::as_str),
            expected_stream,
            "case {:?}: expected the stream the files are served from, got {update:?}",
            case.case
        );

        let entries = update
            .get("files")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {:?}: expected a files array, got {update:?}", case.case));
        assert_eq!(
            entries.len(),
            case.files.len(),
            "case {:?}: expected one entry per requested file, got {entries:?}",
            case.case
        );

        for (index, (requested, entry)) in case.files.iter().zip(entries).enumerate() {
            let where_ = format!("{} / files[{index}]", case.case);

            assert_eq!(
                entry.get("filepath"),
                requested.get("fileName"),
                "case {where_:?}: expected the requested file name as the device's path, \
                 got {entry:?}"
            );
            assert_eq!(
                entry.get("fileType"),
                requested.get("fileType"),
                "case {where_:?}: expected the requested fileType, got {entry:?}"
            );

            // A streamed file is addressed by its id IN the stream; an S3 object has no
            // such id, so the entry is told apart by the file's position in `files`.
            let expected_id = requested
                .pointer("/fileLocation/stream/fileId")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| i64::try_from(index).expect("a file index fits an i64"));
            assert_eq!(
                entry.get("fileid").and_then(Value::as_i64),
                Some(expected_id),
                "case {where_:?}: expected fileid {expected_id}, got {entry:?}"
            );

            let bucket = requested
                .pointer("/fileLocation/s3Location/bucket")
                .and_then(Value::as_str);
            let key = requested
                .pointer("/fileLocation/s3Location/key")
                .and_then(Value::as_str);
            let url = entry.get("update_data_url").and_then(Value::as_str);
            match (bucket, key) {
                (Some(bucket), Some(key)) => {
                    let url = url.unwrap_or_else(|| {
                        panic!(
                            "case {where_:?}: expected the download location of \
                             {bucket}/{key}, got {entry:?}"
                        )
                    });
                    // The location is an address a device DIALS, so it names the object
                    // on the endpoint THIS emulator is reached on — the bucket and key in
                    // the path, the way a path-style Amazon S3 URL carries them — and no
                    // host AWS serves its own buckets at.
                    assert!(
                        url.starts_with(&format!("{SERVED_AT}/{bucket}/{key}?")),
                        "case {where_:?}: expected the object's address on this emulator \
                         ({SERVED_AT}/{bucket}/{key}), got {url:?}"
                    );
                    assert!(
                        !url.contains("amazonaws.com"),
                        "case {where_:?}: a download location must name no AWS host, got \
                         {url:?}"
                    );
                    // And it is PRE-SIGNED, which is what `awsJobPresignedUrlConfig` is
                    // about: a signature, the credential scope of the update's own
                    // Region, and the lifetime asked for — the member's documented
                    // default when none was.
                    let expires = case
                        .expires_in_sec
                        .and_then(|seconds| u64::try_from(seconds).ok())
                        .unwrap_or(DEFAULT_PRESIGNED_URL_EXPIRY_SEC);
                    for expected in [
                        "X-Amz-Algorithm=AWS4-HMAC-SHA256".to_string(),
                        "X-Amz-Signature=".to_string(),
                        format!("%2F{REGION}%2Fs3%2Faws4_request"),
                        format!("X-Amz-Expires={expires}"),
                    ] {
                        assert!(
                            url.contains(&expected),
                            "case {where_:?}: expected the pre-signed location to carry \
                             {expected:?}, got {url:?}"
                        );
                    }
                    let version = requested
                        .pointer("/fileLocation/s3Location/version")
                        .and_then(Value::as_str);
                    assert_eq!(
                        url.contains(&format!("versionId={}", version.unwrap_or_default())),
                        version.is_some(),
                        "case {where_:?}: the object version is stated only when the \
                         request pinned one, got {url:?}"
                    );
                }
                _ => assert!(
                    url.is_none(),
                    "case {where_:?}: a location naming no bucket and key is no download \
                     location, got {url:?}"
                ),
            }
        }

        for withheld in &case.withheld {
            assert!(
                !document.contains(withheld),
                "case {:?}: expected {withheld:?} to stay out of the document, got \
                 {document:?}",
                case.case
            );
        }

        // An OTA update names no substitution parameters, so both views of its job's
        // document are the document as generated.
        for before_substitution in [false, true] {
            let view = emulator
                .job_document_before(&job_id, before_substitution)
                .await;
            assert_eq!(
                view.text("document").as_deref(),
                Some(document.as_str()),
                "case {:?} (beforeSubstitution={before_substitution}): expected the \
                 generated document, got {:?}",
                case.case,
                view.body
            );
        }

        // ROUND TRIP through the data plane: every targeted device's execution carries
        // the same copy, which is what a device agent downloads from.
        for thing_name in case.targets {
            assert_eq!(
                emulator.execution_document(thing_name, &job_id).as_deref(),
                Some(document.as_str()),
                "case {:?}: expected {thing_name}'s execution to carry the job's \
                 document, got {:?}",
                case.case,
                emulator.execution_document(thing_name, &job_id)
            );
        }
    }
}

/// One row of the download-location table.
struct DownloadCase {
    case: &'static str,
    ota_update_id: &'static str,
    /// The device the update targets.
    thing: &'static str,
    /// The `s3Location.key` the staged firmware image lives at.
    key: &'static str,
    /// The image staged at that key — the bytes a device following the location must get
    /// back.
    image: &'static [u8],
    /// `awsJobPresignedUrlConfig.expiresInSec`, when the row asks for one.
    expires_in_sec: Option<i64>,
    /// How long the published location must then stay valid, written out here rather
    /// than recomputed from the row: the member documents "Valid values are 60 - 3600,
    /// the default value is 1800 seconds", so a request naming none is 1800 and one
    /// naming a value outside those bounds is held at the nearer bound.
    expected_expires_sec: u64,
}

/// RULE: the download location a generated OTA job document states for a file is an
/// address THIS emulator answers on — the file's Amazon S3 object, pre-signed against the
/// endpoint the emulator is reached at — so a device that follows it as published is
/// served the very bytes staged at that bucket and key.
///
/// The rows span what the rule generalises over: where the object sits in the bucket (at
/// its root, under a prefix, under a key carrying characters a URL must encode), how long
/// the request asked the location to stay valid (the documented bounds, a value on either
/// side of them, a value no lifetime at all, and the default for a request that asked for
/// nothing), and what the image is (text and bytes no text encoding survives, so the
/// equality is over the bytes).
#[tokio::test]
async fn the_download_location_a_job_document_states_serves_the_staged_image() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;

    let cases: &[DownloadCase] = &[
        DownloadCase {
            case: "an object at the bucket's root, no lifetime asked for",
            ota_update_id: "ota-fetch-root",
            thing: "fetch-sensor-a",
            key: "firmware.bin",
            image: b"the firmware image at the bucket root",
            expires_in_sec: None,
            expected_expires_sec: DEFAULT_PRESIGNED_URL_EXPIRY_SEC,
        },
        DownloadCase {
            case: "an object under a prefix, at the shortest documented lifetime",
            ota_update_id: "ota-fetch-prefixed",
            thing: "fetch-sensor-b",
            key: "firmware/2.0.0/image.bin",
            image: b"the firmware image under a prefix",
            expires_in_sec: Some(60),
            expected_expires_sec: 60,
        },
        DownloadCase {
            case: "a key carrying characters a URL must encode, at the longest documented \
                   lifetime",
            ota_update_id: "ota-fetch-encoded",
            thing: "fetch-sensor-c",
            key: "firmware/release 2.0+rc1/image.bin",
            image: b"the firmware image behind an encoded key",
            expires_in_sec: Some(3600),
            expected_expires_sec: 3600,
        },
        DownloadCase {
            case: "an image of bytes no text encoding survives",
            ota_update_id: "ota-fetch-binary",
            thing: "fetch-sensor-d",
            key: "firmware/binary.bin",
            image: &[0x00, 0xff, 0x10, 0x80, 0x7f, 0x00, 0xfe],
            expires_in_sec: Some(1800),
            expected_expires_sec: 1800,
        },
        DownloadCase {
            case: "a lifetime under the published minimum is held at the minimum",
            ota_update_id: "ota-fetch-under",
            thing: "fetch-sensor-e",
            key: "firmware/under.bin",
            image: b"the firmware image of a too-short lifetime",
            expires_in_sec: Some(1),
            expected_expires_sec: MIN_PRESIGNED_URL_EXPIRY_SEC,
        },
        DownloadCase {
            case: "a lifetime that is no lifetime at all is held at the minimum",
            ota_update_id: "ota-fetch-negative",
            thing: "fetch-sensor-f",
            key: "firmware/negative.bin",
            image: b"the firmware image of a negative lifetime",
            expires_in_sec: Some(-1),
            expected_expires_sec: MIN_PRESIGNED_URL_EXPIRY_SEC,
        },
        DownloadCase {
            case: "a lifetime over the published maximum is held at the maximum",
            ota_update_id: "ota-fetch-over",
            thing: "fetch-sensor-g",
            key: "firmware/over.bin",
            image: b"the firmware image of a too-long lifetime",
            expires_in_sec: Some(86_400),
            expected_expires_sec: MAX_PRESIGNED_URL_EXPIRY_SEC,
        },
    ];

    for case in cases {
        emulator.given_image(case.key, case.image).await;
        assert_eq!(
            emulator.create_thing(case.thing).await.status,
            200,
            "case {:?}: the target device must be registrable",
            case.case
        );

        let mut request = json!({
            "targets": [thing_arn(case.thing)],
            "files": [s3_file("firmware.bin", case.key)],
            "protocols": ["HTTP"],
            "roleArn": ROLE_ARN,
        });
        if let Some(seconds) = case.expires_in_sec {
            request["awsJobPresignedUrlConfig"] = json!({"expiresInSec": seconds});
        }
        let created = emulator.create_ota_update(case.ota_update_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected 200 from the create, got {} {:?}",
            case.case, created.status, created.body
        );

        let job_id = job_id_of(case.ota_update_id);
        let document = emulator
            .job_document(&job_id)
            .await
            .text("document")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the deployed job to report a document",
                    case.case
                )
            });
        let update = afr_ota(case.case, &document);
        let location = update
            .pointer("/files/0/update_data_url")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a download location, got {update:?}",
                    case.case
                )
            })
            .to_string();

        // Published for a device to DIAL, so it is an address of this emulator.
        assert_eq!(
            url_host(&location),
            url_host(SERVED_AT),
            "case {:?}: expected the location to name this emulator's own host, got \
             {location:?}",
            case.case
        );

        // And it stays valid for the lifetime the member documents for this row.
        let expires = format!("X-Amz-Expires={}", case.expected_expires_sec);
        assert!(
            location.contains(&expires),
            "case {:?}: expected the location to carry {expires:?}, got {location:?}",
            case.case
        );

        // Followed as published, it serves the object staged at that bucket and key.
        let (status, served) = emulator.fetch(&location).await;
        assert_eq!(
            status,
            200,
            "case {:?}: expected the published location {location:?} to serve the image, \
             got {status} with {:?}",
            case.case,
            String::from_utf8_lossy(&served)
        );
        assert_eq!(
            served.as_slice(),
            case.image,
            "case {:?}: expected the staged image back from the published location, got \
             {served:?}",
            case.case
        );

        // The same object read with `GetObject`, no location involved, answers the same
        // bytes: the location addresses a real object of the emulator's own Amazon S3,
        // under the bucket the request named.
        let object_path = url_path(&location);
        assert!(
            object_path.starts_with(&format!("/{BUCKET}/")),
            "case {:?}: expected the location to address an object of {BUCKET}, got \
             {object_path:?}",
            case.case
        );
        let (status, directly) = emulator.s3("GET", object_path, Vec::new()).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the staged object to be readable directly, got {status}",
            case.case
        );
        assert_eq!(
            directly, served,
            "case {:?}: expected the location and Amazon S3 to serve the same bytes",
            case.case
        );
    }
}

/// One row of the code-signing table.
struct SigningCase {
    case: &'static str,
    ota_update_id: &'static str,
    /// The `codeSigning` member of the requested file.
    code_signing: Value,
    /// The `certfile` the entry must state, if any.
    certfile: Option<&'static str>,
    /// The key the signature must be stated under, and its value, if any.
    signature: Option<(&'static str, String)>,
}

/// RULE: the code-signing material a requested file carries is stated in that file's
/// document entry under the keys a device verifies the image by — the signing
/// certificate's path on the device, and a supplied signature under the key naming the
/// algorithms it was made with.
#[tokio::test]
async fn the_code_signing_material_a_file_carries_is_in_its_document_entry() {
    let emulator = Emulator::new();

    // The signature bytes travel as a base64 blob (`CodeSigningSignature$inlineDocument`
    // is "A base64 encoded binary representation of the code signing signature").
    let signature_bytes = b"a signature over the firmware image".to_vec();
    let signature = aws_smithy_types::base64::encode(signature_bytes.as_slice());

    let cases: Vec<SigningCase> = vec![
        SigningCase {
            case: "a custom signature with its algorithms and a certificate chain",
            ota_update_id: "ota-sign-custom",
            code_signing: json!({
                "customCodeSigning": {
                    "signature": {"inlineDocument": signature.clone()},
                    "certificateChain": {"certificateName": "signer-cert.pem"},
                    // FreeRTOS's own example pair, named in the member's documentation.
                    "hashAlgorithm": "SHA256",
                    "signatureAlgorithm": "ECDSA"
                }
            }),
            certfile: Some("signer-cert.pem"),
            signature: Some(("sig-sha256-ecdsa", signature.clone())),
        },
        SigningCase {
            case: "another algorithm pair names another signature key",
            ota_update_id: "ota-sign-rsa",
            code_signing: json!({
                "customCodeSigning": {
                    "signature": {"inlineDocument": signature.clone()},
                    "hashAlgorithm": "SHA1",
                    "signatureAlgorithm": "RSA"
                }
            }),
            certfile: None,
            signature: Some(("sig-sha1-rsa", signature.clone())),
        },
        SigningCase {
            case: "a signing profile naming the certificate's path on the device",
            ota_update_id: "ota-sign-profile",
            code_signing: json!({
                "startSigningJobParameter": {
                    "signingProfileName": "firmware-profile",
                    "signingProfileParameter": {
                        "platform": "AmazonFreeRTOS-Default",
                        "certificatePathOnDevice": "/device/certs/signer.crt"
                    }
                }
            }),
            certfile: Some("/device/certs/signer.crt"),
            signature: None,
        },
        SigningCase {
            case: "a signature whose algorithms the request did not name is not stated",
            ota_update_id: "ota-sign-unlabelled",
            code_signing: json!({
                "customCodeSigning": {
                    "signature": {"inlineDocument": signature.clone()},
                    "certificateChain": {"certificateName": "signer-cert.pem"}
                }
            }),
            certfile: Some("signer-cert.pem"),
            signature: None,
        },
        SigningCase {
            case: "a signing job done elsewhere puts no signature in the document",
            ota_update_id: "ota-sign-signer-job",
            code_signing: json!({"awsSignerJobId": "signer-job-1"}),
            certfile: None,
            signature: None,
        },
    ];

    for case in &cases {
        let mut file = s3_file("firmware.bin", "firmware.bin");
        file["codeSigning"] = case.code_signing.clone();
        let request = json!({
            "targets": [thing_arn("doc-signing-sensor")],
            "files": [file],
            "protocols": ["HTTP"],
            "roleArn": ROLE_ARN,
        });

        let created = emulator.create_ota_update(case.ota_update_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected 200, got {} {:?}",
            case.case, created.status, created.body
        );

        let job_id = job_id_of(case.ota_update_id);
        let reply = emulator.job_document(&job_id).await;
        let document = reply.text("document").unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the deployed job to report a document, got {:?}",
                case.case, reply.body
            )
        });
        let update = afr_ota(case.case, &document);
        let entry = update
            .pointer("/files/0")
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: expected one file entry, got {update:?}", case.case));

        assert_eq!(
            entry.get("certfile").and_then(Value::as_str),
            case.certfile,
            "case {:?}: expected the signing certificate the request named, got {entry:?}",
            case.case
        );
        match &case.signature {
            Some((key, expected)) => assert_eq!(
                entry.get(*key).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {:?}: expected the supplied signature under {key:?}, got {entry:?}",
                case.case
            ),
            None => {
                let signature_keys: Vec<&String> = entry
                    .as_object()
                    .map(|entry| entry.keys().filter(|key| key.starts_with("sig-")).collect())
                    .unwrap_or_default();
                assert!(
                    signature_keys.is_empty(),
                    "case {:?}: expected no signature to be stated, got {signature_keys:?}",
                    case.case
                );
            }
        }
    }
}

/// RULE: `JobDocument` is bounded (`len 0..32768`), so a request whose files would
/// generate a document past that bound is refused — no update, no job and no document
/// a reader could not report — while one that fits is deployed.
#[tokio::test]
async fn a_generated_document_past_what_a_job_document_may_hold_is_refused() {
    let emulator = Emulator::new();

    // `S3Key` is modelled with a minimum of 1 and NO maximum, so the key alone decides
    // how large the generated document grows.
    let fitting_key = format!("firmware/{}.bin", "k".repeat(30_000));
    let fitting = emulator
        .create_ota_update(
            "ota-doc-large",
            &json!({
                "targets": [thing_arn("doc-sensor-large")],
                "files": [s3_file("firmware.bin", &fitting_key)],
                "protocols": ["HTTP"],
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    assert_eq!(
        fitting.status, 200,
        "case \"a document that still fits\": expected 200, got {} {:?}",
        fitting.status, fitting.body
    );
    let document = emulator
        .job_document(&job_id_of("ota-doc-large"))
        .await
        .text("document")
        .unwrap_or_default();
    assert!(
        document.contains(&fitting_key) && document.len() <= MAX_JOB_DOCUMENT_BYTES,
        "case \"a document that still fits\": expected the requested key inside a \
         document of at most {MAX_JOB_DOCUMENT_BYTES} bytes, got {} bytes",
        document.len()
    );

    let oversized_key = format!("firmware/{}.bin", "k".repeat(MAX_JOB_DOCUMENT_BYTES));
    let refused = emulator
        .create_ota_update(
            "ota-doc-oversized",
            &json!({
                "targets": [thing_arn("doc-sensor-oversized")],
                "files": [s3_file("firmware.bin", &oversized_key)],
                "protocols": ["HTTP"],
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "case \"a document past the bound\": expected 400, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
        "case \"a document past the bound\": expected InvalidRequestException, got {:?}",
        refused.error_code
    );
    assert!(
        refused
            .message()
            .contains(&format!("at most {MAX_JOB_DOCUMENT_BYTES} bytes")),
        "case \"a document past the bound\": expected the refusal to state the bound, \
         got {:?}",
        refused.message()
    );

    // The refused create recorded nothing: neither the update nor the job it would
    // have deployed exists.
    let update = emulator.get_ota_update("ota-doc-oversized").await;
    assert_eq!(
        update.status, 404,
        "case \"a document past the bound\": the refused create must record no update, \
         got {} {:?}",
        update.status, update.body
    );
    let job = emulator.job_document(&job_id_of("ota-doc-oversized")).await;
    assert_eq!(
        job.status, 404,
        "case \"a document past the bound\": the refused create must deploy no job, got \
         {} {:?}",
        job.status, job.body
    );
    assert_eq!(
        emulator
            .execution_document("doc-sensor-oversized", &job_id_of("ota-doc-oversized")),
        None,
        "case \"a document past the bound\": the refused create must queue no execution"
    );
}
