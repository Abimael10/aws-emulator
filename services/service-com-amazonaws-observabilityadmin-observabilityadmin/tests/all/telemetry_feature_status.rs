// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for this service's telemetry-FEATURE surface: the
//! telemetry-config (telemetry evaluation) feature of an account and of an
//! organization, and the resource tags for telemetry (telemetry enrichment)
//! feature.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`,
//! restJson1 serialization, the modelled-constraint walk and the handlers — so what
//! is asserted is what a signed AWS SDK request observes.
//!
//! The rule under test, over the whole surface rather than one operation: a
//! feature-status read answers the modelled 200 document (or, where the model gives
//! one for a feature that was never started, that modelled error) computed from the
//! state the feature's own `Start`/`Stop` calls recorded for the region the request
//! was signed for — the transient status until the transition that call began comes
//! due and the terminal one from then on — and never an unmodelled error. Its
//! consequences, each asserted over a table of inputs:
//!
//! 1. a feature no call has ever named reports its own "never started" answer:
//!    `NOT_STARTED` for telemetry evaluation, the modelled 404 for telemetry
//!    enrichment;
//! 2. a read is a read: two reads with no call between them agree, and a read of an
//!    unconfigured feature stores nothing;
//! 3. the status a read reports is the one the last `Start`/`Stop` for that region
//!    is due to have reached, and the multi-region members are reported exactly when
//!    "multi-region telemetry evaluation is active";
//! 4. the account's feature and its organization's are separate features;
//! 5. a `Start` whose multi-region scope the model forbids is a 400
//!    `ValidationException`, and a redundant `Stop` is an idempotent no-op.
//!
//! The last section of this file is about the telemetry-config AUDIT — `ListResourceTelemetry`,
//! "a list of telemetry configurations for Amazon Web Services resources supported by
//! telemetry config" — which is the same feature's read over the account's RESOURCES rather
//! than over the feature itself: see the rule stated above that section.

use std::sync::Arc;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_observabilityadmin_observabilityadmin::state::{
    ComAmazonawsObservabilityadminObservabilityadminState, EVALUATION_TRANSITION_DELAY_MS,
    MAX_TELEMETRY_FEATURE_REGIONS,
};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness this test binary already has: one emulator mounted exactly as the
// router mounts it, a signed request, and the response accessors. Imported rather
// than copied so the two suites cannot drift apart.
use super::telemetry_rules::{call, emulator, error_code, Emulator, ACCOUNT};

/// The region most requests below sign for.
const HOME_REGION: &str = "us-west-2";

/// Long enough after a `Start` or a `Stop` for the transition it began to have come
/// due, whatever scheduling delay the test run adds on top.
fn past_transition_delay() -> Duration {
    Duration::from_millis(EVALUATION_TRANSITION_DELAY_MS as u64 + 250)
}

/// One string member of a response document.
fn member(body: &Value, name: &str) -> Option<String> {
    body.get(name).and_then(Value::as_str).map(str::to_string)
}

/// Which telemetry-config feature a case drives: the account's own, or its
/// organization's. The two are the same three operations under different routes, so
/// every evaluation case below runs once per scope.
#[derive(Debug, Clone, Copy)]
enum Scope {
    Account,
    Organization,
}

impl Scope {
    const ALL: [Scope; 2] = [Scope::Account, Scope::Organization];

    fn start_uri(self) -> &'static str {
        match self {
            Scope::Account => "/StartTelemetryEvaluation",
            Scope::Organization => "/StartTelemetryEvaluationForOrganization",
        }
    }

    fn stop_uri(self) -> &'static str {
        match self {
            Scope::Account => "/StopTelemetryEvaluation",
            Scope::Organization => "/StopTelemetryEvaluationForOrganization",
        }
    }

    fn status_uri(self) -> &'static str {
        match self {
            Scope::Account => "/GetTelemetryEvaluationStatus",
            Scope::Organization => "/GetTelemetryEvaluationStatusForOrganization",
        }
    }
}

async fn start_evaluation(
    emulator: &Emulator,
    scope: Scope,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    call(emulator, region, scope.start_uri(), body).await
}

async fn stop_evaluation(emulator: &Emulator, scope: Scope, region: &str) -> (u16, Value) {
    call(emulator, region, scope.stop_uri(), &json!({})).await
}

async fn evaluation_status(emulator: &Emulator, scope: Scope, region: &str) -> (u16, Value) {
    call(emulator, region, scope.status_uri(), &json!({})).await
}

