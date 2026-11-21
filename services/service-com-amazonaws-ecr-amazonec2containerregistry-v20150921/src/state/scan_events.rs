// SPDX-License-Identifier: CC0-1.0

//! The EventBridge event this registry publishes when a basic image scan FINISHES, and the
//! scan a push starts on a repository configured to scan images on push.
//!
//! # The rule this module implements
//!
//! A repository whose scanning configuration asks for it is scanned on every push: "the
//! setting that determines whether images are scanned after being pushed to a repository. If
//! set to `true`, images will be scanned after being pushed"
//! (`ImageScanningConfiguration$scanOnPush`) — and the registry-wide scanning configuration
//! asks for the same thing for the repositories its `SCAN_ON_PUSH` rules match. So a push
//! into such a repository starts the same basic scan `StartImageScan` starts, under the same
//! published limits, and the scan is readable afterwards through the operations that report
//! one.
//!
//! A basic scan is a RUN with a beginning and an end, and its end is an event: once the
//! clock has made a run's completion due, the registry publishes it onto the account's
//! default event bus ([`super::image_events::DEFAULT_EVENT_BUS`]) under the `source` this
//! service owns ([`super::image_events::EVENT_SOURCE`]) and the `detail-type` of an image
//! scan ([`IMAGE_SCAN_DETAIL_TYPE`]), carrying in the event's `detail` the status the run
//! ended in, the repository and image it examined, that image's tags and how many findings
//! it reports per severity. A rule on that bus whose `EventPattern` matches then delivers
//! it to that rule's targets, which is how a scan result reaches a queue, a function or a
//! workflow without the client polling for it.
//!
//! # How a run becomes an event
//!
//! Nothing here decides what the scan came to: the run recorded on the image does
//! ([`super::ImageScan`]), so the event is READ BACK OUT of that record at the instant its
//! completion falls due — the same record `DescribeImageScanFindings` answers from. A run
//! that was replaced by a later one, an image that was deleted before its scan finished and
//! a repository that is gone publish nothing, because there is no completed run to report.
//!
//! The publication is armed by the call that STARTED the run — or, for a push of an image
//! that is not scanned again inside its window, by the push that found the run already
//! finished — and runs on a task of its own: the start answers while the scan is still in
//! its non-terminal status, so there is nobody left to publish for when the completion falls
//! due. The task holds no lock of this store across the hop into EventBridge, and
//! publication is best effort exactly as [`super::image_events`]'s is — an emulator with no
//! EventBridge mounted has nowhere to publish, and the scan itself still stands.

use ::std::collections::BTreeMap;
use ::std::sync::Arc;
use ::std::time::SystemTime;

use ::smithy_server_runtime::EmbeddedEmulator;

use super::image_events::{DEFAULT_EVENT_BUS, EVENT_SOURCE};
use super::{service_state, ComAmazonawsEcrAmazonec2containerregistryV20150921State};

/// The `detail-type` of the event published when a basic image scan has finished.
pub const IMAGE_SCAN_DETAIL_TYPE: &str = "ECR Image Scan";

/// How many armed scan publications may be waiting for their completion at once, over this
/// registry.
///
/// THIS CEILING IS THE EMULATOR'S, NOT AWS'S: every push into a repository scanned on push
/// arms one, so a client driving pushes drives tasks, and this is what keeps the work in
/// flight bounded however many pushes arrive. Past it the run is still started, recorded
/// and readable — only its event is not published, which is the same best-effort standing
/// the publication has when no event bus is mounted at all. It is counted on the registry's
/// own store
/// ([`ComAmazonawsEcrAmazonec2containerregistryV20150921State::armed_scan_publications`]),
/// so two emulators in one process never spend each other's budget.
pub const MAX_ARMED_PUBLICATIONS: usize = 256;

/// How long past a run's due instant a publication looks for its completion.
///
/// The due instant is stated on the WALL clock and the wait is served by the MONOTONIC one,
/// so a sleep that ends at the due instant can still find the wall clock a moment short of
/// it; this is the margin that keeps the first look from being too early.
const DUE_SLACK: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// How many times one armed publication looks for its run's completion before giving up.
///
/// A look that finds the run still going waits [`DUE_SLACK`] and looks again, so this bounds
/// the work one arm can do when the two clocks disagree by more than that margin. The scan
/// itself is readable whether or not the event is ever published.
const PUBLICATION_LOOKS: usize = 8;

