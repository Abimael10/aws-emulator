// SPDX-License-Identifier: CC0-1.0

//! How a manual DEPLOYMENT's job and its upload destinations reach the wire.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `amplify` signing name, and every upload FOLLOWS the
//! URL the service published, with no credential of its own — so what is asserted is
//! what an AWS SDK client would see.
//!
//! The rules under test, each over a TABLE spanning the class it generalises over rather
//! than over one recorded request:
//!
//! 1. A job summary reports a member only where the job HAS a value for it. A job that
//!    deploys CONTENT ran no commit of a third-party repository provider and is no kind
//!    of repository build, so `commitId`, `commitMessage`, `commitTime` and `jobType`
//!    are absent from every answer carrying it — never the empty string, and never an
//!    invented instant — and a job whose work has not run yet reports no `startTime`
//!    either. Asserted wherever such a summary is answered: the `StartDeployment`
//!    response, `GetJob`, `ListJobs` and `StopJob`.
//! 2. The two ways a summary names its job are not the same string: `jobId` is the
//!    branch's bare job number, and the last segment of `jobArn` is that number in a
//!    ten-digit field.
//! 3. An upload destination `CreateDeployment` hands back is a PRESIGNED URL on the base
//!    this emulator publishes — it carries the SigV4 query fields, because it is
//!    followed with no `Authorization` header — and the two destination members follow
//!    the form of the request: a request naming files is answered with `fileUploadUrls`
//!    and NO `zipUploadUrl` member, and one naming none with `zipUploadUrl` and NO
//!    `fileUploadUrls` member. Neither is ever sent empty. One destination is minted per
//!    file the request names, and no more than a deployment may carry.
//! 4. A `BUCKET_PREFIX` source URL names a FOLDER of a bucket, so one that does not end
//!    in the path separator is refused with the modelled 400 before any job exists, and
//!    spends no job number.
//!
//! The harness (`Emulator`, `Reply`) is the one `app_branch_lifecycle` already drives;
//! only the cases are new.

use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_amplify_amplify::ops::start_deployment::{
    MAX_DEPLOYMENT_ASSETS, PREFIX_WITHOUT_TRAILING_SEPARATOR,
};

use crate::app_branch_lifecycle::{list_member, string_member, Emulator, Reply};

/// How wide the job-number field of a `jobArn` is.
///
/// `JobArn` is modelled as free text and `JobId` as `pattern ^[0-9]+$`
/// (`smithy_query(query="shape",shape="JobSummary")`), so the form is recorded rather
/// than modelled: Amplify writes the job number into a fixed-width field of leading
/// zeroes in the ARN while `jobId` reports it bare.
const JOB_ARN_NUMBER_DIGITS: usize = 10;

/// The members a job that deploys CONTENT has no value for, whatever it deploys and
/// whether or not its work has run.
///
/// Source: `JobSummary`'s member list
/// (`smithy_query(query="shape",shape="JobSummary")`). `commitId`, `commitMessage` and
/// `commitTime` describe the commit "from a third-party repository provider for the
/// job", which a manually deployed app has none of, and `jobType` names which kind of
/// job ran — `MANUAL`, `RELEASE`, `RETRY` or `WEB_HOOK` — which a deployment of content
/// is none of.
const UNREPORTED_ON_A_DEPLOYMENT: &[&str] =
    &["commitId", "commitMessage", "commitTime", "jobType"];

/// The member a job reports only once its work HAS run: "the start date and time for the
/// job" (`smithy_query(query="shape",shape="JobSummary")`), which a queued job has not
/// reached.
const START_TIME: &str = "startTime";

/// The status a job carries while nothing has finished it.
const PENDING: &str = "PENDING";

/// The terminal status of a deployment whose files were read and published.
const SUCCEED: &str = "SUCCEED";

/// The terminal status of a deployment whose files could not be published.
const FAILED: &str = "FAILED";

/// The terminal status of a stopped job.
const CANCELLED: &str = "CANCELLED";

/// A declared `MD5Hash` for a file a reservation names: the member is modelled
/// `length 0..32` with no further constraint
/// (`smithy_query(query="shape",shape="FileMap")`), and nothing in these rules reads it.
const DECLARED_HASH: &str = "d41d8cd98f00b204e9800998ecf8427e";