/// `Status`, and the per-region statuses keyed by region, of one status read.
fn region_statuses(body: &Value) -> Vec<(String, String)> {
    body.get("RegionStatuses")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        member(entry, "Region").unwrap_or_else(|| "<no Region>".to_string()),
                        member(entry, "Status").unwrap_or_else(|| "<no Status>".to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The telemetry-config feature of a region no `Start` or `Stop` ever named has
/// never been onboarded, which the `Status` enum's own `NOT_STARTED` reports — and
/// reading it stores nothing, so a later reader sees exactly the same.
#[tokio::test]
async fn an_unconfigured_evaluation_feature_reports_not_started_without_storing_anything() {
    // Rows differ in the region signed for, so the answer cannot come from one
    // hardcoded region: an unconfigured feature reports the same in every one.
    let regions = ["us-west-2", "eu-central-1", "ap-southeast-2"];
    for scope in Scope::ALL {
        let emulator = emulator();
        for region in regions {
            let case = format!("{scope:?} in {region}");
            for read in 0..2 {
                let (status, body) = evaluation_status(&emulator, scope, region).await;
                assert_eq!(
                    status, 200,
                    "case {case}, read {read}: expected 200, got {status} {body:?}"
                );
                assert_eq!(
                    member(&body, "Status").as_deref(),
                    Some("NOT_STARTED"),
                    "case {case}, read {read}: expected Status NOT_STARTED, got {body:?}"
                );
                // Both multi-region members are "only present when multi-region
                // telemetry evaluation is active", and `FailureReason` only in a
                // FAILED_* status.
                for absent in ["HomeRegion", "RegionStatuses", "FailureReason"] {
                    assert!(
                        body.get(absent).is_none(),
                        "case {case}, read {read}: expected no {absent}, got {body:?}"
                    );
                }
            }
        }

        // A read is a read: nothing above configured a feature, so nothing was
        // stored for any region, under either scope.
        let state = emulator
            .engine
            .state::<ComAmazonawsObservabilityadminObservabilityadminState>();
        let store = state.lock();
        assert!(
            store.telemetry_evaluations.is_empty()
                && store.organization_telemetry_evaluations.is_empty(),
            "case {scope:?}: a status read must store nothing, got account={:?} organization={:?}",
            store.telemetry_evaluations,
            store.organization_telemetry_evaluations
        );
    }
}

/// The status a read reports is the one the last `Start` for that region is due to
/// have reached, and the multi-region members are reported exactly when the scope
/// named a spoke region.
#[tokio::test]
async fn a_started_evaluation_feature_reports_starting_then_running() {
    /// One row: what the `Start` asked for, and how many spoke regions that is.
    struct Case {
        name: &'static str,
        body: Value,
        /// The spoke regions the scope names, or `None` for "every region of the
        /// home region's partition bar the home region", which the row cannot spell
        /// out by hand.
        spokes: Option<Vec<&'static str>>,
    }

    let cases = [
        Case {
            name: "no multi-region scope",
            body: json!({}),
            spokes: Some(Vec::new()),
        },
        Case {
            name: "one named spoke region",
            body: json!({ "Regions": ["eu-west-1"] }),
            spokes: Some(vec!["eu-west-1"]),
        },
        Case {
            name: "three named spoke regions, out of order",
            body: json!({ "Regions": ["sa-east-1", "eu-west-1", "ap-northeast-1"] }),
            // Reported in a stable order, whatever order the request listed.
            spokes: Some(vec!["ap-northeast-1", "eu-west-1", "sa-east-1"]),
        },
        Case {
            name: "AllRegions",
            body: json!({ "AllRegions": true }),
            spokes: None,
        },
        Case {
            // "The current region is always implicitly included": it is not a spoke
            // of its own configuration, whether or not the request names it.
            name: "the current region named beside a spoke region",
            body: json!({ "Regions": [HOME_REGION, "eu-west-1"] }),
            spokes: Some(vec!["eu-west-1"]),
        },
    ];

    for scope in Scope::ALL {
        for case in &cases {
            let label = format!("{scope:?}: {}", case.name);
            let emulator = emulator();

            let (status, body) = start_evaluation(&emulator, scope, HOME_REGION, &case.body).await;
            assert_eq!(
                status, 200,
                "case {label}: expected the start to answer 200, got {status} {body:?}"
            );

            // The transient status is observable: the transition the start began is
            // not due yet, so this read — made straight after it — sees STARTING.
            let (status, started) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                status, 200,
                "case {label}: expected 200 from the status read, got {status} {started:?}"
            );
            assert_eq!(
                member(&started, "Status").as_deref(),
                Some("STARTING"),
                "case {label}: expected Status STARTING straight after the start, got {started:?}"
            );
            assert!(
                started.get("FailureReason").is_none(),
                "case {label}: expected no FailureReason in a non-failed status, got {started:?}"
            );

            let multi_region = case.spokes.as_ref().is_none_or(|spokes| !spokes.is_empty());
            assert_eq!(
                member(&started, "HomeRegion").as_deref(),
                multi_region.then_some(HOME_REGION),
                "case {label}: HomeRegion is reported exactly when multi-region \
                 evaluation is active, got {started:?}"
            );
            if let Some(expected) = &case.spokes {
                let reported = region_statuses(&started);
                let names: Vec<String> = reported.iter().map(|(name, _)| name.clone()).collect();
                assert_eq!(
                    names,
                    expected.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
                    "case {label}: expected RegionStatuses for {expected:?}, got {reported:?}"
                );
                assert!(
                    reported.iter().all(|(_, status)| status == "STARTING"),
                    "case {label}: every spoke region is STARTING before the start \
                     comes due, got {reported:?}"
                );
            } else {
                let reported = region_statuses(&started);
                assert!(
                    reported.len() > 1 && !reported.iter().any(|(name, _)| name == HOME_REGION),
                    "case {label}: AllRegions names every other region of the home \
                     region's partition, got {reported:?}"
                );
            }

            tokio::time::sleep(past_transition_delay()).await;

            let (status, running) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                status, 200,
                "case {label}: expected 200 after the start came due, got {status} {running:?}"
            );
            assert_eq!(
                member(&running, "Status").as_deref(),
                Some("RUNNING"),
                "case {label}: expected Status RUNNING once the start came due, got {running:?}"
            );
            let reported = region_statuses(&running);
            assert!(
                reported.iter().all(|(_, status)| status == "RUNNING"),
                "case {label}: every spoke region is RUNNING once the start came due, \
                 got {reported:?}"
            );

            // A spoke region the scope named was onboarded too, and reports the
            // configuration's home region as its management point.
            if let Some(spoke) = reported.first().map(|(name, _)| name.clone()) {
                let (status, from_spoke) = evaluation_status(&emulator, scope, &spoke).await;
                assert_eq!(
                    status, 200,
                    "case {label}: expected 200 from the {spoke} read, got {status} {from_spoke:?}"
                );
                assert_eq!(
                    member(&from_spoke, "Status").as_deref(),
                    Some("RUNNING"),
                    "case {label}: expected the spoke region {spoke} to report RUNNING, \
                     got {from_spoke:?}"
                );
                assert_eq!(
                    member(&from_spoke, "HomeRegion").as_deref(),
                    Some(HOME_REGION),
                    "case {label}: expected the spoke region {spoke} to report the home \
                     region, got {from_spoke:?}"
                );
            }

            // A region the scope never named has its own, never-started feature.
            let (status, elsewhere) = evaluation_status(&emulator, scope, "me-south-1").await;
            let expected_elsewhere = if case.spokes.is_none() {
                // AllRegions named every region of the partition, me-south-1 included.
                "RUNNING"
            } else {
                "NOT_STARTED"
            };
            assert_eq!(
                (status, member(&elsewhere, "Status").as_deref()),
                (200, Some(expected_elsewhere)),
                "case {label}: expected me-south-1 to report {expected_elsewhere}, \
                 got {status} {elsewhere:?}"
            );
        }
    }
}

