// SPDX-License-Identifier: CC0-1.0

//! WHICH job executions the IoT package-version reporting writes a thing's reserved
//! `$package` named shadow for, across THREE services: the control plane that holds the
//! account's package configuration and the job (`iot`), the Jobs data plane the device
//! reports its execution through (`iot-jobs-data`), and the Device Shadow data plane the
//! `$package` shadow is read from (`iot-data`).
//!
//! The rule under test: while an account has package-version reporting by jobs turned on
//! (`iot:UpdatePackageConfiguration$versionUpdateByJobsConfig` — "Configuration to manage
//! job's package version reporting. This updates the thing's reserved named shadow that the
//! job targets"), the SERVICE records what a job installed into the targeted thing's
//! `$package` shadow when, and only when, that thing's execution of the job reaches
//! `SUCCEEDED` — "the package version Amazon Resource Names (ARNs) that are installed on
//! the device when the job SUCCESSFULLY completes"
//! (`iot:CreateJobRequest$destinationPackageVersions`). So the write is decided by the
//! status the device reports and by what the job names: every OTHER status a device may
//! report ends the execution without anything being reported, and a succeeded job that
//! names no package version has nothing to report either. The write is a merge into the
//! one shadow record `UpdateThingShadow` writes and `GetThingShadow` reads, so a report
//! lands beside what a device already put there rather than replacing the shadow.
//!
//! The rows differ along what the rule generalises over: the terminal status the device
//! reports (the whole `JobExecutionStatus` catalog a device may move an execution to —
//! `smithy_query(query="shape",shape="JobExecutionStatus")` reports
//! `QUEUED | IN_PROGRESS | SUCCEEDED | FAILED | TIMED_OUT | REJECTED | REMOVED |
//! CANCELED`), whether the device took the work up first or reported straight from the
//! queue, and whether the job names a package version at all.
//!
//! `core/router/tests/all/iot_package_version_reporting_by_jobs.rs` covers the other half
//! of the same rule — WHAT is written for a succeeded execution (one entry per ARN, keyed
//! by package name) and that the account's configuration is what arranges it at all.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-east-2";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The named shadow AWS IoT reserves for a thing's software package versions.
const PACKAGE_SHADOW: &str = "$package";

/// The role the account names when it turns the reporting on
/// (`VersionUpdateByJobsConfig$roleArn`).
const REPORTING_ROLE: &str = "arn:aws:iam::000000000000:role/iot-package-version-reporting";

/// The package every row's job installs, and the version of it that is published before
/// any job deploys it ("The package version must be in either the Published or Deprecated
/// state when the job deploys").
const PACKAGE_NAME: &str = "status-gated.firmware";
const VERSION_NAME: &str = "4.2.0-rc_1";

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// The one status a device can report that falls the package-version reporting due: "the
/// package version Amazon Resource Names (ARNs) that are installed on the device when the job
/// SUCCESSFULLY completes" (`iot:CreateJobRequest$destinationPackageVersions`).
const SUCCEEDED: &str = "SUCCEEDED";

/// One device, the job it is targeted by, and the status it reports for its execution.
struct Row {
    case: &'static str,
    thing: &'static str,
    job_id: &'static str,
    /// The status the device reports, from the `JobExecutionStatus` catalog above.
    status: &'static str,
    /// Whether the device takes the work up (`StartNextPendingJobExecution`, which leaves
    /// the execution `IN_PROGRESS`) before reporting `status`, or reports straight from
    /// the queue.
    takes_the_work_up: bool,
    /// Whether the job names the published package version.
    installs: bool,
}

