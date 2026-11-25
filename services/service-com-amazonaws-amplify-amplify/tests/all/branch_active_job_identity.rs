// SPDX-License-Identifier: CC0-1.0

//! How a branch IDENTIFIES the job that is active on it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `amplify` signing name, and every upload follows the URL the
//! service published — so what is asserted is what an AWS SDK client would see.
//!
//! The rules under test, each over a TABLE spanning the class it generalises over rather
//! than over one recorded job:
//!
//! 1. A member that REPORTS a job's number carries it bare (`JobSummary$jobId`, modelled
//!    `pattern ^[0-9]+$`), while a member that IDENTIFIES a job's place among its branch's
//!    jobs carries that number in a fixed field of ten digits — the last segment of
//!    `JobSummary$jobArn` and `Branch$activeJobId` (modelled `length 0..1000` with no
//!    pattern, so the form is the service's rather than the model's,
//!    `smithy_query(query="shape",shape="Branch")`). The two forms therefore agree digit
//!    for digit once the padding is stripped, which is the correlation a client performs,
//!    and the rule holds whatever the width of the number and whichever call started the
//!    job.
//! 2. A branch's `activeJobId` names the last job something RAN on it, so a job that only
//!    reserved a number — a `CreateDeployment` no `StartDeployment` followed — never
//!    becomes it: that job stands in the `PENDING` status a client polls an unstarted job
//!    through, reaches the terminal `CANCELLED` when `StopJob` ends it, and leaves both
//!    `activeJobId` and `totalNumberOfJobs` exactly where the branch's last RUN job left
//!    them, while `ListJobs` reports the branch's whole history newest first with each job
//!    in its own status. A branch created with no `displayName` carries its own name drawn
//!    into the alphabet of a host label as that prefix, whatever the branch then deploys.

use serde_json::{json, Value};

use crate::app_branch_lifecycle::{list_member, string_member, Emulator, Reply};
// The poll-to-terminal wait the deployment suites already drive, so no suite holds its
// own idea of what a settled job is.
use crate::branch_deployments::settled_status;

/// How wide the fixed job-number field is.
///
/// Recorded rather than modelled: `JobArn` is free text and `ActiveJobId` carries no
/// pattern, and Amplify writes the job number into a ten-digit field of leading zeroes in
/// both.
const JOB_NUMBER_FIELD_DIGITS: usize = 10;

/// How many jobs each row of the table starts, which carries the number past its first
/// decade so the field is asserted at more than one width of number.
const JOBS_PER_BRANCH: u64 = 11;

/// The job number in the fixed field every identifying member carries it in.
fn in_the_number_field(job_id: &str) -> String {
    format!("{job_id:0>width$}", width = JOB_NUMBER_FIELD_DIGITS)
}

/// `StartJob` over the wire.
async fn start_job(emulator: &Emulator, app_id: &str, branch: &str, request: &Value) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/branches/{branch}/jobs"),
            Some(request),
        )
        .await
}

/// `CreateDeployment` over the wire.
async fn create_deployment(emulator: &Emulator, app_id: &str, branch: &str) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/branches/{branch}/deployments"),
            Some(&json!({})),
        )
        .await
}

/// `StartDeployment` over the wire.
async fn start_deployment(
    emulator: &Emulator,
    app_id: &str,
    branch: &str,
    job_id: &str,
) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/branches/{branch}/deployments/start"),
            Some(&json!({ "jobId": job_id })),
        )
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

/// Follow an upload destination exactly as a client does: the `PUT` the service
/// published, against the URL it published.
async fn upload(emulator: &Emulator, case: &str, url: &str, body: &[u8]) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(url)
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("an upload destination is a well-formed URL");
    let status = emulator.dispatch(request).await.status().as_u16();
    assert_eq!(
        status, 200,
        "case {case:?}: the destination {url:?} must accept the upload, got {status}"
    );
}

/// A `.zip` carrying one entry per path named, as a real archive a deployment can be.
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

/// A branch on `app_id`, and the ARN it answers with.
async fn branch_arn_of_new_branch(emulator: &Emulator, app_id: &str, branch: &str) -> String {
    let created = emulator
        .create_branch(app_id, &json!({ "branchName": branch }))
        .await;
    assert_eq!(
        created.status, 200,
        "app {app_id}: expected HTTP 200 from CreateBranch on {branch:?}, got {created:?}"
    );
    string_member(&created.member("branch"), "branchArn")
}