/// The base this emulator publishes for the URLs it hands out.
///
/// This harness mounts the router without serving it on a port, so the base it signs for
/// is the framework's own unserved base; a served emulator signs for its data port
/// instead. Either way the rule asserted is the same one — a destination is on the base
/// THIS emulator publishes, never on a host only AWS serves.
const EMULATOR_BASE: &str = smithy_server_runtime::UNSERVED_URL_BASE;

/// The query fields a SigV4 query-presigned URL carries, which is what makes an upload
/// destination followable with no `Authorization` header of its own.
const PRESIGNING_QUERY_FIELDS: &[&str] = &[
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];

/// How long a case waiting for a clock-owed status change gives up after: generous
/// enough that a loaded machine still reaches the condition, since what every case here
/// is about is the summary of a SETTLED job and never how fast it settled.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again. A blocking pause, because this
/// workspace's `tokio` carries no `time` feature and a `#[tokio::test]` body owns its
/// runtime thread for the duration of the case.
const BETWEEN_POLLS: Duration = Duration::from_millis(25);

/// The statuses a job has reached the END of its life in, which is the condition a case
/// waiting on the clock looks for.
///
/// `JobStatus` is modelled `CREATED | PENDING | PROVISIONING | RUNNING | FAILED |
/// SUCCEED | CANCELLING | CANCELLED` (`smithy_query(query="shape",shape="JobStatus")`);
/// of those, a job that has FAILED, SUCCEEDed or been CANCELLED is over.
const TERMINAL_STATUSES: &[&str] = &[FAILED, SUCCEED, CANCELLED];

/// `GetJob` for `job_id`, read again until the clock has settled that job out of its
/// non-terminal status or a generous deadline passes: a case waits for the CONDITION
/// rather than for a fixed time, so it does not turn on how fast the host runs.
async fn settled_job(
    emulator: &Emulator,
    app_id: &str,
    branch: &str,
    job_id: &str,
    case: &str,
) -> Reply {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reply = get_job(emulator, app_id, branch, job_id).await;
        let summary = read_summary(&reply);
        if TERMINAL_STATUSES.contains(&string_member(&summary, "status").as_str()) {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: job {job_id:?} never settled: last read {summary:?}"
        );
        sleep(BETWEEN_POLLS);
    }
}

/// `CreateDeployment` over the wire.
async fn create_deployment(
    emulator: &Emulator,
    app_id: &str,
    branch: &str,
    request: &Value,
) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/branches/{branch}/deployments"),
            Some(request),
        )
        .await
}

/// `StartDeployment` over the wire.
async fn start_deployment(
    emulator: &Emulator,
    app_id: &str,
    branch: &str,
    request: &Value,
) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/branches/{branch}/deployments/start"),
            Some(request),
        )
        .await
}

/// `GetJob` over the wire.
async fn get_job(emulator: &Emulator, app_id: &str, branch: &str, job_id: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/apps/{app_id}/branches/{branch}/jobs/{job_id}"),
            None,
        )
        .await
}

/// `ListJobs` over the wire.
async fn list_jobs(emulator: &Emulator, app_id: &str, branch: &str) -> Reply {
    emulator
        .call("GET", &format!("/apps/{app_id}/branches/{branch}/jobs"), None)
        .await
}

/// `StopJob` over the wire.
async fn stop_job(emulator: &Emulator, app_id: &str, branch: &str, job_id: &str) -> Reply {
    emulator
        .call(
            "DELETE",
            &format!("/apps/{app_id}/branches/{branch}/jobs/{job_id}/stop"),
            None,
        )
        .await
}

/// Follow an upload destination exactly as a client does: the `PUT` the plane publishes,
/// against the URL the service published, with NO credential of any kind.
async fn upload(emulator: &Emulator, case: &str, url: &str, body: &[u8]) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(url)
        .header("content-type", "application/zip")
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("an upload destination is a well-formed URL");
    let status = emulator.dispatch(request).await.status().as_u16();
    assert_eq!(
        status, 200,
        "case {case:?}: the destination {url:?} must accept the upload, got HTTP {status}"
    );
}

