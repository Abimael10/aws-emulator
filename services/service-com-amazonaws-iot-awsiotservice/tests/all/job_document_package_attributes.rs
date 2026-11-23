// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the SOFTWARE PACKAGE CATALOG placeholders in a job
//! document: `${aws:iot:package:<packageName>:version:<versionName>:attributes:<key>}`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /packages/{packageName}`, `PUT /packages/{packageName}/versions/{versionName}`,
//! `PATCH /packages/{packageName}/versions/{versionName}`, `PUT /jobs/{jobId}` and
//! `GET /jobs/{jobId}/job-document` signed for the `iot` signing name — so what is
//! asserted is the document a client and a device read back through the modelled surface,
//! not the state struct.
//!
//! The rule under test: a job document's post-substitution view resolves every
//! `${aws:iot:package:…:attributes:<key>}` placeholder to the value that attribute holds
//! on a package version the job is ASSOCIATED with through `destinationPackageVersions`
//! — "the package version Amazon Resource Names (ARNs) that are installed on the device
//! when the job successfully completes"
//! (`smithy_query(query="doc",shape="CreateJobRequest",
//! member="destinationPackageVersions")`), whose `attributes` are "metadata that can be
//! used to define a package version's configuration. For example, the S3 file location,
//! configuration options that are being sent to the device or fleet"
//! (`CreatePackageVersionRequest$attributes`). A placeholder nothing the job carries
//! binds — an attribute the named version does not hold, a version the job is not
//! associated with, a package the catalog does not hold — is reported as written, and
//! `beforeSubstitution` (`GetJobDocumentRequest$beforeSubstitution`, default false)
//! reports the document as written, placeholders intact.
//!
//! The table spans what the rule generalises over — how many placeholders a document
//! carries and how many versions they address, the lifecycle status of the version they
//! resolve from (`PUBLISHED`, `DEPRECATED`), the form of the attribute key and value
//! (`ResourceAttributes`: key pattern `^[a-zA-Z0-9:_-]+$`, value pattern
//! `^[^\p{C}]+$`), the package and version name forms (`PackageName` / `VersionName`,
//! pattern `^[a-zA-Z0-9-_.]+$`), and placeholders of the two forms mixed in one document
//! — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::JobExecutionStore;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service.
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
    body: Value,
}

impl Reply {
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can read the SHARED job-execution store the control
/// plane writes the device-facing copy of the document into.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
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
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    async fn create_package(&self, package_name: &str) -> Reply {
        self.call("PUT", &format!("/packages/{package_name}"), Some(&json!({})))
            .await
    }

    async fn create_package_version(
        &self,
        package_name: &str,
        version_name: &str,
        attributes: &[(&str, &str)],
    ) -> Reply {
        let attributes: Value = Value::Object(
            attributes
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        );
        self.call(
            "PUT",
            &format!("/packages/{package_name}/versions/{version_name}"),
            Some(&json!({ "attributes": attributes })),
        )
        .await
    }

    /// `UpdatePackageVersion` with the lifecycle action that assigns the version its
    /// status (`PUBLISH` / `DEPRECATE`).
    async fn update_package_version(
        &self,
        package_name: &str,
        version_name: &str,
        action: &str,
    ) -> Reply {
        self.call(
            "PATCH",
            &format!("/packages/{package_name}/versions/{version_name}"),
            Some(&json!({ "action": action })),
        )
        .await
    }

    async fn get_package_version(&self, package_name: &str, version_name: &str) -> Reply {
        self.call(
            "GET",
            &format!("/packages/{package_name}/versions/{version_name}"),
            None,
        )
        .await
    }

    async fn register_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    /// `GetJobDocument` with the `beforeSubstitution` query parameter left out, which is
    /// the modelled default (`false`).
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

    /// Put one package version in the catalog, give it `attributes`, move it to the
    /// status `action` assigns, and answer the `packageVersionArn` the catalog reports
    /// for it — the ARN a job names it by.
    async fn published_version(&self, version: &CatalogVersion) -> String {
        let created = self.create_package(version.package).await;
        assert_eq!(
            created.status, 200,
            "the package {:?} must be creatable, got {created:?}",
            version.package
        );
        let created = self
            .create_package_version(version.package, version.version, &version.attributes)
            .await;
        assert_eq!(
            created.status, 200,
            "the version {:?} of {:?} must be creatable, got {created:?}",
            version.version, version.package
        );
        let updated = self
            .update_package_version(version.package, version.version, version.action)
            .await;
        assert_eq!(
            updated.status, 200,
            "the {:?} of {:?} must answer 200, got {updated:?}",
            version.action, version.version
        );
        let read = self
            .get_package_version(version.package, version.version)
            .await;
        assert_eq!(
            read.status, 200,
            "the version must read back, got {read:?}"
        );
        assert_eq!(
            read.text("status").as_deref(),
            Some(version.expected_status),
            "the {:?} must leave the version in {:?}, got {read:?}",
            version.action,
            version.expected_status
        );
        read.text("packageVersionArn")
            .expect("the catalog reports the version's ARN")
    }
}

