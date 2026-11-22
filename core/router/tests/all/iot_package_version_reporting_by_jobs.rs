// SPDX-License-Identifier: CC0-1.0

//! The PACKAGE VERSION REPORTING a finished IoT job performs, across THREE services: the
//! control plane that holds the account's package configuration and the job (`iot`), the
//! Jobs data plane the device finishes its execution through (`iot-jobs-data`), and the
//! Device Shadow data plane the reserved `$package` named shadow is read from
//! (`iot-data`).
//!
//! This suite needs the WHOLE router: the configuration is written by one crate, the
//! execution is finished through a second and the shadow the service writes is served by a
//! third, so a single-service test cannot see the effect at all.
//!
//! The rule under test: while an account has package-version reporting by jobs turned on
//! (`iot:UpdatePackageConfiguration$versionUpdateByJobsConfig` — "If configured, Jobs
//! updates the thing's reserved named shadow with the package version information up on
//! successful job completion"), the SERVICE — not the device — records what a job
//! installed as soon as a targeted thing's execution of that job reaches `SUCCEEDED`: the
//! thing's `$package` named shadow gains, under `state.reported`, one entry per ARN of the
//! job's `destinationPackageVersions` ("The package version Amazon Resource Names (ARNs)
//! that are installed on the device when the job successfully completes"), keyed by the
//! package name the ARN carries and holding that ARN's version name under `version`.
//! Nothing else writes it: a job that names no package version, a thing whose execution
//! has not finished and an account that has the reporting turned off are each reported as
//! a shadow that does not exist, and a later job's report MERGES into the shadow rather
//! than replacing it. The job's own association is readable too — `DescribeJob` reports
//! the `destinationPackageVersions` the create named.
//!
//! The rows differ along what the rule generalises over: how many package versions one job
//! installs (one, two, three), the form and length of the package and version names
//! (dotted, hyphenated, underscored, single-character, and a version name at its published
//! maximum length), and how many things one job targets.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The named shadow AWS IoT reserves for a thing's software package versions, which is the
/// shadow `versionUpdateByJobsConfig` reporting targets.
const PACKAGE_SHADOW: &str = "$package";

/// The role the account names when it turns the reporting on: "the role that grants
/// permission to the IoT jobs service to update the reserved named shadow when the job
/// successfully completes" (`VersionUpdateByJobsConfig$roleArn`).
const REPORTING_ROLE: &str = "arn:aws:iam::000000000000:role/iot-package-version-reporting";

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One package version a job installs: the package that holds it and the version name.
///
/// The names exercise what `PackageName` (`len 1..128`, pattern `^[a-zA-Z0-9-_.]+$`) and
/// `VersionName` (`len 1..64`, same pattern) admit —
/// `smithy_query(query="shape",shape="PackageName,VersionName")`.
struct Installed {
    package_name: &'static str,
    version_name: &'static str,
}

/// One job, the devices it targets and the package versions it installs.
struct Row {
    case: &'static str,
    job_id: &'static str,
    /// The devices the job targets. The FIRST one finishes its execution; any other must
    /// be left with no `$package` shadow at all, because its own execution has not
    /// finished.
    things: &'static [&'static str],
    installed: &'static [Installed],
}

/// A version name of exactly 64 characters — the published maximum length of
/// `VersionName` — built from what its pattern admits.
const LONGEST_VERSION_NAME: &str =
    "2026.01.01-release-candidate.with-a-very-long-but-legal-name-xyz";

/// The rows: one package version, two, and three, over package and version names of
/// different forms and lengths, and over jobs that target one device and two.
const ROWS: &[Row] = &[
    Row {
        case: "one package version, dotted version name",
        job_id: "pkg-report-single",
        things: &["pkg-report-laser-1"],
        installed: &[Installed {
            package_name: "fleet-firmware",
            version_name: "1.0.0",
        }],
    },
    Row {
        case: "two package versions, underscored and single-character names",
        job_id: "pkg-report-pair",
        things: &["pkg-report-laser-2"],
        installed: &[
            Installed {
                package_name: "edge_agent",
                version_name: "20260101",
            },
            Installed {
                package_name: "a",
                version_name: "1",
            },
        ],
    },
    Row {
        case: "three package versions, one at the published maximum version-name length",
        job_id: "pkg-report-triple",
        things: &["pkg-report-laser-3", "pkg-report-laser-4"],
        installed: &[
            Installed {
                package_name: "sensor.driver",
                version_name: "2.1.3-rc.1",
            },
            Installed {
                package_name: "boot.loader-v2",
                version_name: LONGEST_VERSION_NAME,
            },
            Installed {
                package_name: "telemetry_collector.agent",
                version_name: "0.9",
            },
        ],
    },
];