/// The three readings of one job's identity, asserted together: the summary's bare number,
/// the fixed field of its ARN, and the branch's own view of its active job.
async fn assert_one_identity(
    emulator: &Emulator,
    case: &str,
    app_id: &str,
    branch: &str,
    branch_arn: &str,
    summary: &Value,
    expected_number: &str,
) {
    let job_id = string_member(summary, "jobId");
    assert_eq!(
        job_id, expected_number,
        "case {case:?}: jobId reports the branch's bare job number, got {summary:?}"
    );
    let field = in_the_number_field(&job_id);
    assert_eq!(
        string_member(summary, "jobArn"),
        format!("{branch_arn}/jobs/{field}"),
        "case {case:?}: the ARN carries the number in its ten-digit field, got {summary:?}"
    );

    let record = emulator.get_branch(app_id, branch).await.member("branch");
    let active = string_member(&record, "activeJobId");
    assert_eq!(
        active, field,
        "case {case:?}: activeJobId identifies the job in the same ten-digit field, \
         got {record:?}"
    );
    // The correlation a client performs: the identified job and the reported number are
    // the same job once the field's padding is stripped.
    assert_eq!(
        active.trim_start_matches('0'),
        job_id,
        "case {case:?}: the identified job must be the job that was started, got {record:?}"
    );
}

#[tokio::test]
async fn a_started_build_is_identified_by_its_number_in_the_ten_digit_field() {
    let emulator = Emulator::new();
    // A repository app, because `StartJob` builds the app's connected repository.
    let app_id = emulator
        .app_id_of_new_repository_app("active-job-identity")
        .await;
    let branch_arn = branch_arn_of_new_branch(&emulator, &app_id, "main").await;

    for number in 1..=JOBS_PER_BRANCH {
        let case = format!("build {number}");
        let started = start_job(&emulator, &app_id, "main", &json!({ "jobType": "RELEASE" })).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartJob, got {started:?}"
        );
        assert_one_identity(
            &emulator,
            &case,
            &app_id,
            "main",
            &branch_arn,
            &started.member("jobSummary"),
            &number.to_string(),
        )
        .await;
    }
}

#[tokio::test]
async fn a_started_manual_deployment_is_identified_the_same_way() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("active-deployment-identity").await;

    // Two branches, so the deployment that is started runs under a one-digit number on
    // one and a two-digit number on the other: a branch numbers its own jobs, and the
    // second branch is carried past its first decade by reservations it then cancels —
    // which spends the numbers without leaving a job in flight.
    for (branch, spent, expected_number) in [("main", 0u64, "1"), ("second", 10u64, "11")] {
        let branch_arn = branch_arn_of_new_branch(&emulator, &app_id, branch).await;
        for number in 1..=spent {
            let case = format!("{branch}: spending number {number}");
            let reserved = create_deployment(&emulator, &app_id, branch).await;
            assert_eq!(
                reserved.status, 200,
                "case {case:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}"
            );
            let stopped = stop_job(
                &emulator,
                &app_id,
                branch,
                &string_member(&reserved.body, "jobId"),
            )
            .await;
            assert_eq!(
                stopped.status, 200,
                "case {case:?}: expected HTTP 200 from StopJob, got {stopped:?}"
            );
        }

        let case = format!("{branch}: the deployment that is started");
        let reserved = create_deployment(&emulator, &app_id, branch).await;
        assert_eq!(
            reserved.status, 200,
            "case {case:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}"
        );
        let job_id = string_member(&reserved.body, "jobId");
        upload(
            &emulator,
            &case,
            &string_member(&reserved.body, "zipUploadUrl"),
            &zip_of(&["index.html"]),
        )
        .await;
        let started = start_deployment(&emulator, &app_id, branch, &job_id).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected HTTP 200 from StartDeployment, got {started:?}"
        );
        assert_one_identity(
            &emulator,
            &case,
            &app_id,
            branch,
            &branch_arn,
            &started.member("jobSummary"),
            expected_number,
        )
        .await;
    }
}

/// The `JobStatus` values rule 2 turns on, written out as AWS spells them
/// (`smithy_query(query="shape",shape="JobStatus")`: `CREATED | PENDING | PROVISIONING |
/// RUNNING | FAILED | SUCCEED | CANCELLING | CANCELLED`): a job nothing has started is
/// `PENDING`, one `StopJob` ended `CANCELLED`, a deployment that published its site
/// `SUCCEED`, and a repository build this emulator has no environment to run `FAILED`.
const PENDING: &str = "PENDING";
const CANCELLED: &str = "CANCELLED";
const DEPLOYED: &str = "SUCCEED";
const BUILT_WITHOUT_AN_ENVIRONMENT: &str = "FAILED";

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