/// Every status a device may move its execution to, over both ways it can get there: the two
/// non-terminal ones (a progress report, and an execution reported as still queued), every
/// terminal one, and a succeeded execution of a job that installs nothing. `REJECTED` is only
/// reachable from the queue ("Only queued jobs can be rejected"), so that is the way it is
/// driven here.
const ROWS: &[Row] = &[
    Row {
        case: "succeeded after the device took the work up",
        thing: "pkg-status-succeeded",
        job_id: "pkg-status-succeeded-job",
        status: SUCCEEDED,
        takes_the_work_up: true,
        installs: true,
    },
    Row {
        case: "succeeded straight from the queue",
        thing: "pkg-status-succeeded-from-queue",
        job_id: "pkg-status-succeeded-from-queue-job",
        status: SUCCEEDED,
        takes_the_work_up: false,
        installs: true,
    },
    Row {
        case: "failed",
        thing: "pkg-status-failed",
        job_id: "pkg-status-failed-job",
        status: "FAILED",
        takes_the_work_up: true,
        installs: true,
    },
    Row {
        case: "rejected from the queue",
        thing: "pkg-status-rejected",
        job_id: "pkg-status-rejected-job",
        status: "REJECTED",
        takes_the_work_up: false,
        installs: true,
    },
    Row {
        case: "timed out",
        thing: "pkg-status-timed-out",
        job_id: "pkg-status-timed-out-job",
        status: "TIMED_OUT",
        takes_the_work_up: true,
        installs: true,
    },
    Row {
        case: "canceled",
        thing: "pkg-status-canceled",
        job_id: "pkg-status-canceled-job",
        status: "CANCELED",
        takes_the_work_up: true,
        installs: true,
    },
    Row {
        case: "removed",
        thing: "pkg-status-removed",
        job_id: "pkg-status-removed-job",
        status: "REMOVED",
        takes_the_work_up: false,
        installs: true,
    },
    Row {
        case: "still in progress",
        thing: "pkg-status-in-progress",
        job_id: "pkg-status-in-progress-job",
        status: "IN_PROGRESS",
        takes_the_work_up: false,
        installs: true,
    },
    Row {
        case: "reported as still queued",
        thing: "pkg-status-queued",
        job_id: "pkg-status-queued-job",
        status: "QUEUED",
        takes_the_work_up: false,
        installs: true,
    },
    Row {
        case: "succeeded, but the job names no package version",
        thing: "pkg-status-succeeded-empty",
        job_id: "pkg-status-succeeded-empty-job",
        status: SUCCEEDED,
        takes_the_work_up: true,
        installs: false,
    },
];