/// A `Stop` is what moves the feature to `STOPPING` and then `STOPPED`, for the
/// region it was signed for and — from the home region of a multi-region
/// configuration — for that configuration's spoke regions; and repeating it changes
/// nothing.
#[tokio::test]
async fn a_stopped_evaluation_feature_reports_stopping_then_stopped() {
    // Rows differ in what preceded the stop and in how many stops follow it: a
    // never-started region, a single-region start, a multi-region start, and a stop
    // repeated up to four times.
    let cases: [(&str, Option<Value>, usize); 5] = [
        ("never started", None, 1),
        ("single-region start", Some(json!({})), 1),
        (
            "multi-region start",
            Some(json!({ "Regions": ["eu-west-1", "ap-northeast-1"] })),
            1,
        ),
        ("single-region start, stopped twice", Some(json!({})), 2),
        (
            "multi-region start, stopped four times",
            Some(json!({ "Regions": ["eu-west-1"] })),
            4,
        ),
    ];

    for scope in Scope::ALL {
        for (name, start, stops) in &cases {
            let label = format!("{scope:?}: {name}");
            let emulator = emulator();
            if let Some(body) = start {
                let (status, response) = start_evaluation(&emulator, scope, HOME_REGION, body).await;
                assert_eq!(
                    status, 200,
                    "case {label}: expected the start to answer 200, got {status} {response:?}"
                );
            }

            for stop in 0..*stops {
                let (status, response) = stop_evaluation(&emulator, scope, HOME_REGION).await;
                assert_eq!(
                    status, 200,
                    "case {label}, stop {stop}: expected 200, got {status} {response:?}"
                );
            }

            let (status, stopping) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&stopping, "Status").as_deref()),
                (200, Some("STOPPING")),
                "case {label}: expected Status STOPPING straight after the stop, \
                 got {status} {stopping:?}"
            );

            tokio::time::sleep(past_transition_delay()).await;

            let (status, stopped) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&stopped, "Status").as_deref()),
                (200, Some("STOPPED")),
                "case {label}: expected Status STOPPED once the stop came due, \
                 got {status} {stopped:?}"
            );
            for (region, region_status) in region_statuses(&stopped) {
                assert_eq!(
                    region_status, "STOPPED",
                    "case {label}: the stop reached the spoke region {region} too, \
                     got {region_status}"
                );
                let (status, from_spoke) = evaluation_status(&emulator, scope, &region).await;
                assert_eq!(
                    (status, member(&from_spoke, "Status").as_deref()),
                    (200, Some("STOPPED")),
                    "case {label}: expected the spoke region {region} to report STOPPED, \
                     got {status} {from_spoke:?}"
                );
            }
        }
    }
}

/// The account's telemetry-config feature and its organization's are separate
/// features: onboarding one leaves the other exactly as it was.
#[tokio::test]
async fn the_account_and_organization_evaluation_features_are_separate() {
    for started in Scope::ALL {
        let emulator = emulator();
        let (status, response) =
            start_evaluation(&emulator, started, HOME_REGION, &json!({ "Regions": ["eu-west-1"] }))
                .await;
        assert_eq!(
            status, 200,
            "case {started:?}: expected the start to answer 200, got {status} {response:?}"
        );

        for scope in Scope::ALL {
            let expected = match (started, scope) {
                (Scope::Account, Scope::Account) | (Scope::Organization, Scope::Organization) => {
                    "STARTING"
                }
                _ => "NOT_STARTED",
            };
            let (status, body) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&body, "Status").as_deref()),
                (200, Some(expected)),
                "case started={started:?}, read={scope:?}: expected {expected}, \
                 got {status} {body:?}"
            );
        }
    }
}

