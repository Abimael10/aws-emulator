// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the SOFTWARE BILL OF MATERIALS a package version
//! carries: `AssociateSbomWithPackageVersion` and the `GetPackageVersion` reader that
//! proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: a PUT that attaches ONE sub-resource to a resource records the
//! value it carries on the resource it names, answers with the record it persisted and
//! with the status of the work that attachment started, and the single-resource reader
//! reports that same record; attaching a value the resource does not hold yet replaces
//! whatever it held; attaching the value it ALREADY holds records nothing twice and is
//! refused with the operation's modelled conflict (unless it repeats the
//! `@idempotencyToken` that attached it, when it is that one request arriving twice);
//! and a resource the store does not hold is refused with the modelled not-found error
//! instead of a success. Here that means `AssociateSbomWithPackageVersion` records the
//! `sbom` on the version named by `(packageName, versionName)` and answers
//! `packageName` / `versionName` / `sbom` / `sbomValidationStatus` — the `IN_PROGRESS`
//! the validation it starts is answered in — `GetPackageVersion` reports that same
//! `sbom` and the status that validation holds by the read, a second association of a
//! DIFFERENT bill of materials replaces the one held, a second association of the SAME
//! one is a 409 `ConflictException`, and a `(packageName, versionName)` pair the catalog
//! does not hold is a 404 `ResourceNotFoundException`.
//!
//! The table spans what that rule generalises over — which members of the SBOM's
//! `s3Location` the request names beside the bucket `version` it pins (`bucket`, `key`,
//! each alone, and a location naming none of them at all), whether the association
//! carries a `clientToken` and how long it is, the
//! names that identify the version, and the region the call is signed for — rather than
//! one recorded request.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::SBOM_VALIDATION_STATUS_VALUES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// The status an association's validation is answered in. A value of
/// `SbomValidationStatus` (`smithy_query(query="shape",shape="SbomValidationStatus")`),
/// which is `IN_PROGRESS | FAILED | SUCCEEDED`.
const IN_PROGRESS: &str = "IN_PROGRESS";

/// The status that validation holds once it has completed — the terminal value of the
/// same modelled enum.
const SUCCEEDED: &str = "SUCCEEDED";

/// How long a case waits for the validation an association started to complete before
/// failing: far longer than the span the emulator records, so a loaded machine does not
/// fail a case.
const VALIDATION_DEADLINE: Duration = Duration::from_secs(60);