/// A basic scan that was just STARTED, as the call that started it hands it over for
/// publication.
///
/// It names the run rather than describing it: what the run came to is read off the stored
/// record when its completion falls due, so an event never states an outcome that was
/// decided anywhere but on the image.
#[derive(Debug, Clone)]
pub struct StartedScan {
    /// The Region of the registry holding that repository: a repository's identity is its
    /// account, its Region and its name ([`super::repository_key`]).
    pub region: String,
    /// The repository holding the image the run examines.
    pub repository_name: String,
    /// The content digest of that image's manifest.
    pub image_digest: String,
    /// When the call that started this run was made — what identifies THIS run among the
    /// runs that image may carry over its life, so a run replaced by a later start is not
    /// published in that later run's place.
    pub started_at: SystemTime,
    /// When the clock owes this run its completion ([`super::IMAGE_SCAN_DELAY`]): the
    /// instant the event reports and the instant it is published at.
    pub complete_due_at: SystemTime,
}

/// One finished basic scan, as the event published for it reports it.
///
/// Every field is read off the run recorded on the image, so the event reports the scan a
/// read of the same instant reports and never a second, invented copy of it.
#[derive(Debug, Clone)]
pub struct ScanCompletion {
    /// The repository holding the scanned image.
    pub repository_name: String,
    /// That repository's ARN, which the event names as the resource it is about.
    pub repository_arn: String,
    /// The content digest of the scanned image's manifest.
    pub image_digest: String,
    /// Every tag pointing at that image when the run finished. An untagged image carries
    /// none, which the event states as an empty list.
    pub image_tags: Vec<String>,
    /// The terminal `ScanStatus` the run ended in — [`super::SCAN_STATUS_COMPLETE`] for a
    /// run that examined the image, [`super::SCAN_STATUS_FAILED`] for one that could not.
    pub scan_status: String,
    /// How many findings the run reports per `FindingSeverity`. A run that reports none
    /// counts none, which is the empty object.
    pub finding_severity_counts: BTreeMap<String, i32>,
    /// WHEN the run finished — the instant the clock owed it, not the instant the event
    /// happened to be published.
    pub at: SystemTime,
}

impl ScanCompletion {
    /// The event `detail` this finished run is reported in.
    ///
    /// Public because it IS the contract: the members a rule's `EventPattern` filters on,
    /// which is what a suite of this crate holds the published event to.
    #[must_use]
    pub fn detail(&self) -> ::serde_json::Value {
        let mut detail = ::serde_json::Map::new();
        detail.insert(
            "scan-status".to_string(),
            ::serde_json::Value::String(self.scan_status.clone()),
        );
        detail.insert(
            "repository-name".to_string(),
            ::serde_json::Value::String(self.repository_name.clone()),
        );
        detail.insert(
            "image-digest".to_string(),
            ::serde_json::Value::String(self.image_digest.clone()),
        );
        detail.insert(
            "image-tags".to_string(),
            ::serde_json::Value::Array(
                self.image_tags
                    .iter()
                    .map(|tag| ::serde_json::Value::String(tag.clone()))
                    .collect(),
            ),
        );
        detail.insert(
            "finding-severity-counts".to_string(),
            ::serde_json::Value::Object(
                self.finding_severity_counts
                    .iter()
                    .map(|(severity, count)| {
                        (
                            severity.clone(),
                            ::serde_json::Value::Number((*count).into()),
                        )
                    })
                    .collect(),
            ),
        );
        ::serde_json::Value::Object(detail)
    }
}

impl super::Repo {
    /// Whether this repository's OWN scanning configuration asks for images pushed into it
    /// to be scanned on arrival.
    ///
    /// The ONE reading of that setting: "the setting that determines whether images are
    /// scanned after being pushed to a repository. If set to `true`, images will be scanned
    /// after being pushed" (`ImageScanningConfiguration$scanOnPush`), modelled
    /// `@default(false)`. Both the push and the scanning configuration this repository
    /// reports read it here, so the two can never disagree.
    #[must_use]
    pub fn scans_on_push(&self) -> bool {
        self.image_scanning_config.scan_on_push
    }
}

