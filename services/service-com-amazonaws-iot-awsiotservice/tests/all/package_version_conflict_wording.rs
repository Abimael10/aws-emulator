// SPDX-License-Identifier: CC0-1.0

//! How `CreatePackageVersion` WORDS the refusal of a create whose
//! `(packageName, versionName)` pair the Software Package Catalog already holds, end to
//! end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is the
//! status, the `x-amzn-errortype` header a client resolves the error shape from, and the
//! `message` an SDK surfaces, not an internal function's return value.
//!
//! The rule under test: a `CreatePackageVersion` for a pair the catalog already holds,
//! which is not the request that minted that record arriving again under its own
//! `@idempotencyToken`, is refused `ConflictException` (HTTP 409, "The request conflicts
//! with the current state of the resource" —
//! `smithy_query(query="operation",operation="CreatePackageVersion")`) stating the failed
//! conditional check as a duplicate request and naming the package first and the version
//! second, both taken from the request; the stored record is left exactly as it was.
//!
//! The table spans what that rule generalises over — the form and length of the package
//! and version names, the status the stored version has reached (`DRAFT` as a create
//! records it, and `PUBLISHED`/`DEPRECATED` once `UpdatePackageVersion` has moved it on),
//! whether the repeat carries no `clientToken` or one the record was not minted with, and
//! the region the call is signed for — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::NEW_PACKAGE_VERSION_STATUS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// The wire error shape of a create that conflicts with the record already stored.
const CONFLICT: &str = "ConflictException";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name
/// and `region`: the scope is how the router picks the claiming service. The signature
/// itself is never checked here — the harness turns request authentication off, exactly
/// as an unauthenticated emulator runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>, region: &str) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
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
            error_type,
            body,
        }
    }

    /// `CreatePackage`: `PUT /packages/{packageName}` — the parent a version hangs off.
    async fn create_package(&self, package: &str, region: &str) -> Reply {
        self.call("PUT", &format!("/packages/{package}"), Some(&json!({})), region)
            .await
    }

    /// `CreatePackageVersion`: `PUT /packages/{packageName}/versions/{versionName}`, with
    /// `clientToken` bound to the query string
    /// (`smithy_query(query="operation",operation="CreatePackageVersion")`).
    async fn create_version(
        &self,
        package: &str,
        version: &str,
        body: &Value,
        client_token: Option<&str>,
        region: &str,
    ) -> Reply {
        let path = match client_token {
            Some(token) => format!("/packages/{package}/versions/{version}?clientToken={token}"),
            None => format!("/packages/{package}/versions/{version}"),
        };
        self.call("PUT", &path, Some(body), region).await
    }

    /// `UpdatePackageVersion`: `PATCH /packages/{packageName}/versions/{versionName}` —
    /// how a version leaves the status a create records.
    async fn move_version(&self, package: &str, version: &str, action: &str, region: &str) -> Reply {
        self.call(
            "PATCH",
            &format!("/packages/{package}/versions/{version}"),
            Some(&json!({ "action": action })),
            region,
        )
        .await
    }

    /// `GetPackageVersion`: `GET /packages/{packageName}/versions/{versionName}`.
    async fn get_version(&self, package: &str, version: &str, region: &str) -> Reply {
        self.call(
            "GET",
            &format!("/packages/{package}/versions/{version}"),
            None,
            region,
        )
        .await
    }
}

/// The sentence a create of a pair the catalog already holds must be refused with: the
/// fixed text of the catalog's duplicate-request conditional check, with the package name
/// and then the version name from the REQUEST filling it.
fn duplicate_message(package: &str, version: &str) -> String {
    format!(
        "Conditional check failure: Received duplicate request for package {package} \
         version {version}"
    )
}

/// One stored version, and the repeat create that must be refused.
#[derive(Debug)]
struct Case {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// `PackageName`: length 1..128, pattern `^[a-zA-Z0-9-_.]+$`
    /// (`smithy_query(query="shape",shape="PackageName")`).
    package: String,
    /// `VersionName`: length 1..64, pattern `^[a-zA-Z0-9-_.]+$`
    /// (`smithy_query(query="shape",shape="VersionName")`).
    version: String,
    /// The `UpdatePackageVersion$action` applied before the repeat, when the row moves
    /// the version off the status a create records (`PUBLISH` / `DEPRECATE`, the actions
    /// `smithy_query(query="shape",shape="PackageVersionAction")` lists).
    action: Option<&'static str>,
    /// The status the stored version holds when the repeat arrives.
    stored_status: &'static str,
    /// The `clientToken` the stored version was minted with, if any.
    minted_with: Option<&'static str>,
    /// The `clientToken` the repeat carries: `None` for a repeat with no token at all,
    /// and otherwise one the record was NOT minted with.
    repeated_with: Option<&'static str>,
    region: &'static str,
}

