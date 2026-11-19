// SPDX-License-Identifier: CC0-1.0

//! The seam between the readiness a domain PUBLISHES and the state a call that starts
//! further work on it DEMANDS, driven over the real wire path: a `restJson1` request
//! through the multi-service router into the mounted service, so what is asserted is what
//! a client sees.
//!
//! The rule under test:
//!
//! > the readiness `DescribeDomain` publishes (`DomainStatus$Created` with
//! > `DomainStatus$Processing`, "true if OpenSearch Service is processing configuration
//! > changes, false if the configuration is active") and the state `UpgradeDomain` admits
//! > a run in are ONE condition — so the first observation of a quiet domain is enough:
//! > an upgrade issued on it is accepted rather than refused for work in flight, whatever
//! > kind of work the domain has just finished, and from there the SERVICE carries the run
//! > to its terminal `UPGRADE` step with no call from the caller to advance it.
//!
//! So the cases span what the domain has just finished when the upgrade is issued — the
//! provisioning a create starts, a configuration change, a service software deployment and
//! an engine version upgrade (every kind of work `DomainProcessingStatusType` names on a
//! domain that exists), plus the cancel that ends a change early, which is the one way a
//! domain becomes quiet without the clock. One test per case, each on its own emulator, so
//! a failure names the kind of work it was about and no case waits behind another.
//! [`super`] spans the other axis, the hop the upgrade goes along; this suite adds no
//! second copy of its harness, its table reading or its convergence waits.

use serde_json::{json, Value};

use super::*;
use crate::config_change_cancellation::cancel;
use crate::domain_lifecycle::settled_domain;
use crate::service_software_update::{seed, start as start_service_software, with_update_available};

/// The service software release a case makes available for the deployment it starts: a
/// release label of this service's own `R<date>` form, later than the one a domain comes
/// up on, so there is something to deploy. The same label the sibling service-software
/// suites deploy.
const AVAILABLE_RELEASE: &str = "R20250601";

/// A configuration change for a case to start: the smallest `UpdateDomainConfig` that
/// touches an option, so the domain really goes `Modifying`.
fn a_configuration_change() -> Value {
    json!({ "AccessPolicies": "{\"Version\":\"2012-10-17\"}" })
}

/// What the domain has just finished when the case issues its upgrade.
#[derive(Debug, Clone, Copy)]
enum Finished {
    /// The provisioning `CreateDomain` starts.
    Provisioning,
    /// A configuration change `UpdateDomainConfig` started.
    ConfigurationChange,
    /// A service software deployment `StartServiceSoftwareUpdate` started.
    ServiceSoftwareDeployment,
    /// An earlier engine version upgrade `UpgradeDomain` started.
    EngineVersionUpgrade,
    /// A configuration change a `CancelDomainConfigChange` ended before it landed — the
    /// one way a domain becomes quiet without the clock landing anything.
    CancelledConfigurationChange,
}

impl Finished {
    /// How a failure names this case.
    fn case(self) -> &'static str {
        match self {
            Self::Provisioning => "its provisioning",
            Self::ConfigurationChange => "a configuration change",
            Self::ServiceSoftwareDeployment => "a service software deployment",
            Self::EngineVersionUpgrade => "an earlier engine version upgrade",
            Self::CancelledConfigurationChange => "a cancelled configuration change",
        }
    }

    /// The domain this case drives, inside `DomainName`'s `@length(min: 3, max: 28)` and
    /// its `^[a-z][a-z0-9\-]+$` pattern.
    fn domain(self) -> &'static str {
        match self {
            Self::Provisioning => "ready-after-provisioning",
            Self::ConfigurationChange => "ready-after-a-change",
            Self::ServiceSoftwareDeployment => "ready-after-a-deployment",
            Self::EngineVersionUpgrade => "ready-after-an-upgrade",
            Self::CancelledConfigurationChange => "ready-after-a-cancel",
        }
    }
}

