// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of a job's PACKAGE ASSOCIATION: the
//! `destinationPackageVersions` a `CreateJob` names, and the `DescribeJob` that reports
//! them back.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /jobs/{jobId}` and `GET /jobs/{jobId}` signed for the `iot` signing name — so what
//! is asserted is what a client reads back through the modelled surface, not the state
//! struct.
//!
//! The rule under test: `destinationPackageVersions` is "the package version Amazon
//! Resource Names (ARNs) that are installed on the device when the job successfully
//! completes" (`smithy_query(query="doc",shape="CreateJobRequest",
//! member="destinationPackageVersions")`), so it is part of the job the create persists:
//! a reader reports the ARNs verbatim and in the order the create named them, a job that
//! named none reports no list at all rather than an empty one, and a create naming more
//! than the published maximum of 25 ARNs — "Up to 25 package version ARNs are allowed",
//! the same member's documentation — is refused with the modelled `LimitExceededException`
//! (HTTP 410) and persists no job.
//!
//! That association is also what the version reporting a finished job performs is computed
//! from, which is why it has to outlive the create; the reporting itself crosses three
//! services and is covered by `core/router/tests/all/iot_package_version_reporting_by_jobs.rs`.
//!
//! The table spans what the rule generalises over — how many ARNs one job names (none,
//! one, two, and the published maximum), the order they are named in, and the form of the
//! package and version names each ARN carries — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// How many package version ARNs one job may name, as
/// `CreateJobRequest$destinationPackageVersions` publishes it: "Up to 25 package version
/// ARNs are allowed."
const MAX_DESTINATION_PACKAGE_VERSIONS: usize = 25;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service.
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
    /// The `destinationPackageVersions` the reported job carries, or `Value::Null` when it
    /// carries none.
    fn destination_package_versions(&self) -> Value {
        self.body
            .pointer("/job/destinationPackageVersions")
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
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

    async fn register_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    async fn describe_job(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}"), None).await
    }
}

/// The thing ARN a target entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN of one package version, in the form the catalog reports —
/// `arn:aws:iot:<region>:<account>:package/<packageName>/version/<versionName>`
/// (`CreatePackageVersion$packageVersionArn`).
fn package_version_arn(package_name: &str, version_name: &str) -> String {
    format!(
        "arn:aws:iot:{REGION}:{ACCOUNT_ID}:package/{package_name}/version/{version_name}"
    )
}

/// One create, and the package version ARNs a reader must report for it.
struct Case {
    case: &'static str,
    job_id: &'static str,
    /// `(packageName, versionName)` per ARN the create names, in that order.
    installed: Vec<(&'static str, &'static str)>,
}

/// Every row: no ARN at all, one, two in an order that is not sorted, and the published
/// maximum of 25. The names exercise what `PackageName` and `VersionName` admit (`len
/// 1..128` / `len 1..64`, pattern `^[a-zA-Z0-9-_.]+$`).
fn cases() -> Vec<Case> {
    let mut many: Vec<(&'static str, &'static str)> = Vec::new();
    // 25 distinct packages, which is the top of the published range.
    for (package, version) in PACKAGE_CATALOG
        .iter()
        .cycle()
        .zip(VERSION_NAMES.iter().cycle())
        .take(MAX_DESTINATION_PACKAGE_VERSIONS)
    {
        many.push((*package, *version));
    }
    vec![
        Case {
            case: "no package version at all",
            job_id: "pkg-none",
            installed: Vec::new(),
        },
        Case {
            case: "one package version",
            job_id: "pkg-one",
            installed: vec![("fleet-firmware", "1.0.0")],
        },
        Case {
            case: "two package versions, named out of sorted order",
            job_id: "pkg-two",
            installed: vec![("zeta_agent", "2026.01"), ("alpha.driver", "0-9")],
        },
        Case {
            case: "the published maximum of 25 package versions",
            job_id: "pkg-max",
            installed: many,
        },
    ]
}

