// SPDX-License-Identifier: CC0-1.0

//! What one investigation CONCLUDES, end to end over the wire (the harness lives in
//! the `detector_lifecycle` suite).
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: an investigation whose analysis pass has ended reports that pass's
//! conclusions, derived from the findings the detector holds — and reports no `Error`,
//! which belongs to an investigation that FAILED.
//!
//! 1. The terminal status of an accepted investigation is `COMPLETED`, and the body
//!    then carries `Metadata` (the product, its feature and the engine version that
//!    produced the results), `Cloud`, `RiskLevel`, `Risk`, `Confidence` and `Summary`,
//!    with NO `Error` member.
//! 2. `RiskLevel` is "the assessed risk level of the investigated threat"
//!    (`smithy_query(shape="Investigation")`), so it is the band the HIGHEST severity
//!    among the analysed findings sits in, read off those findings' own reported
//!    severities through `GetFindings`. An account with no findings has no threat to
//!    assess, which is `Info`.
//! 3. `Confidence` is "the confidence level of the investigation's assessment": it is
//!    `Unknown` exactly when the pass read no findings at all, and never DROPS as more
//!    findings corroborate the assessed band.
//! 4. `Summary` is "a structured summary of the investigation findings, including
//!    affected resources, threat assessment, and recommended remediation steps", so it
//!    names the account and Region the pass covered, every affected resource type the
//!    analysed findings report and the finding types they came from.
//! 5. The conclusions are STORED, not recomputed per read: every later read of one
//!    investigation reports the same assessment, and `ListInvestigations` reports the
//!    same `RiskLevel` and `Confidence` for it, plus the summary view's own `Title`.
//! 6. A prompt that names findings the account holds narrows the pass to them, so an
//!    investigation of one Low finding is assessed Low even while the account holds a
//!    Critical one.
//!
//! The rows differ along what the rule generalises over: how many findings the detector
//! holds (none, one, several), WHICH severity bands they fall in, how many of them
//! corroborate the top band, and which resource types they affect.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use crate::detector_lifecycle::{
    all_finding_ids, call, investigation_detector, new_router, Router,
};
use service_com_amazonaws_guardduty_guarddutyapiservice::state::INVESTIGATION_RUNNING;

/// The `InvestigationStatus` an accepted investigation reaches once its pass has run
/// (`smithy_query(shape="InvestigationStatus")`).
const COMPLETED: &str = "COMPLETED";

/// `CloudProvider`'s only member — "currently, only `AWS` is supported"
/// (`smithy_query(query="doc",shape="CloudDetails",member="Provider")`).
const CLOUD_PROVIDER: &str = "AWS";

/// The `RiskLevel` members in ascending order of risk: the enum is `Info | Low |
/// Medium | High | Critical` (`smithy_query(shape="RiskLevel")`).
const RISK_LEVELS_ASCENDING: [&str; 5] = ["Info", "Low", "Medium", "High", "Critical"];

/// The `Confidence` members in ascending order: `Unknown | Low | Medium | High`
/// (`smithy_query(shape="Confidence")`).
const CONFIDENCES_ASCENDING: [&str; 4] = ["Unknown", "Low", "Medium", "High"];

/// GuardDuty's finding severity BANDS, each with the lowest numeric severity in it:
/// "Low 1.0–3.9, Medium 4.0–6.9, High 7.0–8.9, Critical 9.0–10.0" (GuardDuty
/// "Severity levels for GuardDuty findings"). The band names are also `RiskLevel`
/// members, which is what lets a severity be read as a risk level.
const SEVERITY_BANDS: [(f64, &str); 4] = [
    (9.0, "Critical"),
    (7.0, "High"),
    (4.0, "Medium"),
    (1.0, "Low"),
];

/// How long a poll may wait for the clock to end an analysis before the test gives up.
/// Generous on purpose: the loop ends on the CONDITION it looks for, never on a
/// duration, so a loaded host takes more attempts rather than a different outcome.
const ENDED_DEADLINE: Duration = Duration::from_secs(30);