/// The hop a case upgrades along: a source the service publishes targets for and the next
/// version it reaches, read from [`super::hops`] — the table the SERVICE publishes
/// (`GetCompatibleVersions`) — rather than written here, so the pair is one this service
/// admits.
async fn first_hop(emulator: &Emulator) -> (String, String) {
    let case = "the published compatibility table";
    let hop = hops(emulator)
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case}: expected the table to publish a hop"));
    (hop.source, hop.target)
}

/// The versions the service publishes as upgrade targets of `source`.
async fn published_targets(emulator: &Emulator, source: &str, case: &str) -> Vec<String> {
    let answer = compatible(emulator, None).await;
    published_entries(&answer, case)
        .iter()
        .find(|entry| string_member(entry, "SourceVersion", case) == source)
        .map(|entry| target_versions(entry, case))
        .unwrap_or_else(|| {
            panic!("case {case}: expected the published table to carry a row for {source}")
        })
}

/// The quiescence a client waits for before it starts further work on a domain: the FIRST
/// read that reports it created with nothing in flight — [`settled_domain`] polls for
/// exactly that condition — CONFIRMED by the next read, which is what a readiness poll
/// written for this service requires of two consecutive observations. Answers the
/// descriptor the confirming read saw.
///
/// Waits for the CONDITION rather than for a stretch of time, so a case passes on a fast
/// host and on a loaded one.
async fn quiescent(emulator: &Emulator, name: &str, case: &str) -> Value {
    settled_domain(emulator, name).await;
    let again = descriptor(&emulator.describe(name).await, case);
    assert!(
        again["Created"].as_bool() == Some(true) && !flag(&again, "Processing", case),
        "case {case}: expected the read confirming an idle domain to report it created \
         with nothing in flight, got {again:?}"
    );
    again
}

/// Put the domain of `finished` through the work that case is about, and answer the engine
/// version it runs afterwards together with the version its upgrade then asks for.
///
/// Every case starts from a created domain, which is also the case that asserts nothing
/// more: [`super::idle_domain`] is what creates it, so the transient state a create
/// answers with is asserted once, where it belongs.
async fn finished_working(
    emulator: &Emulator,
    finished: Finished,
    hop: &(String, String),
    case: &str,
) -> (String, String) {
    let name = finished.domain();
    let (source, target) = (hop.0.clone(), hop.1.clone());
    idle_domain(emulator, name, &source, case).await;

    match finished {
        Finished::Provisioning => (source, target),
        Finished::ConfigurationChange => {
            let answer = emulator.update_config(name, a_configuration_change()).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the configuration change accepted, got {} with {:?}",
                answer.status, answer.body
            );
            (source, target)
        }
        Finished::ServiceSoftwareDeployment => {
            let running = string_member(
                &quiescent(emulator, name, case).await["ServiceSoftwareOptions"],
                "CurrentVersion",
                case,
            );
            // The release the deployment moves the domain onto: a precondition written
            // into this service's own store, the way the sibling service-software suites
            // write theirs.
            seed(
                emulator,
                name,
                with_update_available(&running, AVAILABLE_RELEASE),
            );
            let answer = start_service_software(emulator, name, json!({})).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the deployment to start, got {} with {:?}",
                answer.status, answer.body
            );
            (source, target)
        }
        Finished::EngineVersionUpgrade => {
            let answer = upgrade(emulator, name, &target, false).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the earlier upgrade accepted, got {} with {:?}",
                answer.status, answer.body
            );
            // The earlier run is carried to its terminal step by the service, so what the
            // case then asserts is an upgrade issued on a domain whose last run LANDED.
            polled_to_terminal(emulator, name, case).await;
            let further = published_targets(emulator, &target, case).await;
            let beyond = nearest(&further, "OpenSearch_", case);
            (target, beyond)
        }
        Finished::CancelledConfigurationChange => {
            let answer = emulator.update_config(name, a_configuration_change()).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected the configuration change accepted, got {} with {:?}",
                answer.status, answer.body
            );
            let cancelled = cancel(emulator, name, None).await;
            assert_eq!(
                cancelled.status, 200,
                "case {case}: expected the change in flight cancelled, got {} with {:?}",
                cancelled.status, cancelled.body
            );
            (source, target)
        }
    }
}

