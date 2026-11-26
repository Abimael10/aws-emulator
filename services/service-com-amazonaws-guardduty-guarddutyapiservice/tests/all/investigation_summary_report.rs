// SPDX-License-Identifier: CC0-1.0

//! WHAT one investigation's `Summary` reports, end to end over the wire (the harness
//! lives in the `detector_lifecycle` suite).
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: `Summary` is "a structured summary of the investigation findings, including
//! affected resources, threat assessment, and recommended remediation steps"
//! (`smithy_query(shape="Investigation")`), so the report an ended pass serves is a
//! report of THAT investigation — the request it was initiated with and the evidence it
//! analysed — and not a fixed text. Two investigations that asked different things, or
//! analysed different findings, therefore do not read alike, and a report always carries
//! each part the documentation names.
//!
//! The rows differ along what the rule generalises over: the `TriggerPrompt` the
//! investigation was initiated with (its wording, its length, the characters it carries),
//! and how much evidence the detector held when the pass ran (none, one finding, several
//! over several resource types).
//!
//! The assessment members beside it — `RiskLevel`, `Risk`, `Confidence` and the listing's
//! `Title` — are the `investigation_analysis` suite's rules; this suite owns the report.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use crate::detector_lifecycle::{call, investigation_detector, new_router, Router};
use service_com_amazonaws_guardduty_guarddutyapiservice::state::INVESTIGATION_RUNNING;

/// How long a poll may wait for the clock to end an analysis before the test gives up.
/// Generous on purpose: the loop ends on the CONDITION it looks for, never on a duration,
/// so a loaded host takes more attempts rather than a different outcome.
const ENDED_DEADLINE: Duration = Duration::from_secs(30);

/// How long the poll pauses between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The headings the report carries, one per part the member's documentation names, plus
/// the opening section that states what was investigated.
const SECTIONS: [&str; 4] = [
    "## Investigation summary",
    "## Affected resources",
    "## Threat assessment",
    "## Recommended remediation",
];

/// The prompts the rows initiate their investigations with: a plain request, one that
/// names a Region and a resource, a long one, and one carrying punctuation and non-ASCII
/// characters — a `TriggerPrompt` is "the natural-language prompt that initiated this
/// investigation" (`smithy_query(shape="Investigation")`) with a modelled `len 1..2048`,
/// so none of that is any part of what a report may carry.
fn prompts() -> Vec<(&'static str, String)> {
    vec![
        ("a plain request", "Investigate this account".to_string()),
        (
            "a request naming a Region and a resource",
            "Investigate the EC2 instance activity in eu-west-1".to_string(),
        ),
        (
            "a long request",
            format!("Investigate {}", "unexpected activity ".repeat(40)),
        ),
        (
            "a request carrying punctuation and non-ASCII characters",
            "Enquête : activité suspecte (compte partagé) — à vérifier ?".to_string(),
        ),
    ]
}

/// The sample finding types a row gives its detector. The types are members of this
/// service's sample-finding catalogue (the types `CreateSampleFindings` generates, itself
/// the GuardDuty finding-types reference).
fn evidence_sets() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("a detector holding no findings", &[]),
        (
            "a detector holding one finding",
            &["Recon:EC2/Portscan"],
        ),
        (
            "a detector holding three findings over three resource types",
            &[
                "AttackSequence:IAM/CompromisedCredentials",
                "Policy:S3/BucketAnonymousAccessGranted",
                "Impact:EC2/PortSweep",
            ],
        ),
    ]
}

/// Generate one sample finding per entry of `types`; an empty list generates none,
/// because `CreateSampleFindings` reads an empty `findingTypes` as "every supported type".
async fn create_samples(router: &Router, detector_id: &str, types: &[&str], case: &str) {
    if types.is_empty() {
        return;
    }
    let (status, body) = call(
        router,
        "POST",
        &format!("/detector/{detector_id}/findings/create"),
        Some(json!({"findingTypes": types})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: create-sample-findings expected 200, got {status} {body:?}"
    );
}

/// Initiate one investigation with `prompt` and return its id.
async fn initiate(router: &Router, detector_id: &str, prompt: &str, case: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/detector/{detector_id}/investigation"),
        Some(json!({"triggerPrompt": prompt})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: create-investigation expected 200, got {status} {body:?}"
    );
    body["investigationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an investigationId, got {body:?}"))
        .to_string()
}

