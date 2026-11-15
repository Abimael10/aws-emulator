// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of WHAT `StartPatientInsightsJob` writes.
//!
//! The sibling suite (`patient_insights_job_lifecycle`) pins what an ACCEPTED start
//! persists and what `GetPatientInsightsJob` projects back out of it. This one pins the
//! write discipline around it, asserted over tables rather than one request:
//!
//! 1. A start writes a `PatientInsightsJob` record ONLY when it is accepted: whatever
//!    refuses it — an identifier that resolves to no live domain, a domain addressed
//!    through another Region's endpoint, a domain on its way out, or a request the model
//!    rejects — the store's jobs are exactly the records, with exactly the identifiers,
//!    creation instants and parameters, that they were before the call. Which refusal each
//!    earns is the patient-insights runtime endpoint's: a `domainId` it cannot resolve a
//!    live domain for is its authorization failure (401 `AccessDeniedException`), and a
//!    value of the request outside its shape is the modelled 400 `ValidationException`.
//! 2. A start is NOT deduplicated on `clientToken` at any point of a job's life: a second
//!    start carrying a token already used mints a job of its own, and leaves the job the
//!    first call stored exactly as the clock has taken it — identifiers, creation instant,
//!    parameters, status and output location included.
//!
//! The job's own store is inspected through the SHARED engine the service is mounted on,
//! which is the only way to assert that a refused call wrote NOTHING (this service models
//! no operation that lists jobs).
//!
//! The harness (`Emulator`, `error_code`, `string_at`), the two routes, the reader and the
//! web-app setup that keeps a domain in `DELETING` are the sibling suites': one way of
//! driving this service, not two.
//!
//! Constraints quoted beside each row are the model's own
//! (`smithy_query(query="closure",operation="StartPatientInsightsJob",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`).

use std::collections::BTreeSet;
use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_connecthealth_connecthealth::state::{self};

use crate::arn_resolution_during_domain_deletion::web_app_setup;
use crate::domain_subscription_lifecycle::{error_code, string_at, Emulator};
use crate::patient_insights_job_lifecycle::{get_job, is_terminal, poll_until, start_path};

/// A `StartPatientInsightsJob` body carrying the required members only.
fn job_request() -> Value {
    json!({
        "patientContext": { "patientId": "patient-7781" },
        "insightsContext": { "insightsType": "PRE_VISIT" },
        "encounterContext": { "encounterReason": "Medication review" },
        "userContext": { "role": "CLINICIAN", "userId": "clinician-7781" },
        "inputDataConfig": { "s3Sources": [ { "uri": "s3://clinic-input/patient-7781/" } ] },
        "outputDataConfig": { "s3OutputPath": "s3://clinic-output/insights/" },
    })
}

/// A domain to start jobs in, created through `CreateDomain` with the request given (the
/// sibling suite's helper takes a name; a row here needs a web app as well).
async fn domain_with(emulator: &Emulator, request: &Value) -> String {
    string_at(&emulator.create_domain(request).await, "domainId")
}

/// Everything `StartPatientInsightsJob` OWNS in this service's store, read straight out
/// of the shared engine: one line per job record.
///
/// Across EVERY domain, including one in `DELETING`: a refused start must write nothing
/// anywhere, not merely nothing under the domain it named. Each line carries what the
/// START decided and the clock never touches — the identifiers, the ARN, the creation
/// instant and the parameters the job was stored with — so a refusal that overwrote an
/// existing record in place is caught too, while the status and last-updated instant the
/// CLOCK owns are deliberately left out (those move on their own, call or no call).
fn stored_jobs(emulator: &Emulator) -> BTreeSet<String> {
    let store = state::service_state(&**emulator.engine()).expect("this service's own store");
    let store = store.lock();
    let mut lines = BTreeSet::new();
    for domain in store.domains.values() {
        for (job_id, job) in &domain.patient_insights_jobs {
            let created = job
                .created_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            lines.insert(format!(
                "job {}/{job_id} arn={} created={created}ns spec={:?}",
                domain.domain_id, job.arn, job.spec
            ));
        }
    }
    lines
}