/// A multi-region scope the model's own prose forbids is refused with the modelled
/// 400 `ValidationException`, and refusing it stores nothing.
#[tokio::test]
async fn an_unusable_evaluation_scope_is_refused() {
    // Each row is a rule stated by `Regions`'/`AllRegions`' own documentation: the
    // two forms are "mutually exclusive", and a region must be one the home
    // region's partition has.
    let refused: [(&str, Value); 3] = [
        ("both forms of the scope", json!({ "Regions": ["eu-west-1"], "AllRegions": true })),
        ("a region of another partition", json!({ "Regions": ["cn-north-1"] })),
        ("a region that does not exist", json!({ "Regions": ["xx-nowhere-1"] })),
    ];
    let accepted: [(&str, Value); 5] = [
        ("neither form", json!({})),
        ("AllRegions alone", json!({ "AllRegions": true })),
        ("AllRegions false beside a list", json!({ "Regions": ["eu-west-1"], "AllRegions": false })),
        // "The current region is always implicitly included": naming it asks for
        // nothing the call does not already do, so it is accepted and is not a
        // spoke of its own configuration.
        ("the current region named", json!({ "Regions": [HOME_REGION] })),
        (
            "the current region named beside another",
            json!({ "Regions": ["eu-west-1", HOME_REGION] }),
        ),
    ];

    for scope in Scope::ALL {
        for (name, body) in &refused {
            let label = format!("{scope:?}: {name}");
            let emulator = emulator();
            let (status, response) = start_evaluation(&emulator, scope, HOME_REGION, body).await;
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, "ValidationException"),
                "case {label}: expected a 400 ValidationException, got {status} {response:?}"
            );

            let (status, after) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&after, "Status").as_deref()),
                (200, Some("NOT_STARTED")),
                "case {label}: a refused start stores nothing, got {status} {after:?}"
            );
        }

        for (name, body) in &accepted {
            let label = format!("{scope:?}: {name}");
            let emulator = emulator();
            let (status, response) = start_evaluation(&emulator, scope, HOME_REGION, body).await;
            assert_eq!(
                status, 200,
                "case {label}: expected the start to answer 200, got {status} {response:?}"
            );
            let (status, after) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&after, "Status").as_deref()),
                (200, Some("STARTING")),
                "case {label}: expected the accepted start to be recorded, got {status} {after:?}"
            );
        }
    }
}

/// Repeating a `Start` is the same onboarding: it must not push a feature that has
/// already reached `RUNNING` back into `STARTING`.
#[tokio::test]
async fn repeating_a_start_does_not_restart_a_running_feature() {
    let scopes_bodies = [json!({}), json!({ "Regions": ["eu-west-1"] }), json!({ "AllRegions": true })];
    for scope in Scope::ALL {
        for body in &scopes_bodies {
            let label = format!("{scope:?}: {body}");
            let emulator = emulator();
            let (status, response) = start_evaluation(&emulator, scope, HOME_REGION, body).await;
            assert_eq!(
                status, 200,
                "case {label}: expected the first start to answer 200, got {status} {response:?}"
            );
            tokio::time::sleep(past_transition_delay()).await;

            let (status, response) = start_evaluation(&emulator, scope, HOME_REGION, body).await;
            assert_eq!(
                status, 200,
                "case {label}: expected the second start to answer 200, got {status} {response:?}"
            );
            let (status, after) = evaluation_status(&emulator, scope, HOME_REGION).await;
            assert_eq!(
                (status, member(&after, "Status").as_deref()),
                (200, Some("RUNNING")),
                "case {label}: a repeated start of the same scope leaves the feature \
                 RUNNING, got {status} {after:?}"
            );
        }
    }
}

async fn start_enrichment(emulator: &Emulator, region: &str) -> (u16, Value) {
    call(emulator, region, "/StartTelemetryEnrichment", &json!({})).await
}

async fn stop_enrichment(emulator: &Emulator, region: &str) -> (u16, Value) {
    call(emulator, region, "/StopTelemetryEnrichment", &json!({})).await
}

async fn enrichment_status(emulator: &Emulator, region: &str) -> (u16, Value) {
    call(emulator, region, "/GetTelemetryEnrichmentStatus", &json!({})).await
}

/// The managed view ARN prefix this emulator must mint for `region`: the modelled
/// `AwsResourceExplorerManagedViewArn` pattern fixes every field but the region, the
/// account, and the id the service assigns the view it creates — which is per-view and
/// so is asserted by its FORM below rather than by a value.
fn expected_view_arn_prefix(region: &str) -> String {
    format!(
        "arn:aws:resource-explorer-2:{region}:{ACCOUNT}:managed-view/\
         AWSManagedViewForResourceTagsForTelemetry/"
    )
}

/// Whether `id` has the canonical hyphenated UUID form (8-4-4-4-12 lowercase hex) the
/// service assigns a managed view in.
fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()))
}

/// Assert `arn` is the ARN of a managed view of `region`: the prefix the pattern fixes,
/// then the id the service assigned the view.
fn assert_view_arn(case: &str, region: &str, arn: Option<&String>) {
    let prefix = expected_view_arn_prefix(region);
    let arn = arn.map(String::as_str).unwrap_or("<none>");
    let Some(id) = arn.strip_prefix(&prefix) else {
        panic!("case {case}: expected a managed view ARN under {prefix}, got {arn:?}");
    };
    assert!(
        is_uuid(id),
        "case {case}: expected the view's service-assigned id to be a hyphenated UUID, \
         got {id:?} in {arn:?}"
    );
}

