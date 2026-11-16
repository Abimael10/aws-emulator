// SPDX-License-Identifier: CC0-1.0

//! The S3 OBJECT behind a package version's software bill of materials, end to end over
//! the wire, with AWS IoT and Amazon S3 mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the object an
//! `AssociateSbomWithPackageVersion` names is an object staged with `PutObject`.
//!
//! The rules asserted here:
//!
//! * a bill of materials IS the S3 object it names, so an association whose location the
//!   service cannot read is refused (HTTP 400 `ValidationException`, "SBOM not accessible
//!   by the service. …") with NOTHING recorded — and the reader of the version then
//!   reports no `sbom` and no `sbomValidationStatus` at all, rather than a validation
//!   nothing started;
//! * the location has to PIN a version of that object: one that names none is refused
//!   ("S3 object version for SBOM cannot be null") before the object store is asked
//!   anything;
//! * an association whose object the store serves is recorded, answered `IN_PROGRESS`,
//!   and read back by `GetPackageVersion`.
//!
//! The table spans what the rules generalise over rather than one recorded request: WHAT
//! the store answers about the location (the object is there, the key is not, the bucket
//! is not, the version is not), and the nesting and character set of the key.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    INACCESSIBLE_SBOM_MESSAGE, SBOM_VERSION_REQUIRED_MESSAGE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for. Amazon S3 places a bucket created with no
/// `LocationConstraint` in us-east-1, which is also the Region IoT serves a request in, so
/// one Region label serves every call here.
const REGION: &str = "us-east-1";

/// The bucket the staged bills of materials live in.
const BUCKET: &str = "sbom-documents";

/// A bucket name no `CreateBucket` below ever creates, so the store holds nothing under
/// it.
const ABSENT_BUCKET: &str = "no-such-sbom-bucket";