/// An app with one branch on it, and the branch ARN every `jobArn` of it extends. No
/// repository: "manually deployed apps are not connected to a Git repository"
/// (`smithy_query(query="doc",shape="CreateDeployment")`).
async fn branch_arn_of_new_branch(emulator: &Emulator, app_id: &str, branch: &str) -> String {
    let created = emulator
        .create_branch(app_id, &json!({ "branchName": branch }))
        .await;
    assert_eq!(
        created.status, 200,
        "branch {branch:?}: expected HTTP 200 from CreateBranch, got {created:?}"
    );
    string_member(&created.member("branch"), "branchArn")
}

/// A `.zip` carrying one entry per path named, as a real archive a deployment can be:
/// the archive format is the `zip` crate's, here and in the handler that reads it back.
fn zip_of(paths: &[&str]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut buffer);
    for path in paths {
        writer
            .start_file(*path, zip::write::SimpleFileOptions::default())
            .expect("an archive entry starts");
        std::io::Write::write_all(&mut writer, b"deployed").expect("an archive entry is written");
    }
    writer.finish().expect("the archive closes");
    buffer.into_inner()
}

/// The `summary` of a `GetJob` answer.
fn read_summary(reply: &Reply) -> Value {
    reply
        .member("job")
        .get("summary")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Rules 1 and 2, applied to one answered summary.
fn assert_summary_of_a_deployment(
    case: &str,
    answered_by: &str,
    summary: &Value,
    branch_arn: &str,
    job_id: &str,
    status: &str,
    work_has_run: bool,
) {
    assert_eq!(
        string_member(summary, "jobId"),
        job_id,
        "case {case:?}: {answered_by} must report the bare job number, got {summary:?}"
    );
    let padded = format!("{job_id:0>width$}", width = JOB_ARN_NUMBER_DIGITS);
    assert_eq!(
        string_member(summary, "jobArn"),
        format!("{branch_arn}/jobs/{padded}"),
        "case {case:?}: {answered_by} must address the job with its number in a \
         {JOB_ARN_NUMBER_DIGITS}-digit field, got {summary:?}"
    );
    assert_eq!(
        string_member(summary, "status"),
        status,
        "case {case:?}: {answered_by} must report the status {status}, got {summary:?}"
    );
    for member in UNREPORTED_ON_A_DEPLOYMENT {
        assert!(
            summary.get(member).is_none(),
            "case {case:?}: {answered_by} must leave out {member}, which a job that \
             deploys content has no value for, got {summary:?}"
        );
    }
    assert_eq!(
        summary.get(START_TIME).is_some(),
        work_has_run,
        "case {case:?}: {answered_by} must report {START_TIME} only once the job's work \
         has run (expected present: {work_has_run}), got {summary:?}"
    );
}

/// One way of deploying content, and what the deployment's job settles to.
struct DeploymentRow {
    /// The case name every assertion of this row quotes.
    case: &'static str,
    /// What `CreateDeployment` is asked to upload, or `None` for a deployment started
    /// from a source URL instead of from a reservation.
    reservation: Option<Value>,
    /// The entries of the archive uploaded to the reservation's zip destination, when the
    /// row uploads one.
    archive: Option<&'static [&'static str]>,
    /// The files uploaded to the reservation's per-file destinations, by name.
    files: &'static [&'static str],
    /// The `StartDeployment` request, for a row that names a source URL.
    source: Option<Value>,
    /// The terminal status the clock settles this deployment's job to.
    status: &'static str,
}

/// The rows differ along what rule 1 generalises over: whether the content was reserved
/// or named by URL, whether it is one archive or one upload per file, how many files it
/// carries, and whether the job ends up publishing anything at all. What the summary
/// reports is the same in every one of them.
fn deployment_rows() -> Vec<DeploymentRow> {
    vec![
        DeploymentRow {
            case: "a reserved deployment uploaded as one archive",
            reservation: Some(json!({})),
            archive: Some(&["index.html", "assets/app.js"]),
            files: &[],
            source: None,
            status: SUCCEED,
        },
        DeploymentRow {
            case: "a reserved deployment whose archive carries no root document",
            reservation: Some(json!({})),
            archive: Some(&["site/index.html"]),
            files: &[],
            source: None,
            status: FAILED,
        },
        DeploymentRow {
            case: "a reserved deployment uploaded one file at a time",
            reservation: Some(json!({
                "fileMap": { "index.html": DECLARED_HASH, "assets/app.js": DECLARED_HASH },
            })),
            archive: None,
            files: &["index.html", "assets/app.js"],
            source: None,
            status: SUCCEED,
        },
        DeploymentRow {
            case: "a reserved deployment of a single named file",
            reservation: Some(json!({ "fileMap": { "index.html": DECLARED_HASH } })),
            archive: None,
            files: &["index.html"],
            source: None,
            status: SUCCEED,
        },
        DeploymentRow {
            case: "a deployment of an archive behind a URL outside this emulator",
            reservation: None,
            archive: None,
            files: &[],
            source: Some(json!({
                "sourceUrl": "https://downloads.example.test/site.zip",
                "sourceUrlType": "ZIP",
            })),
            status: FAILED,
        },
    ]
}