/// The resource tags for telemetry feature reports what its own `Start`/`Stop`
/// recorded, per region, and the modelled 404 for a region neither has ever named.
#[tokio::test]
async fn the_enrichment_feature_reports_what_start_and_stop_recorded() {
    // Rows differ in the region signed for: each region has its own feature, so the
    // answer cannot come from one hardcoded region or one global switch.
    let regions = ["us-west-2", "eu-west-1", "ap-south-1"];
    let emulator = emulator();

    for region in regions {
        // Never started: the modelled 404 is the only answer that distinguishes
        // "never started" from Stopped.
        let (status, body) = enrichment_status(&emulator, region).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {region}, never started: expected a 404 ResourceNotFoundException, \
             got {status} {body:?}"
        );
    }

    for region in regions {
        let (status, started) = start_enrichment(&emulator, region).await;
        assert_eq!(
            (status, member(&started, "Status").as_deref()),
            (202, Some("Running")),
            "case {region}: expected the modelled 202 with Status Running, \
             got {status} {started:?}"
        );
        assert_view_arn(
            region,
            region,
            member(&started, "AwsResourceExplorerManagedViewArn").as_ref(),
        );

        // The read reports the same pair the start recorded.
        let (status, read) = enrichment_status(&emulator, region).await;
        assert_eq!(
            (status, member(&read, "Status").as_deref()),
            (200, Some("Running")),
            "case {region}: expected 200 with Status Running, got {status} {read:?}"
        );
        assert_eq!(
            member(&read, "AwsResourceExplorerManagedViewArn"),
            member(&started, "AwsResourceExplorerManagedViewArn"),
            "case {region}: the view the start minted is the view the read reports, \
             got {read:?}"
        );
    }

    // Starting one region's feature leaves every other region's exactly as it was,
    // so a region whose feature was never started still answers the modelled 404.
    let (status, untouched) = enrichment_status(&emulator, "me-south-1").await;
    assert_eq!(
        (status, error_code(&untouched).as_str()),
        (404, "ResourceNotFoundException"),
        "case me-south-1: a start elsewhere must not start this region's feature, \
         got {status} {untouched:?}"
    );
}

/// A redundant `Stop` of the resource tags for telemetry feature is an idempotent
/// no-op: however many times it is repeated, and whether or not the feature was ever
/// started, the status read reports exactly `Stopped` — never a conflict and never
/// the 404 of a feature nothing has named.
#[tokio::test]
async fn a_redundant_enrichment_stop_is_an_idempotent_no_op() {
    // Rows differ in whether a start preceded the stops and in how many stops
    // follow, which is what the invariant generalises over.
    let cases: [(&str, bool, usize); 6] = [
        ("never started, stopped once", false, 1),
        ("never started, stopped twice", false, 2),
        ("never started, stopped five times", false, 5),
        ("started, stopped once", true, 1),
        ("started, stopped twice", true, 2),
        ("started, stopped four times", true, 4),
    ];

    for (name, start_first, stops) in cases {
        let emulator = emulator();
        let mut minted: Option<String> = None;
        if start_first {
            let (status, started) = start_enrichment(&emulator, HOME_REGION).await;
            assert_eq!(
                status, 202,
                "case {name}: expected the start to answer 202, got {status} {started:?}"
            );
            minted = member(&started, "AwsResourceExplorerManagedViewArn");
        }

        for stop in 0..stops {
            let (status, stopped) = stop_enrichment(&emulator, HOME_REGION).await;
            assert_eq!(
                (status, member(&stopped, "Status").as_deref()),
                (202, Some("Stopped")),
                "case {name}, stop {stop}: expected the modelled 202 with Status Stopped, \
                 got {status} {stopped:?}"
            );
            // `StopTelemetryEnrichmentOutput` models no view ARN member.
            assert!(
                stopped.get("AwsResourceExplorerManagedViewArn").is_none(),
                "case {name}, stop {stop}: the stop output has no view ARN member, \
                 got {stopped:?}"
            );
        }

        let (status, read) = enrichment_status(&emulator, HOME_REGION).await;
        assert_eq!(
            (status, member(&read, "Status").as_deref()),
            (200, Some("Stopped")),
            "case {name}: expected 200 with Status exactly Stopped after {stops} stop(s), \
             got {status} {read:?}"
        );
        assert!(
            read.get("AwsResourceExplorerManagedViewArn").is_none(),
            "case {name}: the view is reported only while the feature is enabled, got {read:?}"
        );

        // Starting it again reads the one view the first start minted, rather than a
        // second view per start.
        let (status, restarted) = start_enrichment(&emulator, HOME_REGION).await;
        assert_eq!(
            (status, member(&restarted, "Status").as_deref()),
            (202, Some("Running")),
            "case {name}: expected the restart to answer 202 Running, got {status} {restarted:?}"
        );
        assert_view_arn(
            name,
            HOME_REGION,
            member(&restarted, "AwsResourceExplorerManagedViewArn").as_ref(),
        );
        if let Some(minted) = minted {
            assert_eq!(
                member(&restarted, "AwsResourceExplorerManagedViewArn"),
                Some(minted.clone()),
                "case {name}: expected the restart to reuse the view {minted}, got {restarted:?}"
            );
        }
    }
}