/// What `state.reported` must hold once a row's job has been reported: one entry per
/// installed package version, keyed by package name.
fn expected_reported(installed: &[Installed]) -> Value {
    Value::Object(
        installed
            .iter()
            .map(|entry| {
                (
                    entry.package_name.to_string(),
                    json!({ "version": entry.version_name }),
                )
            })
            .collect(),
    )
}

#[test]
fn a_succeeded_job_reports_its_package_versions_into_the_things_package_shadow() {
    std::thread::Builder::new()
        .name("iot-package-version-reporting".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let call = |method: &str, url: String, signing_name: &str, body: String| {
                let request = http::Request::builder()
                    .method(method)
                    .uri(url)
                    .header("authorization", authorization(signing_name))
                    .header("content-type", "application/json")
                    .body(bytes::Bytes::from(body))
                    .expect("a well-formed request");
                let response = runtime.block_on(router.dispatch(request));
                let status = response.status().as_u16();
                let error_code = response
                    .headers()
                    .get("x-amzn-errortype")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let rendered = String::from_utf8_lossy(response.body()).to_string();
                let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
                (status, error_code, rendered, parsed)
            };

            // The control plane, signed for `iot`.
            let control = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://iot.{REGION}.amazonaws.com{path}"),
                    "iot",
                    body,
                )
            };
            // The Jobs data plane, signed for `iot-jobs-data`.
            let device = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://data.jobs.iot.{REGION}.amazonaws.com{path}"),
                    "iot-jobs-data",
                    body,
                )
            };
            // The Device Shadow data plane, signed for `iotdata`.
            let shadows = |method: &str, path: String| {
                call(
                    method,
                    format!("https://data-ats.iot.{REGION}.amazonaws.com{path}"),
                    "iotdata",
                    String::new(),
                )
            };

            // `GET /things/{thingName}/shadow?name=$package`, as its status, error code and
            // parsed document.
            let package_shadow = |thing: &str| -> (u16, String, String, Value) {
                shadows(
                    "GET",
                    format!("/things/{thing}/shadow?name={PACKAGE_SHADOW}"),
                )
            };

            // Register a thing, and a package version in the catalog, and create the job
            // that installs it; the device then finishes its execution.
            let register_thing = |case: &str, thing: &str| {
                let (status, _, rendered, _) =
                    control("POST", format!("/things/{thing}"), "{}".to_string());
                assert_eq!(
                    status, 200,
                    "case {case:?}: registering {thing} must answer 200, got {status} \
                     with {rendered}"
                );
            };
            // A published package version, and the ARN the catalog minted for it — which is
            // the ARN a job names and the one the reporting reads the names out of.
            let publish_version = |case: &str, package: &str, version: &str| -> String {
                let (status, _, rendered, _) =
                    control("PUT", format!("/packages/{package}"), "{}".to_string());
                assert!(
                    status == 200 || status == 409,
                    "case {case:?}: creating package {package} must answer 200 (or 409 when \
                     it already exists), got {status} with {rendered}"
                );
                let (status, _, rendered, parsed) = control(
                    "PUT",
                    format!("/packages/{package}/versions/{version}"),
                    "{}".to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: creating version {version} of {package} must answer 200, \
                     got {status} with {rendered}"
                );
                let arn = parsed
                    .get("packageVersionArn")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: no packageVersionArn in {rendered}")
                    })
                    .to_string();
                // "The package version must be in either the Published or Deprecated state
                // when the job deploys" — `CreateJobRequest$destinationPackageVersions`.
                let (status, _, rendered, _) = control(
                    "PATCH",
                    format!("/packages/{package}/versions/{version}"),
                    json!({ "action": "PUBLISH" }).to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: publishing {package} {version} must answer 200, got \
                     {status} with {rendered}"
                );
                arn
            };
            let create_job =
                |case: &str, job_id: &str, things: &[&str], package_versions: &[String]| {
                    let request = json!({
                        "targets": things
                            .iter()
                            .map(|thing| format!(
                                "arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing}"
                            ))
                            .collect::<Vec<String>>(),
                        "targetSelection": "SNAPSHOT",
                        "document": r#"{"operation":"install"}"#,
                        "destinationPackageVersions": package_versions,
                    });
                    let (status, _, rendered, _) =
                        control("PUT", format!("/jobs/{job_id}"), request.to_string());
                    assert_eq!(
                        status, 200,
                        "case {case:?}: creating job {job_id} must answer 200, got {status} \
                         with {rendered}"
                    );
                };
            let finish_execution = |case: &str, thing: &str, job_id: &str| {
                let (status, _, rendered, parsed) = device(
                    "PUT",
                    format!("/things/{thing}/jobs/$next"),
                    "{}".to_string(),
                );
                assert_eq!(
                    (status, parsed["execution"]["jobId"].as_str()),
                    (200, Some(job_id)),
                    "case {case:?}: {thing} must be handed its execution of {job_id}, got \
                     {status} with {rendered}"
                );
                let (status, _, rendered, _) = device(
                    "POST",
                    format!("/things/{thing}/jobs/{job_id}"),
                    json!({ "status": "SUCCEEDED" }).to_string(),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: finishing {job_id} on {thing} must answer 200, got \
                     {status} with {rendered}"
                );
            };
            // Turn the account's reporting on or off, and prove the configuration reads back.
            let configure_reporting = |enabled: bool| {
                let mut config = json!({ "enabled": enabled });
                if enabled {
                    config["roleArn"] = json!(REPORTING_ROLE);
                }
                let (status, _, rendered, _) = control(
                    "PATCH",
                    "/package-configuration".to_string(),
                    json!({ "versionUpdateByJobsConfig": config }).to_string(),
                );
                assert_eq!(
                    status, 200,
                    "turning the reporting {} must answer 200, got {status} with {rendered}",
                    if enabled { "on" } else { "off" }
                );
                let (status, _, rendered, parsed) =
                    control("GET", "/package-configuration".to_string(), String::new());
                assert_eq!(
                    (
                        status,
                        parsed["versionUpdateByJobsConfig"]["enabled"].as_bool()
                    ),
                    (200, Some(enabled)),
                    "the configuration must read back as written, got {status} with {rendered}"
                );
            };

            // PHASE 0 — an account that has NOT turned the reporting on: a job that names a
            // package version still finishes, and no `$package` shadow is written for it.
            // The configuration is what arranges the effect, so without it there is nothing
            // to read.
            let case = "reporting never configured";
            register_thing(case, "pkg-report-unconfigured");
            let unconfigured_arn = publish_version(case, "unconfigured-firmware", "1.0.0");
            create_job(
                case,
                "pkg-report-unconfigured-job",
                &["pkg-report-unconfigured"],
                &[unconfigured_arn],
            );
            finish_execution(case, "pkg-report-unconfigured", "pkg-report-unconfigured-job");
            let (status, error_code, rendered, _) = package_shadow("pkg-report-unconfigured");
            assert_eq!(
                (status, error_code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: with the reporting off no shadow may exist, got {status} \
                 {error_code} with {rendered}"
            );

            // PHASE 1 — with the reporting on, every row's finished execution is reported.
            configure_reporting(true);

            for row in ROWS {
                let case = row.case;
                for thing in row.things {
                    register_thing(case, thing);
                }
                let arns: Vec<String> = row
                    .installed
                    .iter()
                    .map(|entry| publish_version(case, entry.package_name, entry.version_name))
                    .collect();
                create_job(case, row.job_id, row.things, &arns);

                // The job's own association is readable: `DescribeJob` reports the ARNs the
                // create named, in that order.
                let (status, _, rendered, parsed) =
                    control("GET", format!("/jobs/{}", row.job_id), String::new());
                assert_eq!(
                    (status, parsed["job"]["destinationPackageVersions"].clone()),
                    (200, json!(arns)),
                    "case {case:?}: expected the job to report {arns:?}, got {status} \
                     with {rendered}"
                );

                // Until the device finishes, there is nothing to report.
                let thing = row.things[0];
                let (status, error_code, rendered, _) = package_shadow(thing);
                assert_eq!(
                    (status, error_code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: before the execution finishes no shadow may exist, got \
                     {status} {error_code} with {rendered}"
                );

                finish_execution(case, thing, row.job_id);

                let expected = expected_reported(row.installed);
                let (status, _, rendered, parsed) = package_shadow(thing);
                assert_eq!(
                    (status, parsed["state"]["reported"].clone()),
                    (200, expected.clone()),
                    "case {case:?}: expected {expected} reported for {thing}, got {status} \
                     with {rendered}"
                );
                assert_eq!(
                    parsed["version"].as_i64(),
                    Some(1),
                    "case {case:?}: the first write of a shadow is version 1, got {rendered}"
                );
                for entry in row.installed {
                    assert!(
                        parsed["metadata"]["reported"][entry.package_name]["version"]
                            ["timestamp"]
                            .as_i64()
                            .unwrap_or_default()
                            > 0,
                        "case {case:?}: every reported leaf carries a write timestamp, got \
                         {rendered}"
                    );
                }

                // Reading it does not change it.
                let (_, _, rendered_again, parsed_again) = package_shadow(thing);
                assert_eq!(
                    parsed_again["state"], parsed["state"],
                    "case {case:?}: a read must not change the shadow, got {rendered_again}"
                );
                assert_eq!(
                    parsed_again["version"], parsed["version"],
                    "case {case:?}: a read must not consume a version, got {rendered_again}"
                );

                // A thing of the same job whose own execution has NOT finished has nothing
                // reported for it: the reporting follows the EXECUTION, not the job.
                for other in row.things.iter().skip(1) {
                    let (status, error_code, rendered, _) = package_shadow(other);
                    assert_eq!(
                        (status, error_code.as_str()),
                        (404, "ResourceNotFoundException"),
                        "case {case:?}: {other} has not finished, so it must have no shadow, \
                         got {status} {error_code} with {rendered}"
                    );
                }
            }

            // PHASE 2 — a LATER job's report merges into the shadow the first one wrote: the
            // package it installs is added and the entries already there survive.
            let case = "a second job merges into the shadow";
            let thing = ROWS[0].things[0];
            let merged_arn = publish_version(case, "fleet-firmware", "2.0.0");
            let added_arn = publish_version(case, "diagnostics", "7");
            create_job(
                case,
                "pkg-report-merge",
                &[thing],
                &[merged_arn, added_arn],
            );
            finish_execution(case, thing, "pkg-report-merge");
            let (status, _, rendered, parsed) = package_shadow(thing);
            assert_eq!(
                (status, parsed["state"]["reported"].clone()),
                (
                    200,
                    json!({
                        // Replaced by the later job, which installed another version of it.
                        "fleet-firmware": { "version": "2.0.0" },
                        // Added by the later job.
                        "diagnostics": { "version": "7" },
                    })
                ),
                "case {case:?}: expected the merged report for {thing}, got {status} \
                 with {rendered}"
            );
            assert_eq!(
                parsed["version"].as_i64(),
                Some(2),
                "case {case:?}: the second write consumes the next version, got {rendered}"
            );

            // PHASE 3 — with the reporting turned OFF again, a finished job reports nothing:
            // a thing that has never been reported for stays without a shadow, and the one
            // already written is left exactly as it was.
            let before = parsed["state"].clone();
            configure_reporting(false);
            let case = "reporting turned back off";
            register_thing(case, "pkg-report-laser-off");
            let off_arn = publish_version(case, "fleet-firmware", "3.0.0");
            create_job(
                case,
                "pkg-report-off",
                &["pkg-report-laser-off", thing],
                &[off_arn],
            );
            finish_execution(case, "pkg-report-laser-off", "pkg-report-off");
            finish_execution(case, thing, "pkg-report-off");
            let (status, error_code, rendered, _) = package_shadow("pkg-report-laser-off");
            assert_eq!(
                (status, error_code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: with the reporting off nothing may be written, got {status} \
                 {error_code} with {rendered}"
            );
            let (status, _, rendered, parsed) = package_shadow(thing);
            assert_eq!(
                (status, parsed["state"].clone(), parsed["version"].as_i64()),
                (200, before, Some(2)),
                "case {case:?}: the shadow written while the reporting was on must be \
                 untouched, got {status} with {rendered}"
            );
        })
        .expect("spawn the package-version reporting thread")
        .join()
        .expect("a succeeded job must report its package versions into the $package shadow");
}