/// The modelled error a refused association carries (`@httpError 400`).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The status an association's validation is answered in.
const IN_PROGRESS: &str = "IN_PROGRESS";

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
        let version_id = response
            .headers()
            .get("x-amz-version-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        Reply {
            status,
            raw,
            version_id,
        }
    }

    /// One IoT control-plane call: restJson1, the body carrying the operation's members.
    async fn iot(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send(method, path, "iot", "application/json", payload)
            .await
    }

    /// `PUT /<bucket>` — `CreateBucket` — and `PUT /<bucket>?versioning`, so every write
    /// below is a VERSION of its key that a reader can name.
    async fn given_bucket(&self) {
        let reply = self
            .send("PUT", &format!("/{BUCKET}"), "s3", "application/xml", Vec::new())
            .await;
        assert_eq!(
            reply.status, 200,
            "the bill-of-materials bucket must be created first, got {} {}",
            reply.status,
            reply.text()
        );
        let versioning = self
            .send(
                "PUT",
                &format!("/{BUCKET}?versioning"),
                "s3",
                "application/xml",
                b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>"
                    .to_vec(),
            )
            .await;
        assert_eq!(
            versioning.status, 200,
            "the bucket must take a versioning configuration, got {} {}",
            versioning.status,
            versioning.text()
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject` — staging `contents` as a bill of materials,
    /// answering the version id the store minted for that write.
    async fn given_object(&self, key: &str, contents: &str) -> String {
        let reply = self
            .send(
                "PUT",
                &format!("/{BUCKET}/{key}"),
                "s3",
                "application/json",
                contents.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "the document {key} must be staged, got {} {}",
            reply.status,
            reply.text()
        );
        reply
            .version_id
            .clone()
            .unwrap_or_else(|| panic!("the write of {key} reported no version id"))
    }

    /// `PUT /packages/{packageName}` — `CreatePackage`.
    async fn create_package(&self, package: &str) -> Reply {
        self.iot("PUT", &format!("/packages/{package}"), Some(&json!({})))
            .await
    }

    /// `PUT /packages/{packageName}/versions/{versionName}` — `CreatePackageVersion`.
    async fn create_version(&self, package: &str, version: &str) -> Reply {
        self.iot(
            "PUT",
            &format!("/packages/{package}/versions/{version}"),
            Some(&json!({ "description": "a version that may carry a bill of materials" })),
        )
        .await
    }

    /// `PUT /packages/{packageName}/versions/{versionName}/sbom` —
    /// `AssociateSbomWithPackageVersion`.
    async fn associate_sbom(&self, package: &str, version: &str, sbom: &Value) -> Reply {
        self.iot(
            "PUT",
            &format!("/packages/{package}/versions/{version}/sbom"),
            Some(&json!({ "sbom": sbom })),
        )
        .await
    }

    /// `GET /packages/{packageName}/versions/{versionName}` — `GetPackageVersion`, the
    /// reader that proves what an association recorded and that a refused one recorded
    /// nothing.
    async fn get_version(&self, package: &str, version: &str) -> Reply {
        self.iot(
            "GET",
            &format!("/packages/{package}/versions/{version}"),
            None,
        )
        .await
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    raw: bytes::Bytes,
    /// The version id the object store minted for a write, when the answer carries one.
    version_id: Option<String>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.raw)
            .unwrap_or_else(|error| panic!("a response body is JSON ({error}): {}", self.text()))
    }

    /// One string member of the answer, or `None` when the answer carries none.
    fn member(&self, name: &str) -> Option<String> {
        self.json()[name].as_str().map(str::to_string)
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }
}

/// RULE: an association whose S3 location the service cannot read is refused, and the
/// version then carries no bill of materials and no validation status at all.
#[tokio::test]
async fn an_sbom_the_service_cannot_read_is_refused_and_records_nothing() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;
    let held_version = emulator.given_object("spdx/firmware.json", "{\"spdxVersion\":\"2.3\"}").await;

    // (case, the bucket, the key, the version the location pins)
    let rows = [
        (
            "a key the bucket holds nothing at",
            BUCKET,
            "spdx/never-staged.json",
            held_version.clone(),
        ),
        (
            "a bucket the store does not hold",
            ABSENT_BUCKET,
            "spdx/firmware.json",
            held_version.clone(),
        ),
        (
            "a version of the object the store never minted",
            BUCKET,
            "spdx/firmware.json",
            "3HL4kqtJlcpXroDTDmhZxDTDmhZxDTD".to_string(),
        ),
        (
            "a deeply nested key, held nowhere",
            BUCKET,
            "a/b/c/d/sbom-2.0_final.cdx.json",
            held_version.clone(),
        ),
    ];

    for (index, (case, bucket, key, version)) in rows.iter().enumerate() {
        let package = format!("refused-package-{index}");
        let version_name = "1.0.0";
        assert_eq!(
            emulator.create_package(&package).await.status,
            200,
            "case {case:?}: the package must be creatable"
        );
        assert_eq!(
            emulator.create_version(&package, version_name).await.status,
            200,
            "case {case:?}: the package version must be creatable"
        );

        let sbom = json!({
            "s3Location": { "bucket": bucket, "key": key, "version": version },
        });
        let refused = emulator.associate_sbom(&package, version_name, &sbom).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: an unreadable SBOM must be refused 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            VALIDATION_EXCEPTION,
            "case {case:?}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.member("message").as_deref(),
            Some(INACCESSIBLE_SBOM_MESSAGE),
            "case {case:?}: got {}",
            refused.text()
        );

        // The refused association recorded nothing, so the reader reports neither member.
        let read = emulator.get_version(&package, version_name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the version itself must still be readable, got {}",
            read.text()
        );
        assert_eq!(
            read.json()["sbom"],
            Value::Null,
            "case {case:?}: no SBOM was associated, so none is reported: {}",
            read.text()
        );
        assert_eq!(
            read.json()["sbomValidationStatus"],
            Value::Null,
            "case {case:?}: no validation was started, so no status is reported: {}",
            read.text()
        );
    }
}

/// RULE: the S3 location has to pin the object VERSION the bill of materials was read
/// from, and that is decided about the request before the object store is asked anything.
#[tokio::test]
async fn an_sbom_pinning_no_object_version_is_refused() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;
    emulator.given_object("spdx/pinned.json", "{}").await;

    // The rows differ along how the version is left unstated: absent, empty, and blank.
    let rows = [
        (
            "a location naming no version at all",
            json!({ "s3Location": { "bucket": BUCKET, "key": "spdx/pinned.json" } }),
        ),
        (
            "a location whose version is empty",
            json!({ "s3Location": { "bucket": BUCKET, "key": "spdx/pinned.json", "version": "" } }),
        ),
        (
            "a location whose version is blank",
            json!({
                "s3Location": { "bucket": BUCKET, "key": "spdx/pinned.json", "version": "   " },
            }),
        ),
    ];
    for (index, (case, sbom)) in rows.iter().enumerate() {
        let package = format!("unpinned-package-{index}");
        assert_eq!(
            emulator.create_package(&package).await.status,
            200,
            "case {case:?}: the package must be creatable"
        );
        assert_eq!(
            emulator.create_version(&package, "1.0.0").await.status,
            200,
            "case {case:?}: the package version must be creatable"
        );

        let refused = emulator.associate_sbom(&package, "1.0.0", sbom).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.member("message").as_deref(),
            Some(SBOM_VERSION_REQUIRED_MESSAGE),
            "case {case:?}: got {}",
            refused.text()
        );
        assert_eq!(
            emulator
                .get_version(&package, "1.0.0")
                .await
                .json()["sbom"],
            Value::Null,
            "case {case:?}: the refused association recorded nothing"
        );
    }
}

/// RULE: an association whose object the store SERVES is recorded, answered in the
/// non-terminal validation status, and read back by the version's reader.
#[tokio::test]
async fn an_sbom_the_store_serves_is_recorded_and_read_back() {
    let emulator = Emulator::new();
    emulator.given_bucket().await;

    // The rows differ along the nesting and character set of the key, which is what the
    // address generalises over.
    let rows = [
        ("a flat key", "sbom.json"),
        ("a nested key", "spdx/2024/firmware-1.0.0.json"),
        ("a key using the punctuation S3 admits", "cdx/a_b-c.1.0.json"),
    ];
    for (index, (case, key)) in rows.iter().enumerate() {
        let object_version = emulator.given_object(key, "{\"bomFormat\":\"CycloneDX\"}").await;
        let package = format!("served-package-{index}");
        assert_eq!(
            emulator.create_package(&package).await.status,
            200,
            "case {case:?}: the package must be creatable"
        );
        assert_eq!(
            emulator.create_version(&package, "1.0.0").await.status,
            200,
            "case {case:?}: the package version must be creatable"
        );

        let sbom = json!({
            "s3Location": { "bucket": BUCKET, "key": key, "version": object_version },
        });
        let associated = emulator.associate_sbom(&package, "1.0.0", &sbom).await;
        assert_eq!(
            associated.status, 200,
            "case {case:?}: an SBOM the store serves must be accepted, got {} body {}",
            associated.status,
            associated.text()
        );
        assert_eq!(
            associated.member("sbomValidationStatus").as_deref(),
            Some(IN_PROGRESS),
            "case {case:?}: the association starts a validation, got {}",
            associated.text()
        );

        let read = emulator.get_version(&package, "1.0.0").await;
        assert_eq!(
            read.json()["sbom"], sbom,
            "case {case:?}: the reader must report the SBOM the association recorded, got {}",
            read.text()
        );
        // The validation the association started either still runs or has completed by
        // the read — both are statuses of the modelled set, and which one it is depends on
        // the clock rather than on this call.
        let status = read
            .member("sbomValidationStatus")
            .unwrap_or_else(|| panic!("case {case:?}: no sbomValidationStatus in {}", read.text()));
        assert!(
            [IN_PROGRESS, "SUCCEEDED"].contains(&status.as_str()),
            "case {case:?}: expected a validation status of the modelled set, got {status:?}"
        );
    }
}