impl ComAmazonawsEcrAmazonec2containerregistryV20150921State {
    /// Whether an image pushed into the repository `repository_name` is scanned on arrival.
    ///
    /// Two standing instructions ask for it, and either is enough:
    ///
    /// * the repository's own `imageScanningConfiguration`
    ///   ([`super::Repo::scans_on_push`]);
    /// * a rule of the REGISTRY's scanning configuration asking for the `SCAN_ON_PUSH`
    ///   frequency whose repository filters match this repository — the registry-wide
    ///   instruction `PutRegistryScanningConfiguration` stores, whose filter is matched as
    ///   the wildcard its only `filterType` names. A rule carrying no filter at all covers
    ///   every repository, the reading the sibling registry-wide configuration documents for
    ///   its own filters ("if no filters are added, the contents of all repositories are
    ///   replicated", `RepositoryFilter`).
    ///
    /// Only `BASIC` scanning is answered for here: with the `ENHANCED` scan type the scans
    /// are Amazon Inspector's continuous ones, which this registry does not run, so a
    /// registry configured for enhanced scanning starts no basic scan of its own on a push.
    #[must_use]
    pub fn image_is_scanned_on_push(&self, region: &str, repository_name: &str) -> bool {
        if self
            .repository(region, repository_name)
            .is_ok_and(super::Repo::scans_on_push)
        {
            return true;
        }
        let configuration = self.registry_scanning_configuration();
        if configuration.scan_type.as_deref() != Some(super::scanning::BASIC_SCAN_TYPE) {
            return false;
        }
        configuration
            .rules
            .unwrap_or_default()
            .iter()
            .filter(|rule| rule.scan_frequency == super::SCAN_ON_PUSH_FREQUENCY)
            .any(|rule| super::scanning::rule_covers(rule, repository_name))
    }

    /// The basic scan a push of the image `digest` names into the repository
    /// `repository_name` STARTS, so the push can arm the publication of its completion, or
    /// `None` where the push starts none.
    ///
    /// A scan is owed exactly where an image pushed into that repository is scanned on
    /// arrival ([`Self::image_is_scanned_on_push`]). The run it starts is the same one `StartImageScan`
    /// starts, recorded on the image by the same writer
    /// ([`super::Repo::start_basic_scan`]), so a scan on push and a manual scan are one
    /// kind of run and every reader reports them alike.
    ///
    /// It is owed under the published limits that COUNT SCANS rather than callers: an image
    /// already scanned inside [`super::BASIC_SCAN_INTERVAL`] is not scanned again ("a basic
    /// image scan can only be started once per 24 hours on an individual image. This limit
    /// includes if an image was scanned on initial push"), and a registry that has spent
    /// its [`super::MAX_BASIC_SCANS_PER_INTERVAL`] budget starts no further scan. Unlike a
    /// manual start, which refuses the call, a push that owes no scan simply stores the
    /// image: the push is the caller's action, and the scan is the registry's.
    ///
    /// What the run comes to is NOT decided here: every pushed image is examined, and where
    /// basic scanning cannot examine the one that arrived — an image whose filesystem
    /// carries no operating system and no package database, and a document that declares no
    /// filesystem at all — the run ends at the status that says so
    /// ([`super::basic_scan_outcome`]), which is the outcome a scan on push reports for an
    /// image of an unsupported type. A manual start has a call to refuse for that
    /// (`UnsupportedImageTypeException`); a push has none, and the registry's answer is the
    /// run it recorded.
    pub fn start_scan_on_push(
        &mut self,
        region: &str,
        repository_name: &str,
        digest: &str,
        now: SystemTime,
    ) -> Option<StartedScan> {
        // The registry-wide budget spans every repository, so it is read before one
        // repository is borrowed for writing — and so is the standing instruction that
        // decides whether this push is scanned at all.
        if !self.image_is_scanned_on_push(region, repository_name) {
            return None;
        }
        let scans_started = self.basic_scans_started_within(super::BASIC_SCAN_INTERVAL, now);
        let repo = self.repository_mut(region, repository_name).ok()?;
        let image = repo.images.get(digest)?;
        // An image already scanned inside its window is not scanned again: "a basic image
        // scan can only be started once per 24 hours on an individual image. This limit
        // includes if an image was scanned on initial push" (`StartImageScan`). No run
        // begins on this push, so there is no completion for it to announce either — one
        // event is published per scan that finishes, by the call that started that scan.
        if image
            .scan
            .as_ref()
            .is_some_and(|scan| !scan.may_restart_at(now))
        {
            return None;
        }
        if scans_started >= super::MAX_BASIC_SCANS_PER_INTERVAL {
            ::tracing::warn!(
                repository = %repository_name,
                image = %digest,
                "this registry has started the maximum of \
                 {} basic scans in the last {} hours; the pushed image is not scanned",
                super::MAX_BASIC_SCANS_PER_INTERVAL,
                super::BASIC_SCAN_INTERVAL.as_secs() / 3_600
            );
            return None;
        }
        let id = crate::ops::types::ImageIdentifier {
            image_digest: Some(digest.to_string()),
            image_tag: None,
        };
        repo.start_basic_scan(&id, now).ok()?;
        // The run is reported from the record the start wrote, so the publication is armed
        // for the instant that record states and never for a second computation of it.
        let scan = repo.images.get(digest)?.scan.as_ref()?;
        Some(StartedScan {
            region: region.to_string(),
            repository_name: repository_name.to_string(),
            image_digest: digest.to_string(),
            started_at: scan.started_at,
            complete_due_at: scan.complete_due_at,
        })
    }
}

