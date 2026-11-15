// SPDX-License-Identifier: CC0-1.0

//! The S3 OBJECT behind a stream's file, end to end over the wire, with AWS IoT and Amazon
//! S3 mounted on ONE engine — the shape `core/router/src/lib.rs` mounts them in — so the
//! object a `CreateStream` file names is an object staged with `PutObject`.
//!
//! The RULE asserted here: a stream's file IS the S3 object it names, so `CreateStream`
//! creates the stream when the account's object storage holds every file's object and
//! refuses the create (HTTP 400 `InvalidRequestException`, naming the `s3://` address)
//! with nothing persisted when the store ANSWERS that it holds nothing at one of them.
//!
//! The table spans what that rule generalises over rather than one recorded request: how
//! many files the stream serves, whether the missing part is the key or the whole bucket,
//! which position in `files` the unheld object sits at, and the nesting and character set
//! of the key.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for. Amazon S3 places a bucket created with no
/// `LocationConstraint` in us-east-1, which is also the Region IoT serves a request in, so
/// one Region label serves every call here.
const REGION: &str = "us-east-1";

/// The bucket the staged firmware objects live in.
const BUCKET: &str = "firmware-images";

/// A bucket name no `CreateBucket` below ever creates, so the store holds nothing under it.
const ABSENT_BUCKET: &str = "no-such-firmware-bucket";

/// The role IoT reads a stream's S3 files under: its form is the caller's, so one
/// well-formed ARN serves every case.
const STREAM_ROLE: &str = "arn:aws:iam::000000000000:role/IoTStreamRole";

/// The modelled error a refused create carries (`@httpError 400`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// AWS IoT and Amazon S3 on ONE shared engine, behind the real router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// One request through the real router, claimed by the service its credential scope
    /// names. Signature verification is off in this harness, so the signature itself is
    /// never checked.
    async fn send(
        &self,
        method: &str,
        path: &str,
        signing_name: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com{path}"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", content_type)
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        Reply { status, raw }
    }

    /// One IoT control-plane call: restJson1, the body carrying the operation's members.
    async fn iot(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send(method, path, "iot", "application/json", payload)
            .await
    }

    /// `POST /streams/{streamId}` — `CreateStream`.
    async fn create_stream(&self, stream_id: &str, files: &Value) -> Reply {
        self.iot(
            "POST",
            &format!("/streams/{stream_id}"),
            Some(&json!({ "files": files, "roleArn": STREAM_ROLE })),
        )
        .await
    }

    /// `GET /streams/{streamId}` — `DescribeStream`, the reader that proves what a create
    /// persisted and that a refused create persisted nothing.
    async fn describe_stream(&self, stream_id: &str) -> Reply {
        self.iot("GET", &format!("/streams/{stream_id}"), None).await
    }

    /// `PUT /<bucket>` — `CreateBucket` — so the staged objects have a bucket to live in.
    async fn given_bucket(&self) {
        let reply = self
            .send("PUT", &format!("/{BUCKET}"), "s3", "application/xml", Vec::new())
            .await;
        assert_eq!(
            reply.status, 200,
            "the firmware bucket must be created first, got {} {}",
            reply.status,
            reply.text()
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject` — staging `contents` as a firmware file.
    async fn given_object(&self, key: &str, contents: &str) {
        let reply = self
            .send(
                "PUT",
                &format!("/{BUCKET}/{key}"),
                "s3",
                "application/octet-stream",
                contents.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "the firmware object {key} must be staged, got {} {}",
            reply.status,
            reply.text()
        );
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    raw: bytes::Bytes,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.raw)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// One file of a stream: the id a device asks for it by and the bucket and key it is.
fn file_at(file_id: i64, bucket: &str, key: &str) -> Value {
    json!({ "fileId": file_id, "s3Location": { "bucket": bucket, "key": key } })
}

/// RULE: every file's object is held, so the stream is created and reads back with the
/// files the writer sent.
#[tokio::test]
async fn a_stream_whose_files_are_staged_objects_is_created() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;
    // The keys span a flat name, a nested one and one carrying the characters an S3 key
    // admits beyond a path segment.
    for key in ["firmware.bin", "releases/v2/kernel.bin", "Odd_Key-1.ROOTFS.img"] {
        emulator.given_object(key, "firmware bytes").await;
    }

    // The rows span how many files one stream serves and which staged objects they are.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "one file",
            "single",
            json!([file_at(0, BUCKET, "firmware.bin")]),
        ),
        (
            "a nested key",
            "nested",
            json!([file_at(0, BUCKET, "releases/v2/kernel.bin")]),
        ),
        (
            "a key carrying upper case, an underscore and a hyphen",
            "odd-key",
            json!([file_at(7, BUCKET, "Odd_Key-1.ROOTFS.img")]),
        ),
        (
            "three files, every one of them staged",
            "three",
            json!([
                file_at(0, BUCKET, "firmware.bin"),
                file_at(1, BUCKET, "releases/v2/kernel.bin"),
                file_at(255, BUCKET, "Odd_Key-1.ROOTFS.img"),
            ]),
        ),
    ];

    for (case, stream_id, files) in &rows {
        let created = emulator.create_stream(stream_id, files).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        assert_eq!(
            created.json()["streamVersion"],
            json!(1),
            "case {case:?}: a stream that has had one write is at version 1, got {}",
            created.text()
        );

        let read = emulator.describe_stream(stream_id).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the read to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.json()["streamInfo"]["files"],
            *files,
            "case {case:?}: the read must report the files the writer sent, got {}",
            read.text()
        );
    }
}

/// RULE: the object store ANSWERING that it holds nothing at a file's address refuses the
/// create, which states that address, and leaves no stream behind.
#[tokio::test]
async fn a_stream_whose_file_names_an_unheld_object_is_refused() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;
    emulator.given_object("firmware.bin", "firmware bytes").await;

    // The rows span which part of the address the store holds nothing for (a key in a
    // bucket that IS there, a bucket it does not hold at all), the nesting of that key,
    // and where in `files` the unheld object sits.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a key the bucket does not hold",
            json!([file_at(0, BUCKET, "absent.bin")]),
            format!("s3://{BUCKET}/absent.bin"),
        ),
        (
            "a nested key the bucket does not hold",
            json!([file_at(0, BUCKET, "releases/v3/kernel.bin")]),
            format!("s3://{BUCKET}/releases/v3/kernel.bin"),
        ),
        (
            "a bucket the account does not hold",
            json!([file_at(0, ABSENT_BUCKET, "firmware.bin")]),
            format!("s3://{ABSENT_BUCKET}/firmware.bin"),
        ),
        (
            "a later file naming an unheld object",
            json!([
                file_at(0, BUCKET, "firmware.bin"),
                file_at(1, BUCKET, "absent.bin"),
            ]),
            format!("s3://{BUCKET}/absent.bin"),
        ),
    ];

    for (case, files, address) in &rows {
        let refused = emulator.create_stream("refused", files).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            INVALID_REQUEST,
            "case {case:?}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("The S3 object {address} does not exist"),
            "case {case:?}: the refusal must state the object that is not there"
        );
        let read = emulator.describe_stream("refused").await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: a refused create must leave no stream behind, got {} body {}",
            read.status,
            read.text()
        );
    }

    // The create of the same id succeeds once every file it names is an object the store
    // holds, so the refusals above blocked nothing else.
    let created = emulator
        .create_stream("refused", &json!([file_at(0, BUCKET, "firmware.bin")]))
        .await;
    assert_eq!(
        created.status,
        200,
        "case the files are all staged: expected 200, got {} body {}",
        created.status,
        created.text()
    );
}