/// Rules 1, 2 and 3: what a deployment's job reports, wherever it is answered, and what
/// the reservation that created it handed back.
#[tokio::test]
async fn a_deployments_job_reports_only_what_a_deployment_has() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("deployment-projection").await;

    let mut queued: Vec<(&'static str, String, String, String, &'static str)> = Vec::new();
    for (index, row) in deployment_rows().into_iter().enumerate() {
        // One row per BRANCH: a branch serializes its deployments, and these rules are
        // about one deployment's job.
        let branch = format!("row-{index}");
        let branch_arn = branch_arn_of_new_branch(&emulator, &app_id, &branch).await;

        let start = match &row.reservation {
            Some(request) => {
                let reserved = create_deployment(&emulator, &app_id, &branch, request).await;
                assert_eq!(
                    reserved.status, 200,
                    "case {:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}",
                    row.case
                );
                assert_destinations(row.case, &reserved, row.files);
                let job_id = string_member(&reserved.body, "jobId");
                if let Some(entries) = row.archive {
                    let url = string_member(&reserved.body, "zipUploadUrl");
                    upload(&emulator, row.case, &url, &zip_of(entries)).await;
                }
                for file_name in row.files {
                    let url = destination_for(&reserved, file_name, row.case);
                    upload(
                        &emulator,
                        row.case,
                        &url,
                        format!("the bytes of {file_name}").as_bytes(),
                    )
                    .await;
                }
                json!({ "jobId": job_id })
            }
            None => row
                .source
                .clone()
                .expect("a row deploys either a reservation or a source URL"),
        };

        let started = start_deployment(&emulator, &app_id, &branch, &start).await;
        assert_eq!(
            started.status, 200,
            "case {:?}: expected HTTP 200 from StartDeployment, got {started:?}",
            row.case
        );
        let summary = started.member("jobSummary");
        let job_id = string_member(&summary, "jobId");
        assert_summary_of_a_deployment(
            row.case,
            "StartDeployment",
            &summary,
            &branch_arn,
            &job_id,
            PENDING,
            false,
        );

        // The queued job reads back exactly as the start call answered it.
        let read = get_job(&emulator, &app_id, &branch, &job_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: a started deployment's job is readable, got {read:?}",
            row.case
        );
        assert_summary_of_a_deployment(
            row.case,
            "GetJob while the job is queued",
            &read_summary(&read),
            &branch_arn,
            &job_id,
            PENDING,
            false,
        );

        queued.push((row.case, branch, branch_arn, job_id, row.status));
    }

    // Each row's job is waited for on its own, by polling for the status the clock owes
    // it rather than for a span of time.
    for (case, branch, branch_arn, job_id, status) in &queued {
        let read = settled_job(&emulator, &app_id, branch, job_id, case).await;
        assert_summary_of_a_deployment(
            case,
            "GetJob once the work has run",
            &read_summary(&read),
            branch_arn,
            job_id,
            status,
            true,
        );

        let listed = list_jobs(&emulator, &app_id, branch).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200 from ListJobs, got {listed:?}"
        );
        let summaries = list_member(&listed.body, "jobSummaries");
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: one deployment started, so the branch holds one job, got {listed:?}"
        );
        assert_eq!(
            summaries[0],
            read_summary(&read),
            "case {case:?}: a listed job must be the record GetJob answers, got {listed:?}"
        );
    }
}