/// A branch name as it reaches the service in a request PATH.
///
/// `branchName` is an `@httpLabel` of every per-branch route, which a client
/// percent-encodes; of the characters a branch name admits only the path separator is not
/// an unreserved URI character, so that is the one written encoded.
fn in_a_path(branch_name: &str) -> String {
    branch_name.replace('/', "%2F")
}

/// The default domain prefix a branch created without a `displayName` carries: its own
/// name drawn into the alphabet of a host label — letters, digits and the hyphen — with
/// every other character replaced and nothing else changed.
fn one_host_label(branch_name: &str) -> String {
    branch_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// One branch's history: what RAN on it before a deployment was reserved, and how many
/// reservations were then made and stopped.
struct ReservedHistoryCase {
    case: &'static str,
    /// The branch's name, which is also what its default domain prefix is derived from.
    branch_name: &'static str,
    /// How many repository builds `StartJob` runs on the branch first — the rows with any
    /// are run on an app connected to a repository provider, which is what `StartJob`
    /// needs.
    builds: u64,
    /// How many manual deployments are reserved, uploaded, started and settled first.
    deployments: u64,
    /// How many deployments are then reserved and stopped without ever being started.
    reservations: u64,
}

/// The table for rule 2: one row per shape a branch's history takes around the
/// reservations, spanning what ran before them (nothing, a deployment, several
/// deployments, a build, a build and a deployment), how many reservations follow, and
/// whether the job numbers stay inside their first decade.
fn reserved_history_cases() -> Vec<ReservedHistoryCase> {
    vec![
        ReservedHistoryCase {
            case: "one deployment that ran, then one reservation",
            branch_name: "main",
            builds: 0,
            deployments: 1,
            reservations: 1,
        },
        ReservedHistoryCase {
            case: "two deployments that ran, then one reservation",
            branch_name: "release.2",
            builds: 0,
            deployments: 2,
            reservations: 1,
        },
        ReservedHistoryCase {
            case: "a build that ran, then two reservations",
            branch_name: "feature/a",
            builds: 1,
            deployments: 0,
            reservations: 2,
        },
        ReservedHistoryCase {
            case: "a build and a deployment that ran, then one reservation",
            branch_name: "team/area/topic",
            builds: 1,
            deployments: 1,
            reservations: 1,
        },
        ReservedHistoryCase {
            case: "nothing ran before the reservations",
            branch_name: "staging_1",
            builds: 0,
            deployments: 0,
            reservations: 3,
        },
        ReservedHistoryCase {
            case: "reservations carrying the history past its first decade",
            branch_name: "Main-Two",
            builds: 0,
            deployments: 1,
            reservations: 10,
        },
    ]
}

/// The status the branch's job `number` stands in at the end of a row: the builds ran
/// first, then the deployments, then the reservations that were stopped.
fn status_of(number: u64, row: &ReservedHistoryCase) -> &'static str {
    if number <= row.builds {
        BUILT_WITHOUT_AN_ENVIRONMENT
    } else if number <= row.builds + row.deployments {
        DEPLOYED
    } else {
        CANCELLED
    }
}

/// The status `GetJob` reports for one job.
async fn job_status(emulator: &Emulator, app_id: &str, branch: &str, job_id: &str) -> String {
    let read = get_job(emulator, app_id, branch, job_id).await;
    assert_eq!(
        read.status, 200,
        "branch {branch:?}: expected HTTP 200 from GetJob on {job_id:?}, got {read:?}"
    );
    let summary = read
        .member("job")
        .get("summary")
        .cloned()
        .unwrap_or(Value::Null);
    string_member(&summary, "status")
}

/// The status `ListJobs` reports for one job, or `None` when the listing does not carry
/// it at all.
async fn listed_status(
    emulator: &Emulator,
    app_id: &str,
    branch: &str,
    job_id: &str,
) -> Option<String> {
    let listed = list_jobs(emulator, app_id, branch).await;
    assert_eq!(
        listed.status, 200,
        "branch {branch:?}: expected HTTP 200 from ListJobs, got {listed:?}"
    );
    list_member(&listed.body, "jobSummaries")
        .iter()
        .find(|summary| string_member(summary, "jobId") == job_id)
        .map(|summary| string_member(summary, "status"))
}

