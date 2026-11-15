// SPDX-License-Identifier: CC0-1.0

//! The identity of a `StartPatientInsightsJob` JOB: every accepted start is a job of its
//! own, whatever `clientToken` it carries and whether or not the parameters beside it have
//! been submitted before.
//!
//! Every case drives the service the way a client does — a signed restJson1 request
//! through the router's own dispatch — and asserts the RULE rather than one recorded
//! body: `clientToken` is an `@idempotencyToken`, which is a rule about how an SDK FILLS
//! that member rather than a promise that the service deduplicates on it, so a token
//! already used starts a second, independent job with its own `jobId` and `jobArn`, and
//! what each job reads back is the parameter set ITS OWN start supplied.
//!
//! The rows generalise over everything that can make two parameter sets differ, and over
//! the same set submitted twice: whether an optional member was supplied at all, whether
//! the list it carries is empty, how many elements it carries, in which order, and which
//! section of `inputDataConfig` a value sits in. The empty list is one of the values the
//! member admits: `S3Sources` declares `@length {max: 10}` and no minimum
//! (`smithy_query(query="shape",shape="InputDataConfig",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::domain_subscription_lifecycle::{string_at, Emulator};
use crate::patient_insights_job_lifecycle::{get_job, minimal_body, start_path};
use crate::patient_insights_job_terminal_output::domain_of;

/// `S3Sources`: `@length {max: 10}`, with no minimum.
const MAX_S3_SOURCES: usize = 10;

/// An `s3Sources` list of `n` distinct sources, each a valid `S3Uri`.
fn sources(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|index| json!({ "uri": format!("s3://clinic-input/chart-{index}.json") }))
            .collect(),
    )
}

/// A `StartPatientInsightsJob` body whose `inputDataConfig` is `config` and whose
/// `clientToken` is `token`; every other member is the minimal required set, so a pair of
/// bodies differs in exactly the parameters the row varies.
fn body_with(config: &Value, token: &str) -> Value {
    let mut body = minimal_body();
    body["inputDataConfig"] = config.clone();
    body["clientToken"] = json!(token);
    body
}

/// The pairs of `inputDataConfig` the model admits as DIFFERENT: one row per way two of
/// them can differ without differing in any other member. Neither spelling of a row may
/// be read as the other, in either order.
fn differing_pairs() -> Vec<(String, Value, Value)> {
    let endpoint = "https://fhir.example.org/r4";
    // A value that is a valid `S3Uri` and a valid `oauthToken` at once, so the row below
    // can present it in each section in turn.
    let shared = "s3://clinic-input/chart-0.json";
    vec![
        (
            "an absent s3Sources against an empty one".to_string(),
            json!({}),
            json!({ "s3Sources": [] }),
        ),
        (
            "an empty s3Sources against a single source".to_string(),
            json!({ "s3Sources": [] }),
            json!({ "s3Sources": sources(1) }),
        ),
        (
            "one source against two".to_string(),
            json!({ "s3Sources": sources(1) }),
            json!({ "s3Sources": sources(2) }),
        ),
        (
            format!("{} sources against the modelled maximum of {MAX_S3_SOURCES}", MAX_S3_SOURCES - 1),
            json!({ "s3Sources": sources(MAX_S3_SOURCES - 1) }),
            json!({ "s3Sources": sources(MAX_S3_SOURCES) }),
        ),
        (
            "the same two sources in the other order".to_string(),
            json!({ "s3Sources": [
                { "uri": "s3://clinic-input/chart-0.json" },
                { "uri": "s3://clinic-input/chart-1.json" },
            ] }),
            json!({ "s3Sources": [
                { "uri": "s3://clinic-input/chart-1.json" },
                { "uri": "s3://clinic-input/chart-0.json" },
            ] }),
        ),
        (
            "one source against a different source".to_string(),
            json!({ "s3Sources": [{ "uri": "s3://clinic-input/chart-0.json" }] }),
            json!({ "s3Sources": [{ "uri": "s3://clinic-input/chart-9.json" }] }),
        ),
        (
            "an absent s3Sources against an empty one, beside a configured FHIR server"
                .to_string(),
            json!({ "fhirServer": { "fhirEndpoint": endpoint } }),
            json!({ "fhirServer": { "fhirEndpoint": endpoint }, "s3Sources": [] }),
        ),
        (
            "an absent fhirServer against a configured one".to_string(),
            json!({ "s3Sources": sources(1) }),
            json!({ "fhirServer": { "fhirEndpoint": endpoint }, "s3Sources": sources(1) }),
        ),
        (
            "one value carried by the FHIR server against the same value as a source"
                .to_string(),
            json!({ "fhirServer": { "fhirEndpoint": endpoint, "oauthToken": shared } }),
            json!({
                "fhirServer": { "fhirEndpoint": endpoint },
                "s3Sources": [{ "uri": shared }],
            }),
        ),
        (
            "an input config with neither source against one with both".to_string(),
            json!({}),
            json!({
                "fhirServer": { "fhirEndpoint": endpoint, "oauthToken": "token-value" },
                "s3Sources": sources(2),
            }),
        ),
    ]
}