/// The region a request is signed for is a store key, so the key space is the AWS
/// region catalog and not the caller's text: a credential scope naming anything else
/// is served in the emulator's default region, which is what keeps every
/// region-keyed feature store bounded by [`MAX_TELEMETRY_FEATURE_REGIONS`].
#[tokio::test]
async fn a_region_outside_the_catalog_is_served_in_the_default_region() {
    // Rows differ in the shape of the unrecognised scope: a plausible but
    // nonexistent code, a real-looking code with a bogus index, and a long one.
    let fabricated = [
        "xx-nowhere-1",
        "us-west-9",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-1",
    ];
    /// The region `crate::state::DEFAULT_REGION` names, which an unrecognised scope
    /// falls back to.
    const DEFAULT_REGION: &str = "us-east-1";

    for region in fabricated {
        let emulator = emulator();
        let (status, response) =
            start_evaluation(&emulator, Scope::Account, region, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {region}: expected the start to answer 200, got {status} {response:?}"
        );
        let (status, started) = start_enrichment(&emulator, region).await;
        assert_eq!(
            status, 202,
            "case {region}: expected the enrichment start to answer 202, got {status} {started:?}"
        );
        assert_view_arn(
            region,
            DEFAULT_REGION,
            member(&started, "AwsResourceExplorerManagedViewArn").as_ref(),
        );

        // The default region is where both features were recorded, and it is the ONLY
        // key either store holds: a fabricated region cannot add one.
        let (status, by_default) =
            evaluation_status(&emulator, Scope::Account, DEFAULT_REGION).await;
        assert_eq!(
            (status, member(&by_default, "Status").as_deref()),
            (200, Some("STARTING")),
            "case {region}: expected the default region to carry the start, \
             got {status} {by_default:?}"
        );
        let state = emulator
            .engine
            .state::<ComAmazonawsObservabilityadminObservabilityadminState>();
        let store = state.lock();
        let evaluation_keys: Vec<&String> = store.telemetry_evaluations.keys().collect();
        let enrichment_keys: Vec<&String> = store.telemetry_enrichments.keys().collect();
        assert_eq!(
            (evaluation_keys.as_slice(), enrichment_keys.as_slice()),
            (
                [&DEFAULT_REGION.to_string()].as_slice(),
                [&DEFAULT_REGION.to_string()].as_slice()
            ),
            "case {region}: expected both stores keyed only by the default region, \
             got {evaluation_keys:?} and {enrichment_keys:?}"
        );
        assert!(
            store.telemetry_evaluations.len() <= MAX_TELEMETRY_FEATURE_REGIONS
                && store.telemetry_enrichments.len() <= MAX_TELEMETRY_FEATURE_REGIONS,
            "case {region}: expected each feature store within its {MAX_TELEMETRY_FEATURE_REGIONS} \
             record bound, got {} and {}",
            store.telemetry_evaluations.len(),
            store.telemetry_enrichments.len()
        );
    }
}

/// The enrichment status read is a read: it neither starts nor stops the feature, so
/// repeated reads agree and a 404 stays a 404.
#[tokio::test]
async fn an_enrichment_status_read_changes_nothing() {
    let emulator = emulator();
    for read in 0..3 {
        let (status, body) = enrichment_status(&emulator, HOME_REGION).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "read {read}: expected the 404 to stay a 404, got {status} {body:?}"
        );
    }

    let (status, started) = start_enrichment(&emulator, HOME_REGION).await;
    assert_eq!(
        status, 202,
        "expected the start to answer 202, got {status} {started:?}"
    );
    for read in 0..3 {
        let (status, body) = enrichment_status(&emulator, HOME_REGION).await;
        assert_eq!(
            (status, member(&body, "Status").as_deref()),
            (200, Some("Running")),
            "read {read}: expected a read to leave the feature Running, got {status} {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The telemetry-config AUDIT: ListResourceTelemetry
// ---------------------------------------------------------------------------
//
// The rule these cases pin: an audit reports one telemetry configuration per resource of a
// supported type the account holds in the region the request was signed for, each carrying
// the state the service that OWNS the resource holds for that kind of telemetry, and the
// request only narrows and pages that report. The resources are Amazon EC2's, so EC2 is
// mounted beside this service on one engine and every identifier and every telemetry state
// below is one EC2 itself handed out.
//
// The resource-backed walk over the whole surface — paging, the identifier-prefix filter and
// the audit following a resource whose telemetry is turned off or which is deleted — lives in
// `core/router/tests/all/observabilityadmin_resource_telemetry.rs`, because it drives several
// services; what is here is this operation's own contract: the configuration it computes, the
// order the requested types put it in, and the requests it refuses.

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// `AWS::EC2::Instance` and `AWS::EC2::VPC`, two `ResourceType` values
/// (`smithy_query(query="shape", shape="ResourceType")`).
const INSTANCE_TYPE: &str = "AWS::EC2::Instance";
const VPC_TYPE: &str = "AWS::EC2::VPC";

/// The `TelemetrySourceType` value `AWS::EC2::VPC` correlates with.
const VPC_FLOW_LOGS: &str = "VPC_FLOW_LOGS";

/// The tag the fixture puts on the monitored instance, for the row that narrows by tag.
const AUDIT_TAG_KEY: &str = "TelemetryAudited";
const AUDIT_TAG_VALUE: &str = "yes";

/// This service and Amazon EC2 over ONE shared engine: the audit reads the resources it
/// reports from EC2's own API, so without EC2 mounted there is nothing to audit.
fn audited_emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_observabilityadmin_observabilityadmin::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// POST one ec2Query action that must succeed, answered with its XML body.
///
/// The body is `application/x-www-form-urlencoded`, so each VALUE is percent-encoded by the
/// `percent-encoding` crate rather than by hand; the field names below are plain identifiers
/// and dots, which that form carries literally.
async fn ec2(emulator: &Emulator, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        let value = utf8_percent_encode(value, NON_ALPHANUMERIC);
        body.push_str(&format!("&{name}={value}"));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{HOME_REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDOBSTEST/20260101/{HOME_REGION}/ec2/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed test request");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let answer = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {answer}"
    );
    answer
}

/// The text of the first `<name>…</name>` element of an XML answer. A reader, not an XML
/// parser, and only for the documents this emulator itself produced.
fn xml_element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body
        .find(&open)
        .map(|at| at + open.len())
        .unwrap_or_else(|| panic!("no <{name}> element in {body}"));
    let end = body[start..]
        .find(&close)
        .map(|at| at + start)
        .unwrap_or_else(|| panic!("no </{name}> element in {body}"));
    body[start..end].to_string()
}

/// Launch one instance, with detailed monitoring on or off, and answer its id.
async fn launch_instance(emulator: &Emulator, monitored: bool, tagged: bool) -> String {
    let monitoring = monitored.to_string();
    let mut params = vec![
        ("ImageId", AMI),
        ("MinCount", "1"),
        ("MaxCount", "1"),
        // "Specifies whether detailed monitoring is enabled for the instance"
        // (`RunInstancesRequest$Monitoring`).
        ("Monitoring.Enabled", monitoring.as_str()),
    ];
    if tagged {
        params.extend([
            ("TagSpecification.1.ResourceType", "instance"),
            ("TagSpecification.1.Tag.1.Key", AUDIT_TAG_KEY),
            ("TagSpecification.1.Tag.1.Value", AUDIT_TAG_VALUE),
        ]);
    }
    xml_element(&ec2(emulator, "RunInstances", &params).await, "instanceId")
}

/// Create one VPC and answer its id.
async fn create_vpc(emulator: &Emulator, cidr: &str) -> String {
    xml_element(
        &ec2(emulator, "CreateVpc", &[("CidrBlock", cidr)]).await,
        "vpcId",
    )
}

/// One `ListResourceTelemetry` call that must succeed, as the configurations it reported.
async fn audit(emulator: &Emulator, case: &str, body: &Value) -> Vec<Value> {
    let (status, answer) = call(emulator, HOME_REGION, "/ListResourceTelemetry", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answer:?}"
    );
    assert_eq!(
        member(&answer, "NextToken"),
        None,
        "case {case:?}: the resources of these cases fit one page, so the audit carries no \
         NextToken, got {answer:?}"
    );
    answer
        .get("TelemetryConfigurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The configuration the audit reported for the resource `identifier` names.
fn audited(configurations: &[Value], case: &str, identifier: &str) -> Value {
    configurations
        .iter()
        .find(|entry| member(entry, "ResourceIdentifier").as_deref() == Some(identifier))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no configuration for {identifier} in {configurations:?}")
        })
}