/// Rule 1 over the one answer that is not a read: `StopJob` of a deployment whose work
/// has begun reports the same summary, in the status the stop put it in.
#[tokio::test]
async fn stopping_a_deployment_reports_the_same_summary() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("deployment-stopped").await;
    let branch_arn = branch_arn_of_new_branch(&emulator, &app_id, "main").await;

    let reserved = create_deployment(&emulator, &app_id, "main", &json!({})).await;
    let job_id = string_member(&reserved.body, "jobId");
    let url = string_member(&reserved.body, "zipUploadUrl");
    upload(
        &emulator,
        "a stopped deployment",
        &url,
        &zip_of(&["index.html"]),
    )
    .await;
    let started = start_deployment(&emulator, &app_id, "main", &json!({ "jobId": job_id })).await;
    assert_eq!(
        started.status, 200,
        "case \"a stopped deployment\": expected HTTP 200 from StartDeployment, got {started:?}"
    );

    let stopped = stop_job(&emulator, &app_id, "main", &job_id).await;
    assert_eq!(
        stopped.status, 200,
        "case \"a stopped deployment\": expected HTTP 200 from StopJob, got {stopped:?}"
    );
    // The work had begun, so the stop is still in flight: `CANCELLING` is the status its
    // own answer carries, and the clock settles it to `CANCELLED`.
    assert_summary_of_a_deployment(
        "a stopped deployment",
        "StopJob",
        &stopped.member("jobSummary"),
        &branch_arn,
        &job_id,
        "CANCELLING",
        false,
    );

    let read = settled_job(
        &emulator,
        &app_id,
        "main",
        &job_id,
        "a stopped deployment",
    )
    .await;
    assert_summary_of_a_deployment(
        "a stopped deployment",
        "GetJob once the stop has settled",
        &read_summary(&read),
        &branch_arn,
        &job_id,
        CANCELLED,
        true,
    );
}

/// Rule 3: which destination member an answer carries follows the form of the request,
/// and every destination it carries is presigned.
fn assert_destinations(case: &str, reserved: &Reply, named_files: &[&str]) {
    let per_file = reserved.body.get("fileUploadUrls");
    let zip = reserved.body.get("zipUploadUrl");
    let mut destinations: Vec<String> = Vec::new();
    if named_files.is_empty() {
        assert!(
            per_file.is_none(),
            "case {case:?}: a request naming no file selects the zipped form, so \
             fileUploadUrls is not part of the answer, got {reserved:?}"
        );
        let url = zip
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a zipUploadUrl, got {reserved:?}"));
        destinations.push(url.to_string());
    } else {
        assert!(
            zip.is_none(),
            "case {case:?}: a request naming files selects the per-file form, so \
             zipUploadUrl is not part of the answer, got {reserved:?}"
        );
        for file_name in named_files {
            destinations.push(destination_for(reserved, file_name, case));
        }
        let named = per_file
            .and_then(Value::as_object)
            .map(serde_json::Map::len)
            .unwrap_or_default();
        assert_eq!(
            named,
            named_files.len(),
            "case {case:?}: one destination per file the request named, got {reserved:?}"
        );
    }

    for url in &destinations {
        assert!(
            url.starts_with(&format!("{EMULATOR_BASE}/apps/")),
            "case {case:?}: a destination must address this emulator's own base, got {url:?}"
        );
        for field in PRESIGNING_QUERY_FIELDS {
            assert!(
                url.contains(&format!("{field}=")),
                "case {case:?}: a destination is followed with no credential, so it must \
                 carry the presigning field {field}, got {url:?}"
            );
        }
    }
}

/// The destination the reservation handed out for `file_name`.
fn destination_for(reserved: &Reply, file_name: &str, case: &str) -> String {
    reserved
        .body
        .get("fileUploadUrls")
        .and_then(|urls| urls.get(file_name))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the reservation names no destination for {file_name:?}: {reserved:?}")
        })
        .to_string()
}