#[test]
fn only_a_succeeded_execution_reports_the_package_versions_its_job_installs() {
    std::thread::Builder::new()
        .name("iot-package-reporting-status".to_string())
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
            let shadows = |method: &str, path: String, body: String| {
                call(
                    method,
                    format!("https://data-ats.iot.{REGION}.amazonaws.com{path}"),
                    "iotdata",
                    body,
                )
            };
            let package_shadow = |thing: &str| -> (u16, String, String, Value) {
                shadows(
                    "GET",
                    format!("/things/{thing}/shadow?name={PACKAGE_SHADOW}"),
                    String::new(),
                )
            };

            let register_thing = |case: &str, thing: &str| {
                let (status, _, rendered, _) =
                    control("POST", format!("/things/{thing}"), "{}".to_string());
                assert_eq!(
                    status, 200,
                    "case {case:?}: registering {thing} must answer 200, got {status} \
                     with {rendered}"
                );
            };
            // A published package version, and the ARN the catalog minted for it — the ARN
            // a job names and the one the reporting reads the names out of.
            let publish_version = |case: &str, package: &str, version: &str| -> String {
                let (status, _, rendered, _) =
                    control("PUT", format!("/packages/{package}"), "{}".to_string());
                assert_eq!(
                    status, 200,
                    "case {case:?}: creating package {package} must answer 200, got {status} \
                     with {rendered}"
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
            let create_job = |case: &str, job_id: &str, thing: &str, arns: &[String]| {
                let mut request = json!({
                    "targets": [format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing}")],
                    "targetSelection": "SNAPSHOT",
                    "document": r#"{"operation":"install"}"#,
                });
                if !arns.is_empty() {
                    request["destinationPackageVersions"] = json!(arns);
                }
                let (status, _, rendered, _) =
                    control("PUT", format!("/jobs/{job_id}"), request.to_string());
                assert_eq!(
                    status, 200,
                    "case {case:?}: creating job {job_id} must answer 200, got {status} \
                     with {rendered}"
                );
            };
            // `StartNextPendingJobExecution`: the device takes the work up, which leaves
            // its execution IN_PROGRESS.
            let take_the_work_up = |case: &str, thing: &str, job_id: &str| {
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
            };
            // `UpdateJobExecution`: the device reports `status`, and the reported execution
            // state is asserted so no row can pass on an update that never took effect.
            let report_status = |case: &str, thing: &str, job_id: &str, status_name: &str| {
                let (status, _, rendered, parsed) = device(
                    "POST",
                    format!("/things/{thing}/jobs/{job_id}"),
                    json!({
                        "status": status_name,
                        "includeJobExecutionState": true,
                    })
                    .to_string(),
                );
                assert_eq!(
                    (status, parsed["executionState"]["status"].as_str()),
                    (200, Some(status_name)),
                    "case {case:?}: reporting {status_name} for {job_id} on {thing} must \
                     answer 200 with that status, got {status} with {rendered}"
                );
            };

            // The account turns the reporting on, and the configuration reads back: it is
            // what arranges the effect every row below is judged against.
            let (status, _, rendered, _) = control(
                "PATCH",
                "/package-configuration".to_string(),
                json!({
                    "versionUpdateByJobsConfig": {
                        "enabled": true,
                        "roleArn": REPORTING_ROLE,
                    }
                })
                .to_string(),
            );
            assert_eq!(
                status, 200,
                "turning the reporting on must answer 200, got {status} with {rendered}"
            );
            let (status, _, rendered, parsed) =
                control("GET", "/package-configuration".to_string(), String::new());
            assert_eq!(
                (
                    status,
                    parsed["versionUpdateByJobsConfig"]["enabled"].as_bool(),
                    parsed["versionUpdateByJobsConfig"]["roleArn"].as_str(),
                ),
                (200, Some(true), Some(REPORTING_ROLE)),
                "the configuration must read back as written, got {status} with {rendered}"
            );

            let installed_arn = publish_version("the published package version", PACKAGE_NAME, VERSION_NAME);
            // What a reported `$package` shadow must hold for a row whose job installs it.
            let expected_reported = json!({ PACKAGE_NAME: { "version": VERSION_NAME } });

            for row in ROWS {
                let case = row.case;
                register_thing(case, row.thing);
                let arns: Vec<String> = if row.installs {
                    vec![installed_arn.clone()]
                } else {
                    Vec::new()
                };
                create_job(case, row.job_id, row.thing, &arns);

                // Before the device reports anything there is nothing to report.
                let (status, error_code, rendered, _) = package_shadow(row.thing);
                assert_eq!(
                    (status, error_code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: before the execution finishes no shadow may exist, got \
                     {status} {error_code} with {rendered}"
                );

                if row.takes_the_work_up {
                    take_the_work_up(case, row.thing, row.job_id);
                }
                report_status(case, row.thing, row.job_id, row.status);

                let (status, error_code, rendered, parsed) = package_shadow(row.thing);
                // What the rule decides, derived rather than declared per row: the service
                // reports what a job installed when, and only when, the execution of a job
                // that names a package version reaches SUCCEEDED.
                let must_be_reported = row.status == SUCCEEDED && row.installs;
                if must_be_reported {
                    assert_eq!(
                        (status, parsed["state"]["reported"].clone()),
                        (200, expected_reported.clone()),
                        "case {case:?}: expected {expected_reported} reported for \
                         {thing}, got {status} with {rendered}",
                        thing = row.thing
                    );
                } else {
                    assert_eq!(
                        (status, error_code.as_str()),
                        (404, "ResourceNotFoundException"),
                        "case {case:?}: an execution that reported {reported} must leave \
                         {thing} with no shadow, got {status} {error_code} with {rendered}",
                        reported = row.status,
                        thing = row.thing
                    );
                }
            }

            // The report MERGES into the one shadow record a client writes and reads: what
            // a device put there itself survives the service's own write, and the service's
            // entry is read back through the same `GetThingShadow`.
            let case = "the service reports into a shadow the device already wrote";
            let thing = "pkg-status-device-written";
            register_thing(case, thing);
            let (status, _, rendered, parsed) = shadows(
                "POST",
                format!("/things/{thing}/shadow?name={PACKAGE_SHADOW}"),
                json!({ "state": { "reported": { "device-written": { "version": "0.1" } } } })
                    .to_string(),
            );
            assert_eq!(
                (status, parsed["version"].as_i64()),
                (200, Some(1)),
                "case {case:?}: the device's own write must answer 200 at version 1, got \
                 {status} with {rendered}"
            );
            create_job(
                case,
                "pkg-status-merge-job",
                thing,
                std::slice::from_ref(&installed_arn),
            );
            take_the_work_up(case, thing, "pkg-status-merge-job");
            report_status(case, thing, "pkg-status-merge-job", "SUCCEEDED");
            let (status, _, rendered, parsed) = package_shadow(thing);
            assert_eq!(
                (
                    status,
                    parsed["state"]["reported"].clone(),
                    parsed["version"].as_i64()
                ),
                (
                    200,
                    json!({
                        "device-written": { "version": "0.1" },
                        PACKAGE_NAME: { "version": VERSION_NAME },
                    }),
                    Some(2)
                ),
                "case {case:?}: the report must merge into what the device wrote, got \
                 {status} with {rendered}"
            );
        })
        .expect("spawn the package-version reporting status thread")
        .join()
        .expect("only a succeeded execution may report its job's package versions");
}