/// How long the poll pauses between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: what it exercises and the sample finding types the detector is given.
///
/// The types are members of this service's sample-finding catalogue (the types
/// `CreateSampleFindings` generates, itself the GuardDuty finding-types reference); the
/// SEVERITY of each is never written here — every expectation below is computed from
/// the severities the findings themselves report through `GetFindings`.
struct Row {
    case: &'static str,
    finding_types: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an account with no findings at all",
            finding_types: &[],
        },
        Row {
            case: "one finding in the lowest band",
            finding_types: &["UnauthorizedAccess:EC2/SSHBruteForce"],
        },
        Row {
            case: "one finding in the middle of the scale",
            finding_types: &["Recon:EC2/Portscan"],
        },
        Row {
            case: "two findings, the worse of them high",
            finding_types: &["Backdoor:EC2/C&CActivity.B", "Discovery:S3/AnomalousBehavior"],
        },
        Row {
            case: "three findings over three resource types, the worst critical",
            finding_types: &[
                "AttackSequence:IAM/CompromisedCredentials",
                "Policy:S3/BucketAnonymousAccessGranted",
                "Impact:EC2/PortSweep",
            ],
        },
        Row {
            case: "two findings in the same top band, corroborating each other",
            finding_types: &[
                "Backdoor:EC2/C&CActivity.B",
                "Exfiltration:S3/MaliciousIPCaller",
            ],
        },
        Row {
            case: "four findings, three of them in the top band",
            finding_types: &[
                "AttackSequence:IAM/CompromisedCredentials",
                "AttackSequence:S3/CompromisedData",
                "CredentialAccess:Kubernetes/MaliciousIPCaller",
                "Discovery:S3/AnomalousBehavior",
            ],
        },
    ]
}

/// The risk level a numeric finding severity sits in, by [`SEVERITY_BANDS`]: the lowest
/// band whose floor the severity reaches, and `Info` below the scale's own minimum.
fn band_of(severity: f64) -> &'static str {
    SEVERITY_BANDS
        .iter()
        .find(|(floor, _)| severity >= *floor)
        .map_or("Info", |(_, band)| band)
}

/// Where a graded enum value sits on its scale.
fn rank(scale: &[&str], value: &str) -> usize {
    scale
        .iter()
        .position(|member| *member == value)
        .unwrap_or_else(|| panic!("{value:?} is not a member of {scale:?}"))
}

/// Generate one sample finding per entry of `types`; an empty list generates none,
/// because `CreateSampleFindings` reads an empty `findingTypes` as "every supported
/// type".
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