/// Every row of the table: the name forms at both ends of the two members' published
/// length bounds, each status a stored version can hold, and a repeat with no token and
/// one with a token the record does not carry.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a dotted version of a dash-separated package, still DRAFT, repeated \
                   with no token",
            package: "catalog-package".to_string(),
            version: "1.0.0".to_string(),
            action: None,
            stored_status: NEW_PACKAGE_VERSION_STATUS,
            minted_with: None,
            repeated_with: None,
            region: REGION,
        },
        Case {
            case: "the shortest legal names, PUBLISHED, repeated with no token",
            package: "p".to_string(),
            version: "v".to_string(),
            action: Some("PUBLISH"),
            stored_status: "PUBLISHED",
            minted_with: None,
            repeated_with: None,
            region: "eu-west-3",
        },
        Case {
            // 128 and 64 are the published maxima of `PackageName` and `VersionName`.
            case: "names at the published maximum length, DEPRECATED, repeated with no \
                   token",
            package: "p".repeat(128),
            version: "v".repeat(64),
            action: Some("DEPRECATE"),
            stored_status: "DEPRECATED",
            minted_with: None,
            repeated_with: None,
            region: "ap-southeast-2",
        },
        Case {
            case: "an underscored version of a dotted package, PUBLISHED, repeated with \
                   another token",
            package: "org.example.firmware".to_string(),
            version: "2026_11_05-rc_1".to_string(),
            action: Some("PUBLISH"),
            stored_status: "PUBLISHED",
            minted_with: Some("00000000-0000-4000-8000-000000000001-minted"),
            repeated_with: Some("00000000-0000-4000-8000-000000000002-repeat"),
            region: REGION,
        },
        Case {
            case: "a version minted with no token, still DRAFT, repeated WITH a token",
            package: "mixed_Case-9.pkg".to_string(),
            version: "Build.0042-beta_7".to_string(),
            action: None,
            stored_status: NEW_PACKAGE_VERSION_STATUS,
            minted_with: None,
            repeated_with: Some("00000000-0000-4000-8000-000000000003-repeat"),
            region: "us-west-2",
        },
    ]
}

/// The duplicate-version refusal: its status, its error shape, its sentence, and that it
/// leaves the stored record alone — over every row of the table.
#[tokio::test]
async fn a_duplicate_package_version_create_is_refused_as_a_duplicate_request() {
    for case in cases() {
        // A fresh emulator per row, so no row's refusal depends on what another left.
        let emulator = Emulator::new();
        let Case {
            case: label,
            package,
            version,
            action,
            stored_status,
            minted_with,
            repeated_with,
            region,
        } = case;

        let created_package = emulator.create_package(&package, region).await;
        assert_eq!(
            created_package.status, 200,
            "case {label:?}: the package must be creatable, got {created_package:?}"
        );
        let created = emulator
            .create_version(
                &package,
                &version,
                &json!({ "description": "the record the repeat must not touch" }),
                minted_with,
                region,
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the first create must answer 200, got {created:?}"
        );

        if let Some(action) = action {
            let moved = emulator.move_version(&package, &version, action, region).await;
            assert_eq!(
                moved.status, 200,
                "case {label:?}: applying {action:?} must answer 200, got {moved:?}"
            );
        }
        let before = emulator.get_version(&package, &version, region).await;
        assert_eq!(
            before.text("status").as_deref(),
            Some(stored_status),
            "case {label:?}: expected the stored version at {stored_status:?}, got \
             {before:?}"
        );

        let refused = emulator
            .create_version(&package, &version, &json!({}), repeated_with, region)
            .await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: a repeat create must be refused 409, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(CONFLICT),
            "case {label:?}: expected x-amzn-errortype {CONFLICT:?}, got {:?}",
            refused.error_type
        );
        let expected = duplicate_message(&package, &version);
        assert_eq!(
            refused.message(),
            expected,
            "case {label:?}: expected message {expected:?}, got {:?}",
            refused.message()
        );

        let after = emulator.get_version(&package, &version, region).await;
        assert_eq!(
            after.body, before.body,
            "case {label:?}: the refused create must change nothing, got {after:?}"
        );
    }
}

/// The refusal names the pair from the REQUEST and nothing else: a second package holding
/// a version of the same name is refused about its OWN package, so the sentence is
/// rendered per request rather than carried from one record.
#[tokio::test]
async fn the_refusal_names_the_package_and_version_of_the_request() {
    let emulator = Emulator::new();
    let packages = ["left-package", "right.package", "third_package"];
    let version = "1.2.3";

    for package in packages {
        let created_package = emulator.create_package(package, REGION).await;
        assert_eq!(
            created_package.status, 200,
            "case {package:?}: the package must be creatable, got {created_package:?}"
        );
        let created = emulator
            .create_version(package, version, &json!({}), None, REGION)
            .await;
        assert_eq!(
            created.status, 200,
            "case {package:?}: the first create must answer 200, got {created:?}"
        );
    }

    for package in packages {
        let refused = emulator
            .create_version(package, version, &json!({}), None, REGION)
            .await;
        let expected = duplicate_message(package, version);
        assert_eq!(
            refused.message(),
            expected,
            "case {package:?}: expected message {expected:?}, got {:?}",
            refused.message()
        );
    }
}

/// The repeat that carries the token the record was minted with is NOT a conflict: it is
/// that one request arriving twice, so it answers the record. This pins the conflict
/// wording to the duplicate branch rather than to every second create.
#[tokio::test]
async fn the_minting_token_repeated_answers_the_record_instead_of_conflicting() {
    let emulator = Emulator::new();
    let token = "00000000-0000-4000-8000-000000000004-idempotent";
    let package = "idempotent-package";
    let version = "3.1.4";

    let created_package = emulator.create_package(package, REGION).await;
    assert_eq!(
        created_package.status, 200,
        "the package must be creatable, got {created_package:?}"
    );
    let created = emulator
        .create_version(package, version, &json!({}), Some(token), REGION)
        .await;
    assert_eq!(
        created.status, 200,
        "the first create must answer 200, got {created:?}"
    );

    let repeated = emulator
        .create_version(package, version, &json!({}), Some(token), REGION)
        .await;
    assert_eq!(
        repeated.status, 200,
        "the same request repeated must answer 200, got {repeated:?}"
    );
    assert_eq!(
        repeated.body, created.body,
        "the same request repeated must answer the same record, got {repeated:?}"
    );
}