/// The 25 package names the maximum-length row names, each distinct.
const PACKAGE_CATALOG: [&str; 25] = [
    "p00", "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10", "p11", "p12",
    "p13", "p14", "p15", "p16", "p17", "p18", "p19", "p20", "p21", "p22", "p23", "p24",
];

/// The version-name forms that row cycles through.
const VERSION_NAMES: [&str; 3] = ["1", "2.0.1-rc.2", "release_candidate"];

/// A job carries the package version ARNs its create named, verbatim and in order, and a
/// reader reports exactly those.
#[tokio::test]
async fn describe_job_reports_the_destination_package_versions_the_create_named() {
    for case in cases() {
        let emulator = Emulator::new();
        let Case {
            case: label,
            job_id,
            installed,
        } = case;

        let thing = format!("{job_id}-device");
        let registered = emulator.register_thing(&thing).await;
        assert_eq!(
            registered.status, 200,
            "case {label:?}: registering the target must answer 200, got {registered:?}"
        );

        let arns: Vec<String> = installed
            .iter()
            .map(|(package, version)| package_version_arn(package, version))
            .collect();
        let mut request = json!({
            "targets": [thing_arn(&thing)],
            "document": r#"{"operation":"install"}"#,
        });
        if !arns.is_empty() {
            request["destinationPackageVersions"] = json!(arns);
        }
        let created = emulator.create_job(job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the create must answer 200, got {created:?}"
        );

        let read = emulator.describe_job(job_id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: the read must answer 200, got {read:?}"
        );
        let expected = if arns.is_empty() {
            Value::Null
        } else {
            json!(arns)
        };
        assert_eq!(
            read.destination_package_versions(),
            expected,
            "case {label:?}: expected {expected}, got {read:?}"
        );

        // A read does not change what a later reader sees.
        let again = emulator.describe_job(job_id).await;
        assert_eq!(
            again.destination_package_versions(),
            expected,
            "case {label:?}: the association must be stable across reads, got {again:?}"
        );
    }
}

/// A create naming MORE than the published maximum of 25 package version ARNs is refused,
/// and nothing is persisted for it.
#[tokio::test]
async fn a_create_past_the_published_package_version_maximum_is_refused() {
    let emulator = Emulator::new();
    let thing = "pkg-over-limit-device";
    assert_eq!(
        emulator.register_thing(thing).await.status,
        200,
        "registering the target must answer 200"
    );

    // One past the published maximum, and the maximum itself as the row that must be
    // accepted beside it.
    for (label, count, expected_status) in [
        ("the published maximum", MAX_DESTINATION_PACKAGE_VERSIONS, 200),
        (
            "one past the published maximum",
            MAX_DESTINATION_PACKAGE_VERSIONS + 1,
            410,
        ),
    ] {
        let arns: Vec<String> = (0..count)
            .map(|index| package_version_arn(&format!("bulk-{index}"), "1.0.0"))
            .collect();
        let job_id = format!("pkg-limit-{count}");
        let created = emulator
            .create_job(
                &job_id,
                &json!({
                    "targets": [thing_arn(thing)],
                    "document": r#"{"operation":"install"}"#,
                    "destinationPackageVersions": arns,
                }),
            )
            .await;
        assert_eq!(
            created.status, expected_status,
            "case {label:?}: {count} ARNs must answer {expected_status}, got {created:?}"
        );

        let read = emulator.describe_job(&job_id).await;
        if expected_status == 200 {
            assert_eq!(
                read.destination_package_versions(),
                json!(arns),
                "case {label:?}: expected every named ARN reported, got {read:?}"
            );
        } else {
            assert_eq!(
                (created.error_code.as_deref(), read.status),
                (Some("LimitExceededException"), 404),
                "case {label:?}: the refusal must be the modelled limit error and must \
                 persist no job, got {created:?} and {read:?}"
            );
        }
    }
}