/// The thing ARN a target entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:000000000000:thing/{thing_name}")
}

/// A package-attribute placeholder as AWS IoT's Software Package Catalog writes one into
/// a job document.
fn package_placeholder(package_name: &str, version_name: &str, attribute_key: &str) -> String {
    format!("${{aws:iot:package:{package_name}:version:{version_name}:attributes:{attribute_key}}}")
}

/// A `documentParameters` placeholder, the other substitution form a job document carries.
fn parameter_placeholder(key: &str) -> String {
    format!("${{aws:iot:parameter:{key}}}")
}

/// One package version to put in the catalog before a case's job is created.
struct CatalogVersion {
    package: &'static str,
    version: &'static str,
    attributes: Vec<(&'static str, &'static str)>,
    /// The `UpdatePackageVersion$action` that moves it out of `DRAFT`.
    action: &'static str,
    /// The `PackageVersionStatus` that action assigns.
    expected_status: &'static str,
}

impl CatalogVersion {
    fn published(
        package: &'static str,
        version: &'static str,
        attributes: Vec<(&'static str, &'static str)>,
    ) -> Self {
        Self {
            package,
            version,
            attributes,
            action: "PUBLISH",
            expected_status: "PUBLISHED",
        }
    }

    fn deprecated(
        package: &'static str,
        version: &'static str,
        attributes: Vec<(&'static str, &'static str)>,
    ) -> Self {
        Self {
            package,
            version,
            attributes,
            action: "DEPRECATE",
            expected_status: "DEPRECATED",
        }
    }
}

/// One row: a catalog, the versions the job is associated with, the document as written
/// and the document the resolved view must report.
struct Case {
    case: &'static str,
    job_id: &'static str,
    catalog: Vec<CatalogVersion>,
    /// `(packageName, versionName)` per entry of the job's `destinationPackageVersions`.
    associated: Vec<(&'static str, &'static str)>,
    parameters: Vec<(&'static str, &'static str)>,
    document: String,
    resolved: String,
}

/// An attribute value in the form a deployment puts in the catalog: an artifact location.
const ARTIFACT_LOCATION: &str = "https://artifacts.invalid/bundles/agent-7.2.1.tar.gz";
/// A second artifact location, so two associated versions resolve to different values.
const OTHER_LOCATION: &str = "s3://fleet-artifacts/driver/0-9/driver.bin";
/// An attribute value that is not a URL at all: `ResourceAttributeValue` admits any text
/// without control characters (pattern `^[^\p{C}]+$`).
const CHECKSUM: &str = "sha256:9f2b 00c1-{inner}";