/// The finished run `started` names, as the store holds it at `now`, or `None` where there
/// is nothing to report.
///
/// Nothing is reported for a repository or an image the registry no longer holds, for a run
/// whose completion the clock has not made due yet, and for an image whose run is not the
/// one that was armed — a later start replaces it, and that start arms its own publication.
#[must_use]
pub fn completed_run(
    store: &ComAmazonawsEcrAmazonec2containerregistryV20150921State,
    started: &StartedScan,
    now: SystemTime,
) -> Option<ScanCompletion> {
    let repo = store
        .repository(&started.region, &started.repository_name)
        .ok()?;
    let image = repo.images.get(&started.image_digest)?;
    let scan = image.scan.as_ref()?;
    if scan.started_at != started.started_at || !scan.is_complete(now) {
        return None;
    }
    Some(ScanCompletion {
        repository_name: repo.name.clone(),
        repository_arn: repo.arn.clone(),
        image_digest: image.digest.clone(),
        image_tags: image.tags.iter().cloned().collect(),
        scan_status: scan
            .to_status(now)
            .status
            .unwrap_or_else(|| super::SCAN_STATUS_FAILED.to_string()),
        finding_severity_counts: scan.finding_severity_counts().into_iter().collect(),
        at: scan.complete_due_at,
    })
}

/// Arm the publication of the run `started` names for the instant its completion falls due.
///
/// Returns at once: the call that started the run has to answer while the scan is still
/// `IN_PROGRESS`, so the event is published from a task of its own. The task waits for the
/// instant the clock owes the completion — it does not stand in for it, because the run's
/// own record is what every reader derives the status from — and then publishes whatever
/// the store holds for that run ([`completed_run`]), so a run that is gone or was replaced
/// publishes nothing.
///
/// The wait is measured against the wall clock the record's due instant is stated in, while
/// the sleep is measured on the monotonic one, so a run can still read as going when the
/// sleep ends: the task then looks again, up to [`PUBLICATION_LOOKS`] times
/// [`DUE_SLACK`] apart, rather than dropping an event a reader can already see the
/// completion of.
pub fn publish_when_due<Eng: Send + Sync + 'static>(
    engine: Arc<Eng>,
    callback: &EmbeddedEmulator,
    region: &str,
    started: StartedScan,
) {
    // The arm is counted on the registry's own store BEFORE the task starts, so concurrent
    // pushes cannot each see a free slot; the guard gives the count back however the task
    // ends — including a task dropped at runtime shutdown or unwound by a panic, which a
    // trailing statement would leak.
    let Some(arm) = ArmedPublication::take(&engine) else {
        ::tracing::warn!(
            repository = %started.repository_name,
            image = %started.image_digest,
            "{MAX_ARMED_PUBLICATIONS} Amazon ECR image scan completions are already waiting \
             to be published; this scan runs and is readable, but its event is not published"
        );
        return;
    };
    let callback = callback.clone();
    let region = region.to_string();
    ::tokio::spawn(async move {
        let _arm = arm;
        let mut wait = started
            .complete_due_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            // A due instant a clock jump put far in the future cannot make this task wait
            // longer than the whole span the completion was promised in: a run is queued
            // and then runs, so that span is both of the clock's spans together.
            .min(super::IMAGE_SCAN_QUEUE_DELAY + super::IMAGE_SCAN_DELAY)
            .saturating_add(DUE_SLACK);
        for _ in 0..PUBLICATION_LOOKS {
            ::tokio::time::sleep(wait).await;
            // The guard is dropped before EventBridge is reached, so this service's store
            // is never held across a call into another service's.
            let Some(state) = service_state(&*engine) else {
                break;
            };
            let found = {
                let store = state.lock();
                let still_running = running_run(&store, &started, SystemTime::now());
                (
                    completed_run(&store, &started, SystemTime::now()),
                    still_running,
                )
            };
            match found {
                (Some(completion), _) => {
                    publish(&callback, &region, &completion).await;
                    break;
                }
                // The record is gone, or the image carries another run now: there is no
                // completion of THIS run for anybody to be told about.
                (None, false) => break,
                // Still going: look again.
                (None, true) => wait = DUE_SLACK,
            }
        }
    });
}