/// How often the version is read while waiting for its validation to complete.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

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
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The error message a modelled failure carries.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
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

    /// `CreatePackage`: `PUT /packages/{packageName}`.
    async fn create_package(&self, package: &str, region: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/packages/{package}"),
            Some(&json!({})),
            region,
        )
        .await
    }

    /// `CreatePackageVersion`: `PUT /packages/{packageName}/versions/{versionName}`.
    async fn create_version(
        &self,
        package: &str,
        version: &str,
        body: &Value,
        region: &str,
    ) -> Reply {
        self.call(
            "PUT",
            &format!("/packages/{package}/versions/{version}"),
            Some(body),
            region,
        )
        .await
    }

    /// `AssociateSbomWithPackageVersion`:
    /// `PUT /packages/{packageName}/versions/{versionName}/sbom`, with `clientToken`
    /// bound to the query string
    /// (`smithy_query(query="operation",operation="AssociateSbomWithPackageVersion")`).
    async fn associate_sbom(
        &self,
        package: &str,
        version: &str,
        sbom: &Value,
        client_token: Option<&str>,
        region: &str,
    ) -> Reply {
        let path = match client_token {
            Some(token) => {
                format!("/packages/{package}/versions/{version}/sbom?clientToken={token}")
            }
            None => format!("/packages/{package}/versions/{version}/sbom"),
        };
        self.call("PUT", &path, Some(&json!({ "sbom": sbom })), region)
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

    /// The version's `sbomValidationStatus` once the validation its association started
    /// has completed, waited for by READING the version until it reports something other
    /// than the in-progress status or [`VALIDATION_DEADLINE`] passes — never by assuming
    /// how long the host takes to get there.
    async fn validation_status_when_settled(
        &self,
        package: &str,
        version: &str,
        region: &str,
    ) -> String {
        let deadline = tokio::time::Instant::now() + VALIDATION_DEADLINE;
        loop {
            let read = self.get_version(package, version, region).await;
            let status = read.text("sbomValidationStatus").unwrap_or_default();
            if status != IN_PROGRESS || tokio::time::Instant::now() >= deadline {
                return status;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// One association request: a label, the names that identify the version, the region it
/// is signed for, the `sbom` the association carries, and the `clientToken` it carries.
struct Case {
    label: &'static str,
    package: String,
    version: String,
    region: &'static str,
    sbom: Value,
    client_token: Option<String>,
}

/// The table every case list below is built from. It spans what the rule generalises
/// over:
///
/// * which members of the SBOM's `s3Location` the request names beside the bucket
///   `version` every location pins — `Sbom` is one
///   optional `s3Location` and an `S3Location` is `bucket` / `key` / `version`
///   (`smithy_query(query="shape",shape="Sbom,S3Location")`), so the rows cover
///   a bucket and a key, each one alone, and an SBOM naming no location at
///   all;
/// * whether the association carries a `clientToken` — the `@idempotencyToken`, modelled
///   `length 36..64, pattern ^\S{36,64}$`, so the rows cover absent, the shortest legal
///   token and the longest;
/// * the form and length of `versionName` — modelled
///   `length 1..64, pattern ^[a-zA-Z0-9-_.]+$` — so the rows include the shortest legal
///   name, the longest and one using every character class the pattern allows;
/// * the region the call is signed for.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a bucket and a key, no client token",
            package: "sbom-package".to_string(),
            version: "1.0.0".to_string(),
            region: REGION,
            sbom: json!({
                "s3Location": {
                    "bucket": "sbom-bucket",
                    "key": "sboms/v1.spdx.json",
                    "version": "1HL4kqtJlcpXroDTDmk"
                }
            }),
            client_token: None,
        },
        Case {
            label: "a versioned S3 object and the shortest legal client token",
            package: "versioned-sbom-package".to_string(),
            version: "v".to_string(),
            region: REGION,
            sbom: json!({
                "s3Location": {
                    "bucket": "sbom-bucket",
                    "key": "sboms/v2.cdx.json",
                    "version": "3HL4kqtJlcpXroDTDmJ"
                }
            }),
            client_token: Some("t".repeat(36)),
        },
        Case {
            label: "a bucket alone and the longest legal client token",
            package: "bucket.only_package-1".to_string(),
            version: "Ver_01-aZ.9".to_string(),
            region: "eu-west-2",
            sbom: json!({ "s3Location": { "bucket": "sbom-bucket", "version": "2HL4kqtJlcpXroDTDmk" } }),
            client_token: Some("T".repeat(64)),
        },
        Case {
            label: "a key alone, in another region",
            package: "key-only-package".to_string(),
            version: "2.0.0-rc1".to_string(),
            region: "ap-southeast-1",
            sbom: json!({
                "s3Location": { "key": "sboms/key-only.json", "version": "4HL4kqtJlcpXroDTDmk" }
            }),
            client_token: None,
        },
        Case {
            label: "an SBOM naming no S3 location at all",
            package: "locationless-package".to_string(),
            version: "l".repeat(64),
            region: REGION,
            sbom: json!({}),
            client_token: None,
        },
    ]
}

/// A second bill of materials for the same version, differing from the case's own in
/// every member that names the object it is: the replacement a "a different SBOM
/// replaces the one held" case associates.
fn other_sbom(label: &str) -> Value {
    json!({
        "s3Location": {
            "bucket": "replacement-bucket",
            "key": format!("sboms/{}.spdx.json", label.replace(' ', "-")),
            "version": "ZZZ.replacement"
        }
    })
}

/// Create the package and the version a case associates its SBOM with, and answer what
/// `GetPackageVersion` reported for the version before any association.
async fn version_without_sbom(emulator: &Emulator, case: &Case) -> Reply {
    let Case {
        label,
        package,
        version,
        region,
        ..
    } = case;
    assert_eq!(
        emulator.create_package(package, region).await.status,
        200,
        "case {label:?}: the package must be creatable"
    );
    let created = emulator
        .create_version(
            package,
            version,
            &json!({
                "description": "a version that will carry a bill of materials",
                "artifact": { "s3Location": { "bucket": "ota-bucket", "key": "firmware.bin" } },
            }),
            region,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: the version must be creatable, got {created:?}"
    );
    let read = emulator.get_version(package, version, region).await;
    assert!(
        read.body.get("sbom").is_none() && read.body.get("sbomValidationStatus").is_none(),
        "case {label:?}: a version nobody associated an SBOM with carries none, got {read:?}"
    );
    read
}

#[tokio::test]
async fn an_association_records_the_sbom_the_version_reads_back() {
    let emulator = Emulator::new();

    for case in cases() {
        let Case {
            label,
            package,
            version,
            region,
            sbom,
            client_token,
        } = &case;
        let before = version_without_sbom(&emulator, &case).await;

        let associated = emulator
            .associate_sbom(package, version, sbom, client_token.as_deref(), region)
            .await;
        assert_eq!(
            associated.status, 200,
            "case {label:?}: an association with an existing version must succeed, \
             got {associated:?}"
        );
        assert_eq!(
            associated.text("packageName").as_ref(),
            Some(package),
            "case {label:?}: the association must report the package it recorded on, \
             got {associated:?}"
        );
        assert_eq!(
            associated.text("versionName").as_ref(),
            Some(version),
            "case {label:?}: the association must report the version it recorded on, \
             got {associated:?}"
        );
        assert_eq!(
            associated.body.get("sbom"),
            Some(sbom),
            "case {label:?}: the association must report the SBOM it recorded, \
             got {associated:?}"
        );
        // "The status of the INITIAL validation for the software bill of materials
        // against the Software Package Data Exchange (SPDX) and CycloneDX industry
        // standard formats": that validation has only just started, so the association
        // reports the non-terminal status.
        assert_eq!(
            associated.text("sbomValidationStatus").as_deref(),
            Some(IN_PROGRESS),
            "case {label:?}: expected the initial validation status {IN_PROGRESS:?}, \
             got {associated:?}"
        );

        let read = emulator.get_version(package, version, region).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: the version must stay readable, got {read:?}"
        );
        assert_eq!(
            read.body.get("sbom"),
            Some(sbom),
            "case {label:?}: the reader must report the SBOM the association recorded, \
             got {read:?}"
        );
        let status = read.text("sbomValidationStatus").unwrap_or_default();
        assert!(
            SBOM_VALIDATION_STATUS_VALUES.contains(&status.as_str()),
            "case {label:?}: the reported validation status must be a modelled \
             SbomValidationStatus, got {status:?}"
        );
        // The association changes the version's bill of materials and NOTHING else the
        // create recorded.
        for member in [
            "packageVersionArn",
            "description",
            "status",
            "creationDate",
            "artifact",
        ] {
            assert_eq!(
                read.body.get(member),
                before.body.get(member),
                "case {label:?}: an association must leave {member} as the create \
                 recorded it, got {read:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_validation_an_association_starts_completes() {
    let emulator = Emulator::new();

    for case in cases() {
        let Case {
            label,
            package,
            version,
            region,
            sbom,
            client_token,
        } = &case;
        version_without_sbom(&emulator, &case).await;

        assert_eq!(
            emulator
                .associate_sbom(package, version, sbom, client_token.as_deref(), region)
                .await
                .status,
            200,
            "case {label:?}: the association must succeed"
        );

        // The validation the association started completes on its own, whether or not
        // anybody reads the version, so a reader eventually reports the terminal status.
        let settled = emulator
            .validation_status_when_settled(package, version, region)
            .await;
        assert_eq!(
            settled, SUCCEEDED,
            "case {label:?}: the validation an association started must reach \
             {SUCCEEDED:?}, got {settled:?}"
        );

        // And reading does not move it anywhere else: the SBOM and its settled status
        // are what every later caller sees.
        let read = emulator.get_version(package, version, region).await;
        assert_eq!(
            read.text("sbomValidationStatus").as_deref(),
            Some(SUCCEEDED),
            "case {label:?}: a later read must report the settled status, got {read:?}"
        );
        assert_eq!(
            read.body.get("sbom"),
            Some(sbom),
            "case {label:?}: a later read must report the same SBOM, got {read:?}"
        );
    }
}

#[tokio::test]
async fn associating_the_sbom_the_version_already_holds_is_a_conflict() {
    let emulator = Emulator::new();

    for case in cases() {
        let Case {
            label,
            package,
            version,
            region,
            sbom,
            client_token,
        } = &case;
        version_without_sbom(&emulator, &case).await;

        let associated = emulator
            .associate_sbom(package, version, sbom, client_token.as_deref(), region)
            .await;
        assert_eq!(
            associated.status, 200,
            "case {label:?}: the first association must succeed, got {associated:?}"
        );

        // A second association of the SAME bill of materials, under a token that did
        // not attach it, would record nothing twice: it conflicts with the state the
        // version is in.
        let refused = emulator
            .associate_sbom(package, version, sbom, Some(&"d".repeat(36)), region)
            .await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: a duplicate association must be refused 409, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ConflictException"),
            "case {label:?}: a duplicate association must be refused with the modelled \
             ConflictException, got {refused:?}"
        );
        assert!(
            refused.message().contains(version.as_str())
                && refused.message().contains(package.as_str()),
            "case {label:?}: the refusal must name the version and the package it \
             conflicts with, got {:?}",
            refused.message()
        );

        // And it changed nothing: the SBOM the first association recorded is still the
        // one the version carries.
        let read = emulator.get_version(package, version, region).await;
        assert_eq!(
            read.body.get("sbom"),
            Some(sbom),
            "case {label:?}: a refused association must change nothing, got {read:?}"
        );

        // The one repeat that is NOT a duplicate is the same request arriving twice —
        // the association repeating the `clientToken` that attached the SBOM — which
        // answers from the record.
        if let Some(token) = client_token {
            let repeated = emulator
                .associate_sbom(package, version, sbom, Some(token), region)
                .await;
            assert_eq!(
                repeated.status, 200,
                "case {label:?}: the same request repeated must answer 200, got {repeated:?}"
            );
            for member in ["packageName", "versionName", "sbom"] {
                assert_eq!(
                    repeated.body.get(member),
                    associated.body.get(member),
                    "case {label:?}: the same request repeated must answer the record the \
                     first one wrote, got {repeated:?}"
                );
            }
            // The validation status is the one that holds at the repeat — which, the
            // record being the same one, is a value of the modelled enum and nothing
            // newly started.
            let status = repeated.text("sbomValidationStatus").unwrap_or_default();
            assert!(
                SBOM_VALIDATION_STATUS_VALUES.contains(&status.as_str()),
                "case {label:?}: the repeat must report a modelled SbomValidationStatus, \
                 got {status:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_different_sbom_replaces_the_one_the_version_holds() {
    let emulator = Emulator::new();

    for case in cases() {
        let Case {
            label,
            package,
            version,
            region,
            sbom,
            client_token,
        } = &case;
        version_without_sbom(&emulator, &case).await;

        assert_eq!(
            emulator
                .associate_sbom(package, version, sbom, client_token.as_deref(), region)
                .await
                .status,
            200,
            "case {label:?}: the first association must succeed"
        );

        // A bill of materials the version does NOT hold is a different request, not a
        // duplicate: it is recorded, and it is what the version carries from now on.
        let replacement = other_sbom(label);
        let replaced = emulator
            .associate_sbom(package, version, &replacement, None, region)
            .await;
        assert_eq!(
            replaced.status, 200,
            "case {label:?}: associating a different SBOM must succeed, got {replaced:?}"
        );
        assert_eq!(
            replaced.body.get("sbom"),
            Some(&replacement),
            "case {label:?}: the association must report the SBOM it recorded, \
             got {replaced:?}"
        );
        let read = emulator.get_version(package, version, region).await;
        assert_eq!(
            read.body.get("sbom"),
            Some(&replacement),
            "case {label:?}: the reader must report the SBOM that replaced the first, \
             got {read:?}"
        );

        // The replaced SBOM is no longer the one held, so associating it again is no
        // duplicate either — while the replacement now is.
        let back = emulator
            .associate_sbom(package, version, sbom, None, region)
            .await;
        assert_eq!(
            back.status, 200,
            "case {label:?}: the SBOM no longer held must be associable again, got {back:?}"
        );
        let duplicate = emulator
            .associate_sbom(package, version, sbom, None, region)
            .await;
        assert_eq!(
            duplicate.status, 409,
            "case {label:?}: the SBOM now held must refuse a duplicate, got {duplicate:?}"
        );
    }
}

#[tokio::test]
async fn an_association_with_a_version_the_catalog_does_not_hold_is_not_found() {
    let emulator = Emulator::new();

    let sbom = json!({
        "s3Location": {
            "bucket": "sbom-bucket",
            "key": "sboms/absent.json",
            "version": "5HL4kqtJlcpXroDTDmk"
        }
    });
    assert_eq!(
        emulator
            .create_package("present-package", REGION)
            .await
            .status,
        200,
        "the package must be creatable"
    );
    assert_eq!(
        emulator
            .create_version("present-package", "1.0.0", &json!({}), REGION)
            .await
            .status,
        200,
        "the version must be creatable"
    );

    // Neither half of the pair that identifies a version may be invented: an unknown
    // package and an unknown version of a package that exists are each a not-found.
    let absent = [
        ("absent-package", "1.0.0"),
        ("absent.package_2", "9.9.9"),
        ("present-package", "2.0.0"),
        ("present-package", "v"),
    ];
    for (package, version) in absent {
        let refused = emulator
            .associate_sbom(package, version, &sbom, None, REGION)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {package:?}/{version:?}: an association with an absent version must be \
             refused 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {package:?}/{version:?}: the refusal must be the modelled \
             ResourceNotFoundException, got {refused:?}"
        );
        assert!(
            refused.message().contains(package) && refused.message().contains(version),
            "case {package:?}/{version:?}: the refusal must name the pair it could not \
             find, got {:?}",
            refused.message()
        );
        let read = emulator.get_version(package, version, REGION).await;
        assert_eq!(
            read.status, 404,
            "case {package:?}/{version:?}: the refused association must record nothing, \
             got {read:?}"
        );
    }

    // The version that DOES exist takes the same association, and the pair is what
    // decides: a namesake version under another package stays without an SBOM.
    assert_eq!(
        emulator.create_package("other-package", REGION).await.status,
        200,
        "the second package must be creatable"
    );
    assert_eq!(
        emulator
            .create_version("other-package", "1.0.0", &json!({}), REGION)
            .await
            .status,
        200,
        "the second package's version must be creatable"
    );
    assert_eq!(
        emulator
            .associate_sbom("present-package", "1.0.0", &sbom, None, REGION)
            .await
            .status,
        200,
        "an association with the version that exists must succeed"
    );
    let neighbour = emulator.get_version("other-package", "1.0.0", REGION).await;
    assert!(
        neighbour.body.get("sbom").is_none(),
        "another package's version of the same name carries no SBOM, got {neighbour:?}"
    );
}