/// The branch identifies `ran` as its active job — or identifies none at all, when
/// nothing has run on it.
///
/// `Branch$activeJobId` is `@required` with a `length` minimum of 0
/// (`smithy_query(query="shape",shape="Branch")`) and Amplify reports a member it has no
/// value for as JSON `null`, so a branch nothing has run on carries no id at all.
async fn assert_the_active_job_is(
    emulator: &Emulator,
    case: &str,
    app_id: &str,
    branch: &str,
    at: &str,
    ran: u64,
) {
    let record = emulator.get_branch(app_id, branch).await.member("branch");
    if ran == 0 {
        assert!(
            record.get("activeJobId").is_none_or(Value::is_null),
            "case {case:?} ({at}): a branch nothing has run on identifies no active job, \
             got {record:?}"
        );
        return;
    }
    let expected = in_the_number_field(&ran.to_string());
    assert_eq!(
        string_member(&record, "activeJobId"),
        expected,
        "case {case:?} ({at}): the active job must still be job {ran} in its ten-digit \
         field, got {record:?}"
    );
}

/// `ListJobs` reports exactly `expected` — `(jobId, status)` newest first.
async fn assert_history(
    emulator: &Emulator,
    case: &str,
    app_id: &str,
    branch: &str,
    expected: &[(String, &str)],
) {
    let listed = list_jobs(emulator, app_id, branch).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected HTTP 200 from ListJobs, got {listed:?}"
    );
    let reported: Vec<(String, String)> = list_member(&listed.body, "jobSummaries")
        .iter()
        .map(|summary| {
            (
                string_member(summary, "jobId"),
                string_member(summary, "status"),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(job_id, status)| (job_id.clone(), (*status).to_string()))
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: the branch's history is one job per call, newest first, each in \
         its own status; expected {expected:?}, got {reported:?}"
    );
}