/// Whether the store still holds the run `started` names at `now` and that run has not
/// finished yet — the one case in which waiting longer can still produce a completion.
fn running_run(
    store: &ComAmazonawsEcrAmazonec2containerregistryV20150921State,
    started: &StartedScan,
    now: SystemTime,
) -> bool {
    store
        .repository(&started.region, &started.repository_name)
        .ok()
        .and_then(|repo| repo.images.get(&started.image_digest))
        .and_then(|image| image.scan.as_ref())
        .is_some_and(|scan| scan.started_at == started.started_at && !scan.is_complete(now))
}

/// Publish one finished basic scan onto the account's default EventBridge bus.
///
/// Called with NO store lock held: the publication reaches EventBridge's own handler, which
/// matches the event against the account's rules and delivers it to their targets — and a
/// target may call back into this service.
pub async fn publish(callback: &EmbeddedEmulator, region: &str, completion: &ScanCompletion) {
    let client = ::service_common::eventbridge_client::eventbridge_client(callback, region);
    let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
        .event_bus_name(DEFAULT_EVENT_BUS)
        .source(EVENT_SOURCE)
        .detail_type(IMAGE_SCAN_DETAIL_TYPE)
        // The event's `time` is WHEN the scan finished:
        // `PutEventsRequestEntry$Time` is "the time stamp of the event, per RFC3339. If no
        // time stamp is provided, the time stamp of the PutEvents call is used."
        .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
            super::image_events::epoch_millis(completion.at),
        ))
        // The resource the event is about: the repository holding the scanned image.
        .resources(&completion.repository_arn)
        .detail(completion.detail().to_string())
        .build();
    if let Err(error) = client.put_events().entries(entry).send().await {
        ::tracing::warn!(
            repository = %completion.repository_name,
            image = %completion.image_digest,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "an Amazon ECR image scan completion could not be published to EventBridge"
        );
    }
}

/// One slot of [`MAX_ARMED_PUBLICATIONS`] on ONE registry's store, held for as long as the
/// publication it was taken for exists.
///
/// The slot is given back in `Drop` rather than at the end of the task, so a task the
/// runtime drops while it waits, one that is aborted and one that unwinds all return their
/// slot: a slot that leaked would never come back, and past the ceiling every later scan
/// would run, be readable, and silently publish nothing.
struct ArmedPublication<Eng: Send + Sync + 'static> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> ArmedPublication<Eng> {
    /// Take a slot on the registry `engine` holds, or `None` where it already holds
    /// [`MAX_ARMED_PUBLICATIONS`] of them (or holds no such store at all).
    fn take(engine: &Arc<Eng>) -> Option<Self> {
        let state = service_state(&**engine)?;
        let mut store = state.lock();
        if store.armed_scan_publications >= MAX_ARMED_PUBLICATIONS {
            return None;
        }
        store.armed_scan_publications += 1;
        drop(store);
        Some(Self {
            engine: Arc::clone(engine),
        })
    }
}

impl<Eng: Send + Sync + 'static> Drop for ArmedPublication<Eng> {
    fn drop(&mut self) {
        if let Some(state) = service_state(&*self.engine) {
            let mut store = state.lock();
            store.armed_scan_publications = store.armed_scan_publications.saturating_sub(1);
        }
    }
}