fn cases() -> Vec<Case> {
    let firmware = package_placeholder("fleet-firmware", "1.0.0", "firmwareUrl");
    let driver = package_placeholder("alpha.driver", "0-9", "s3_location");
    // An attribute key carrying the ':' and '_' `ResourceAttributeKey` admits (pattern
    // `^[a-zA-Z0-9:_-]+$`).
    let checksum = package_placeholder("zeta_agent", "2026.01-rc.2", "artifact:checksum_v2");
    vec![
        Case {
            case: "one placeholder resolves to the attribute on the associated version",
            job_id: "pkg-doc-one",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: Vec::new(),
            document: format!(r#"{{"firmwareUrl":"{firmware}"}}"#),
            resolved: format!(r#"{{"firmwareUrl":"{ARTIFACT_LOCATION}"}}"#),
        },
        Case {
            case: "the same placeholder resolves at every occurrence",
            job_id: "pkg-doc-repeated",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: Vec::new(),
            document: format!(r#"{{"url":"{firmware}","fallbackUrl":"{firmware}"}}"#),
            resolved: format!(
                r#"{{"url":"{ARTIFACT_LOCATION}","fallbackUrl":"{ARTIFACT_LOCATION}"}}"#
            ),
        },
        Case {
            case: "three placeholders over two associated versions, one of them deprecated",
            job_id: "pkg-doc-two-versions",
            catalog: vec![
                CatalogVersion::published(
                    "alpha.driver",
                    "0-9",
                    vec![("s3_location", OTHER_LOCATION)],
                ),
                CatalogVersion::deprecated(
                    "zeta_agent",
                    "2026.01-rc.2",
                    vec![
                        ("artifact:checksum_v2", CHECKSUM),
                        ("unused-attribute", "not referenced by the document"),
                    ],
                ),
            ],
            // Named out of sorted order, which must not change what resolves.
            associated: vec![("zeta_agent", "2026.01-rc.2"), ("alpha.driver", "0-9")],
            parameters: Vec::new(),
            document: format!(
                r#"{{"driver":"{driver}","checksum":"{checksum}","again":"{driver}"}}"#
            ),
            resolved: format!(
                r#"{{"driver":"{OTHER_LOCATION}","checksum":"{CHECKSUM}","again":"{OTHER_LOCATION}"}}"#
            ),
        },
        Case {
            case: "an attribute the associated version does not carry stays as written",
            job_id: "pkg-doc-unknown-attribute",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: Vec::new(),
            document: format!(
                r#"{{"url":"{firmware}","notes":"{}"}}"#,
                package_placeholder("fleet-firmware", "1.0.0", "releaseNotes")
            ),
            resolved: format!(
                r#"{{"url":"{ARTIFACT_LOCATION}","notes":"{}"}}"#,
                package_placeholder("fleet-firmware", "1.0.0", "releaseNotes")
            ),
        },
        Case {
            case: "a version the catalog holds but the job is not associated with stays as written",
            job_id: "pkg-doc-unassociated",
            catalog: vec![
                CatalogVersion::published(
                    "fleet-firmware",
                    "1.0.0",
                    vec![("firmwareUrl", ARTIFACT_LOCATION)],
                ),
                CatalogVersion::published(
                    "alpha.driver",
                    "0-9",
                    vec![("s3_location", OTHER_LOCATION)],
                ),
            ],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: Vec::new(),
            document: format!(r#"{{"url":"{firmware}","driver":"{driver}"}}"#),
            resolved: format!(r#"{{"url":"{ARTIFACT_LOCATION}","driver":"{driver}"}}"#),
        },
        Case {
            case: "a package the catalog does not hold stays as written",
            job_id: "pkg-doc-unknown-package",
            catalog: Vec::new(),
            associated: Vec::new(),
            parameters: Vec::new(),
            document: format!(r#"{{"url":"{firmware}"}}"#),
            resolved: format!(r#"{{"url":"{firmware}"}}"#),
        },
        Case {
            case: "a document parameter and a package attribute resolve in one document",
            job_id: "pkg-doc-mixed",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: vec![("action", "install")],
            document: format!(
                r#"{{"operation":"{}","url":"{firmware}","retry":"{}"}}"#,
                parameter_placeholder("action"),
                parameter_placeholder("action")
            ),
            resolved: format!(
                r#"{{"operation":"install","url":"{ARTIFACT_LOCATION}","retry":"install"}}"#
            ),
        },
        Case {
            case: "a package placeholder before a parameter placeholder resolves in document order",
            job_id: "pkg-doc-order",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: vec![("action", "install")],
            document: format!(
                r#"{{"url":"{firmware}","operation":"{}"}}"#,
                parameter_placeholder("action")
            ),
            resolved: format!(
                r#"{{"url":"{ARTIFACT_LOCATION}","operation":"install"}}"#
            ),
        },
        Case {
            case: "a document with no placeholder at all is unchanged by an association",
            job_id: "pkg-doc-none",
            catalog: vec![CatalogVersion::published(
                "fleet-firmware",
                "1.0.0",
                vec![("firmwareUrl", ARTIFACT_LOCATION)],
            )],
            associated: vec![("fleet-firmware", "1.0.0")],
            parameters: Vec::new(),
            document: r#"{"operation":"reboot"}"#.to_string(),
            resolved: r#"{"operation":"reboot"}"#.to_string(),
        },
    ]
}

/// RULE: the post-substitution view of a job document resolves every
/// `${aws:iot:package:…:attributes:<key>}` placeholder to that attribute's value on a
/// package version the job's `destinationPackageVersions` name, and leaves a placeholder
/// nothing the job carries binds as written.
#[tokio::test]
async fn a_package_attribute_placeholder_resolves_from_the_jobs_associated_versions() {
    for case in cases() {
        let emulator = Emulator::new();
        let Case {
            case: label,
            job_id,
            catalog,
            associated,
            parameters,
            document,
            resolved,
        } = case;

        // The catalog the job's association points into, built through the modelled
        // package operations.
        let mut arns: std::collections::BTreeMap<(&str, &str), String> =
            std::collections::BTreeMap::new();
        for version in &catalog {
            let arn = emulator.published_version(version).await;
            arns.insert((version.package, version.version), arn);
        }

        let thing_name = format!("{job_id}-device");
        let registered = emulator.register_thing(&thing_name).await;
        assert_eq!(
            registered.status, 200,
            "case {label:?}: registering the target must answer 200, got {registered:?}"
        );

        let mut request = json!({
            "targets": [thing_arn(&thing_name)],
            "targetSelection": "SNAPSHOT",
            "document": document,
        });
        if !associated.is_empty() {
            let named: Vec<String> = associated
                .iter()
                .map(|key| {
                    arns.get(key)
                        .unwrap_or_else(|| {
                            panic!("case {label:?}: the catalog holds {key:?}");
                        })
                        .clone()
                })
                .collect();
            request["destinationPackageVersions"] = json!(named);
        }
        if !parameters.is_empty() {
            request["documentParameters"] = Value::Object(
                parameters
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), json!(value)))
                    .collect(),
            );
        }
        let created = emulator.create_job(job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the job must be creatable, got {created:?}"
        );

        // The default view (`beforeSubstitution` left out) is the RESOLVED one.
        let defaulted = emulator.job_document(job_id).await;
        assert_eq!(
            defaulted.status, 200,
            "case {label:?}: the read must answer 200, got {defaulted:?}"
        );
        assert_eq!(
            defaulted.text("document"),
            Some(resolved.clone()),
            "case {label:?}: expected {resolved:?}, got {:?}",
            defaulted.text("document")
        );
        let spelled_out = emulator.job_document_before(job_id, false).await;
        assert_eq!(
            spelled_out.text("document"),
            Some(resolved.clone()),
            "case {label:?}: beforeSubstitution=false must report the resolved view, got {:?}",
            spelled_out.text("document")
        );

        // The pre-substitution view is the document as written, placeholders intact.
        let before = emulator.job_document_before(job_id, true).await;
        assert_eq!(
            before.text("document"),
            Some(document.clone()),
            "case {label:?}: beforeSubstitution=true must report the document as written, got {:?}",
            before.text("document")
        );

        // Round trip: the copy the targeted device is handed is the resolved document.
        assert_eq!(
            emulator.execution_document(&thing_name, job_id),
            Some(resolved.clone()),
            "case {label:?}: the device's execution must carry the resolved document, got {:?}",
            emulator.execution_document(&thing_name, job_id)
        );

        // A read does not change what a later reader sees.
        let again = emulator.job_document(job_id).await;
        assert_eq!(
            again.text("document"),
            Some(resolved.clone()),
            "case {label:?}: the resolved view must be stable across reads, got {:?}",
            again.text("document")
        );
    }
}

/// RULE: a resolved document carries no unresolved placeholder of the package form for a
/// version the job names and an attribute that version holds — whatever the attribute's
/// value looks like, and whichever of the two allowed lifecycle statuses the version is
/// in ("the package version must be in either the Published or Deprecated state when the
/// job deploys", `CreateJobRequest$destinationPackageVersions`).
#[tokio::test]
async fn the_package_placeholder_form_does_not_survive_the_resolved_view() {
    // The value forms a catalog attribute carries: a presigned-looking URL, an S3 URI,
    // text with spaces and punctuation, and a lone character — every one of them legal
    // `ResourceAttributeValue` text (pattern `^[^\p{C}]+$`, length 1..).
    let values = [
        "https://artifacts.invalid/firmware.bin?versionId=3",
        "s3://fleet-artifacts/agent/agent.bin",
        "build 7.2.1 (candidate), sha256:0a1b",
        "x",
    ];
    for (index, value) in values.iter().enumerate() {
        for action in ["PUBLISH", "DEPRECATE"] {
            let emulator = Emulator::new();
            let version = if action == "PUBLISH" {
                CatalogVersion::published("value-forms", "1.0.0", vec![("payload", value)])
            } else {
                CatalogVersion::deprecated("value-forms", "1.0.0", vec![("payload", value)])
            };
            let arn = emulator.published_version(&version).await;

            let thing_name = format!("value-form-{index}-{action}-device");
            assert_eq!(
                emulator.register_thing(&thing_name).await.status,
                200,
                "case {value:?}/{action}: registering the target must answer 200"
            );
            let document = format!(
                r#"{{"payload":"{}"}}"#,
                package_placeholder("value-forms", "1.0.0", "payload")
            );
            let job_id = format!("value-form-{index}-{}", action.to_lowercase());
            let created = emulator
                .create_job(
                    &job_id,
                    &json!({
                        "targets": [thing_arn(&thing_name)],
                        "document": document,
                        "destinationPackageVersions": [arn],
                    }),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {value:?}/{action}: the job must be creatable, got {created:?}"
            );

            let read = emulator.job_document(&job_id).await;
            let served = read.text("document").unwrap_or_default();
            assert_eq!(
                served,
                format!(r#"{{"payload":"{value}"}}"#),
                "case {value:?}/{action}: expected the attribute value in the document, got \
                 {served:?}"
            );
            assert!(
                !served.contains("${aws:iot:package"),
                "case {value:?}/{action}: no package placeholder may survive the resolved \
                 view, got {served:?}"
            );
        }
    }
}