/// Rule 2: a reserved deployment stands `PENDING`, stops `CANCELLED`, and never becomes
/// the job its branch identifies as active.
#[tokio::test]
async fn a_reserved_deployment_never_becomes_the_branchs_active_job() {
    for row in reserved_history_cases() {
        let emulator = Emulator::new();
        // A row that runs a build needs an app connected to a repository provider, which
        // is what `StartJob` builds; reserving and starting a deployment takes no view of
        // the app's repository.
        let app_id = if row.builds > 0 {
            emulator
                .app_id_of_new_repository_app("reserved-history")
                .await
        } else {
            emulator.app_id_of_new_app("reserved-history").await
        };

        let created = emulator
            .create_branch(&app_id, &json!({ "branchName": row.branch_name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from CreateBranch, got {created:?}",
            row.case
        );
        let record = created.member("branch");
        let prefix = one_host_label(row.branch_name);
        assert_eq!(
            string_member(&record, "displayName"),
            prefix,
            "case {:?}: the branch's default domain prefix is its name as one host label, \
             expected {prefix:?}, got {record:?}",
            row.case
        );
        assert_eq!(
            string_member(&record, "branchName"),
            row.branch_name,
            "case {:?}: the branch keeps the name the request sent, got {record:?}",
            row.case
        );
        let branch_arn = string_member(&record, "branchArn");
        // Every later call addresses the branch by the name the request sent, encoded as
        // a client encodes a path label.
        let branch = in_a_path(row.branch_name);

        let mut number = 0u64;

        for _ in 0..row.builds {
            number += 1;
            let started = start_job(
                &emulator,
                &app_id,
                &branch,
                &json!({ "jobType": "RELEASE" }),
            )
            .await;
            assert_eq!(
                started.status, 200,
                "case {:?}: expected HTTP 200 from StartJob, got {started:?}",
                row.case
            );
            assert_one_identity(
                &emulator,
                row.case,
                &app_id,
                &branch,
                &branch_arn,
                &started.member("jobSummary"),
                &number.to_string(),
            )
            .await;
            let settled = settled_status(&emulator, &app_id, &branch, &number.to_string()).await;
            assert_eq!(
                settled, BUILT_WITHOUT_AN_ENVIRONMENT,
                "case {:?}: a build this emulator has no environment to run ends \
                 {BUILT_WITHOUT_AN_ENVIRONMENT}, got {settled:?}",
                row.case
            );
        }

        for _ in 0..row.deployments {
            number += 1;
            let reserved = create_deployment(&emulator, &app_id, &branch).await;
            assert_eq!(
                reserved.status, 200,
                "case {:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}",
                row.case
            );
            let job_id = string_member(&reserved.body, "jobId");
            assert_eq!(
                job_id,
                number.to_string(),
                "case {:?}: the call spends the branch's next job number, got {reserved:?}",
                row.case
            );
            upload(
                &emulator,
                row.case,
                &string_member(&reserved.body, "zipUploadUrl"),
                &zip_of(&["index.html"]),
            )
            .await;
            let started = start_deployment(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                started.status, 200,
                "case {:?}: expected HTTP 200 from StartDeployment, got {started:?}",
                row.case
            );
            let settled = settled_status(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                settled, DEPLOYED,
                "case {:?}: a deployment that published its site ends {DEPLOYED}, \
                 got {settled:?}",
                row.case
            );
        }

        // The last job something RAN on this branch, which is the one its `activeJobId`
        // has to keep naming through everything below; 0 where nothing ran at all.
        let ran = number;
        assert_the_active_job_is(
            &emulator,
            row.case,
            &app_id,
            &branch,
            "before any reservation",
            ran,
        )
        .await;
        // `totalNumberOfJobs` counts the BUILDS the branch has run, which is what the
        // reservations below must not touch either.
        let built = emulator
            .get_branch(&app_id, &branch)
            .await
            .member("branch");
        assert_eq!(
            string_member(&built, "totalNumberOfJobs"),
            row.builds.to_string(),
            "case {:?}: the branch counts the builds it ran, got {built:?}",
            row.case
        );

        for _ in 0..row.reservations {
            number += 1;
            let reserved = create_deployment(&emulator, &app_id, &branch).await;
            assert_eq!(
                reserved.status, 200,
                "case {:?}: expected HTTP 200 from CreateDeployment, got {reserved:?}",
                row.case
            );
            let job_id = string_member(&reserved.body, "jobId");
            assert_eq!(
                job_id,
                number.to_string(),
                "case {:?}: the reservation spends the branch's next job number, \
                 got {reserved:?}",
                row.case
            );

            // Nothing has started it, which is the status a client polls such a job
            // through — from both readers of it.
            let status = job_status(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                status, PENDING,
                "case {:?}: job {job_id} is reserved and unstarted, so it is {PENDING}, \
                 got {status:?}",
                row.case
            );
            let listed = listed_status(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                listed.as_deref(),
                Some(PENDING),
                "case {:?}: the listing reports job {job_id} the way GetJob does, \
                 got {listed:?}",
                row.case
            );
            assert_the_active_job_is(
                &emulator,
                row.case,
                &app_id,
                &branch,
                "with the reservation outstanding",
                ran,
            )
            .await;

            // Stopping a job nothing had begun is complete in the stop's own answer.
            let stopped = stop_job(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                stopped.status, 200,
                "case {:?}: expected HTTP 200 from StopJob, got {stopped:?}",
                row.case
            );
            assert_eq!(
                string_member(&stopped.member("jobSummary"), "status"),
                CANCELLED,
                "case {:?}: stopping a job nothing started is complete at once, \
                 got {stopped:?}",
                row.case
            );
            let status = job_status(&emulator, &app_id, &branch, &job_id).await;
            assert_eq!(
                status, CANCELLED,
                "case {:?}: job {job_id} stays {CANCELLED} after the stop, got {status:?}",
                row.case
            );
            assert_the_active_job_is(
                &emulator,
                row.case,
                &app_id,
                &branch,
                "after the reservation was stopped",
                ran,
            )
            .await;
        }

        // The whole history: one job per call made on the branch, newest first, each in
        // its own status.
        let expected: Vec<(String, &str)> = (1..=number)
            .rev()
            .map(|job| (job.to_string(), status_of(job, &row)))
            .collect();
        assert_history(&emulator, row.case, &app_id, &branch, &expected).await;

        // And the branch's own view of its jobs is where its last RUN job left it.
        let record = emulator
            .get_branch(&app_id, &branch)
            .await
            .member("branch");
        assert_eq!(
            string_member(&record, "totalNumberOfJobs"),
            row.builds.to_string(),
            "case {:?}: a reservation is not a build the branch ran, got {record:?}",
            row.case
        );
        assert_eq!(
            string_member(&record, "displayName"),
            prefix,
            "case {:?}: the branch's default domain prefix is untouched by its jobs, \
             got {record:?}",
            row.case
        );
        assert_the_active_job_is(
            &emulator,
            row.case,
            &app_id,
            &branch,
            "at the end of the row",
            ran,
        )
        .await;
    }
}