#[tokio::test]
async fn one_token_presented_with_either_spelling_starts_a_job_for_each() {
    let emulator = Emulator::new("us-west-2");
    let domain_id = domain_of(&emulator, "clinic-request-identity").await;

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (index, (case, left, right)) in differing_pairs().into_iter().enumerate() {
        // Each ORDER on its own token, so neither spelling is privileged: whichever of
        // the two started the first job, the other one starts a job of its own under the
        // same token.
        for (order, first, second) in [
            (0, left.clone(), right.clone()),
            (1, right.clone(), left.clone()),
        ] {
            let case = format!(
                "{case}, {} spelling first",
                if order == 0 { "first" } else { "second" }
            );
            let case = case.as_str();
            let token = format!("token-{index}-{order}");
            let (status, started) = emulator
                .send(
                    "POST",
                    &start_path(&domain_id),
                    Some(&body_with(&first, &token)),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the first start must answer 200, got {status} with {started}"
            );
            let job_id = string_at(&started, "jobId");
            assert!(
                minted.insert(job_id.clone()),
                "case {case:?}: every accepted start must mint its own job, got the \
                 reused {job_id:?}"
            );

            let (status, again) = emulator
                .send(
                    "POST",
                    &start_path(&domain_id),
                    Some(&body_with(&second, &token)),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the same token with different parameters must answer 200, \
                 got {status} with {again}"
            );
            let second_job = string_at(&again, "jobId");
            assert!(
                minted.insert(second_job.clone()),
                "case {case:?}: a start repeating a token must mint its own job, got the \
                 reused {second_job:?}"
            );

            // Each job reads back with the parameter set ITS OWN start supplied, so the
            // second start neither replayed nor overwrote the first.
            for (which, job, expected) in [
                ("the first job", &job_id, &first),
                ("the second job", &second_job, &second),
            ] {
                let fetched = get_job(&emulator, case, &domain_id, job).await;
                assert_eq!(
                    fetched.get("inputDataConfig").cloned().unwrap_or(Value::Null),
                    *expected,
                    "case {case:?}: {which} must carry the parameters it was started \
                     with, got {fetched}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_same_parameters_presented_again_start_another_job() {
    let emulator = Emulator::new("eu-west-1");
    let domain_id = domain_of(&emulator, "clinic-request-replay").await;

    // Every spelling the table names, each presented three times under one token of its
    // own: a repeat is still a submission, so the second and third calls each start a job
    // of their own, and every one of them reads back with the parameters it was given.
    let mut configs: Vec<(String, Value)> = Vec::new();
    for (case, left, right) in differing_pairs() {
        configs.push((format!("{case} (first spelling)"), left));
        configs.push((format!("{case} (second spelling)"), right));
    }

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (index, (case, config)) in configs.into_iter().enumerate() {
        let case = case.as_str();
        let token = format!("replay-{index}");
        let body = body_with(&config, &token);
        for attempt in ["first call", "second call", "third call"] {
            let (status, started) = emulator
                .send("POST", &start_path(&domain_id), Some(&body))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?} / {attempt}: a start must answer 200, got {status} with \
                 {started}"
            );
            let job_id = string_at(&started, "jobId");
            assert!(
                minted.insert(job_id.clone()),
                "case {case:?} / {attempt}: every accepted start must mint its own job, \
                 got the reused {job_id:?}"
            );

            // What the start persisted is what the reader projects: an absent member stays
            // absent and a supplied empty list reads back as an empty list, which is what
            // makes the two spellings observably different records.
            let fetched = get_job(&emulator, case, &domain_id, &job_id).await;
            assert_eq!(
                fetched.get("inputDataConfig").cloned().unwrap_or(Value::Null),
                config,
                "case {case:?} / {attempt}: inputDataConfig must read back exactly as it \
                 was persisted, got {fetched}"
            );
        }
    }
}