/// THE RULE, for a domain that has just finished one kind of work: the readiness it
/// publishes and the admission of further work on it are one condition — the upgrade
/// issued on the first observation of a quiet domain is accepted, and the service alone
/// then carries that run to its terminal `UPGRADE` step, its history entry and the engine
/// version it asked for.
async fn an_upgrade_issued_on_the_first_quiet_read_converges(finished: Finished) {
    let emulator = Emulator::new();
    let hop = first_hop(&emulator).await;
    let case = format!("a domain that has just finished {}", finished.case());
    let name = finished.domain();
    let (source, target) = finished_working(&emulator, finished, &hop, &case).await;

    // What a client waits for, and the whole of what it waits for: the domain reports
    // itself quiet, and the upgrade issued on that observation is ADMITTED — no refusal
    // about background tasks stands between the two.
    let idle = quiescent(&emulator, name, &case).await;
    assert_eq!(
        string_member(&idle, "EngineVersion", &case),
        source,
        "case {case}: expected the domain on the version it runs before the upgrade, got \
         {idle:?}"
    );
    let answer = upgrade(&emulator, name, &target, false).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the upgrade to {target} admitted on the observation the \
         client waited for, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        !string_member(&answer.body, "UpgradeId", &case).is_empty(),
        "case {case}: expected the run started to be identified, got {:?}",
        answer.body
    );

    // From here the only call made is the one a client is told to poll: nothing is asked
    // to carry the run forward.
    let converged = polled_to_terminal(&emulator, name, &case).await;
    assert_eq!(
        converged.step, UPGRADE_STEP_UPGRADE,
        "case {case}: expected the run to finish on the upgrade itself, got {:?} / {:?}",
        converged.step, converged.step_status
    );
    assert_eq!(
        converged.upgrade_name,
        expected_name(&source, &target, false),
        "case {case}: expected the run described by the versions it goes between, got {:?}",
        converged.upgrade_name
    );

    let entry = newest_entry(&emulator, name, &case).await;
    assert_terminal_entry(&entry, &converged.upgrade_name, &converged.step_status, &case);

    let landed = descriptor(&emulator.describe(name).await, &case);
    assert_eq!(
        string_member(&landed, "EngineVersion", &case),
        target,
        "case {case}: expected the domain on the version the upgrade named, got {landed:?}"
    );
    assert!(
        !flag(&landed, "Processing", &case) && !flag(&landed, "UpgradeProcessing", &case),
        "case {case}: expected the domain idle again once the run is terminal, got {landed:?}"
    );
}

#[tokio::test]
async fn a_domain_that_has_just_been_provisioned_admits_the_upgrade_and_converges() {
    an_upgrade_issued_on_the_first_quiet_read_converges(Finished::Provisioning).await;
}

#[tokio::test]
async fn a_domain_whose_configuration_change_landed_admits_the_upgrade_and_converges() {
    an_upgrade_issued_on_the_first_quiet_read_converges(Finished::ConfigurationChange).await;
}

#[tokio::test]
async fn a_domain_whose_service_software_deployment_landed_admits_the_upgrade_and_converges() {
    an_upgrade_issued_on_the_first_quiet_read_converges(Finished::ServiceSoftwareDeployment).await;
}

#[tokio::test]
async fn a_domain_whose_earlier_upgrade_landed_admits_the_next_one_and_converges() {
    an_upgrade_issued_on_the_first_quiet_read_converges(Finished::EngineVersionUpgrade).await;
}

#[tokio::test]
async fn a_domain_whose_change_was_cancelled_admits_the_upgrade_and_converges() {
    an_upgrade_issued_on_the_first_quiet_read_converges(Finished::CancelledConfigurationChange)
        .await;
}