/// Rule 3's bound: a reservation mints one destination per file the request names, and
/// no more than a deployment may carry — the published ceiling is accepted, and one file
/// past it is refused with the modelled quota error.
#[tokio::test]
async fn a_reservation_mints_no_more_destinations_than_a_deployment_carries() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("destination-bound").await;

    // The published bound and one past it. One branch per case, because a branch
    // serializes its deployments.
    for (case, named, accepted) in [
        ("one file", 1, true),
        ("at the bound", MAX_DEPLOYMENT_ASSETS, true),
        ("one file past the bound", MAX_DEPLOYMENT_ASSETS + 1, false),
    ] {
        let branch = format!("named-{named}");
        branch_arn_of_new_branch(&emulator, &app_id, &branch).await;
        let file_map: serde_json::Map<String, Value> = (0..named)
            .map(|index| (format!("assets/file-{index}.html"), json!(DECLARED_HASH)))
            .collect();
        let reserved = create_deployment(
            &emulator,
            &app_id,
            &branch,
            &json!({ "fileMap": Value::Object(file_map) }),
        )
        .await;
        if !accepted {
            assert_eq!(
                reserved.status, 429,
                "case {case:?}: expected the modelled quota refusal, got {reserved:?}"
            );
            assert_eq!(
                reserved.error_code.as_deref(),
                Some("LimitExceededException"),
                "case {case:?}: expected LimitExceededException, got {:?}",
                reserved.error_code
            );
            assert!(
                reserved
                    .message()
                    .is_some_and(|message| message.contains(&named.to_string())
                        && message.contains(&MAX_DEPLOYMENT_ASSETS.to_string())),
                "case {case:?}: the refusal states the count named and the ceiling, got {:?}",
                reserved.message()
            );
            continue;
        }
        assert_eq!(
            reserved.status, 200,
            "case {case:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}"
        );
        let minted = reserved
            .body
            .get("fileUploadUrls")
            .and_then(Value::as_object)
            .map(serde_json::Map::len)
            .unwrap_or_default();
        assert_eq!(
            minted, named,
            "case {case:?}: one destination per file named, got {minted}"
        );
    }
}

/// Rule 4: a `BUCKET_PREFIX` source URL that does not end in the path separator is
/// refused, and spends no job number.
#[tokio::test]
async fn a_prefix_source_must_name_a_folder() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("prefix-separator").await;
    branch_arn_of_new_branch(&emulator, &app_id, "main").await;

    // The rows differ along what the rule generalises over: how deep the location is and
    // whether it names a path at all. Every one of them is a bucket-and-prefix source
    // written without the separator that makes it one.
    let refused = [
        ("a bucket and no path", "s3://deploy-source"),
        ("one path segment", "s3://deploy-source/src"),
        ("a nested path", "s3://deploy-source/sites/app/src"),
        (
            "a path whose last segment looks like a file",
            "s3://deploy-source/src/index.html",
        ),
    ];
    for (case, source_url) in refused {
        let reply = start_deployment(
            &emulator,
            &app_id,
            "main",
            &json!({ "sourceUrl": source_url, "sourceUrlType": "BUCKET_PREFIX" }),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected the modelled HTTP 400, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("BadRequestException"),
            "case {case:?}: expected the modelled BadRequestException, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message().as_deref(),
            Some(PREFIX_WITHOUT_TRAILING_SEPARATOR),
            "case {case:?}: expected the recorded refusal text, got {:?}",
            reply.message()
        );
    }

    // Every refused call is refused BEFORE a job exists, so it spends no job number: the
    // branch's history is still empty.
    let listed = list_jobs(&emulator, &app_id, "main").await;
    assert!(
        list_member(&listed.body, "jobSummaries").is_empty(),
        "case \"a refused prefix spends no job number\": a refusal reaches no job, got \
         {listed:?}"
    );

    // A location that DOES name a folder is not refused for this reason — whatever the
    // read of it then finds.
    for (case, source_url) in [
        ("the bucket root", "s3://deploy-source/"),
        ("a folder one level down", "s3://deploy-source/src/"),
        ("a folder several levels down", "s3://deploy-source/a/b/c/"),
    ] {
        let reply = start_deployment(
            &emulator,
            &app_id,
            "main",
            &json!({ "sourceUrl": source_url, "sourceUrlType": "BUCKET_PREFIX" }),
        )
        .await;
        assert_ne!(
            reply.message().as_deref(),
            Some(PREFIX_WITHOUT_TRAILING_SEPARATOR),
            "case {case:?}: a source URL that names a folder is not refused for naming \
             none, got {reply:?}"
        );
    }
}