#[tokio::test]
async fn a_refused_start_writes_no_job_record() {
    let home = "us-west-2";
    let elsewhere = "eu-central-1";
    let emulator = Emulator::new(home);
    let domain_id = domain_with(&emulator, &json!({ "name": "clinic-write-discipline" })).await;

    // One accepted start, so the refusals below are asserted against a store that
    // already holds a record: a refused call must leave THAT record alone too, rather
    // than being asserted only against an empty store.
    let mut accepted = job_request();
    accepted["clientToken"] = json!("token-write-discipline");
    let (status, started) = emulator
        .send("POST", &start_path(&domain_id), Some(&accepted))
        .await;
    assert_eq!(
        status, 200,
        "the first start must be accepted, got {status} with {started}"
    );
    let job_id = string_at(&started, "jobId");

    // A domain whose deletion cannot finish inside the deleting call, so it is still
    // held in `DELETING` when the row below addresses it.
    let deleting = domain_with(
        &emulator,
        &json!({ "name": "clinic-deleting", "webAppSetupConfiguration": web_app_setup() }),
    )
    .await;
    let (status, deleted) = emulator
        .send("DELETE", &format!("/domain/{deleting}"), None)
        .await;
    assert_eq!(
        status, 200,
        "DeleteDomain must be accepted, got {status} with {deleted}"
    );

    let before = stored_jobs(&emulator);
    assert_eq!(
        before.len(),
        1,
        "the accepted start must have written one record, got {before:?}"
    );
    assert!(
        before
            .iter()
            .any(|line| line.contains(&format!("job {domain_id}/{job_id} "))),
        "the accepted start's record must be held under its domain, got {before:?}"
    );

    // A request the MODEL rejects: `outputDataConfig` is @required, and `DomainId`
    // carries `@length 20..25` with pattern `^(hai-|dom-)[a-z0-9]+$`.
    let mut incomplete = job_request();
    incomplete
        .as_object_mut()
        .expect("an object body")
        .remove("outputDataConfig");

    // The rows differ along what refuses the start: which identifier fails to resolve,
    // which Region the request is signed for, the domain's own lifecycle state, and the
    // modelled shape of the request.
    let rows: Vec<(&str, &str, String, Value, u16, &str)> = vec![
        (
            "a domainId no domain holds",
            home,
            "dom-0123456789abcdef0".to_string(),
            job_request(),
            401,
            "AccessDeniedException",
        ),
        (
            "a live domain of another Region",
            elsewhere,
            domain_id.clone(),
            job_request(),
            401,
            "AccessDeniedException",
        ),
        (
            "a domain on its way out",
            home,
            deleting.clone(),
            job_request(),
            401,
            "AccessDeniedException",
        ),
        (
            "a domainId outside the modelled pattern",
            home,
            "clinic-not-an-id-0000".to_string(),
            job_request(),
            400,
            "ValidationException",
        ),
        (
            "a request missing a required member",
            home,
            domain_id.clone(),
            incomplete,
            400,
            "ValidationException",
        ),
    ];

    for (case, region, domain, body, expected_status, expected_code) in rows {
        let (status, refused) = emulator
            .send_in(region, "POST", &start_path(&domain), Some(&body))
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected the modelled {expected_status}, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            expected_code,
            "case {case:?}: expected {expected_code}, got {refused}"
        );
        let after = stored_jobs(&emulator);
        assert_eq!(
            after, before,
            "case {case:?}: a refused start must write no job record and change none, \
             expected {before:?}, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_repeated_token_starts_a_second_job_and_leaves_the_first_alone() {
    let emulator = Emulator::new("ap-southeast-2");
    let domain_id = domain_with(&emulator, &json!({ "name": "clinic-idempotent-clock" })).await;

    // The rows differ along HOW FAR the clock has moved the first job when the second
    // start carrying the same token arrives: not yet picked up, picked up, and run to its
    // end — the whole modelled lifecycle this emulator's own jobs walk (`JobStatus`:
    // SUBMITTED -> IN_PROGRESS -> a terminal status). Each row waits by LOOKING at the job
    // until that is what it reports, never by sleeping for the spans it owes.
    type Reached = fn(&str) -> bool;
    let rows: [(&str, Option<Reached>); 3] = [
        ("read straight after the start", None),
        ("once the clock has picked it up", Some(|status| status != "SUBMITTED")),
        ("once it has run to its end", Some(is_terminal)),
    ];

    for (index, (case, reached)) in rows.into_iter().enumerate() {
        let mut body = job_request();
        body["clientToken"] = json!(format!("token-{case}"));
        let (status, first) = emulator
            .send("POST", &start_path(&domain_id), Some(&body))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first start must be accepted, got {status} with {first}"
        );
        let job_id = string_at(&first, "jobId");

        // Where the clock has taken the first job by the time the second start arrives.
        let before = match reached {
            None => {
                let fetched = get_job(&emulator, case, &domain_id, &job_id).await;
                // The span the clock owes is never zero, so a job read straight after its
                // start has not moved yet.
                assert_eq!(
                    string_at(&fetched, "jobStatus"),
                    "SUBMITTED",
                    "case {case:?}: a job read straight after the start must still be \
                     SUBMITTED, got {fetched}"
                );
                fetched
            }
            Some(reached) => poll_until(&emulator, case, &domain_id, &job_id, reached).await,
        };
        let terminal = is_terminal(&string_at(&before, "jobStatus"));
        if terminal {
            assert!(
                before.get("insightsOutput").is_some(),
                "case {case:?}: a completed job must report where its output went, got {before}"
            );
        }

        // The second start: the SAME token and a byte-identical body, which is a job of
        // its own rather than a replay of the first.
        let (status, second) = emulator
            .send("POST", &start_path(&domain_id), Some(&body))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a start repeating a token must be accepted, got {status} with \
             {second}"
        );
        let second_job = string_at(&second, "jobId");
        assert_ne!(
            second_job, job_id,
            "case {case:?}: a start repeating a token must mint a second job, got the \
             first job's id back in {second}"
        );

        // And it left the first job alone: for the terminal row the record reads exactly
        // as it did before, status, status detail and last-updated instant included. For
        // the non-terminal rows the clock may legitimately have moved the job on in
        // between, so what must not change is what the START owns — the identifiers, the
        // creation instant and the parameters.
        let after = get_job(&emulator, case, &domain_id, &job_id).await;
        if terminal {
            assert_eq!(
                after, before,
                "case {case:?}: a second start must leave a terminal record untouched \
                 ({before}), got {after}"
            );
        } else {
            for name in [
                "jobId",
                "jobArn",
                "creationTime",
                "patientContext",
                "insightsContext",
                "encounterContext",
                "userContext",
                "inputDataConfig",
                "outputDataConfig",
            ] {
                assert_eq!(
                    after.get(name),
                    before.get(name),
                    "case {case:?}: a second start must not change the first job's {name}, \
                     expected {before}, got {after}"
                );
            }
        }

        // One record per accepted start, and nothing more: the two starts of this row,
        // plus the two of every row before it.
        let held = stored_jobs(&emulator);
        assert_eq!(
            held.len(),
            2 * (index + 1),
            "case {case:?}: each accepted start must hold exactly one record, expected {} \
             line(s) of stored state, got {held:?}",
            2 * (index + 1)
        );
        for minted in [&job_id, &second_job] {
            assert!(
                held.iter()
                    .any(|line| line.contains(&format!("job {domain_id}/{minted} "))),
                "case {case:?}: the job {minted} a start minted must be held under its \
                 domain, got {held:?}"
            );
        }
    }
}