/// Read the investigation until its pass has ended, or until [`ENDED_DEADLINE`] passes.
async fn ended(router: &Router, detector_id: &str, id: &str, case: &str) -> Value {
    let deadline = std::time::Instant::now() + ENDED_DEADLINE;
    loop {
        let (status, body) = call(
            router,
            "GET",
            &format!("/detector/{detector_id}/investigation/{id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: get-investigation expected 200, got {status} {body:?}"
        );
        let investigation = body["investigation"].clone();
        if investigation["status"] != json!(INVESTIGATION_RUNNING) {
            return investigation;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the analysis never ended within {ENDED_DEADLINE:?}, last read \
             {investigation}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The report one ended investigation serves.
async fn report(router: &Router, detector_id: &str, id: &str, case: &str) -> String {
    let investigation = ended(router, detector_id, id, case).await;
    investigation["summary"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a summary on the ended investigation, got {investigation}")
        })
        .to_string()
}

#[tokio::test]
async fn a_report_states_the_request_it_answers_and_the_evidence_it_analysed() {
    for (evidence_case, finding_types) in evidence_sets() {
        for (prompt_case, prompt) in prompts() {
            // A store per row, so one row's findings and prompts cannot explain
            // another's report.
            let router = new_router();
            let detector_id = investigation_detector(&router).await;
            let case = format!("{evidence_case}, {prompt_case}");
            create_samples(&router, &detector_id, finding_types, &case).await;

            let id = initiate(&router, &detector_id, &prompt, &case).await;
            let served = report(&router, &detector_id, &id, &case).await;

            // The request this investigation answers, so the report is this
            // investigation's and not any other's.
            assert!(
                served.contains(prompt.as_str()),
                "case {case:?}: expected the report to state the request {prompt:?} it answers, \
                 got {served:?}"
            );
            // Every part the member's documentation names.
            for section in SECTIONS {
                assert!(
                    served.contains(section),
                    "case {case:?}: expected the report to carry the {section:?} part, got \
                     {served:?}"
                );
            }
            // And the finding types the pass analysed, which is the evidence the
            // assessment rests on.
            for finding_type in finding_types {
                assert!(
                    served.contains(finding_type),
                    "case {case:?}: expected the report to name the analysed {finding_type}, got \
                     {served:?}"
                );
            }
        }
    }
}

/// Two investigations that asked different things of the SAME account read differently:
/// the report carries the investigation's own request, so a detector holding nothing no
/// longer answers one fixed text for every investigation of it.
#[tokio::test]
async fn two_investigations_of_one_account_do_not_read_alike() {
    for (evidence_case, finding_types) in evidence_sets() {
        let router = new_router();
        let detector_id = investigation_detector(&router).await;
        create_samples(&router, &detector_id, finding_types, evidence_case).await;

        let mut reports: BTreeSet<String> = BTreeSet::new();
        for (prompt_case, prompt) in prompts() {
            let case = format!("{evidence_case}, {prompt_case}");
            let id = initiate(&router, &detector_id, &prompt, &case).await;
            let served = report(&router, &detector_id, &id, &case).await;
            assert!(
                reports.insert(served.clone()),
                "case {case:?}: a report of its own investigation cannot repeat another's, got \
                 {served:?}"
            );
        }
        assert_eq!(
            reports.len(),
            prompts().len(),
            "case {evidence_case:?}: expected one report per investigation, got {reports:?}"
        );
    }
}

/// The report is STORED when the pass ends, not recomputed per read: a later read, and a
/// read taken after the account's findings have changed, serve the report the pass
/// produced.
#[tokio::test]
async fn a_report_is_the_one_the_pass_produced_whatever_the_store_holds_later() {
    let router = new_router();
    let detector_id = investigation_detector(&router).await;
    let case = "a report read again after the account's findings changed";

    let id = initiate(&router, &detector_id, "Investigate this account", case).await;
    let served = report(&router, &detector_id, &id, case).await;

    // Findings produced AFTER the pass ended are not part of the report it produced.
    create_samples(
        &router,
        &detector_id,
        &["Backdoor:EC2/C&CActivity.B"],
        case,
    )
    .await;
    for round in 0..2 {
        let again = report(&router, &detector_id, &id, case).await;
        assert_eq!(
            again, served,
            "case {case:?} (read {round}): expected the stored report {served:?}, got {again:?}"
        );
    }
}