/// The `TelemetryConfigurationState` a resource whose ONE audited kind of telemetry is
/// `audited` and in `state` must carry: every other modelled `TelemetryType` is
/// `NotApplicable` (`smithy_query(query="shape", shape="TelemetryType,TelemetryState")`).
fn expected_telemetry_state(audited: &str, state: &str) -> Value {
    let entries: Vec<(String, Value)> = ["Insights", "Logs", "Metrics", "Traces"]
        .into_iter()
        .map(|telemetry_type| {
            let value = if telemetry_type == audited {
                state
            } else {
                "NotApplicable"
            };
            (telemetry_type.to_string(), Value::String(value.to_string()))
        })
        .collect();
    Value::Object(entries.into_iter().collect())
}

/// An audit computes each resource's telemetry configuration from what EC2 holds for it: an
/// instance's detailed monitoring and a VPC's flow logs, with every other kind of telemetry
/// `NotApplicable`, the tags its owner holds, and the caller's own account.
#[tokio::test]
async fn the_resource_telemetry_audit_reports_what_ec2_holds_for_each_resource() {
    let emulator = audited_emulator();
    let monitored = launch_instance(&emulator, true, true).await;
    let unmonitored = launch_instance(&emulator, false, false).await;
    let vpc = create_vpc(&emulator, "10.90.0.0/16").await;

    // (case, the resource, the telemetry type audited for it, its state, the source type)
    let rows: [(&str, &str, &str, &str, Option<&str>); 3] = [
        (
            "an instance with detailed monitoring",
            &monitored,
            "Metrics",
            "Enabled",
            None,
        ),
        (
            "an instance without it",
            &unmonitored,
            "Metrics",
            "Disabled",
            None,
        ),
        (
            "a VPC with no flow log",
            &vpc,
            "Logs",
            "Disabled",
            Some(VPC_FLOW_LOGS),
        ),
    ];

    let configurations = audit(&emulator, "the whole audit", &json!({})).await;
    for (case, identifier, telemetry_type, state, source_type) in rows {
        let entry = audited(&configurations, case, identifier);
        assert_eq!(
            entry.get("TelemetryConfigurationState"),
            Some(&expected_telemetry_state(telemetry_type, state)),
            "case {case:?}: expected {telemetry_type} {state} and every other telemetry type \
             NotApplicable, got {entry:?}"
        );
        assert_eq!(
            member(&entry, "AccountIdentifier").as_deref(),
            Some(ACCOUNT),
            "case {case:?}: expected the caller's own account, got {entry:?}"
        );
        assert_eq!(
            member(&entry, "TelemetrySourceType").as_deref(),
            source_type,
            "case {case:?}: expected TelemetrySourceType {source_type:?}, got {entry:?}"
        );
    }
    assert_eq!(
        audited(&configurations, "the tagged instance", &monitored).get("ResourceTags"),
        Some(&json!({ AUDIT_TAG_KEY: AUDIT_TAG_VALUE })),
        "expected the tags EC2 holds on the instance, got {configurations:?}"
    );
    assert_eq!(
        audited(&configurations, "the untagged instance", &unmonitored).get("ResourceTags"),
        Some(&json!({})),
        "expected no tags for an instance EC2 holds none for, got {configurations:?}"
    );
}