/// Every finding the detector holds, read back through `GetFindings` — the independent
/// view every expectation below is computed from.
async fn stored_findings(router: &Router, detector_id: &str, case: &str) -> Vec<Value> {
    let ids = all_finding_ids(router, detector_id, 50).await;
    if ids.is_empty() {
        return Vec::new();
    }
    let (status, body) = call(
        router,
        "POST",
        &format!("/detector/{detector_id}/findings/get"),
        Some(json!({"findingIds": ids})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: get-findings expected 200, got {status} {body:?}"
    );
    body["findings"].as_array().cloned().unwrap_or_default()
}

/// Initiate one investigation and return its id.
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

/// The `Investigation` a read reports.
async fn investigation(router: &Router, detector_id: &str, id: &str, case: &str) -> Value {
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
    body["investigation"].clone()
}

/// Read until the pass has ended, or until [`ENDED_DEADLINE`] has passed.
async fn ended(router: &Router, detector_id: &str, id: &str, case: &str) -> Value {
    let deadline = std::time::Instant::now() + ENDED_DEADLINE;
    loop {
        let got = investigation(router, detector_id, id, case).await;
        if got["status"] != json!(INVESTIGATION_RUNNING) {
            return got;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the analysis never ended within {ENDED_DEADLINE:?}, last read {got}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `InvestigationSummary` `ListInvestigations` reports for `id`.
async fn summary(router: &Router, detector_id: &str, id: &str, case: &str) -> Value {
    let (status, body) = call(
        router,
        "POST",
        &format!("/detector/{detector_id}/investigation/list"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: list-investigations expected 200, got {status} {body:?}"
    );
    body["investigations"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list, got {body:?}"))
        .iter()
        .find(|summary| summary["investigationId"] == json!(id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {id} to be listed, got {body:?}"))
}

#[tokio::test]
async fn a_completed_investigation_reports_the_assessment_its_pass_derived_from_the_findings() {
    let account = engine::emulator_account_id();
    // The Region the harness signs for, which is the one the detector and its
    // investigation live in.
    let region = "us-east-1";

    // RULE 3's monotonicity is asserted across the rows, in the order they corroborate
    // the assessed band: (corroborating findings, reported confidence).
    let mut confidence_by_corroboration: Vec<(usize, String)> = Vec::new();

    for Row {
        case,
        finding_types,
    } in rows()
    {
        // A store per row, so one row's findings cannot explain another's assessment.
        let router = new_router();
        let detector_id = investigation_detector(&router).await;
        create_samples(&router, &detector_id, finding_types, case).await;

        // The evidence, as the API itself reports it.
        let findings = stored_findings(&router, &detector_id, case).await;
        assert_eq!(
            findings.len(),
            finding_types.len(),
            "case {case:?}: expected one finding per requested type, got {findings:?}"
        );
        let severities: Vec<f64> = findings
            .iter()
            .map(|finding| {
                finding["severity"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a numeric severity, got {finding}")
                })
            })
            .collect();
        let highest = severities
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let expected_level = match severities.is_empty() {
            true => "Info",
            false => band_of(highest),
        };
        let corroborating = severities
            .iter()
            .filter(|severity| band_of(**severity) == expected_level)
            .count();

        let id = initiate(&router, &detector_id, "assess this account", case).await;
        let got = ended(&router, &detector_id, &id, case).await;

        // RULE 1: the terminal status, the whole assessment, and no error.
        assert_eq!(
            got["status"],
            json!(COMPLETED),
            "case {case:?}: expected {COMPLETED}, got {got}"
        );
        assert!(
            got["error"].is_null(),
            "case {case:?}: an investigation that did not fail reports no error, got {got}"
        );
        assert_eq!(
            got["metadata"],
            json!({
                "product": {"name": "Amazon GuardDuty AI Analyst", "feature": "Investigation"},
                "version": "1.0.0",
            }),
            "case {case:?}: expected the product and version that produced the results, got {:?}",
            got["metadata"]
        );
        assert_eq!(
            got["cloud"],
            json!({"provider": CLOUD_PROVIDER, "region": region, "account": account}),
            "case {case:?}: expected the environment the pass covered, got {:?}",
            got["cloud"]
        );

        // RULE 2: the assessed level is the band of the worst finding analysed.
        assert_eq!(
            got["riskLevel"],
            json!(expected_level),
            "case {case:?}: the highest severity analysed is {highest} ({expected_level}), got \
             {:?}",
            got["riskLevel"]
        );
        let risk = got["risk"].as_str().unwrap_or_default();
        assert!(
            !risk.is_empty() && risk.chars().count() <= 1024,
            "case {case:?}: expected a risk description within the modelled 1024 characters, \
             got {risk:?}"
        );

        // RULE 3: Unknown exactly when nothing was analysed.
        let confidence = got["confidence"].as_str().unwrap_or_default();
        assert_eq!(
            confidence == "Unknown",
            severities.is_empty(),
            "case {case:?}: {} findings were analysed, so Unknown is {}, got {confidence:?}",
            severities.len(),
            severities.is_empty()
        );
        assert!(
            CONFIDENCES_ASCENDING.contains(&confidence),
            "case {case:?}: expected a Confidence member, got {confidence:?}"
        );
        confidence_by_corroboration.push((corroborating, confidence.to_string()));

        // RULE 4: the summary names the environment, the affected resources and the
        // finding types the assessment came from.
        let summary_text = got["summary"].as_str().unwrap_or_default();
        assert!(
            summary_text.contains(account) && summary_text.contains(region),
            "case {case:?}: expected the summary to name account {account} in {region}, got \
             {summary_text:?}"
        );
        let resource_types: BTreeSet<String> = findings
            .iter()
            .filter_map(|finding| finding["resource"]["resourceType"].as_str())
            .map(str::to_string)
            .collect();
        for resource_type in &resource_types {
            assert!(
                summary_text.contains(resource_type.as_str()),
                "case {case:?}: expected the summary to name the affected {resource_type}, got \
                 {summary_text:?}"
            );
        }
        for finding_type in finding_types {
            assert!(
                summary_text.contains(finding_type),
                "case {case:?}: expected the summary to name {finding_type}, got {summary_text:?}"
            );
        }
        assert!(
            RISK_LEVELS_ASCENDING.contains(&got["riskLevel"].as_str().unwrap_or_default()),
            "case {case:?}: expected a RiskLevel member, got {got}"
        );

        // RULE 5: stored, not recomputed — later reads report the same assessment, and
        // the listing agrees with the read.
        for round in 0..2 {
            let again = investigation(&router, &detector_id, &id, case).await;
            assert_eq!(
                again, got,
                "case {case:?} (read {round}): an ended investigation's conclusions are stored, \
                 so every read reports the same ones, got {again} after {got}"
            );
        }
        let listed = summary(&router, &detector_id, &id, case).await;
        for member in ["riskLevel", "confidence"] {
            assert_eq!(
                listed[member], got[member],
                "case {case:?}: the listing and the read must report the same {member}, got \
                 {listed} and {got}"
            );
        }
        let title = listed["title"].as_str().unwrap_or_default();
        assert!(
            !title.is_empty() && title.chars().count() <= 1024,
            "case {case:?}: expected a title within the modelled 1024 characters, got {title:?}"
        );
    }

    // RULE 3, across the rows: more corroborating findings never lower the confidence.
    confidence_by_corroboration.sort_by_key(|(corroborating, _)| *corroborating);
    for pair in confidence_by_corroboration.windows(2) {
        let ((left_count, left), (right_count, right)) = (&pair[0], &pair[1]);
        assert!(
            rank(&CONFIDENCES_ASCENDING, left) <= rank(&CONFIDENCES_ASCENDING, right),
            "case \"confidence grows with corroboration\": {left_count} corroborating findings \
             gave {left:?} and {right_count} gave {right:?}"
        );
    }
}

/// RULE 6: a prompt naming findings the account holds narrows the pass to them, so the
/// assessment is of THOSE findings and not of the whole account.
#[tokio::test]
async fn a_prompt_that_names_a_finding_is_assessed_on_that_finding_alone() {
    let router = new_router();
    let detector_id = investigation_detector(&router).await;
    let case = "a prompt naming one finding";
    // One finding in the lowest band and one in the highest, so an assessment of the
    // named one cannot be mistaken for an assessment of the account.
    create_samples(
        &router,
        &detector_id,
        &[
            "UnauthorizedAccess:EC2/SSHBruteForce",
            "AttackSequence:IAM/CompromisedCredentials",
        ],
        case,
    )
    .await;

    let findings = stored_findings(&router, &detector_id, case).await;
    let lowest = findings
        .iter()
        .min_by(|left, right| {
            left["severity"]
                .as_f64()
                .unwrap_or_default()
                .partial_cmp(&right["severity"].as_f64().unwrap_or_default())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or_else(|| panic!("case {case:?}: expected findings, got {findings:?}"))
        .clone();
    let lowest_id = lowest["id"].as_str().unwrap_or_default().to_string();
    let lowest_band = band_of(lowest["severity"].as_f64().unwrap_or_default());

    // The whole account first: the worst finding decides it.
    let whole = ended(
        &router,
        &detector_id,
        &initiate(&router, &detector_id, "assess this account", case).await,
        case,
    )
    .await;
    let account_band = whole["riskLevel"].as_str().unwrap_or_default().to_string();
    assert!(
        rank(&RISK_LEVELS_ASCENDING, &account_band) > rank(&RISK_LEVELS_ASCENDING, lowest_band),
        "case {case:?}: the account holds a worse finding than {lowest_band}, got \
         {account_band:?}"
    );

    // Then the named finding alone.
    let narrowed = ended(
        &router,
        &detector_id,
        &initiate(
            &router,
            &detector_id,
            &format!("Investigate finding {lowest_id} in this account"),
            case,
        )
        .await,
        case,
    )
    .await;
    assert_eq!(
        narrowed["riskLevel"],
        json!(lowest_band),
        "case {case:?}: a pass narrowed to {lowest_id} is assessed on that finding's band \
         {lowest_band}, got {narrowed}"
    );
    let summary_text = narrowed["summary"].as_str().unwrap_or_default();
    let named_type = lowest["type"].as_str().unwrap_or_default();
    assert!(
        summary_text.contains(named_type),
        "case {case:?}: expected the summary to name {named_type}, got {summary_text:?}"
    );
    let other_type = findings
        .iter()
        .find(|finding| finding["id"].as_str() != Some(lowest_id.as_str()))
        .and_then(|finding| finding["type"].as_str())
        .unwrap_or_default();
    assert!(
        !summary_text.contains(other_type),
        "case {case:?}: a pass narrowed to one finding must not assess {other_type}, got \
         {summary_text:?}"
    );
}