/// `ResourceTypes` selects the types the audit reports and decides the ORDER it reports them
/// in — "if this parameter is provided, the service returns the resources in the same order
/// as specified in the request" — and the tag and telemetry-state filters narrow it to the
/// resources that satisfy them.
#[tokio::test]
async fn the_resource_telemetry_audit_reports_the_types_the_request_asks_for_in_its_order() {
    let emulator = audited_emulator();
    let monitored = launch_instance(&emulator, true, true).await;
    let unmonitored = launch_instance(&emulator, false, false).await;
    create_vpc(&emulator, "10.90.0.0/16").await;

    /// The resource types an audit reported, in report order and without the repeats that
    /// several resources of one type make.
    fn type_sequence(configurations: &[Value]) -> Vec<String> {
        let mut sequence: Vec<String> = Vec::new();
        for entry in configurations {
            let reported = member(entry, "ResourceType").unwrap_or_default();
            if sequence.last() != Some(&reported) {
                sequence.push(reported);
            }
        }
        sequence
    }

    // (case, the types the request names, the type sequence the audit must report)
    let rows: [(&str, Vec<&str>, Vec<&str>); 4] = [
        ("one type", vec![VPC_TYPE], vec![VPC_TYPE]),
        ("the other type", vec![INSTANCE_TYPE], vec![INSTANCE_TYPE]),
        (
            "both, VPCs first",
            vec![VPC_TYPE, INSTANCE_TYPE],
            vec![VPC_TYPE, INSTANCE_TYPE],
        ),
        (
            "both, instances first",
            vec![INSTANCE_TYPE, VPC_TYPE],
            vec![INSTANCE_TYPE, VPC_TYPE],
        ),
    ];
    for (case, requested, expected) in rows {
        let configurations =
            audit(&emulator, case, &json!({ "ResourceTypes": requested })).await;
        assert_eq!(
            type_sequence(&configurations),
            expected,
            "case {case:?}: expected the requested order {expected:?}, got {:?}",
            type_sequence(&configurations)
        );
        assert!(
            configurations.iter().all(|entry| {
                member(entry, "ResourceType")
                    .is_some_and(|reported| requested.contains(&reported.as_str()))
            }),
            "case {case:?}: expected only the requested types, got {configurations:?}"
        );
    }

    // (case, the narrowing request, the resources it must report)
    let narrowed: [(&str, Value, Vec<&String>); 3] = [
        (
            "the telemetry state of the monitored instance",
            json!({ "TelemetryConfigurationState": { "Metrics": "Enabled" } }),
            vec![&monitored],
        ),
        (
            "the telemetry state of the unmonitored one",
            json!({ "TelemetryConfigurationState": { "Metrics": "Disabled" } }),
            vec![&unmonitored],
        ),
        (
            "the tag EC2 holds on one instance",
            json!({ "ResourceTags": { AUDIT_TAG_KEY: AUDIT_TAG_VALUE } }),
            vec![&monitored],
        ),
    ];
    for (case, request, expected) in narrowed {
        let configurations = audit(&emulator, case, &request).await;
        let reported: Vec<String> = configurations
            .iter()
            .filter_map(|entry| member(entry, "ResourceIdentifier"))
            .collect();
        let expected: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

/// A request the model does not admit is refused, and the refusal is this operation's own
/// modelled `ValidationException` — never a page of resources chosen by a value the model
/// does not carry.
#[tokio::test]
async fn the_resource_telemetry_audit_refuses_a_request_the_model_does_not_admit() {
    let emulator = audited_emulator();

    // (case, the request, the status it must be answered with)
    let rows: [(&str, Value, u16); 7] = [
        (
            "a resource type no resource could be of",
            json!({ "ResourceTypes": ["AWS::Nothing::Here"] }),
            400,
        ),
        (
            "a next token that is not hex",
            json!({ "NextToken": "not-a-token" }),
            400,
        ),
        (
            "hex that decodes to something this operation never issued",
            json!({ "NextToken": "6162" }),
            400,
        ),
        // The modelled `ListResourceTelemetryMaxResults` range is 1..50, so each published
        // bound is served and each value one past it is refused.
        ("the smallest page the model admits", json!({ "MaxResults": 1 }), 200),
        ("the largest page the model admits", json!({ "MaxResults": 50 }), 200),
        ("a page size below that range", json!({ "MaxResults": 0 }), 400),
        ("a page size above it", json!({ "MaxResults": 51 }), 400),
    ];

    for (case, request, expected) in rows {
        let (status, body) = call(&emulator, HOME_REGION, "/ListResourceTelemetry", &request).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} {body:?}"
        );
        if expected == 400 {
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {body:?}"
            );
        }
    }
}

/// An audit on an emulator with no owning service to ask reports NO configuration for those
/// resource types: a read that could not be made says nothing about what the account holds,
/// so it is answered as an empty audit rather than as a failure or as invented resources.
#[tokio::test]
async fn an_audit_with_no_owning_service_to_ask_reports_no_configuration() {
    let emulator = emulator();
    for read in 0..2 {
        let case = format!("read {read}");
        let configurations = audit(&emulator, &case, &json!({})).await;
        assert!(
            configurations.is_empty(),
            "case {case:?}: expected no configuration at all, got {configurations:?}"
        );
    }
}
